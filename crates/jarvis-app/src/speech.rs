//! Синтез речи в отдельном потоке `jarvis-tts`: голос грузится ~0.5 с, фраза
//! синтезируется за 0.1–0.2 с — ядро не должно ждать ни того, ни другого.
//!
//! Ядро отправляет [`Speech::say`] с номером запроса и получает
//! [`SpeechEvent::Clip`] с тем же номером. Устаревшие запросы (ядро уже
//! попросило новую фразу) пропускаются, повторяющиеся фразы берутся из кэша.
//!
//! Голос занимает ~100 МБ, а в простое на всё приложение по ТЗ — 150 МБ.
//! Поэтому голос держится в памяти, только пока нужен: [`Speech::prepare`]
//! (ядро зовёт его на «Джарвис», пока человек договаривает команду) загружает
//! его заранее, а после [`IDLE_UNLOAD`] без фраз он выгружается.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use jarvis_audio::AudioError;
use jarvis_audio::tts::{PiperTts, TextToSpeech, VoiceModel};
use jarvis_audio::wav::Clip;
use jarvis_core::commands::Command;
use jarvis_core::slots::{SlotValues, fill};

/// Сколько последних фраз держать готовыми.
const CACHE_SIZE: usize = 16;
/// Столько без фраз — и голос выгружается из памяти.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(30);

pub enum SpeechEvent {
    /// Голос проверен при старте (загружен и прогрет).
    Ready {
        voice: String,
        load_time: Duration,
    },
    /// Голос не загрузился — ответы остаются звуковым пакетом.
    Unavailable(String),
    Clip {
        id: u64,
        clip: Arc<Clip>,
        synth_time: Duration,
    },
    Failed {
        id: u64,
        why: String,
    },
}

pub type SpeechSink = Box<dyn Fn(SpeechEvent) + Send>;
type Loader = Box<dyn Fn() -> Result<Box<dyn TextToSpeech>, AudioError> + Send>;

enum Request {
    Say {
        id: u64,
        text: String,
    },
    /// Скоро может понадобиться — загрузить голос заранее.
    Prepare,
}

/// Ручка потока синтеза. При удалении поток завершается.
pub struct Speech {
    requests: Sender<Request>,
}

impl Speech {
    pub fn spawn(
        voice_name: String,
        voice: VoiceModel,
        speed: f32,
        events: SpeechSink,
    ) -> std::io::Result<Self> {
        let loader: Loader = Box::new(move || {
            PiperTts::new(&voice, speed).map(|t| Box::new(t) as Box<dyn TextToSpeech>)
        });
        Self::spawn_with(voice_name, loader, IDLE_UNLOAD, events)
    }

    fn spawn_with(
        voice_name: String,
        loader: Loader,
        idle_unload: Duration,
        events: SpeechSink,
    ) -> std::io::Result<Self> {
        let (requests, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("jarvis-tts".into())
            .spawn(move || worker(voice_name, loader, idle_unload, events, rx))?;
        Ok(Self { requests })
    }

    pub fn say(&self, id: u64, text: String) {
        let _ = self.requests.send(Request::Say { id, text });
    }

    pub fn prepare(&self) {
        let _ = self.requests.send(Request::Prepare);
    }
}

fn worker(
    voice_name: String,
    loader: Loader,
    idle_unload: Duration,
    events: SpeechSink,
    rx: Receiver<Request>,
) {
    // Первая загрузка — проверка, что голос рабочий.
    let started = Instant::now();
    let mut tts = match loader() {
        Ok(tts) => Some(tts),
        Err(e) => {
            events(SpeechEvent::Unavailable(jarvis_core::error_chain(&e)));
            return;
        }
    };
    events(SpeechEvent::Ready {
        voice: voice_name,
        load_time: started.elapsed(),
    });
    let mut cache = Cache::default();
    loop {
        let request = if tts.is_some() {
            match rx.recv_timeout(idle_unload) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    tts = None;
                    tracing::debug!("голос синтеза выгружен до следующей фразы");
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(r) => r,
                Err(_) => return,
            }
        };
        // Пока синтезировали прошлую фразу, ядро могло попросить новые —
        // нужна только последняя.
        let mut say = match request {
            Request::Say { id, text } => Some((id, text)),
            Request::Prepare => None,
        };
        while let Ok(newer) = rx.try_recv() {
            if let Request::Say { id, text } = newer {
                say = Some((id, text));
            }
        }
        let started = Instant::now();
        if say.as_ref().is_some_and(|(_, text)| cache.peek(text)) {
            // Фраза уже готова — голос не нужен.
        } else if tts.is_none() {
            match loader() {
                Ok(t) => {
                    tracing::debug!(
                        ms = started.elapsed().as_millis() as u64,
                        "голос синтеза загружен"
                    );
                    tts = Some(t);
                }
                Err(e) => {
                    let why = jarvis_core::error_chain(&e);
                    tracing::error!(%why, "голос синтеза не загрузился");
                    if let Some((id, _)) = say {
                        events(SpeechEvent::Failed { id, why });
                    }
                    continue;
                }
            }
        }
        let Some((id, text)) = say else {
            continue;
        };
        let result = match (cache.get(&text), &tts) {
            (Some(clip), _) => Ok(clip),
            (None, Some(engine)) => engine.synthesize(&text).map(|clip| {
                let clip = Arc::new(clip.trimmed());
                cache.put(text, Arc::clone(&clip));
                clip
            }),
            (None, None) => continue, // недостижимо: голос загружен выше
        };
        events(match result {
            Ok(clip) => SpeechEvent::Clip {
                id,
                clip,
                synth_time: started.elapsed(),
            },
            Err(e) => SpeechEvent::Failed {
                id,
                why: jarvis_core::error_chain(&e),
            },
        });
    }
}

/// Кэш последних фраз: вопрос-разрешение одной и той же команды звучит
/// одинаково, синтезировать его заново незачем.
#[derive(Default)]
struct Cache {
    entries: Vec<(String, Arc<Clip>)>,
}

impl Cache {
    fn get(&mut self, text: &str) -> Option<Arc<Clip>> {
        let index = self.entries.iter().position(|(t, _)| t == text)?;
        let entry = self.entries.remove(index);
        let clip = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(clip)
    }

    fn peek(&self, text: &str) -> bool {
        self.entries.iter().any(|(t, _)| t == text)
    }

    fn put(&mut self, text: String, clip: Arc<Clip>) {
        if self.entries.len() >= CACHE_SIZE {
            self.entries.remove(0);
        }
        self.entries.push((text, clip));
    }
}

// ---------- тексты фраз ----------

/// Вопрос-разрешение: что именно произойдёт (раздел 2.1 ТЗ), со значениями
/// слотов («громкость на 40»).
pub fn ask_text(command: &Command, slots: &SlotValues) -> String {
    format!(
        "Сэр, команда «{}» {}. Разрешить?",
        filled_phrase(command, slots),
        fill(&command.description, slots)
            .trim()
            .trim_end_matches('.')
    )
}

/// Первая фраза команды с подставленными слотами.
pub fn filled_phrase(command: &Command, slots: &SlotValues) -> String {
    let phrase = command
        .phrases
        .first()
        .map_or(command.id.as_str(), String::as_str);
    fill(phrase, slots)
}

pub fn ask_forget_permissions_text() -> String {
    "Сэр, забыть все разрешения? Я снова буду спрашивать перед каждой командой.".into()
}

/// Уточняющий вопрос, когда фраза одинаково похожа на две команды.
pub fn clarify_text(first: &str, second: &str) -> String {
    format!("Сэр, вы имели в виду «{first}» или «{second}»?")
}

pub fn elevated_warning_text() -> String {
    "Сэр, я запущен с правами администратора. Команды тоже получат эти права.".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::commands::{ActionSpec, ReplyChoice};
    use jarvis_core::state::Reply;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn command(description: &str) -> Command {
        Command {
            id: "open_youtube".into(),
            pack: "browser".into(),
            phrases: vec!["открой ютуб".into()],
            description: description.into(),
            reply: ReplyChoice::Reply(Reply::Done),
            always_ask: false,
            slots: Default::default(),
            action: ActionSpec::OpenUrl {
                url: "https://www.youtube.com".into(),
            },
            dir: PathBuf::from("commands/browser"),
        }
    }

    #[test]
    fn ask_names_command_and_effect() {
        let text = ask_text(&command("откроет YouTube в браузере."), &SlotValues::new());
        assert_eq!(
            text,
            "Сэр, команда «открой ютуб» откроет YouTube в браузере. Разрешить?"
        );
    }

    #[test]
    fn ask_says_slot_values() {
        let slots: SlotValues = [("level".to_owned(), "40".to_owned())]
            .into_iter()
            .collect();
        let mut c = command("установит громкость {level}%");
        c.phrases = vec!["громкость на {level}".into()];
        assert_eq!(
            ask_text(&c, &slots),
            "Сэр, команда «громкость на 40» установит громкость 40%. Разрешить?"
        );
    }

    /// Поддельный голос: считает загрузки, «синтезирует» тишину.
    struct FakeTts;
    impl TextToSpeech for FakeTts {
        fn synthesize(&self, text: &str) -> Result<Clip, AudioError> {
            Ok(Clip {
                samples: vec![0.5; text.len()],
                sample_rate: 16_000,
            })
        }
    }

    fn spawn_fake(idle: Duration) -> (Speech, Receiver<SpeechEvent>, Arc<AtomicUsize>) {
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&loads);
        let loader: Loader = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeTts) as Box<dyn TextToSpeech>)
        });
        let (tx, rx) = mpsc::channel();
        let sink: SpeechSink = Box::new(move |e| {
            let _ = tx.send(e);
        });
        let speech = Speech::spawn_with("fake".into(), loader, idle, sink).unwrap();
        (speech, rx, loads)
    }

    fn next_clip(rx: &Receiver<SpeechEvent>) -> u64 {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                SpeechEvent::Clip { id, .. } => return id,
                SpeechEvent::Failed { why, .. } => panic!("{why}"),
                _ => {}
            }
        }
    }

    #[test]
    fn voice_is_unloaded_when_idle_and_reloaded_on_demand() {
        let (speech, rx, loads) = spawn_fake(Duration::from_millis(50));
        speech.say(1, "раз".into());
        assert_eq!(next_clip(&rx), 1);
        assert_eq!(loads.load(Ordering::SeqCst), 1, "проверка при старте");

        std::thread::sleep(Duration::from_millis(200)); // голос выгружен
        speech.say(2, "два".into());
        assert_eq!(next_clip(&rx), 2);
        assert_eq!(loads.load(Ordering::SeqCst), 2, "загружен снова");

        std::thread::sleep(Duration::from_millis(200));
        speech.say(3, "два".into());
        assert_eq!(next_clip(&rx), 3);
        assert_eq!(loads.load(Ordering::SeqCst), 2, "из кэша — без загрузки");

        speech.prepare();
        speech.say(4, "четыре".into());
        assert_eq!(next_clip(&rx), 4);
        assert_eq!(loads.load(Ordering::SeqCst), 3, "prepare загружает заранее");
    }

    #[test]
    fn cache_keeps_recent_phrases() {
        let mut cache = Cache::default();
        let clip = || {
            Arc::new(Clip {
                samples: vec![0.0; 4],
                sample_rate: 16_000,
            })
        };
        for i in 0..CACHE_SIZE {
            cache.put(format!("фраза {i}"), clip());
        }
        assert!(cache.get("фраза 0").is_some(), "свежая после обращения");
        cache.put("новая".into(), clip());
        assert!(cache.get("фраза 1").is_none(), "самая старая вытеснена");
        assert!(cache.get("фраза 0").is_some());
        assert!(cache.get("новая").is_some());
    }
}

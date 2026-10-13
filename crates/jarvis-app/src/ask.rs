//! `jarvis --ask "вопрос"`: спросить нейросеть текстом и услышать ответ —
//! тот же «мозг», нарезка на предложения и очередь озвучки, что у демона,
//! без микрофона. Печатает ответ по мере прихода и замеры задержек.
//! Команды, которые попросит нейросеть, только называются — не выполняются.
//!
//! Для проверки без настоящего сервера адрес API можно подменить переменной
//! окружения `JARVIS_LLM_URL` (локальный поддельный сервер в тестах).

use std::io::Write as _;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_app::answer::SpeechQueue;
use jarvis_app::brain::{Brain, BrainEvent, ModelFactory, tool_commands};
use jarvis_audio::pack::VoicePack;
use jarvis_audio::player::Player;
use jarvis_audio::tts::{PiperTts, TextToSpeech};
use jarvis_core::Config;
use jarvis_core::commands::Registry;
use jarvis_core::config::resolve;
use jarvis_llm::claude::{API_URL, ClaudeClient, ClaudeSettings};
use jarvis_llm::{LanguageModel, LlmError};

pub fn run(root: &Path, config: &Config, question: &str) -> anyhow::Result<()> {
    let llm = &config.llm;
    let settings = ClaudeSettings {
        model: llm.model.clone(),
        effort: llm.effort.clone(),
        max_tokens: llm.max_tokens,
        thinking: llm.thinking.clone(),
        fallback: llm.fallback,
    };
    let url = std::env::var("JARVIS_LLM_URL").unwrap_or_else(|_| API_URL.to_owned());
    let factory: ModelFactory = Box::new(move || {
        let key = match std::env::var("JARVIS_LLM_URL") {
            // Поддельный сервер в тестах — настоящий ключ ему не нужен.
            Ok(_) => "sk-ant-local-test".to_owned(),
            Err(_) => jarvis_llm::secrets::api_key()?.ok_or(LlmError::NoKey)?,
        };
        Ok(
            Box::new(ClaudeClient::with_url(key, settings.clone(), url.clone())?)
                as Box<dyn LanguageModel>,
        )
    });
    let registry = Registry::load(&resolve(root, &config.paths.commands));

    // Голос: без него — только текст.
    let tts = config
        .voice
        .tts_voice()
        .and_then(|name| jarvis_audio::models::tts_voice(root, config, name).ok())
        .and_then(|model| PiperTts::new(&model, config.voice.tts_speed).ok());
    let pack = VoicePack::load(&resolve(root, &config.paths.sounds).join(&config.voice.pack))
        .context("звуковой пакет не загружен")?;
    let mut player = Player::new(&pack, config.voice.volume).context("вывод звука не открылся")?;

    let (tx, rx) = mpsc::channel();
    let brain = Brain::spawn(
        factory,
        tool_commands(&registry),
        Duration::from_secs(60),
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )?;
    println!("Вопрос: {question}");
    print!("Ответ: ");
    let _ = std::io::stdout().flush();
    let asked = Instant::now();
    brain.ask(1, question.to_owned());

    let mut queue: SpeechQueue<jarvis_audio::wav::Clip> = SpeechQueue::default();
    let mut printed = 0;
    let mut next_id = 0;
    let mut first_text = None;
    let mut first_audio = None;
    let mut done = false;
    let mut failed = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(BrainEvent::Text { text, .. }) => {
                first_text.get_or_insert_with(|| asked.elapsed());
                print!("{}", &text[printed..]);
                let _ = std::io::stdout().flush();
                printed = text.len();
            }
            Ok(BrainEvent::Sentence { text, .. }) => {
                if let Some(tts) = &tts {
                    next_id += 1;
                    queue.expect(next_id);
                    match tts.synthesize(&text) {
                        Ok(clip) => {
                            queue.ready(next_id, clip);
                        }
                        Err(e) => {
                            queue.failed(next_id);
                            tracing::warn!(error = %jarvis_core::error_chain(&e), "предложение не озвучено");
                        }
                    }
                }
            }
            Ok(BrainEvent::Done { commands, .. }) => {
                for c in commands {
                    print!(
                        "\n[нейросеть просит команду {}{} — в режиме --ask не выполняется]",
                        c.command,
                        c.value.map(|v| format!(" = {v}")).unwrap_or_default()
                    );
                }
                done = true;
            }
            Ok(BrainEvent::Failed { failure, .. }) => {
                failed = Some(failure);
                done = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => done = true,
        }
        let now = Instant::now();
        if let Some(clip) = queue.next(now) {
            let duration = player.play_clip(&clip)?;
            queue.playing(now, duration);
            first_audio.get_or_insert_with(|| asked.elapsed());
        }
        if done && queue.idle(now) {
            break;
        }
    }
    println!();
    if let Some(failure) = failed {
        anyhow::bail!("нейросеть не ответила: {failure:?}");
    }
    let ms = |d: Option<Duration>| d.map_or("—".into(), |d| format!("{} мс", d.as_millis()));
    println!(
        "Первый текст через {}, первый звук через {}, всего {:.1} с.",
        ms(first_text),
        ms(first_audio),
        asked.elapsed().as_secs_f32()
    );
    Ok(())
}

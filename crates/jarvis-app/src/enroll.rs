//! `jarvis --enroll`: запись образца голоса.
//!
//! Пользователь несколько раз говорит «Джарвис» — по подсказкам с разных
//! расстояний и с разной громкостью; детектор речи вырезает каждую попытку,
//! распознаватель проверяет, что это похоже на «Джарвис» (и запоминает, как
//! он слышит именно этот голос), модель говорящего считает вектор голоса.
//! Итог — `data/voiceprint.json` и таблица: на каких попытках настоящий
//! детектор «Джарвис» сработал бы без подсказок.
//!
//! `jarvis --voice-test`: те же попытки, но только проверка — сохранённый
//! образец голоса не меняется; в таблице — узнал бы Джарвис ваш голос.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use jarvis_audio::asr::{Endpointing, create_recognizer, transcribe};
use jarvis_audio::bench::run_wake;
use jarvis_audio::capture::Capture;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::resample::{MODEL_RATE, StreamResampler};
use jarvis_audio::speaker::{SpeakerEncoder, Voiceprint, similarity};
use jarvis_audio::vad::{Vad, VadSettings};
use jarvis_audio::wake::{SherpaWakeWord, WakeMatcher, normalize_words};
use jarvis_audio::wav;
use jarvis_core::Config;

use crate::hearing::data_dir;

pub const SAMPLES: u8 = 5;
/// Попыток в проверке по умолчанию.
pub const TEST_SAMPLES: u8 = 10;
/// Подсказка проверки: всегда одинаково — как вы обычно говорите.
const PLAIN_HINT: &str = "обычным голосом, как обычно";
/// Времени на одну попытку (с учётом пауз и переходов по комнате).
const TIME_PER_SAMPLE: Duration = Duration::from_secs(20);
/// Подсказки по порядку попыток: разные расстояния и громкость, чтобы образец
/// голоса и проверка охватили то, как вы будете говорить на самом деле.
const HINTS: &[&str] = &[
    "обычным голосом, у микрофона",
    "обычным голосом, на расстоянии вытянутой руки",
    "тихо, у микрофона",
    "громко, у микрофона",
    "обычным голосом, в 2–3 метрах",
    "тихо, на расстоянии вытянутой руки",
    "громко, в 2–3 метрах",
    "обычным голосом, отвернувшись от микрофона",
    "быстро и коротко, обычным голосом",
    "медленно, обычным голосом",
    "тихо, в 2–3 метрах",
    "громко, на расстоянии вытянутой руки",
    "обычным голосом, с другого конца комнаты",
    "вполголоса, как будто рядом кто-то спит",
    "обычным голосом, у микрофона — ещё раз",
];
/// Попытка короче или длиннее — точно не одно слово.
const MIN_SECS: f32 = 0.3;
const MAX_SECS: f32 = 2.5;
/// При записи допускаем больше неточностей, чем при срабатывании: цель —
/// узнать, как распознаватель пишет ваше «Джарвис».
const ENROLL_DISTANCE: usize = 2;

struct Enroller {
    recognizer: sherpa_onnx::OnlineRecognizer,
    encoder: SpeakerEncoder,
    matcher: WakeMatcher,
    /// Настоящий детектор «Джарвис» с настройками демона — для проверки.
    live: SherpaWakeWord,
    embeddings: Vec<Vec<f32>>,
    heard: Vec<String>,
    attempts: Vec<Attempt>,
    /// Проверка: с чем сравнивать голос (сохранённый образец).
    reference: Option<Voiceprint>,
}

/// Принятая попытка — для итоговой таблицы.
struct Attempt {
    hint: &'static str,
    peak_db: f32,
    rms_db: f32,
    /// Сработал бы детектор «Джарвис» без подстройки громкости.
    live: bool,
    /// Проверка: сходство с образцом голоса (как его считает работающий Джарвис).
    voice: Option<f32>,
    samples: Vec<f32>,
}

impl Enroller {
    fn new(models: &SpeechModels, config: &Config) -> anyhow::Result<Self> {
        let live_matcher = WakeMatcher::new(&config.wake.words, config.wake.max_edit_distance);
        Ok(Self {
            recognizer: create_recognizer(&models.asr, Endpointing::default())?,
            encoder: SpeakerEncoder::new(&models.speaker)?,
            matcher: WakeMatcher::new(&config.wake.words, ENROLL_DISTANCE),
            live: SherpaWakeWord::new(&models.asr, live_matcher)?,
            embeddings: Vec::new(),
            heard: Vec::new(),
            attempts: Vec::new(),
            reference: None,
        })
    }

    /// Принимает попытку. `Err` — текст, почему она не подошла.
    fn offer(&mut self, samples: &[f32], hint: &'static str) -> Result<String, String> {
        let secs = samples.len() as f32 / MODEL_RATE as f32;
        if !(MIN_SECS..=MAX_SECS).contains(&secs) {
            return Err(format!("длина {secs:.1} с — скажите одно слово «Джарвис»"));
        }
        // Сработал бы демон на эту попытку как есть — тихую, громкую.
        let live = !run_wake(&mut self.live, samples).detections.is_empty();
        let (peak_db, rms_db) = levels(samples);
        // Для образца голоса громкость выравнивается: тихая попытка тоже
        // годится, чтобы узнать тембр.
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let gain = if peak > 1e-4 { 0.5 / peak } else { 1.0 };
        let normalized: Vec<f32> = samples.iter().map(|s| s * gain).collect();
        let samples_raw = samples.to_vec();
        let samples = normalized.as_slice();
        let text = transcribe(&self.recognizer, samples);
        let words = normalize_words(&text);
        let Some(word) = words.iter().find(|w| self.matcher.matches(w)).cloned() else {
            let shown = if text.is_empty() {
                "ничего"
            } else {
                &text
            };
            return Err(format!("услышал «{shown}» — это не похоже на «Джарвис»"));
        };
        let embedding = self
            .encoder
            .embed(samples)
            .ok_or_else(|| "слишком коротко для распознавания голоса".to_owned())?;
        // Работающий Джарвис сравнивает голос по записи как есть, без выравнивания.
        let voice = self.reference.as_ref().map(|print| {
            self.encoder
                .embed(&samples_raw)
                .map_or(0.0, |e| print.similarity(&e))
        });
        self.embeddings.push(embedding);
        self.heard.push(word.clone());
        self.attempts.push(Attempt {
            hint,
            peak_db,
            rms_db,
            live,
            voice,
            samples: samples_raw,
        });
        Ok(word)
    }

    /// Итоговая таблица: громкость каждой попытки и сработал бы детектор.
    fn report(&self) {
        println!();
        println!(
            "{:>3}  {:<46} {:>8} {:>9}  сработал бы{}",
            "№",
            "как говорили",
            "пик, дБ",
            "средн., дБ",
            if self.reference.is_some() {
                "  голос узнан (сходство)"
            } else {
                ""
            }
        );
        for (i, a) in self.attempts.iter().enumerate() {
            let voice = match (a.voice, &self.reference) {
                (Some(s), Some(print)) => format!(
                    "  {:<11} ({s:.2})",
                    if s >= print.threshold {
                        "да"
                    } else {
                        "НЕТ"
                    }
                ),
                _ => String::new(),
            };
            println!(
                "{:>3}  {:<46} {:>8.0} {:>9.0}  {:<11}{voice}",
                i + 1,
                a.hint,
                a.peak_db,
                a.rms_db,
                if a.live { "да" } else { "НЕТ" }
            );
        }
        if let Some(print) = &self.reference {
            let known = self
                .attempts
                .iter()
                .filter(|a| a.voice.is_some_and(|s| s >= print.threshold))
                .count();
            let both = self
                .attempts
                .iter()
                .filter(|a| a.live && a.voice.is_some_and(|s| s >= print.threshold))
                .count();
            println!(
                "Голос узнан: {known} из {n} (порог {:.2}). Джарвис ответил бы: {both} из {n}.",
                print.threshold,
                n = self.attempts.len()
            );
        }
        let missed: Vec<&Attempt> = self.attempts.iter().filter(|a| !a.live).collect();
        if missed.is_empty() {
            println!("Детектор «Джарвис» сработал на всех попытках.");
        } else {
            let loudest_missed = missed.iter().map(|a| a.peak_db).fold(f32::MIN, f32::max);
            println!(
                "Детектор не сработал на {} из {} попыток (самая громкая из них — пик {loudest_missed:.0} дБ). \
                 Пришлите эту таблицу — подстрою чувствительность.",
                missed.len(),
                self.attempts.len()
            );
        }
    }

    /// Попытки в WAV (16 кГц, как их слышат модели) — для разбора.
    fn save_samples(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("не создать {}", dir.display()))?;
        for (i, a) in self.attempts.iter().enumerate() {
            let tag = if a.live { "ok" } else { "miss" };
            let path = dir.join(format!("{:02}_{tag}.wav", i + 1));
            wav::write(
                &path,
                &wav::Clip {
                    samples: a.samples.clone(),
                    sample_rate: MODEL_RATE,
                },
            )?;
        }
        let notes: String = self
            .attempts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                format!(
                    "{:02}\t{}\tпик {:.0} дБ\t{}\n",
                    i + 1,
                    a.hint,
                    a.peak_db,
                    if a.live {
                        "сработал"
                    } else {
                        "НЕ сработал"
                    }
                )
            })
            .collect();
        std::fs::write(dir.join("попытки.txt"), notes)?;
        println!("Записи попыток: {}", dir.display());
        Ok(())
    }

    fn finish(self, models: &SpeechModels, config: &Config) -> anyhow::Result<Voiceprint> {
        // Как распознаватель пишет ваше «Джарвис» — запоминаем непривычные написания.
        let known = WakeMatcher::new(&config.wake.words, config.wake.max_edit_distance);
        let mut heard_as: Vec<String> = self
            .heard
            .iter()
            .filter(|w| !known.matches(w))
            .cloned()
            .collect();
        heard_as.sort();
        heard_as.dedup();

        let print = Voiceprint::from_embeddings(&models.speaker, &self.embeddings, heard_as)
            .context("не удалось построить образец голоса")?;
        println!();
        println!("Сходство образцов с итоговым голосом:");
        for (i, e) in self.embeddings.iter().enumerate() {
            println!("  {}. {:.2}", i + 1, similarity(e, &print.embedding));
        }
        println!("Порог узнавания: {:.2}", print.threshold);
        if !print.heard_as.is_empty() {
            println!("Новые написания слова: {}", print.heard_as.join(", "));
        }
        Ok(print)
    }
}

/// Что делать с попытками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Записать новый образец голоса (подсказки — разные расстояния и громкость).
    Enroll,
    /// Только проверить: узнаёт ли Джарвис голос; образец не меняется.
    Test,
}

/// Запись с микрофона: `count` попыток; `save_samples` — сохранить их в
/// `data/enroll/<время>/` (по умолчанию звук на диск не пишется).
pub fn run_microphone(
    root: &Path,
    config: &Config,
    count: usize,
    save_samples: bool,
    mode: Mode,
) -> anyhow::Result<()> {
    // Работающий Джарвис отвечал бы на каждое «Джарвис» и мешал записи.
    let _instance = jarvis_platform::acquire_single_instance(crate::daemon::INSTANCE_NAME)?
        .context("сначала остановите Джарвиса: jarvis --stop")?;
    let models = SpeechModels::locate(root, config)?;
    let mut enroller = Enroller::new(&models, config)?;
    if mode == Mode::Test {
        enroller.reference = Some(
            Voiceprint::load(&data_dir(root, config), &models.speaker)?
                .context("образца голоса ещё нет — сначала запишите его: jarvis --enroll")?,
        );
    }
    let plain = mode == Mode::Test;
    let mut vad = Vad::new(
        &models.vad,
        VadSettings {
            min_silence: 0.4,
            ..VadSettings::default()
        },
    )?;
    let mut capture =
        Capture::open(config.audio.input_device_name()).context("микрофон не открылся")?;
    let mut resampler = StreamResampler::new(capture.sample_rate(), MODEL_RATE)?;

    if plain {
        println!(
            "Проверка голоса (образец не меняется). Микрофон: {}",
            capture.device_name()
        );
        println!(
            "Скажите «Джарвис» {count} раз — как обычно, с обычного места. После каждого раза — пауза в секунду."
        );
    } else {
        println!("Запись образца голоса. Микрофон: {}", capture.device_name());
        println!(
            "Скажите «Джарвис» {count} раз — так, как подсказывает каждая строка (расстояние и громкость). \
             После каждого раза — пауза в секунду; на переход по комнате времени хватит."
        );
    }
    prompt(0, count, plain);

    let started = Instant::now();
    let timeout = TIME_PER_SAMPLE * count as u32;
    let mut raw = vec![0.0f32; 4096];
    let mut chunk16 = Vec::new();
    while enroller.embeddings.len() < count {
        if started.elapsed() > timeout {
            bail!(
                "время вышло: записано {} из {count}",
                enroller.embeddings.len()
            );
        }
        let n = capture.read(&mut raw);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        chunk16.clear();
        resampler.process(&raw[..n], |out| chunk16.extend_from_slice(out))?;
        for segment in vad.accept(&chunk16) {
            let hint = hint(enroller.embeddings.len(), plain);
            match enroller.offer(&segment, hint) {
                Ok(word) => {
                    let live = enroller.attempts.last().is_some_and(|a| a.live);
                    println!(
                        "✓ услышал «{word}»{}",
                        if live {
                            ""
                        } else {
                            " (детектор сам бы не сработал)"
                        }
                    );
                    if enroller.embeddings.len() < count {
                        prompt(enroller.embeddings.len(), count, plain);
                    }
                }
                Err(why) => {
                    println!("✗ {why}. Ещё раз.");
                    prompt(enroller.embeddings.len(), count, plain);
                }
            }
        }
    }
    drop(capture);
    enroller.report();
    if save_samples {
        let stamp = jarvis_core::journal::now().replace([':', 'T'], "-");
        let stamp: String = stamp.chars().take(19).collect();
        enroller.save_samples(&data_dir(root, config).join("enroll").join(stamp))?;
    }
    match mode {
        Mode::Enroll => save(enroller.finish(&models, config)?, root, config),
        Mode::Test => {
            println!("Образец голоса не менялся.");
            Ok(())
        }
    }
}

/// Образцы из WAV-файлов (по одной попытке в файле) — для проверки без микрофона.
pub fn run_files(root: &Path, config: &Config, files: &[PathBuf]) -> anyhow::Result<()> {
    let models = SpeechModels::locate(root, config)?;
    let mut enroller = Enroller::new(&models, config)?;
    for file in files {
        let clip = wav::read(file)?.resampled(MODEL_RATE)?.trimmed();
        match enroller.offer(&clip.samples, "из файла") {
            Ok(word) => println!("✓ {}: «{word}»", file.display()),
            Err(why) => println!("✗ {}: {why}", file.display()),
        }
    }
    if enroller.embeddings.len() < 3 {
        bail!("подошло меньше трёх образцов из {}", files.len());
    }
    enroller.report();
    save(enroller.finish(&models, config)?, root, config)
}

fn save(print: Voiceprint, root: &Path, config: &Config) -> anyhow::Result<()> {
    let path = print.save(&data_dir(root, config))?;
    println!("Образец сохранён: {}", path.display());
    if !config.speaker.verify {
        println!("Внимание: проверка голоса выключена в конфиге (speaker.verify = false).");
    }
    println!("Перезапустите Джарвиса, чтобы он начал узнавать ваш голос.");
    Ok(())
}

fn hint(index: usize, plain: bool) -> &'static str {
    if plain {
        PLAIN_HINT
    } else {
        HINTS[index % HINTS.len()]
    }
}

fn prompt(done: usize, count: usize, plain: bool) {
    print!("[{}/{count}] «Джарвис» — {}… ", done + 1, hint(done, plain));
    let _ = std::io::stdout().flush();
}

/// Пик и средняя громкость (dBFS).
fn levels(samples: &[f32]) -> (f32, f32) {
    let db = |v: f32| 20.0 * v.max(1e-6).log10();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    (db(peak), db(rms))
}

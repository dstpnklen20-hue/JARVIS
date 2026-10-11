//! `jarvis --enroll`: запись образца голоса.
//!
//! Пользователь несколько раз говорит «Джарвис»; детектор речи вырезает
//! каждую попытку, распознаватель проверяет, что это похоже на «Джарвис»
//! (и запоминает, как он слышит именно этот голос), модель говорящего
//! считает вектор голоса. Итог — `data/voiceprint.json`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use jarvis_audio::asr::{Endpointing, create_recognizer, transcribe};
use jarvis_audio::capture::Capture;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::resample::{MODEL_RATE, StreamResampler};
use jarvis_audio::speaker::{SpeakerEncoder, Voiceprint, similarity};
use jarvis_audio::vad::{Vad, VadSettings};
use jarvis_audio::wake::{WakeMatcher, normalize_words};
use jarvis_audio::wav;
use jarvis_core::Config;

use crate::hearing::data_dir;

pub const SAMPLES: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(90);
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
    embeddings: Vec<Vec<f32>>,
    heard: Vec<String>,
}

impl Enroller {
    fn new(models: &SpeechModels, config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            recognizer: create_recognizer(&models.asr, Endpointing::default())?,
            encoder: SpeakerEncoder::new(&models.speaker)?,
            matcher: WakeMatcher::new(&config.wake.words, ENROLL_DISTANCE),
            embeddings: Vec::new(),
            heard: Vec::new(),
        })
    }

    /// Принимает попытку. `Err` — текст, почему она не подошла.
    fn offer(&mut self, samples: &[f32]) -> Result<String, String> {
        let secs = samples.len() as f32 / MODEL_RATE as f32;
        if !(MIN_SECS..=MAX_SECS).contains(&secs) {
            return Err(format!("длина {secs:.1} с — скажите одно слово «Джарвис»"));
        }
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
        self.embeddings.push(embedding);
        self.heard.push(word.clone());
        Ok(word)
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

/// Запись с микрофона.
pub fn run_microphone(root: &Path, config: &Config) -> anyhow::Result<()> {
    let models = SpeechModels::locate(root, config)?;
    let mut enroller = Enroller::new(&models, config)?;
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

    println!("Запись образца голоса. Микрофон: {}", capture.device_name());
    println!(
        "Скажите «Джарвис» {SAMPLES} раз — как обычно, с того места, откуда будете обращаться. \
         После каждого раза — пауза в секунду."
    );
    prompt(enroller.embeddings.len());

    let started = Instant::now();
    let mut raw = vec![0.0f32; 4096];
    let mut chunk16 = Vec::new();
    while enroller.embeddings.len() < SAMPLES {
        if started.elapsed() > TIMEOUT {
            bail!(
                "время вышло: записано {} из {SAMPLES}",
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
            match enroller.offer(&segment) {
                Ok(word) => {
                    println!("✓ услышал «{word}»");
                    if enroller.embeddings.len() < SAMPLES {
                        prompt(enroller.embeddings.len());
                    }
                }
                Err(why) => {
                    println!("✗ {why}. Ещё раз.");
                    prompt(enroller.embeddings.len());
                }
            }
        }
    }
    drop(capture);
    save(enroller.finish(&models, config)?, root, config)
}

/// Образцы из WAV-файлов (по одной попытке в файле) — для проверки без микрофона.
pub fn run_files(root: &Path, config: &Config, files: &[PathBuf]) -> anyhow::Result<()> {
    let models = SpeechModels::locate(root, config)?;
    let mut enroller = Enroller::new(&models, config)?;
    for file in files {
        let clip = wav::read(file)?.resampled(MODEL_RATE)?.trimmed();
        match enroller.offer(&clip.samples) {
            Ok(word) => println!("✓ {}: «{word}»", file.display()),
            Err(why) => println!("✗ {}: {why}", file.display()),
        }
    }
    if enroller.embeddings.len() < 3 {
        bail!("подошло меньше трёх образцов из {}", files.len());
    }
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

fn prompt(done: usize) {
    print!("[{}/{SAMPLES}] Скажите «Джарвис»… ", done + 1);
    let _ = std::io::stdout().flush();
}

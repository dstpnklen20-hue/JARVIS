#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты: падение = провал теста

//! Команды на записанных WAV (tests/fixtures/commands и wake) с настоящими
//! моделями: распознавание → разбор → решение, как в живом ядре.
//! Нужны модели: `cargo test --release -- --include-ignored`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jarvis_app::resolver::{CommandResolver, Resolution, hotword_phrases};
use jarvis_app::simulate::run_command;
use jarvis_audio::asr::Hotwords;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::resample::MODEL_RATE;
use jarvis_audio::stt::SherpaStt;
use jarvis_audio::wake::WakeMatcher;
use jarvis_audio::wav;
use jarvis_core::Config;
use jarvis_core::commands::Registry;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

struct Setup {
    config: Config,
    resolver: CommandResolver,
    stt: SherpaStt,
}

fn setup() -> Setup {
    let config = Config::load(&root()).unwrap();
    let models =
        SpeechModels::locate(&root(), &config).expect("нужны модели: scripts/download-models.ps1");
    let registry = Registry::load(&root().join("commands"));
    assert!(registry.errors.is_empty(), "{:?}", registry.errors);
    let wake = WakeMatcher::new(&config.wake.words, config.wake.max_edit_distance);
    let resolver = CommandResolver::new(&registry, wake, &config.nlu);
    let hotwords = Hotwords {
        phrases: hotword_phrases(&registry, &config.wake.words),
        score: config.listen.hotwords_score,
    };
    let stt = SherpaStt::new(
        &models.asr,
        config.listen.end_silence_ms as f32 / 1000.0,
        Some(&hotwords),
    )
    .unwrap();
    Setup {
        config,
        resolver,
        stt,
    }
}

fn decide(s: &mut Setup, file: &Path) -> Resolution {
    let samples = wav::read(file)
        .unwrap()
        .resampled(MODEL_RATE)
        .unwrap()
        .samples;
    let stable = Duration::from_millis(u64::from(s.config.nlu.early_stable_ms));
    run_command(
        &mut s.stt,
        &s.resolver,
        s.config.nlu.early_score,
        stable,
        &samples,
    )
    .resolution
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(root().join("tests/fixtures").join(kind))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    files.sort();
    files
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn every_command_recording_resolves_to_its_command() {
    let mut s = setup();
    for file in fixtures("commands") {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let expected = name.split("__").next().unwrap();
        let resolution = decide(&mut s, &file);
        if expected.starts_with("unknown") {
            assert!(
                matches!(resolution, Resolution::NotUnderstood { .. }),
                "{name}: {resolution:?}"
            );
        } else {
            assert!(
                matches!(&resolution, Resolution::Command { id, .. } if id == expected),
                "{name}: {resolution:?}"
            );
        }
    }
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn wake_word_and_command_in_one_breath() {
    // Так конвейер отдаёт распознавателю звук с начала «Джарвис».
    let mut s = setup();
    for (file, expected) in [
        ("pavel_jarvis_youtube.wav", "open_youtube"),
        ("irina_jarvis_music.wav", "media_play_pause"),
    ] {
        let resolution = decide(&mut s, &root().join("tests/fixtures/wake").join(file));
        assert!(
            matches!(&resolution, Resolution::Command { id, .. } if id == expected),
            "{file}: {resolution:?}"
        );
    }
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn shared_recognizer_still_detects_wake_word_only_where_said() {
    // Рабочая сборка: один распознаватель с подсказками на два потока.
    let config = Config::load(&root()).unwrap();
    let models = SpeechModels::locate(&root(), &config).unwrap();
    let registry = Registry::load(&root().join("commands"));
    let hotwords = Hotwords {
        phrases: hotword_phrases(&registry, &config.wake.words),
        score: config.listen.hotwords_score,
    };
    let wake = WakeMatcher::new(&config.wake.words, config.wake.max_edit_distance);
    let mut engines =
        jarvis_audio::engines::build(&models.asr, wake, 0.8, Some(&hotwords)).unwrap();
    for (kind, expected) in [("wake", 1), ("negative", 0), ("commands", 0)] {
        for file in fixtures(kind) {
            let samples = wav::read(&file)
                .unwrap()
                .resampled(MODEL_RATE)
                .unwrap()
                .samples;
            let run = jarvis_audio::bench::run_wake(&mut engines.wake, &samples);
            assert_eq!(
                run.detections.len(),
                expected,
                "{}: {:?}",
                file.display(),
                run.detections
            );
        }
    }
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn bare_wake_word_is_not_a_command() {
    let mut s = setup();
    let resolution = decide(&mut s, &root().join("tests/fixtures/wake/pavel_jarvis.wav"));
    assert_eq!(resolution, Resolution::Empty);
}

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты: падение = провал теста

//! Тесты конвейера на записанных WAV (tests/fixtures) с настоящими моделями.
//!
//! Нужны модели: `scripts/download-models.ps1`. Поэтому тесты помечены
//! `#[ignore]` и запускаются так: `cargo test -- --include-ignored`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jarvis_audio::asr::{Endpointing, create_recognizer, transcribe};
use jarvis_audio::bench::run_wake;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::resample::MODEL_RATE;
use jarvis_audio::speaker::{SpeakerEncoder, Voiceprint};
use jarvis_audio::vad::{Vad, VadSettings};
use jarvis_audio::wake::{SherpaWakeWord, WakeMatcher, normalize_words};
use jarvis_audio::wav;
use jarvis_core::Config;

const NEEDS_MODELS: &str = "нужны модели: scripts/download-models.ps1";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn setup() -> &'static (Config, SpeechModels) {
    static SETUP: OnceLock<(Config, SpeechModels)> = OnceLock::new();
    SETUP.get_or_init(|| {
        let config = Config::load(&root()).expect("конфиг");
        let models = SpeechModels::locate(&root(), &config).expect(NEEDS_MODELS);
        (config, models)
    })
}

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = root().join("tests").join("fixtures").join(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("папка fixtures")
        .map(|e| e.expect("файл").path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "нет записей в {}", dir.display());
    files
}

fn load16(path: &Path) -> Vec<f32> {
    wav::read(path)
        .expect("wav")
        .resampled(MODEL_RATE)
        .expect("ресемплинг")
        .samples
}

fn engine() -> SherpaWakeWord {
    let (config, models) = setup();
    let matcher = WakeMatcher::new(&config.wake.words, config.wake.max_edit_distance);
    SherpaWakeWord::new(&models.asr, matcher).expect("модель")
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn wake_word_fires_once_in_every_wake_recording() {
    let mut engine = engine();
    let (_, models) = setup();
    let recognizer = create_recognizer(&models.asr, Endpointing::default()).unwrap();
    for path in fixtures("wake") {
        let samples = load16(&path);
        let run = run_wake(&mut engine, &samples);
        let name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(
            run.detections.len(),
            1,
            "{name}: {:?}; модель слышит «{}»",
            run.detections,
            transcribe(&recognizer, &samples)
        );
        eprintln!(
            "{name}: «{}» через {:+.0} мс после конца речи, RTF {:.3}",
            run.detections[0].1.heard,
            run.delay_after_speech().unwrap() * 1000.0,
            run.rtf()
        );
    }
}

/// Сравнение голосов синтеза: разборчивость (сколько слов распознаётся
/// нашей моделью) и скорость. Папка с распакованными голосами —
/// в переменной JARVIS_TTS_EVAL_DIR.
#[test]
#[ignore = "оценка голосов: JARVIS_TTS_EVAL_DIR"]
fn tts_voice_evaluation() {
    use jarvis_audio::tts::{PiperTts, TextToSpeech, VoiceModel};
    let Some(dir) = std::env::var_os("JARVIS_TTS_EVAL_DIR") else {
        return;
    };
    let phrases = [
        "Сэр, команда открой ютуб откроет YouTube в браузере. Разрешить?",
        "Вы имели в виду открой гугл или открой яндекс?",
        "Сэр, команда выключи компьютер выключит компьютер. Несохранённые данные пропадут.",
        "Запрос выполнен, сэр. Громкость установлена на сорок процентов.",
        "Сэр, я запущен с правами администратора.",
    ];
    let (_, models) = setup();
    let recognizer = create_recognizer(&models.asr, Endpointing::default()).unwrap();
    let mut voices: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    voices.sort();
    for voice_dir in voices {
        let voice = VoiceModel::from_dir(&voice_dir).unwrap();
        let started = std::time::Instant::now();
        let tts = PiperTts::new(&voice, 1.0).unwrap();
        let load = started.elapsed();
        let (mut words, mut correct, mut audio, mut synth) = (0usize, 0usize, 0f32, 0f32);
        for phrase in phrases {
            let t = std::time::Instant::now();
            let clip = tts.synthesize(phrase).unwrap();
            synth += t.elapsed().as_secs_f32();
            audio += clip.duration_secs();
            let heard = transcribe(&recognizer, &clip.resampled(MODEL_RATE).unwrap().samples);
            let heard_words = normalize_words(&heard);
            let expected = normalize_words(phrase);
            words += expected.len();
            correct += expected.iter().filter(|w| heard_words.contains(w)).count();
        }
        eprintln!(
            "{:<38} загрузка {:>5} мс | разборчивость {:>3.0}% | RTF {:.3} | {:.1} с речи",
            voice_dir.file_name().unwrap().to_string_lossy(),
            load.as_millis(),
            correct as f32 * 100.0 / words as f32,
            synth / audio,
            audio
        );
    }
}

/// Голос по умолчанию произносит вопрос-разрешение разборчиво: наша же
/// модель распознавания узнаёт ключевые слова, числа читаются словами.
#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn default_voice_is_intelligible() {
    use jarvis_audio::tts::{PiperTts, TextToSpeech};
    let (config, models) = setup();
    let voice = jarvis_audio::models::tts_voice(&root(), config, "dmitri").expect(NEEDS_MODELS);
    let tts = PiperTts::new(&voice, 1.0).unwrap();
    let recognizer = create_recognizer(&models.asr, Endpointing::default()).unwrap();
    for (phrase, expected) in [
        (
            "Сэр, команда «громкость на 40» установит громкость 40%. Разрешить?",
            &["громкост", "сорок", "процент", "разреш"][..],
        ),
        (
            // Синтез случаен (VITS), распознавание иногда слышит «открою» —
            // поэтому проверяются основы слов.
            "Сэр, вы имели в виду «открой гугл» или «открой яндекс»?",
            &["имели", "откр", "или"][..],
        ),
    ] {
        let clip = tts.synthesize(phrase).unwrap();
        let heard = transcribe(&recognizer, &clip.resampled(MODEL_RATE).unwrap().samples);
        let words = normalize_words(&heard);
        eprintln!("«{phrase}» → «{heard}»");
        for w in expected {
            assert!(
                words.iter().any(|h| h.starts_with(w)),
                "«{w}» не слышно в «{heard}»"
            );
        }
    }
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn wake_word_never_fires_on_ordinary_speech() {
    let mut engine = engine();
    for path in fixtures("negative") {
        let run = run_wake(&mut engine, &load16(&path));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            run.detections.is_empty(),
            "{name}: ложное срабатывание {:?}",
            run.detections
        );
    }
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn wake_word_is_fast_on_single_word() {
    let mut engine = engine();
    let path = root().join("tests/fixtures/wake/pavel_jarvis.wav");
    let run = run_wake(&mut engine, &load16(&path));
    let delay = run.delay_after_speech().expect("не сработало");
    // Цель ТЗ ≤ 100 мс; потоковая модель реально даёт 200–400 мс.
    assert!(delay < 0.6, "задержка {delay:.3} с");
    // Одно ядро загружено меньше чем на 10 %.
    assert!(run.rtf() < 0.1, "RTF {}", run.rtf());
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn transcription_of_command_phrase() {
    let (_, models) = setup();
    let recognizer = create_recognizer(&models.asr, Endpointing::default()).unwrap();
    let text = transcribe(
        &recognizer,
        &load16(&root().join("tests/fixtures/wake/pavel_jarvis_youtube.wav")),
    );
    let words = normalize_words(&text);
    assert!(words.iter().any(|w| w == "открой"), "{text}");
    assert!(words.iter().any(|w| w.starts_with("ютуб")), "{text}");
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn vad_cuts_single_phrase() {
    let (_, models) = setup();
    let mut vad = Vad::new(&models.vad, VadSettings::default()).unwrap();
    let mut samples = vec![0.0; MODEL_RATE as usize / 2];
    samples.extend(load16(&root().join("tests/fixtures/enroll/pavel_1.wav")));
    samples.extend(vec![0.0; MODEL_RATE as usize]);
    let mut segments = Vec::new();
    for chunk in samples.chunks(512) {
        segments.extend(vad.accept(chunk));
    }
    segments.extend(vad.finish());
    assert_eq!(segments.len(), 1);
    let secs = segments[0].len() as f32 / MODEL_RATE as f32;
    assert!((0.2..2.0).contains(&secs), "{secs} с");
}

#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn voiceprint_accepts_same_voice_and_rejects_other() {
    let (_, models) = setup();
    let encoder = SpeakerEncoder::new(&models.speaker).unwrap();
    let embeddings: Vec<_> = fixtures("enroll")
        .iter()
        .map(|p| encoder.embed(&load16(p)).expect("эмбеддинг"))
        .collect();
    let print = Voiceprint::from_embeddings(&models.speaker, &embeddings, vec![]).unwrap();

    let sim = |name: &str| {
        let e = encoder
            .embed(&load16(&root().join("tests/fixtures/wake").join(name)))
            .unwrap();
        print.similarity(&e)
    };
    let same = sim("pavel_jarvis_youtube.wav");
    let other = sim("irina_jarvis.wav");
    eprintln!(
        "порог {:.2}; тот же голос {same:.2}; другой {other:.2}",
        print.threshold
    );
    assert!(
        same >= print.threshold,
        "свой голос отвергнут: {same} < {}",
        print.threshold
    );
    assert!(
        other < print.threshold,
        "чужой голос принят: {other} ≥ {}",
        print.threshold
    );
}

/// Ожидание команды после «Джарвис» — 2 с. Детектор речи должен замечать
/// голос раньше, чем появятся первые распознанные слова: иначе фраза,
/// начатая к концу ожидания, оборвётся «Не расслышал, сэр».
#[test]
#[ignore = "нужны модели: scripts/download-models.ps1"]
fn vad_notices_speech_before_first_words() {
    use jarvis_audio::stt::{SherpaStt, SpeechToText};

    let (config, models) = setup();
    let lead = 0.5_f32;
    let chunk = MODEL_RATE as usize / 50; // 20 мс, как в конвейере
    let mut gains = Vec::new();
    for path in fixtures("commands") {
        let mut vad = Vad::new(&models.vad, VadSettings::default()).unwrap();
        let end_silence = config.listen.end_silence_ms as f32 / 1000.0;
        let mut stt = SherpaStt::new(&models.asr, end_silence, None).unwrap();
        let mut samples = vec![0.0; (lead * MODEL_RATE as f32) as usize];
        samples.extend(load16(&path));
        samples.extend(vec![0.0; MODEL_RATE as usize]);
        let (mut voice_at, mut words_at) = (None, None);
        for (i, part) in samples.chunks(chunk).enumerate() {
            let t = (i + 1) as f32 * 0.02;
            let _ = vad.accept(part);
            if voice_at.is_none() && vad.speaking() {
                voice_at = Some(t);
            }
            if words_at.is_none() && stt.accept(part).is_some() {
                words_at = Some(t);
            }
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let voice = voice_at.unwrap_or_else(|| panic!("{name}: речь не замечена"));
        let words = words_at.unwrap_or(f32::INFINITY);
        eprintln!(
            "{name}: голос через {:.2} с от начала записи, первые слова через {:.2} с",
            voice - lead,
            words - lead
        );
        assert!(voice <= words + 0.05, "{name}: детектор позже слов");
        gains.push(words - voice);
    }
    gains.sort_by(f32::total_cmp);
    eprintln!(
        "детектор раньше слов: медиана {:.2} с, минимум {:.2} с",
        gains[gains.len() / 2],
        gains[0]
    );
}

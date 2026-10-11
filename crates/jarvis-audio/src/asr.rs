//! Потоковое распознавание речи (sherpa-onnx, zipformer transducer).

use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};

use crate::AudioError;
use crate::models::{AsrModel, path_str};
use crate::resample::MODEL_RATE;

/// Правила конца фразы встроенного детектора sherpa-onnx (секунды).
#[derive(Debug, Clone, Copy)]
pub struct Endpointing {
    /// Тишина без единого распознанного слова.
    pub silence_without_speech: f32,
    /// Тишина после распознанных слов.
    pub silence_after_speech: f32,
    /// Максимальная длина фразы.
    pub max_utterance: f32,
}

impl Default for Endpointing {
    fn default() -> Self {
        Self {
            silence_without_speech: 2.0,
            silence_after_speech: 0.8,
            max_utterance: 15.0,
        }
    }
}

/// Подсказки декодеру (hotwords): фразы, которые стоит предпочитать при
/// сомнениях. Замена ограниченной грамматики Vosk из ТЗ: распознаватель
/// по-прежнему слышит любую речь, но слова команд узнаёт увереннее.
#[derive(Debug, Clone, PartialEq)]
pub struct Hotwords {
    pub phrases: Vec<String>,
    /// Сила подсказки (sherpa-onnx рекомендует 1.5–3).
    pub score: f32,
}

pub fn create_recognizer(
    model: &AsrModel,
    endpointing: Endpointing,
) -> Result<OnlineRecognizer, AudioError> {
    create_recognizer_with(model, endpointing, None)
}

pub fn create_recognizer_with(
    model: &AsrModel,
    endpointing: Endpointing,
    hotwords: Option<&Hotwords>,
) -> Result<OnlineRecognizer, AudioError> {
    let mut config = OnlineRecognizerConfig::default();
    config.feat_config.sample_rate = MODEL_RATE as i32;
    config.model_config.transducer.encoder = Some(path_str(&model.encoder));
    config.model_config.transducer.decoder = Some(path_str(&model.decoder));
    config.model_config.transducer.joiner = Some(path_str(&model.joiner));
    config.model_config.tokens = Some(path_str(&model.tokens));
    // Один поток: модель маленькая, а лишние потоки только греют CPU.
    config.model_config.num_threads = 1;
    match hotwords.filter(|h| !h.phrases.is_empty()) {
        None => config.decoding_method = Some("greedy_search".into()),
        Some(h) => {
            // Подсказки работают только с поиском по нескольким гипотезам.
            config.decoding_method = Some("modified_beam_search".into());
            config.max_active_paths = 4;
            config.model_config.modeling_unit = Some("bpe".into());
            config.model_config.bpe_vocab = Some(path_str(&crate::bpe::ensure_vocab(&model.bpe)?));
            config.hotwords_score = h.score;
            config.hotwords_buf = Some(h.phrases.join("\n").into_bytes());
        }
    }
    config.enable_endpoint = true;
    config.rule1_min_trailing_silence = endpointing.silence_without_speech;
    config.rule2_min_trailing_silence = endpointing.silence_after_speech;
    config.rule3_min_utterance_length = endpointing.max_utterance;
    OnlineRecognizer::create(&config).ok_or_else(|| AudioError::ModelLoad {
        what: "распознавания речи",
        path: model.encoder.clone(),
    })
}

/// Распознаёт законченный фрагмент целиком (для `--enroll` и тестов).
pub fn transcribe(recognizer: &OnlineRecognizer, samples: &[f32]) -> String {
    let stream = recognizer.create_stream();
    stream.accept_waveform(MODEL_RATE as i32, samples);
    // Немного тишины, чтобы модель дописала последнее слово.
    stream.accept_waveform(MODEL_RATE as i32, &[0.0; MODEL_RATE as usize / 2]);
    stream.input_finished();
    while recognizer.is_ready(&stream) {
        recognizer.decode(&stream);
    }
    recognizer
        .get_result(&stream)
        .map(|r| r.text.trim().to_owned())
        .unwrap_or_default()
}

//! Распознавание команд (раздел 4.3 ТЗ): потоковое, с частичными
//! результатами и подсказками (hotwords) из фраз зарегистрированных команд.
//!
//! Работает только после «Джарвис» и в FOLLOW_UP — в режиме ожидания
//! не тратит процессор.

use std::sync::Arc;

use sherpa_onnx::{OnlineRecognizer, OnlineStream};

use crate::AudioError;
use crate::asr::{Endpointing, Hotwords, create_recognizer_with};
use crate::models::AsrModel;
use crate::resample::MODEL_RATE;
use crate::wake::{QUIET_RMS, rms, should_reset};

/// Текст фразы. Номер фразы растёт после каждого конца фразы: по нему
/// ядро понимает, что частичный и итоговый результат — об одном и том же.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub utterance: u64,
    pub text: String,
    /// Фраза закончилась (тишина после речи).
    pub is_final: bool,
}

/// Сменный движок распознавания речи. Принимает 16 кГц моно.
pub trait SpeechToText: Send {
    fn accept(&mut self, samples: &[f32]) -> Option<Transcript>;
    /// Начать новую фразу с чистого листа.
    fn reset(&mut self);
}

/// Правила конца фразы для распознавания команд. Пустую тишину
/// обрабатывает `should_reset`; правило sherpa-onnx здесь только
/// ограничивает поток.
pub fn command_endpointing(end_silence: f32) -> Endpointing {
    Endpointing {
        silence_without_speech: 10.0,
        silence_after_speech: end_silence,
        max_utterance: 15.0,
    }
}

pub struct SherpaStt {
    /// Может быть общим со словом активации (один экземпляр модели в памяти).
    recognizer: Arc<OnlineRecognizer>,
    stream: OnlineStream,
    utterance: u64,
    last_text: String,
    stream_samples: u64,
    quiet_samples: u64,
}

impl SherpaStt {
    /// Со своим распознавателем. `end_silence` — тишина в конце фразы, секунды.
    pub fn new(
        model: &AsrModel,
        end_silence: f32,
        hotwords: Option<&Hotwords>,
    ) -> Result<Self, AudioError> {
        let recognizer = create_recognizer_with(model, command_endpointing(end_silence), hotwords)?;
        Ok(Self::with_recognizer(Arc::new(recognizer)))
    }

    /// На общем распознавателе (см. [`crate::engines`]).
    pub fn with_recognizer(recognizer: Arc<OnlineRecognizer>) -> Self {
        let stream = recognizer.create_stream();
        let mut stt = Self {
            recognizer,
            stream,
            utterance: 0,
            last_text: String::new(),
            stream_samples: 0,
            quiet_samples: 0,
        };
        // Прогрев: первый прогон медленнее остальных.
        stt.accept(&vec![0.0; MODEL_RATE as usize]);
        stt.reset();
        stt
    }

    fn new_stream(&mut self) {
        // Не recognizer.reset(): он не сбрасывает энкодер (см. wake.rs).
        self.stream = self.recognizer.create_stream();
        self.last_text.clear();
        self.stream_samples = 0;
    }
}

impl SpeechToText for SherpaStt {
    fn accept(&mut self, samples: &[f32]) -> Option<Transcript> {
        self.stream.accept_waveform(MODEL_RATE as i32, samples);
        self.stream_samples += samples.len() as u64;
        if rms(samples) < QUIET_RMS {
            self.quiet_samples += samples.len() as u64;
        } else {
            self.quiet_samples = 0;
        }
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }

        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_owned())
            .unwrap_or_default();
        let changed = text != self.last_text;
        self.last_text.clone_from(&text);

        let endpoint = self.recognizer.is_endpoint(&self.stream);
        let has_text = !text.is_empty();
        let secs = |n: u64| n as f32 / MODEL_RATE as f32;
        if endpoint && has_text {
            let utterance = self.utterance;
            self.utterance += 1;
            self.new_stream();
            return Some(Transcript {
                utterance,
                text,
                is_final: true,
            });
        }
        if should_reset(
            endpoint,
            has_text,
            secs(self.stream_samples),
            secs(self.quiet_samples),
        ) {
            // Длинная тишина без слов или слишком длинный поток.
            if has_text {
                let utterance = self.utterance;
                self.utterance += 1;
                self.new_stream();
                return Some(Transcript {
                    utterance,
                    text,
                    is_final: true,
                });
            }
            self.new_stream();
            return None;
        }
        (changed && has_text).then_some(Transcript {
            utterance: self.utterance,
            text,
            is_final: false,
        })
    }

    fn reset(&mut self) {
        if !self.last_text.is_empty() {
            self.utterance += 1;
        }
        self.new_stream();
    }
}

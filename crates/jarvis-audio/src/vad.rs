//! Детектор речи (VAD): находит законченные фразы в потоке 16 кГц.

use jarvis_core::config::VadEngine;
use sherpa_onnx::{VadModelConfig, VoiceActivityDetector};

use crate::AudioError;
use crate::models::{VadModel, path_str};
use crate::resample::MODEL_RATE;

/// Параметры нарезки (секунды).
#[derive(Debug, Clone, Copy)]
pub struct VadSettings {
    /// Тишина, после которой фраза считается законченной.
    pub min_silence: f32,
    /// Короче — не речь (щелчок, кашель).
    pub min_speech: f32,
    pub max_speech: f32,
}

impl Default for VadSettings {
    fn default() -> Self {
        Self {
            min_silence: 0.3,
            min_speech: 0.2,
            max_speech: 8.0,
        }
    }
}

pub struct Vad {
    detector: VoiceActivityDetector,
}

impl Vad {
    pub fn new(model: &VadModel, settings: VadSettings) -> Result<Self, AudioError> {
        let mut config = VadModelConfig::default();
        let path = Some(path_str(&model.path));
        match model.engine {
            VadEngine::Silero => {
                let c = &mut config.silero_vad;
                c.model = path;
                c.threshold = 0.5;
                c.min_silence_duration = settings.min_silence;
                c.min_speech_duration = settings.min_speech;
                c.max_speech_duration = settings.max_speech;
                c.window_size = 512;
            }
            VadEngine::Ten => {
                let c = &mut config.ten_vad;
                c.model = path;
                c.threshold = 0.5;
                c.min_silence_duration = settings.min_silence;
                c.min_speech_duration = settings.min_speech;
                c.max_speech_duration = settings.max_speech;
                c.window_size = 256;
            }
        }
        config.sample_rate = MODEL_RATE as i32;
        config.num_threads = 1;
        let detector =
            VoiceActivityDetector::create(&config, 30.0).ok_or_else(|| AudioError::ModelLoad {
                what: "детектора речи",
                path: model.path.clone(),
            })?;
        Ok(Self { detector })
    }

    /// Принимает отсчёты; возвращает законченные фразы, если есть.
    pub fn accept(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.detector.accept_waveform(samples);
        self.drain()
    }

    /// Сейчас звучит речь.
    pub fn speaking(&self) -> bool {
        self.detector.detected()
    }

    /// Дописать незаконченную фразу (конец записи).
    pub fn finish(&mut self) -> Vec<Vec<f32>> {
        self.detector.flush();
        self.drain()
    }

    pub fn reset(&mut self) {
        self.detector.reset();
    }

    fn drain(&mut self) -> Vec<Vec<f32>> {
        let mut segments = Vec::new();
        while let Some(segment) = self.detector.front() {
            segments.push(segment.samples().to_vec());
            self.detector.pop();
        }
        segments
    }
}

//! Синтез речи (раздел 4.5 ТЗ) — для динамических ответов: вопрос-разрешение
//! с описанием команды, «вы имели в виду X или Y?», позже ответы LLM.
//!
//! Движок за трейтом [`TextToSpeech`]. Основная реализация — Piper (VITS)
//! в sherpa-onnx: тот же движок, что у распознавания, офлайн, русские голоса
//! под MIT-лицензией.

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
    OfflineTtsVitsModelConfig,
};

use crate::AudioError;
use crate::models::path_str;
use crate::wav::Clip;

/// Как звучит синтез: темп и «случайность» Piper (см. `voice.tts_noise`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SynthParams {
    /// 1.0 — обычный темп; больше — быстрее.
    pub speed: f32,
    /// Шум в голосе (noise_scale).
    pub noise: f32,
    /// Разброс длительности звуков (noise_scale_w).
    pub noise_w: f32,
}

impl Default for SynthParams {
    /// Значения Piper по умолчанию.
    fn default() -> Self {
        Self {
            speed: 1.0,
            noise: 0.667,
            noise_w: 0.8,
        }
    }
}

impl SynthParams {
    pub fn from_config(voice: &jarvis_core::config::Voice) -> Self {
        Self {
            speed: voice.tts_speed,
            noise: voice.tts_noise,
            noise_w: voice.tts_noise_w,
        }
    }
}

/// Сменный движок синтеза.
pub trait TextToSpeech: Send {
    fn synthesize(&self, text: &str) -> Result<Clip, AudioError>;
}

/// Файлы голоса Piper.
#[derive(Debug, Clone)]
pub struct VoiceModel {
    pub model: PathBuf,
    pub tokens: PathBuf,
    /// Данные espeak-ng (правила произношения).
    pub data_dir: PathBuf,
}

impl VoiceModel {
    /// Голос из распакованной папки `vits-piper-…` (первый .onnx в ней).
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let model = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|e| e == "onnx"))?;
        Some(Self {
            model,
            tokens: dir.join("tokens.txt"),
            data_dir: dir.join("espeak-ng-data"),
        })
    }
}

pub struct PiperTts {
    tts: OfflineTts,
    /// 1.0 — обычный темп; больше — быстрее.
    speed: f32,
}

impl PiperTts {
    pub fn new(voice: &VoiceModel, params: SynthParams) -> Result<Self, AudioError> {
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                vits: OfflineTtsVitsModelConfig {
                    model: Some(path_str(&voice.model)),
                    tokens: Some(path_str(&voice.tokens)),
                    data_dir: Some(path_str(&voice.data_dir)),
                    noise_scale: params.noise,
                    noise_scale_w: params.noise_w,
                    ..Default::default()
                },
                num_threads: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let tts = OfflineTts::create(&config).ok_or_else(|| AudioError::ModelLoad {
            what: "синтеза речи",
            path: voice.model.clone(),
        })?;
        let engine = Self {
            tts,
            speed: params.speed,
        };
        // Прогрев: первый синтез заметно медленнее.
        let _ = engine.synthesize("Готов.");
        Ok(engine)
    }
}

/// Текст для синтеза: без управляющих символов (нулевой символ уронил бы
/// библиотеку) и лишних пробелов.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl TextToSpeech for PiperTts {
    fn synthesize(&self, text: &str) -> Result<Clip, AudioError> {
        let text = clean_text(text);
        let failed = || AudioError::ModelLoad {
            what: "синтеза речи (генерация)",
            path: PathBuf::new(),
        };
        if text.is_empty() {
            return Err(failed());
        }
        let config = GenerationConfig {
            speed: self.speed,
            ..Default::default()
        };
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(&text, &config, None)
            .ok_or_else(failed)?;
        Ok(Clip {
            samples: audio.samples().to_vec(),
            sample_rate: u32::try_from(audio.sample_rate()).unwrap_or(22_050),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean_text("  Да,\nсэр.\0 "), "Да, сэр.");
        assert_eq!(clean_text("\t"), "");
    }
}

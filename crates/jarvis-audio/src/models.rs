//! Пути к файлам моделей. Папки и наличие файлов берутся из
//! `scripts/assets.json` (см. `jarvis_core::assets`), имена файлов — здесь.

use std::path::{Path, PathBuf};

use jarvis_core::Config;
use jarvis_core::assets::Manifest;
use jarvis_core::config::VadEngine;

use crate::AudioError;

pub const ASR_ASSET: &str = "asr-zipformer-small-ru";
pub const VAD_SILERO_ASSET: &str = "vad-silero";
pub const VAD_TEN_ASSET: &str = "vad-ten";
pub const SPEAKER_ASSET: &str = "speaker-campplus";

/// Потоковая модель распознавания (transducer).
#[derive(Debug, Clone)]
pub struct AsrModel {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

#[derive(Debug, Clone)]
pub struct VadModel {
    pub engine: VadEngine,
    pub path: PathBuf,
}

/// Все модели, нужные слуху.
#[derive(Debug, Clone)]
pub struct SpeechModels {
    pub asr: AsrModel,
    pub vad: VadModel,
    pub speaker: PathBuf,
}

impl SpeechModels {
    /// Находит модели и проверяет, что они установлены целиком.
    pub fn locate(root: &Path, config: &Config) -> Result<Self, AudioError> {
        let manifest = Manifest::load(root)?;
        let asr = manifest.get(ASR_ASSET)?;
        let file = |asset: &jarvis_core::assets::Asset, name: &str| asset.file(root, config, name);
        let vad = match config.vad.engine {
            VadEngine::Silero => VadModel {
                engine: VadEngine::Silero,
                path: file(manifest.get(VAD_SILERO_ASSET)?, "silero_vad.onnx")?,
            },
            VadEngine::Ten => VadModel {
                engine: VadEngine::Ten,
                path: file(manifest.get(VAD_TEN_ASSET)?, "ten-vad.onnx")?,
            },
        };
        let speaker = manifest.get(SPEAKER_ASSET)?;
        let speaker_file = speaker
            .files
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        Ok(Self {
            asr: AsrModel {
                encoder: file(asr, "encoder.int8.onnx")?,
                decoder: file(asr, "decoder.onnx")?,
                joiner: file(asr, "joiner.int8.onnx")?,
                tokens: file(asr, "tokens.txt")?,
            },
            vad,
            speaker: file(speaker, speaker_file)?,
        })
    }
}

/// sherpa-onnx принимает пути строками.
pub(crate) fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

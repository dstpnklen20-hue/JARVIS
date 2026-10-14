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
    /// SentencePiece-модель: нужна для подсказок (hotwords).
    pub bpe: PathBuf,
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
                bpe: file(asr, "bpe.model")?,
            },
            vad,
            speaker: file(speaker, speaker_file)?,
        })
    }
}

/// Папки голосов Piper в папке моделей: `vits-piper-ru_RU-<имя>-medium` у
/// скачиваемых, `vits-piper-ru_RU-<имя>` у своих.
const LOCAL_VOICE_PREFIX: &str = "vits-piper-ru_RU-";

/// Голос синтеза по имени: «dmitri» → ресурс `tts-dmitri` из `assets.json`;
/// имени там нет — свой голос из папки моделей (`vits-piper-ru_RU-jarvis`,
/// например обученный на записях Джарвиса: он не скачивается, а лежит только
/// у владельца).
pub fn tts_voice(
    root: &Path,
    config: &Config,
    name: &str,
) -> Result<crate::tts::VoiceModel, AudioError> {
    let manifest = Manifest::load(root)?;
    let asset = match manifest.get(&format!("tts-{name}")) {
        Ok(asset) => asset,
        Err(unknown) => {
            let dir = models_dir(root, config).join(format!("{LOCAL_VOICE_PREFIX}{name}"));
            return local_voice(&dir).ok_or(unknown.into());
        }
    };
    let model = asset.file(
        root,
        config,
        asset.files.first().map(String::as_str).unwrap_or_default(),
    )?;
    let dir = asset.location(root, config);
    Ok(crate::tts::VoiceModel {
        model,
        tokens: dir.join("tokens.txt"),
        data_dir: dir.join("espeak-ng-data"),
    })
}

/// Имена установленных голосов синтеза: из `assets.json` (по порядку в нём),
/// затем свои из папки моделей.
pub fn installed_voices(root: &Path, config: &Config) -> Vec<String> {
    let Ok(manifest) = Manifest::load(root) else {
        return Vec::new();
    };
    let mut voices: Vec<String> = manifest
        .assets
        .iter()
        .filter_map(|a| a.id.strip_prefix("tts-").map(|name| (a, name)))
        .filter(|(a, _)| {
            a.status(&a.location(root, config)) == jarvis_core::assets::AssetStatus::Ok
        })
        .map(|(_, name)| name.to_owned())
        .collect();
    let known: Vec<&str> = manifest
        .assets
        .iter()
        .filter_map(|a| a.id.strip_prefix("tts-"))
        .collect();
    let local = local_voice_names(&models_dir(root, config), &known);
    voices.extend(local);
    voices
}

fn models_dir(root: &Path, config: &Config) -> PathBuf {
    jarvis_core::config::resolve(root, &config.paths.models)
}

/// Свой голос: в папке есть модель .onnx, tokens.txt и данные espeak-ng.
fn local_voice(dir: &Path) -> Option<crate::tts::VoiceModel> {
    let voice = crate::tts::VoiceModel::from_dir(dir)?;
    (voice.tokens.is_file() && voice.data_dir.is_dir()).then_some(voice)
}

/// Имена своих голосов в папке моделей. Только имена, которые можно указать
/// в `voice.tts` (латиница строчными), — пробные выгрузки вроде
/// `jarvis-e60` не показываются. Скачиваемые голоса (`known`) не повторяются.
fn local_voice_names(models: &Path, known: &[&str]) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(models) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file_name = e.file_name().to_string_lossy().into_owned();
            let name = file_name.strip_prefix(LOCAL_VOICE_PREFIX)?.to_owned();
            let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase());
            (valid && !known.contains(&name.as_str()) && local_voice(&e.path()).is_some())
                .then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// sherpa-onnx принимает пути строками.
pub(crate) fn path_str(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if text.is_ascii() {
        return text;
    }
    // Нативные библиотеки (espeak-ng) не открывают пути с русскими буквами —
    // короткое имя 8.3 латинское. Если короткие имена на томе выключены —
    // путь остаётся прежним (и `jarvis --check` об этом предупредит).
    #[cfg(windows)]
    {
        let short = jarvis_platform::short_path(path)
            .to_string_lossy()
            .into_owned();
        if short.is_ascii() {
            return short;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_voice(models: &Path, dir: &str) {
        let dir = models.join(dir);
        std::fs::create_dir_all(dir.join("espeak-ng-data")).unwrap();
        std::fs::write(dir.join("ru_RU-x.onnx"), b"").unwrap();
        std::fs::write(dir.join("tokens.txt"), b"_ 0\n").unwrap();
    }

    #[test]
    fn local_voices_are_found_by_folder_name() {
        let tmp = tempfile::tempdir().unwrap();
        make_voice(tmp.path(), "vits-piper-ru_RU-jarvis");
        // Пробная выгрузка и скачиваемый голос — не свои голоса.
        make_voice(tmp.path(), "vits-piper-ru_RU-mix-e150");
        make_voice(tmp.path(), "vits-piper-ru_RU-dmitri");
        // Неполная папка (нет tokens.txt) — не голос.
        std::fs::create_dir_all(tmp.path().join("vits-piper-ru_RU-broken/espeak-ng-data")).unwrap();
        std::fs::write(tmp.path().join("vits-piper-ru_RU-broken/m.onnx"), b"").unwrap();
        assert_eq!(local_voice_names(tmp.path(), &["dmitri"]), ["jarvis"]);
    }

    #[test]
    fn local_voice_needs_model_tokens_and_espeak_data() {
        let tmp = tempfile::tempdir().unwrap();
        make_voice(tmp.path(), "v");
        let voice = local_voice(&tmp.path().join("v")).unwrap();
        assert!(voice.model.ends_with("ru_RU-x.onnx"));
        std::fs::remove_dir(tmp.path().join("v/espeak-ng-data")).unwrap();
        assert!(local_voice(&tmp.path().join("v")).is_none());
        assert!(local_voice(&tmp.path().join("missing")).is_none());
    }

    #[test]
    fn missing_models_dir_has_no_local_voices() {
        assert!(local_voice_names(Path::new("Z:/нет/такой/папки"), &[]).is_empty());
    }
}

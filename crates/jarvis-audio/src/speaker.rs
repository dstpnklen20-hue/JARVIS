//! Распознавание говорящего: Джарвис откликается только на ваш голос.
//!
//! `jarvis --enroll` записывает несколько образцов «Джарвис», считает по
//! ним усреднённый вектор голоса (эмбеддинг) и подбирает порог сходства.
//! При срабатывании слова активации вырезанный фрагмент сравнивается с
//! образцом по косинусному сходству.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

use crate::AudioError;
use crate::models::path_str;
use crate::resample::MODEL_RATE;

pub const VOICEPRINT_FILE: &str = "voiceprint.json";
const VOICEPRINT_VERSION: u32 = 1;

/// Порог не опускается ниже: чужие голоса у CAM++ дают сходство < 0.3.
pub const MIN_THRESHOLD: f32 = 0.30;
/// И не поднимается выше: живой голос заметно меняется (расстояние до
/// микрофона, интонация, простуда), свой голос в разных условиях даёт
/// 0.5–0.8. Высокий порог — частые отказы собственному голосу.
pub const MAX_THRESHOLD: f32 = 0.55;
/// Запас между худшим из ваших образцов и порогом.
const THRESHOLD_MARGIN: f32 = 0.20;

pub struct SpeakerEncoder {
    extractor: SpeakerEmbeddingExtractor,
}

impl SpeakerEncoder {
    pub fn new(model: &Path) -> Result<Self, AudioError> {
        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(path_str(model)),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".into()),
        };
        let extractor =
            SpeakerEmbeddingExtractor::create(&config).ok_or_else(|| AudioError::ModelLoad {
                what: "распознавания говорящего",
                path: model.to_path_buf(),
            })?;
        Ok(Self { extractor })
    }

    /// Нормированный вектор голоса для фрагмента 16 кГц.
    /// `None` — фрагмент слишком короткий.
    pub fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(MODEL_RATE as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream).map(normalized)
    }
}

fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Косинусное сходство нормированных векторов.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Образец голоса пользователя.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voiceprint {
    pub version: u32,
    /// Файл модели, которой посчитан вектор: с другой моделью он несравним.
    pub model: String,
    pub embedding: Vec<f32>,
    /// Порог, подобранный по образцам.
    pub threshold: f32,
    /// Как распознаватель написал ваше «Джарвис» в каждом образце.
    pub heard_as: Vec<String>,
    pub samples: usize,
}

impl Voiceprint {
    /// Строит образец по векторам отдельных записей.
    pub fn from_embeddings(
        model: &Path,
        embeddings: &[Vec<f32>],
        heard_as: Vec<String>,
    ) -> Option<Self> {
        let dim = embeddings.first()?.len();
        if embeddings.iter().any(|e| e.len() != dim) {
            return None;
        }
        let mut mean = vec![0.0f32; dim];
        for e in embeddings {
            mean.iter_mut().zip(e).for_each(|(m, x)| *m += x);
        }
        let mean = normalized(mean);
        let worst = embeddings
            .iter()
            .map(|e| similarity(e, &mean))
            .fold(f32::INFINITY, f32::min);
        Some(Self {
            version: VOICEPRINT_VERSION,
            model: model_id(model),
            embedding: mean,
            threshold: (worst - THRESHOLD_MARGIN).clamp(MIN_THRESHOLD, MAX_THRESHOLD),
            heard_as,
            samples: embeddings.len(),
        })
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(VOICEPRINT_FILE)
    }

    /// Загружает образец; `Ok(None)` — образца ещё нет.
    pub fn load(data_dir: &Path, model: &Path) -> Result<Option<Self>, AudioError> {
        let path = Self::path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(AudioError::Read { path, source }),
        };
        let bad = |reason: String| AudioError::Voiceprint {
            path: path.clone(),
            reason,
        };
        let print: Self = serde_json::from_str(&text).map_err(|e| bad(e.to_string()))?;
        if print.version != VOICEPRINT_VERSION {
            return Err(bad(format!(
                "версия {} не поддерживается — запишите голос заново: jarvis --enroll",
                print.version
            )));
        }
        if print.model != model_id(model) {
            return Err(bad(format!(
                "записан моделью {}, сейчас {} — запишите голос заново: jarvis --enroll",
                print.model,
                model_id(model)
            )));
        }
        Ok(Some(print))
    }

    pub fn save(&self, data_dir: &Path) -> Result<PathBuf, AudioError> {
        std::fs::create_dir_all(data_dir).map_err(|source| AudioError::Write {
            path: data_dir.to_path_buf(),
            source,
        })?;
        let path = Self::path(data_dir);
        let text = serde_json::to_string_pretty(self).map_err(|e| AudioError::Voiceprint {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        std::fs::write(&path, text).map_err(|source| AudioError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn similarity(&self, embedding: &[f32]) -> f32 {
        similarity(&self.embedding, embedding)
    }
}

fn model_id(model: &Path) -> String {
    model
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        normalized(v.to_vec())
    }

    #[test]
    fn similarity_of_normalized_vectors() {
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[0.0, 2.0]);
        assert!((similarity(&a, &a) - 1.0).abs() < 1e-6);
        assert!(similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn voiceprint_threshold_follows_worst_sample_with_margin() {
        let model = Path::new("models/x/model.onnx");
        let close = [unit(&[1.0, 0.05]), unit(&[1.0, -0.05]), unit(&[1.0, 0.0])];
        let print = Voiceprint::from_embeddings(model, &close, vec!["джарвис".into()]).unwrap();
        // Образцы почти совпадают → порог упирается в верхнюю границу.
        assert_eq!(print.threshold, MAX_THRESHOLD);
        assert_eq!(print.model, "model.onnx");
        assert_eq!(print.samples, 3);

        let spread = [unit(&[1.0, 0.0]), unit(&[0.0, 1.0])];
        let print = Voiceprint::from_embeddings(model, &spread, vec![]).unwrap();
        // Худшее сходство = cos 45° ≈ 0.707 → порог ≈ 0.557.
        let expected = std::f32::consts::FRAC_1_SQRT_2 - THRESHOLD_MARGIN;
        assert!(
            (print.threshold - expected).abs() < 1e-3,
            "{}",
            print.threshold
        );
    }

    #[test]
    fn voiceprint_rejects_empty_or_mismatched_dims() {
        let model = Path::new("m.onnx");
        assert!(Voiceprint::from_embeddings(model, &[], vec![]).is_none());
        assert!(Voiceprint::from_embeddings(model, &[vec![1.0], vec![1.0, 0.0]], vec![]).is_none());
    }

    #[test]
    fn save_load_roundtrip_and_model_check() {
        let dir = tempfile::tempdir().unwrap();
        let model = Path::new("models/a.onnx");
        assert!(Voiceprint::load(dir.path(), model).unwrap().is_none());

        let print = Voiceprint::from_embeddings(model, &[unit(&[0.6, 0.8])], vec!["жарвис".into()])
            .unwrap();
        print.save(dir.path()).unwrap();
        assert_eq!(Voiceprint::load(dir.path(), model).unwrap(), Some(print));

        let err = Voiceprint::load(dir.path(), Path::new("models/b.onnx")).unwrap_err();
        assert!(err.to_string().contains("образец голоса"), "{err}");
    }
}

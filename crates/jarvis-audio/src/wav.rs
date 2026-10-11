//! Чтение и запись WAV (через sherpa-onnx): тестовые записи, образцы, звуки.

use std::path::Path;

use crate::AudioError;
use crate::models::path_str;
use crate::resample::resample_clip;

/// Моно-клип в памяти.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Clip {
    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }

    /// Клип без тишины по краям (синтезаторы добавляют её, а для ответа
    /// «Да, сэр» каждая миллисекунда в начале — задержка). Оставляет 10 мс.
    pub fn trimmed(&self) -> Clip {
        const LEVEL: f32 = 0.01;
        let pad = self.sample_rate as usize / 100;
        let first = self.samples.iter().position(|s| s.abs() > LEVEL);
        let last = self.samples.iter().rposition(|s| s.abs() > LEVEL);
        let (Some(first), Some(last)) = (first, last) else {
            return self.clone();
        };
        let start = first.saturating_sub(pad);
        let end = (last + pad + 1).min(self.samples.len());
        Clip {
            samples: self.samples[start..end].to_vec(),
            sample_rate: self.sample_rate,
        }
    }

    /// Тот же клип с другой частотой дискретизации.
    pub fn resampled(&self, rate: u32) -> Result<Clip, AudioError> {
        Ok(Clip {
            samples: resample_clip(&self.samples, self.sample_rate, rate)?,
            sample_rate: rate,
        })
    }
}

/// Читает WAV (sherpa-onnx берёт первый канал многоканальных файлов).
pub fn read(path: &Path) -> Result<Clip, AudioError> {
    if !path.is_file() {
        return Err(AudioError::Read {
            path: path.to_path_buf(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
    }
    let wave = sherpa_onnx::Wave::read(&path_str(path)).ok_or_else(|| AudioError::Wav {
        path: path.to_path_buf(),
        reason: "не удалось разобрать (нужен PCM WAV)".into(),
    })?;
    let rate = u32::try_from(wave.sample_rate()).unwrap_or(0);
    if rate == 0 {
        return Err(AudioError::Wav {
            path: path.to_path_buf(),
            reason: "нулевая частота дискретизации".into(),
        });
    }
    Ok(Clip {
        samples: wave.samples().to_vec(),
        sample_rate: rate,
    })
}

pub fn write(path: &Path, clip: &Clip) -> Result<(), AudioError> {
    if sherpa_onnx::write(&path_str(path), &clip.samples, clip.sample_rate as i32) {
        Ok(())
    } else {
        Err(AudioError::Wav {
            path: path.to_path_buf(),
            reason: "не удалось записать".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_keeps_short_padding() {
        let mut samples = vec![0.0; 1600]; // 100 мс тишины
        samples.extend(vec![0.5; 800]);
        samples.extend(vec![0.0; 3200]);
        let clip = Clip {
            samples,
            sample_rate: 16_000,
        };
        let t = clip.trimmed();
        // 10 мс до + 50 мс звука + 10 мс после.
        assert_eq!(t.samples.len(), 160 + 800 + 160);
        assert_eq!(t.samples[160], 0.5);
    }

    #[test]
    fn silent_clip_is_left_alone() {
        let clip = Clip {
            samples: vec![0.0; 100],
            sample_rate: 16_000,
        };
        assert_eq!(clip.trimmed(), clip);
    }
}

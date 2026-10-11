//! Потоковый ресемплинг моно-сигнала к 16 кГц (частота моделей).
//!
//! FFT-ресемплер rubato: качественный антиалиасинг при фиксированном
//! соотношении частот. Работает в потоке обработки, не в аудио-callback.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::AudioError;

/// Частота, с которой работают все модели.
pub const MODEL_RATE: u32 = 16_000;

pub struct StreamResampler {
    inner: Option<Fft<f32>>,
    pending: Vec<f32>,
    output: Vec<f32>,
}

impl StreamResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Self, AudioError> {
        if input_rate == output_rate {
            return Ok(Self {
                inner: None,
                pending: Vec::new(),
                output: Vec::new(),
            });
        }
        // Порция ~10 мс входа: малая задержка при хорошем качестве.
        let chunk = (input_rate as usize / 100).max(64);
        let fft = Fft::<f32>::new(
            input_rate as usize,
            output_rate as usize,
            chunk,
            1,
            FixedSync::Input,
        )
        .map_err(|e| AudioError::Resample(e.to_string()))?;
        let output = vec![0.0; fft.output_frames_max()];
        Ok(Self {
            pending: Vec::with_capacity(fft.input_frames_max() * 4),
            output,
            inner: Some(fft),
        })
    }

    /// Задержка ресемплера в выходных отсчётах.
    pub fn delay(&self) -> usize {
        self.inner.as_ref().map_or(0, |r| r.output_delay())
    }

    /// Принимает входные отсчёты и отдаёт готовые выходные в `sink`
    /// (возможно, несколькими порциями или ни одной).
    pub fn process(
        &mut self,
        input: &[f32],
        mut sink: impl FnMut(&[f32]),
    ) -> Result<(), AudioError> {
        let Some(fft) = self.inner.as_mut() else {
            sink(input);
            return Ok(());
        };
        self.pending.extend_from_slice(input);
        let mut consumed = 0;
        loop {
            let need = fft.input_frames_next();
            if self.pending.len() - consumed < need {
                break;
            }
            let chunk = &self.pending[consumed..consumed + need];
            let input = InterleavedSlice::new(chunk, 1, need)
                .map_err(|e| AudioError::Resample(e.to_string()))?;
            let capacity = self.output.len();
            let mut output = InterleavedSlice::new_mut(&mut self.output, 1, capacity)
                .map_err(|e| AudioError::Resample(e.to_string()))?;
            let (read, written) = fft
                .process_into_buffer(&input, &mut output, None)
                .map_err(|e| AudioError::Resample(e.to_string()))?;
            consumed += read;
            sink(&self.output[..written]);
        }
        self.pending.drain(..consumed);
        Ok(())
    }
}

/// Ресемплинг целого клипа (звуки ответа, WAV-файлы) — не для потока.
pub fn resample_clip(
    samples: &[f32],
    input_rate: u32,
    output_rate: u32,
) -> Result<Vec<f32>, AudioError> {
    if input_rate == output_rate {
        return Ok(samples.to_vec());
    }
    let mut resampler = StreamResampler::new(input_rate, output_rate)?;
    let delay = resampler.delay();
    let mut out = Vec::with_capacity(
        (samples.len() as u64 * output_rate as u64 / input_rate as u64) as usize + delay + 1024,
    );
    resampler.process(samples, |chunk| out.extend_from_slice(chunk))?;
    // Дотолкнуть хвост тишиной, чтобы ресемплер отдал задержанные отсчёты.
    let tail = vec![0.0; (input_rate as usize / 10).max(2048)];
    resampler.process(&tail, |chunk| out.extend_from_slice(chunk))?;
    let expected = (samples.len() as u64 * output_rate as u64 / input_rate as u64) as usize;
    let start = delay.min(out.len());
    let end = (start + expected).min(out.len());
    Ok(out[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// Частота по числу переходов через ноль.
    fn estimate_freq(s: &[f32], rate: u32) -> f32 {
        let crossings = s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * rate as f32 / s.len() as f32
    }

    #[test]
    fn passthrough_when_rates_match() {
        let mut r = StreamResampler::new(16_000, 16_000).unwrap();
        let mut out = Vec::new();
        r.process(&[0.1, 0.2, 0.3], |c| out.extend_from_slice(c))
            .unwrap();
        assert_eq!(out, [0.1, 0.2, 0.3]);
    }

    #[test]
    fn stream_48k_to_16k_keeps_length_and_pitch() {
        let input = sine(440.0, 48_000, 1.0);
        let mut r = StreamResampler::new(48_000, 16_000).unwrap();
        let mut out = Vec::new();
        // Порции произвольного размера, как из кольцевого буфера.
        for chunk in input.chunks(333) {
            r.process(chunk, |c| out.extend_from_slice(c)).unwrap();
        }
        // Почти всё вышло (кроме задержки и неполной последней порции).
        assert!(out.len() > 15_000 && out.len() <= 16_000, "{}", out.len());
        let tail = &out[out.len() / 2..];
        let f = estimate_freq(tail, 16_000);
        assert!((f - 440.0).abs() < 5.0, "частота {f}");
    }

    #[test]
    fn high_frequency_is_filtered_out() {
        // 12 кГц выше Найквиста для 16 кГц — должно подавиться, а не завернуться.
        let input = sine(12_000.0, 48_000, 0.5);
        let out = resample_clip(&input, 48_000, 16_000).unwrap();
        let rms = (out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms < 0.01, "rms {rms}");
    }

    #[test]
    fn clip_44100_to_48000_has_exact_length() {
        let input = sine(1000.0, 44_100, 0.5);
        let out = resample_clip(&input, 44_100, 48_000).unwrap();
        assert_eq!(out.len(), 24_000);
        let f = estimate_freq(&out[2000..22000], 48_000);
        assert!((f - 1000.0).abs() < 10.0, "частота {f}");
    }
}

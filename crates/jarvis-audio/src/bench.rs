//! Прогон движка слова активации по записи — как будто звук идёт с
//! микрофона порциями по 20 мс. Для `jarvis --bench` и тестов на WAV.

use std::time::{Duration, Instant};

use crate::resample::MODEL_RATE;
use crate::wake::{WakeDetection, WakeWordEngine};

/// Порция, которой подаётся звук (как в живом конвейере).
pub const CHUNK: usize = MODEL_RATE as usize / 50;
/// Тишина до и после записи.
const LEAD_SECS: f32 = 0.5;
const TAIL_SECS: f32 = 1.5;
/// Порог «есть звук» для поиска конца речи.
const SPEECH_LEVEL: f32 = 0.01;

#[derive(Debug, Clone)]
pub struct WakeRun {
    /// Срабатывания: момент (секунды от начала записи) и что услышано.
    pub detections: Vec<(f32, WakeDetection)>,
    /// Конец речи в записи (секунды от начала записи), по уровню сигнала.
    pub speech_end: f32,
    /// Длительность поданного звука, включая тишину.
    pub audio_secs: f32,
    /// Чистое время обработки.
    pub processing: Duration,
    /// Самая долгая обработка одной порции.
    pub worst_chunk: Duration,
}

impl WakeRun {
    /// Доля одного ядра CPU, которую занимает распознавание (real-time factor).
    pub fn rtf(&self) -> f64 {
        self.processing.as_secs_f64() / f64::from(self.audio_secs)
    }

    /// Задержка первого срабатывания от конца речи (для записей, где
    /// «Джарвис» — последнее слово). Отрицательная — сработало раньше.
    pub fn delay_after_speech(&self) -> Option<f32> {
        self.detections.first().map(|(t, _)| t - self.speech_end)
    }
}

/// `samples` — 16 кГц моно.
pub fn run_wake(engine: &mut dyn WakeWordEngine, samples: &[f32]) -> WakeRun {
    engine.reset();
    let lead = (LEAD_SECS * MODEL_RATE as f32) as usize;
    let tail = (TAIL_SECS * MODEL_RATE as f32) as usize;
    let mut audio = vec![0.0; lead];
    audio.extend_from_slice(samples);
    audio.extend(std::iter::repeat_n(0.0, tail));

    let speech_end = samples
        .iter()
        .rposition(|s| s.abs() > SPEECH_LEVEL)
        .map_or(0.0, |i| (i + 1) as f32 / MODEL_RATE as f32);

    let mut detections = Vec::new();
    let mut processing = Duration::ZERO;
    let mut worst_chunk = Duration::ZERO;
    let mut fed = 0usize;
    for chunk in audio.chunks(CHUNK) {
        let started = Instant::now();
        let detection = engine.accept(chunk);
        let spent = started.elapsed();
        processing += spent;
        worst_chunk = worst_chunk.max(spent);
        fed += chunk.len();
        if let Some(d) = detection {
            let at = fed as f32 / MODEL_RATE as f32 - LEAD_SECS;
            detections.push((at, d));
        }
    }
    WakeRun {
        detections,
        speech_end,
        audio_secs: audio.len() as f32 / MODEL_RATE as f32,
        processing,
        worst_chunk,
    }
}

/// Конец речи (секунды) — для отчётов по фрагментам.
pub fn speech_end_secs(samples: &[f32]) -> f32 {
    samples
        .iter()
        .rposition(|s| s.abs() > SPEECH_LEVEL)
        .map_or(0.0, |i| (i + 1) as f32 / MODEL_RATE as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Движок-заглушка: срабатывает, когда в порции есть громкий отсчёт.
    struct Loud;
    impl WakeWordEngine for Loud {
        fn accept(&mut self, samples: &[f32]) -> Option<WakeDetection> {
            samples.iter().any(|s| *s > 0.9).then(|| WakeDetection {
                heard: "громко".into(),
                started_secs_ago: None,
            })
        }
        fn reset(&mut self) {}
        fn name(&self) -> &'static str {
            "loud"
        }
    }

    #[test]
    fn timing_is_relative_to_recording_start() {
        // 1 с тишины, потом 1 с громкого звука.
        let mut samples = vec![0.0; MODEL_RATE as usize];
        samples.extend(vec![1.0; MODEL_RATE as usize]);
        let run = run_wake(&mut Loud, &samples);
        assert!((run.speech_end - 2.0).abs() < 1e-3);
        let (first, _) = &run.detections[0];
        // Первая громкая порция заканчивается через 20 мс после начала звука.
        assert!((first - 1.02).abs() < 1e-3, "{first}");
        assert!((run.audio_secs - 4.0).abs() < 1e-3);
        assert!(run.delay_after_speech().unwrap() < 0.0);
    }
}

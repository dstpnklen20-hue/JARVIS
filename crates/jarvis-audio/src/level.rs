//! Текущая громкость микрофона и вывода — для живого реактора в главном
//! окне. Пишется из звуковых потоков (одна атомарная запись: без
//! блокировок и аллокаций, можно из callback'а реального времени), читается
//! окном.

use std::sync::atomic::{AtomicU32, Ordering};

/// Громкость 0…1 (шкала в децибелах — так её слышит человек).
pub struct Level(AtomicU32);

impl Level {
    const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// Записать громкость по среднеквадратичному значению сигнала.
    pub fn set_rms(&self, rms: f32) {
        self.0.store(loudness(rms).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Микрофон (то, что слышит модель; пока Джарвис говорит — тишина).
pub static MIC: Level = Level::new();
/// Вывод: голос Джарвиса.
pub static OUTPUT: Level = Level::new();

/// −55 дБ и тише — 0, −15 дБ и громче — 1.
const FLOOR_DB: f32 = -55.0;
const CEIL_DB: f32 = -15.0;

fn loudness(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Среднеквадратичное значение отсчётов.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_maps_decibels_to_unit_range() {
        assert_eq!(loudness(0.0), 0.0);
        assert_eq!(loudness(f32::NAN), 0.0);
        assert_eq!(loudness(1.0), 1.0);
        // −35 дБ — середина шкалы.
        assert!((loudness(10f32.powf(-35.0 / 20.0)) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn level_keeps_last_value() {
        let level = Level::new();
        level.set_rms(1.0);
        assert_eq!(level.get(), 1.0);
        level.set_rms(0.0);
        assert_eq!(level.get(), 0.0);
        assert!((rms(&[0.5, -0.5]) - 0.5).abs() < 1e-6);
    }
}

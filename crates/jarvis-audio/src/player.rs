//! Воспроизведение готовых ответов.
//!
//! Выходной поток открыт постоянно: ответ начинает звучать через один период
//! буфера устройства, без задержки на открытие. Клипы при загрузке
//! переводятся в частоту устройства, а callback только копирует отсчёты —
//! без блокировок и аллокаций (команды приходят через lock-free очередь).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SampleFormat, SizedSample};
use jarvis_core::state::Reply;
use ringbuf::traits::{Consumer as _, Producer as _, Split as _};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::AudioError;
use crate::error::device_err;
use crate::pack::VoicePack;

enum Command {
    Play { samples: Arc<[f32]>, gain: f32 },
    Stop,
}

pub struct Player {
    _stream: cpal::Stream,
    commands: HeapProd<Command>,
    clips: HashMap<Reply, Vec<Arc<[f32]>>>,
    last: HashMap<Reply, usize>,
    rng: u64,
    device_rate: u32,
    gain: f32,
}

impl Player {
    /// Открывает устройство вывода по умолчанию и готовит клипы пакета.
    pub fn new(pack: &VoicePack, volume: f32) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoDefaultOutput)?;
        let supported = device.default_output_config().map_err(device_err)?;
        let config = supported.config();
        let device_rate = config.sample_rate;
        let channels = usize::from(config.channels).max(1);

        let mut clips = HashMap::new();
        for reply in Reply::ALL {
            let variants = pack
                .variants(reply)
                .iter()
                .map(|clip| {
                    clip.trimmed()
                        .resampled(device_rate)
                        .map(|c| Arc::<[f32]>::from(c.samples))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !variants.is_empty() {
                clips.insert(reply, variants);
            }
        }

        let (producer, consumer) = HeapRb::<Command>::new(16).split();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, channels, consumer),
            SampleFormat::I16 => build::<i16>(&device, &config, channels, consumer),
            SampleFormat::I32 => build::<i32>(&device, &config, channels, consumer),
            SampleFormat::U16 => build::<u16>(&device, &config, channels, consumer),
            other => return Err(AudioError::UnsupportedFormat(other.to_string())),
        }?;
        stream.play().map_err(device_err)?;
        tracing::info!(
            device = %device,
            rate = device_rate,
            channels,
            pack = %pack.name,
            "вывод звука готов"
        );

        Ok(Self {
            _stream: stream,
            commands: producer,
            clips,
            last: HashMap::new(),
            rng: seed(),
            device_rate,
            gain: volume,
        })
    }

    /// Проигрывает один из вариантов ответа (прерывая текущий звук).
    /// Возвращает длительность или `None`, если в пакете нет такого ответа.
    pub fn play(&mut self, reply: Reply) -> Option<Duration> {
        let variants = self.clips.get(&reply)?;
        self.rng = xorshift(self.rng);
        let index = pick(variants.len(), self.last.get(&reply).copied(), self.rng);
        self.last.insert(reply, index);
        let samples = Arc::clone(&variants[index]);
        let duration = Duration::from_secs_f64(samples.len() as f64 / f64::from(self.device_rate));
        if self
            .commands
            .try_push(Command::Play {
                samples,
                gain: self.gain,
            })
            .is_err()
        {
            tracing::warn!("очередь звука переполнена, ответ пропущен");
            return None;
        }
        Some(duration)
    }

    /// Немедленно замолчать.
    pub fn stop(&mut self) {
        let _ = self.commands.try_push(Command::Stop);
    }
}

/// Индекс варианта: случайный, но не тот же, что в прошлый раз.
fn pick(len: usize, last: Option<usize>, random: u64) -> usize {
    match (len, last) {
        (0 | 1, _) => 0,
        (_, Some(last)) if last < len => {
            let offset = 1 + (random as usize) % (len - 1);
            (last + offset) % len
        }
        _ => (random as usize) % len,
    }
}

fn xorshift(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos | 1
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut commands: HeapCons<Command>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let mut current: Option<(Arc<[f32]>, usize, f32)> = None;
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |out: &mut [T], _info| {
                // Реальное время: без блокировок и аллокаций. Старый клип при
                // замене только уменьшает счётчик ссылок — память держит Player.
                while let Some(cmd) = commands.try_pop() {
                    current = match cmd {
                        Command::Play { samples, gain } => Some((samples, 0, gain)),
                        Command::Stop => None,
                    };
                }
                for frame in out.chunks_exact_mut(channels) {
                    let value = match current.as_mut() {
                        Some((samples, pos, gain)) if *pos < samples.len() => {
                            let v = samples[*pos] * *gain;
                            *pos += 1;
                            v.clamp(-1.0, 1.0)
                        }
                        _ => 0.0,
                    };
                    let sample = T::from_sample_(value);
                    frame.iter_mut().for_each(|s| *s = sample);
                }
                if matches!(&current, Some((samples, pos, _)) if *pos >= samples.len()) {
                    current = None;
                }
            },
            |err: cpal::Error| {
                if crate::capture::is_fatal(err.kind()) {
                    tracing::error!(error = %err, "ошибка потока вывода звука");
                } else {
                    tracing::debug!(error = %err, "сбой потока вывода звука");
                }
            },
            None,
        )
        .map_err(device_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_never_repeats_last_variant() {
        let mut rng = 12345u64;
        let mut last = None;
        for _ in 0..200 {
            rng = xorshift(rng);
            let i = pick(3, last, rng);
            assert!(i < 3);
            assert_ne!(Some(i), last);
            last = Some(i);
        }
    }

    #[test]
    fn pick_handles_single_and_stale_last() {
        assert_eq!(pick(1, Some(0), 99), 0);
        assert_eq!(pick(0, None, 99), 0);
        assert!(pick(2, Some(7), 99) < 2);
    }

    #[test]
    fn all_variants_get_used() {
        let mut rng = 1u64;
        let mut seen = [false; 4];
        let mut last = None;
        for _ in 0..100 {
            rng = xorshift(rng);
            let i = pick(4, last, rng);
            seen[i] = true;
            last = Some(i);
        }
        assert!(seen.iter().all(|&s| s));
    }
}

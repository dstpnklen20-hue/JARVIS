//! Воспроизведение готовых ответов.
//!
//! Выходной поток открыт постоянно: ответ начинает звучать через один период
//! буфера устройства, без задержки на открытие. Клипы при загрузке
//! переводятся в частоту устройства, а callback только копирует отсчёты —
//! без блокировок и аллокаций (команды приходят через lock-free очередь).
//!
//! Если поток сломался (сменилось устройство вывода по умолчанию, наушники
//! отключили — WASAPI в этом случае поток не переключает, а останавливает),
//! [`Player::maintain`] открывает вывод заново на текущем устройстве.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
    output: Output,
    /// Клипы пакета в исходной частоте — для пересчёта, если новое
    /// устройство вывода работает на другой частоте.
    sources: HashMap<Reply, Vec<crate::wav::Clip>>,
    clips: ReplyClips,
    last: HashMap<Reply, usize>,
    rng: u64,
    gain: f32,
    /// Последние произвольные клипы. Поток вывода отпускает клип, когда
    /// он доиграл или его сменил следующий; пока клип есть и здесь, его
    /// память не освобождается в потоке реального времени.
    adhoc: VecDeque<Arc<[f32]>>,
    /// Последняя попытка переоткрыть сломанный вывод.
    last_reopen: Option<Instant>,
}

/// Открытый поток вывода.
struct Output {
    _stream: cpal::Stream,
    commands: HeapProd<Command>,
    device_rate: u32,
    /// Поток сломался — нужно открыть заново (ставит callback ошибок).
    broken: Arc<AtomicBool>,
}

/// Варианты каждого ответа, готовые для устройства вывода.
type ReplyClips = HashMap<Reply, Vec<Arc<[f32]>>>;

/// Сколько последних произвольных клипов держать (хватило бы двух).
const ADHOC_KEEP: usize = 4;
/// Не чаще — попытки переоткрыть вывод, пока устройства нет.
const REOPEN_INTERVAL: Duration = Duration::from_secs(1);

impl Player {
    /// Открывает устройство вывода по умолчанию и готовит клипы пакета.
    pub fn new(pack: &VoicePack, volume: f32) -> Result<Self, AudioError> {
        let sources: HashMap<_, _> = Reply::ALL
            .into_iter()
            .map(|reply| {
                (
                    reply,
                    pack.variants(reply)
                        .iter()
                        .map(|c| c.trimmed())
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let output = Output::open()?;
        let clips = resample_all(&sources, output.device_rate)?;
        tracing::info!(pack = %pack.name, "звуковой пакет готов");
        Ok(Self {
            output,
            sources,
            clips,
            last: HashMap::new(),
            rng: seed(),
            gain: volume,
            adhoc: VecDeque::new(),
            last_reopen: None,
        })
    }

    /// Если поток вывода сломался — открыть заново на текущем устройстве по
    /// умолчанию (не чаще раза в секунду). Ядро вызывает это каждый такт;
    /// пока всё в порядке, это одна атомарная проверка.
    pub fn maintain(&mut self) {
        if !self.output.broken.load(Ordering::Relaxed) {
            return;
        }
        if self
            .last_reopen
            .is_some_and(|t| t.elapsed() < REOPEN_INTERVAL)
        {
            return;
        }
        self.last_reopen = Some(Instant::now());
        let reopened = Output::open().and_then(|output| {
            let clips = if output.device_rate == self.output.device_rate {
                None
            } else {
                Some(resample_all(&self.sources, output.device_rate)?)
            };
            Ok((output, clips))
        });
        match reopened {
            Ok((output, clips)) => {
                self.output = output;
                if let Some(clips) = clips {
                    self.clips = clips;
                }
                self.adhoc.clear();
                self.last_reopen = None;
            }
            Err(e) => {
                tracing::warn!(error = %jarvis_core::error_chain(&e), "вывод звука пока не открывается, пробую снова");
            }
        }
    }

    /// Проигрывает один из вариантов ответа (прерывая текущий звук).
    /// Возвращает длительность или `None`, если в пакете нет такого ответа.
    pub fn play(&mut self, reply: Reply) -> Option<Duration> {
        self.maintain();
        let variants = self.clips.get(&reply)?;
        self.rng = xorshift(self.rng);
        let index = pick(variants.len(), self.last.get(&reply).copied(), self.rng);
        self.last.insert(reply, index);
        let samples = Arc::clone(&variants[index]);
        let duration =
            Duration::from_secs_f64(samples.len() as f64 / f64::from(self.output.device_rate));
        if self
            .output
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

    /// Проигрывает произвольный клип (действие `say` со своим файлом).
    /// Ресемплинг — здесь, не в потоке вывода.
    pub fn play_clip(&mut self, clip: &crate::wav::Clip) -> Result<Duration, AudioError> {
        self.maintain();
        let samples: Arc<[f32]> = clip
            .trimmed()
            .resampled(self.output.device_rate)?
            .samples
            .into();
        let duration =
            Duration::from_secs_f64(samples.len() as f64 / f64::from(self.output.device_rate));
        self.adhoc.push_back(Arc::clone(&samples));
        if self.adhoc.len() > ADHOC_KEEP {
            self.adhoc.pop_front();
        }
        self.output
            .commands
            .try_push(Command::Play {
                samples,
                gain: self.gain,
            })
            .map_err(|_| AudioError::Device("очередь звука переполнена".into()))?;
        Ok(duration)
    }

    /// Немедленно замолчать.
    pub fn stop(&mut self) {
        let _ = self.output.commands.try_push(Command::Stop);
    }
}

impl Output {
    /// Открывает устройство вывода по умолчанию.
    fn open() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoDefaultOutput)?;
        let supported = device.default_output_config().map_err(device_err)?;
        let config = supported.config();
        let channels = usize::from(config.channels).max(1);
        let broken = Arc::new(AtomicBool::new(false));
        let (producer, consumer) = HeapRb::<Command>::new(16).split();
        let flag = Arc::clone(&broken);
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, channels, consumer, flag),
            SampleFormat::I16 => build::<i16>(&device, &config, channels, consumer, flag),
            SampleFormat::I32 => build::<i32>(&device, &config, channels, consumer, flag),
            SampleFormat::U16 => build::<u16>(&device, &config, channels, consumer, flag),
            other => return Err(AudioError::UnsupportedFormat(other.to_string())),
        }?;
        stream.play().map_err(device_err)?;
        tracing::info!(device = %device, rate = config.sample_rate, channels, "вывод звука открыт");
        Ok(Self {
            _stream: stream,
            commands: producer,
            device_rate: config.sample_rate,
            broken,
        })
    }
}

/// Клипы пакета в частоте устройства.
fn resample_all(
    sources: &HashMap<Reply, Vec<crate::wav::Clip>>,
    rate: u32,
) -> Result<ReplyClips, AudioError> {
    sources
        .iter()
        .map(|(reply, clips)| {
            let resampled = clips
                .iter()
                .map(|c| c.resampled(rate).map(|c| Arc::<[f32]>::from(c.samples)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((*reply, resampled))
        })
        .collect()
}

/// Ошибка потока вывода, после которой поток надо открыть заново.
/// Xrun и отказ в приоритете реального времени — поток продолжает работать;
/// DeviceChanged — поток сам переключился на новое устройство.
fn needs_reopen(kind: cpal::ErrorKind) -> bool {
    crate::capture::is_fatal(kind)
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
    broken: Arc<AtomicBool>,
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
            move |err: cpal::Error| {
                if needs_reopen(err.kind()) {
                    // Повторные ошибки того же потока не засоряют лог.
                    if !broken.swap(true, Ordering::Relaxed) {
                        tracing::warn!(error = %err, "поток вывода звука сломался — переоткрою");
                    }
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

    /// Сломанный поток (как после смены устройства по умолчанию) открывается
    /// заново, и ответы снова играют.
    #[test]
    #[ignore = "нужно устройство вывода звука"]
    fn broken_output_is_reopened() {
        let pack_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sounds/default");
        let pack = VoicePack::load(&pack_dir).unwrap();
        let mut player = Player::new(&pack, 0.0).unwrap();
        let old = Arc::clone(&player.output.broken);

        player.output.broken.store(true, Ordering::Relaxed);
        player.maintain();
        assert!(
            !player.output.broken.load(Ordering::Relaxed),
            "вывод открыт заново"
        );
        assert!(!Arc::ptr_eq(&old, &player.output.broken), "это новый поток");
        assert!(player.play(Reply::Wake).is_some());

        // Пока устройство не открывается, попытки — не чаще раза в секунду.
        player.output.broken.store(true, Ordering::Relaxed);
        player.last_reopen = Some(Instant::now());
        player.maintain();
        assert!(
            player.output.broken.load(Ordering::Relaxed),
            "рано для новой попытки"
        );
    }

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

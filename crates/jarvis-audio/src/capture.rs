//! Захват звука с микрофона.
//!
//! Callback cpal работает в потоке реального времени: он **не блокируется и
//! не выделяет память** — только сводит каналы в моно и кладёт отсчёты в
//! lock-free кольцевой буфер. Всё остальное делает поток обработки,
//! читающий буфер через [`Capture::read`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer as _, Observer as _, Producer as _, Split as _};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::AudioError;
use crate::error::device_err;

/// Ёмкость буфера: столько секунд звука переживёт подвисание обработки.
const BUFFER_SECS: usize = 2;

/// Счётчики, которые callback обновляет без блокировок.
#[derive(Debug, Default)]
pub struct CaptureStats {
    /// Отсчёты, не поместившиеся в буфер (обработка не успевала).
    pub dropped: AtomicU64,
    /// Устройство сообщило о неисправимой ошибке (например, микрофон отключили).
    pub failed: AtomicBool,
    /// Мелкие сбои, которые поток пережил (xrun и т. п.).
    pub glitches: AtomicU64,
}

pub struct Capture {
    // Поток держит микрофон, пока жив этот объект.
    _stream: cpal::Stream,
    consumer: HeapCons<f32>,
    sample_rate: u32,
    device_name: String,
    stats: Arc<CaptureStats>,
}

impl Capture {
    /// Открывает микрофон: `None` — системный по умолчанию, иначе первое
    /// устройство, в имени которого есть `device` (без учёта регистра).
    pub fn open(device: Option<&str>) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = match device {
            None => host
                .default_input_device()
                .ok_or(AudioError::NoDefaultInput)?,
            Some(part) => {
                let wanted = part.to_lowercase();
                host.input_devices()
                    .map_err(device_err)?
                    .find(|d| d.to_string().to_lowercase().contains(&wanted))
                    .ok_or_else(|| AudioError::DeviceNotFound(part.to_owned()))?
            }
        };
        let device_name = device.to_string();
        let supported = device.default_input_config().map_err(device_err)?;
        let config = supported.config();
        let sample_rate = config.sample_rate;
        let channels = usize::from(config.channels).max(1);

        let rb = HeapRb::<f32>::new(sample_rate as usize * BUFFER_SECS);
        let (producer, consumer) = rb.split();
        let stats = Arc::new(CaptureStats::default());

        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, channels, producer, &stats),
            SampleFormat::I16 => build::<i16>(&device, &config, channels, producer, &stats),
            SampleFormat::I32 => build::<i32>(&device, &config, channels, producer, &stats),
            SampleFormat::U16 => build::<u16>(&device, &config, channels, producer, &stats),
            SampleFormat::U8 => build::<u8>(&device, &config, channels, producer, &stats),
            SampleFormat::F64 => build::<f64>(&device, &config, channels, producer, &stats),
            other => return Err(AudioError::UnsupportedFormat(other.to_string())),
        }?;
        stream.play().map_err(device_err)?;
        tracing::info!(
            device = %device_name,
            rate = sample_rate,
            channels,
            format = %supported.sample_format(),
            "микрофон открыт"
        );

        Ok(Self {
            _stream: stream,
            consumer,
            sample_rate,
            device_name,
            stats,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn stats(&self) -> &CaptureStats {
        &self.stats
    }

    /// Сколько отсчётов ждёт в буфере.
    pub fn available(&self) -> usize {
        self.consumer.occupied_len()
    }

    /// Забирает до `buf.len()` моно-отсчётов; возвращает, сколько забрал.
    pub fn read(&mut self, buf: &mut [f32]) -> usize {
        self.consumer.pop_slice(buf)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    stats: &Arc<CaptureStats>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let data_stats = Arc::clone(stats);
    let error_stats = Arc::clone(stats);
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _info| {
                // Реальное время: без блокировок и аллокаций.
                let mut dropped = 0u64;
                for frame in data.chunks_exact(channels) {
                    if producer.try_push(downmix(frame)).is_err() {
                        dropped += 1;
                    }
                }
                if dropped > 0 {
                    data_stats.dropped.fetch_add(dropped, Ordering::Relaxed);
                }
            },
            move |err: cpal::Error| {
                // Ошибки редки и не на горячем пути — здесь можно логировать.
                if is_fatal(err.kind()) {
                    tracing::error!(error = %err, "ошибка потока микрофона");
                    error_stats.failed.store(true, Ordering::Release);
                } else {
                    error_stats.glitches.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(error = %err, "сбой потока микрофона, поток продолжает работать");
                }
            },
            None,
        )
        .map_err(device_err)
}

/// После каких ошибок поток мёртв и микрофон надо открывать заново.
///
/// Сбой буфера (xrun), переключение потока на новое устройство по
/// умолчанию и отказ в приоритете реального времени поток переживает —
/// переоткрывать его в этих случаях вредно: само переоткрытие вызывает
/// новые сбои (так было в первом живом тесте).
pub(crate) fn is_fatal(kind: cpal::ErrorKind) -> bool {
    use cpal::ErrorKind as K;
    !matches!(kind, K::Xrun | K::DeviceChanged | K::RealtimeDenied)
}

/// Среднее по каналам одного кадра.
fn downmix<T>(frame: &[T]) -> f32
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let sum: f32 = frame.iter().map(|&s| f32::from_sample_(s)).sum();
    sum / frame.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_failures_reopen_microphone() {
        use cpal::ErrorKind as K;
        for kind in [K::Xrun, K::DeviceChanged, K::RealtimeDenied] {
            assert!(!is_fatal(kind), "{kind:?}");
        }
        for kind in [
            K::DeviceNotAvailable,
            K::StreamInvalidated,
            K::BackendError,
            K::DeviceBusy,
        ] {
            assert!(is_fatal(kind), "{kind:?}");
        }
    }

    #[test]
    fn downmix_averages_channels_of_any_format() {
        assert_eq!(downmix(&[0.5f32, -0.5]), 0.0);
        assert_eq!(downmix(&[0.25f32]), 0.25);
        // i16: полная шкала → около ±1.
        let v = downmix(&[i16::MAX, i16::MAX]);
        assert!((v - 1.0).abs() < 1e-3, "{v}");
        let v = downmix(&[i16::MIN]);
        assert!((v + 1.0).abs() < 1e-3, "{v}");
    }
}

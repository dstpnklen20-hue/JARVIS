//! `jarvis --record файл.wav`: отладочная запись с микрофона.
//!
//! По умолчанию Джарвис звук на диск не пишет (раздел 2.5 ТЗ); эта команда —
//! явное включение записи для отладки: слышно, что получает модель.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_audio::capture::Capture;
use jarvis_audio::resample::{MODEL_RATE, StreamResampler};
use jarvis_audio::wav::{self, Clip};
use jarvis_core::Config;

pub fn run(config: &Config, path: &Path, seconds: u32) -> anyhow::Result<()> {
    let mut capture =
        Capture::open(config.audio.input_device_name()).context("микрофон не открылся")?;
    let mut resampler = StreamResampler::new(capture.sample_rate(), MODEL_RATE)?;
    println!("Запись {seconds} с с «{}»…", capture.device_name());

    let duration = Duration::from_secs(u64::from(seconds));
    let started = Instant::now();
    let mut raw = vec![0.0f32; 4096];
    let mut samples = Vec::with_capacity(MODEL_RATE as usize * seconds as usize);
    while started.elapsed() < duration {
        let n = capture.read(&mut raw);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        resampler.process(&raw[..n], |out| samples.extend_from_slice(out))?;
    }
    drop(capture);

    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    let db = |v: f32| 20.0 * v.max(1e-6).log10();
    println!(
        "Уровень: пик {:.0} dBFS, средний {:.0} dBFS{}",
        db(peak),
        db(rms),
        if peak < 0.01 {
            " — почти тишина: проверьте микрофон и его громкость в Windows"
        } else {
            ""
        }
    );
    wav::write(
        path,
        &Clip {
            samples,
            sample_rate: MODEL_RATE,
        },
    )?;
    println!("Сохранено: {}", path.display());
    Ok(())
}

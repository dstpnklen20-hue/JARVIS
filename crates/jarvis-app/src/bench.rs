//! `jarvis --bench`: замеры конвейера слуха на WAV-файлах (раздел 4.6 ТЗ).
//!
//! Без аргументов прогоняет tests/fixtures/wake и tests/fixtures/negative.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context as _;
use jarvis_audio::bench::run_wake;
use jarvis_audio::resample::MODEL_RATE;
use jarvis_audio::speaker::SpeakerEncoder;
use jarvis_audio::wake::SherpaWakeWord;
use jarvis_audio::wav;
use jarvis_core::Config;

use crate::hearing;

pub fn run(root: &Path, config: &Config, files: &[PathBuf]) -> anyhow::Result<()> {
    let files = if files.is_empty() {
        default_files(root)?
    } else {
        files.to_vec()
    };
    let hearing = hearing::prepare(root, config)?;

    let started = Instant::now();
    let mut engine = SherpaWakeWord::new(&hearing.models.asr, hearing.matcher.clone())?;
    let load_asr = started.elapsed();
    let started = Instant::now();
    let encoder = SpeakerEncoder::new(&hearing.models.speaker)?;
    let _ = encoder.embed(&vec![0.0; MODEL_RATE as usize]);
    let load_speaker = started.elapsed();

    println!(
        "Загрузка и прогрев: распознавание {} мс, голос {} мс",
        load_asr.as_millis(),
        load_speaker.as_millis()
    );
    println!("Слова активации: {}", hearing.matcher.targets().join(", "));
    println!();
    println!(
        "{:<28} {:<10} {:>12} {:>9} {:>10} {:>8} {:>8}",
        "файл", "услышано", "от конца, мс", "RTF", "порция, мс", "голос", "голос, мс"
    );

    let mut total_audio = 0.0f64;
    let mut total_proc = 0.0f64;
    let mut single_word = Vec::new();
    for file in &files {
        let samples = wav::read(file)?.resampled(MODEL_RATE)?.samples;
        let run = run_wake(&mut engine, &samples);
        total_audio += f64::from(run.audio_secs);
        total_proc += run.processing.as_secs_f64();

        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let heard = run
            .detections
            .iter()
            .map(|(_, d)| d.heard.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let delay = run
            .delay_after_speech()
            .map_or("—".to_owned(), |d| format!("{:+.0}", d * 1000.0));
        if run.detections.len() == 1 && run.delay_after_speech().is_some_and(|d| d >= 0.0) {
            single_word.extend(run.delay_after_speech());
        }

        // Сходство голоса с образцом (если записан).
        let (voice, voice_ms) = match &hearing.voiceprint {
            Some((print, _)) if !run.detections.is_empty() => {
                let t = Instant::now();
                let sim = encoder.embed(&samples).map(|e| print.similarity(&e));
                (
                    sim.map_or("—".into(), |s| format!("{s:.2}")),
                    format!("{}", t.elapsed().as_millis()),
                )
            }
            _ => ("—".into(), "—".into()),
        };
        println!(
            "{:<28} {:<10} {:>12} {:>9.3} {:>10.1} {:>8} {:>8}",
            name,
            if heard.is_empty() { "—" } else { &heard },
            delay,
            run.rtf(),
            run.worst_chunk.as_secs_f64() * 1000.0,
            voice,
            voice_ms
        );
    }

    let rtf = total_proc / total_audio.max(1e-9);
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!();
    println!(
        "Нагрузка распознавания: {:.1}% одного ядра = {:.2}% всего CPU ({cores} потоков)",
        rtf * 100.0,
        rtf * 100.0 / cores as f64
    );
    if !single_word.is_empty() {
        let avg = single_word.iter().sum::<f32>() / single_word.len() as f32;
        let max = single_word.iter().copied().fold(0.0f32, f32::max);
        println!(
            "Срабатывание после конца «Джарвис»: в среднем {:.0} мс, худшее {:.0} мс \
             (точность замера — 20 мс; плюс ~10–20 мс вывода звука)",
            avg * 1000.0,
            max * 1000.0
        );
    }
    println!("«от конца, мс» < 0 — слово сработало раньше конца фразы (оно было не последним).");
    Ok(())
}

fn default_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for kind in ["wake", "negative"] {
        let dir = root.join("tests").join("fixtures").join(kind);
        let mut found: Vec<_> = std::fs::read_dir(&dir)
            .with_context(|| format!("нет папки {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "wav"))
            .collect();
        found.sort();
        files.extend(found);
    }
    Ok(files)
}

//! `jarvis --bench`: замеры конвейера слуха на WAV-файлах (раздел 4.6 ТЗ).
//!
//! Без аргументов прогоняет tests/fixtures/wake и tests/fixtures/negative.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use jarvis_app::resolver::{CommandResolver, Resolution, hotword_phrases};
use jarvis_app::simulate::run_command;
use jarvis_audio::asr::Hotwords;
use jarvis_audio::bench::run_wake;
use jarvis_audio::engines::{self, Engines};
use jarvis_audio::resample::MODEL_RATE;
use jarvis_audio::speaker::SpeakerEncoder;
use jarvis_audio::stt::SherpaStt;
use jarvis_audio::wav;
use jarvis_core::Config;
use jarvis_core::commands::Registry;
use jarvis_core::config::resolve;

use crate::hearing;

pub fn run(root: &Path, config: &Config, files: &[PathBuf]) -> anyhow::Result<()> {
    let explicit_files_empty = files.is_empty();
    let files = if files.is_empty() {
        default_files(root)?
    } else {
        files.to_vec()
    };
    let hearing = hearing::prepare(root, config)?;
    let registry = Registry::load(&resolve(root, &config.paths.commands));
    let hotwords = (config.listen.hotwords_score > 0.0).then(|| Hotwords {
        phrases: hotword_phrases(&registry, &config.wake.words),
        score: config.listen.hotwords_score,
    });

    // Та же сборка, что в демоне: один распознаватель, два потока.
    let started = Instant::now();
    let Engines {
        wake: mut engine,
        stt,
    } = engines::build(
        &hearing.models.asr,
        hearing.matcher.clone(),
        config.listen.end_silence_ms as f32 / 1000.0,
        hotwords.as_ref(),
    )?;
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

    if explicit_files_empty {
        let resolver = CommandResolver::new(&registry, hearing.matcher.clone(), &config.nlu);
        bench_commands(root, config, &resolver, stt)?;
    }
    Ok(())
}

/// Команды: распознавание, сопоставление, когда принято решение.
/// Ожидаемый id — имя файла до «__» (`unknown_*` — «не понял»).
fn bench_commands(
    root: &Path,
    config: &Config,
    resolver: &CommandResolver,
    mut stt: SherpaStt,
) -> anyhow::Result<()> {
    let early_stable = Duration::from_millis(u64::from(config.nlu.early_stable_ms));

    println!();
    println!(
        "Команды (конец фразы — тишина {} мс; раннее решение — сходство ≥ {}, текст стабилен {} мс):",
        config.listen.end_silence_ms, config.nlu.early_score, config.nlu.early_stable_ms
    );
    println!(
        "{:<34} {:<26} {:<18} {:>3} {:>12} {:>6}",
        "файл", "текст", "решение", "", "от конца, мс", "как"
    );
    let mut ok = 0;
    let mut total = 0;
    let mut delays = Vec::new();
    for file in files_in(root, "commands")? {
        let name = file
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let expected = name
            .split("__")
            .next()
            .unwrap_or_default()
            .replacen("builtin_", "builtin:", 1);
        let samples = wav::read(&file)?.resampled(MODEL_RATE)?.samples;
        let run = run_command(
            &mut stt,
            resolver,
            config.nlu.early_score,
            early_stable,
            &samples,
        );
        let decided = match &run.resolution {
            Resolution::Command { id, .. } => id.clone(),
            Resolution::NotUnderstood { .. } => "не понял".into(),
            Resolution::Ambiguous { .. } => "переспросить".into(),
            Resolution::Empty => "—".into(),
        };
        let correct = if expected.starts_with("unknown") {
            matches!(run.resolution, Resolution::NotUnderstood { .. })
        } else {
            decided == expected
        };
        total += 1;
        if correct {
            ok += 1;
            // Задержку считаем по выполненным командам: «не понял» по смыслу
            // ждёт конца фразы.
            if matches!(run.resolution, Resolution::Command { .. })
                && let Some(d) = run.decided_after_speech
            {
                delays.push(d);
            }
        }
        println!(
            "{:<34} {:<26} {:<18} {:>3} {:>12} {:>6}",
            name,
            run.text.chars().take(26).collect::<String>(),
            decided,
            if correct { "✓" } else { "✗" },
            run.decided_after_speech
                .map_or("—".into(), |d| format!("{:+.0}", d * 1000.0)),
            if run.early { "рано" } else { "итог" }
        );
    }
    println!();
    println!("Верно: {ok} из {total}");
    if !delays.is_empty() {
        delays.sort_by(f32::total_cmp);
        let avg = delays.iter().sum::<f32>() / delays.len() as f32;
        let median = delays[delays.len() / 2];
        let max = delays[delays.len() - 1];
        let within = delays.iter().filter(|d| **d <= 0.5).count();
        println!(
            "Команда выполнена после конца фразы: в среднем {:.0} мс, медиана {:.0} мс, худшее {:.0} мс; \
             ≤ 500 мс (цель ТЗ) — {within} из {}",
            avg * 1000.0,
            median * 1000.0,
            max * 1000.0,
            delays.len()
        );
    }
    Ok(())
}

fn files_in(root: &Path, kind: &str) -> anyhow::Result<Vec<PathBuf>> {
    let dir = root.join("tests").join("fixtures").join(kind);
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("нет папки {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    found.sort();
    Ok(found)
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

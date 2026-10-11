//! Подготовка слуха из конфига: модели, образец голоса, слова активации.
//! Общая для демона, `--bench` и `--check`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use jarvis_audio::models::SpeechModels;
use jarvis_audio::speaker::Voiceprint;
use jarvis_audio::wake::WakeMatcher;
use jarvis_core::Config;
use jarvis_core::config::{SpeakerThreshold, resolve};

pub struct Hearing {
    pub models: SpeechModels,
    /// Образец голоса и действующий порог, если проверка включена и образец есть.
    pub voiceprint: Option<(Voiceprint, f32)>,
    pub matcher: WakeMatcher,
}

pub fn data_dir(root: &Path, config: &Config) -> PathBuf {
    resolve(root, &config.paths.data)
}

pub fn prepare(root: &Path, config: &Config) -> anyhow::Result<Hearing> {
    let models = SpeechModels::locate(root, config).context("модели не найдены")?;

    let voiceprint = if config.speaker.verify {
        match Voiceprint::load(&data_dir(root, config), &models.speaker)? {
            Some(print) => {
                let threshold = match config.speaker.threshold {
                    SpeakerThreshold::Auto => print.threshold,
                    SpeakerThreshold::Fixed(t) => t,
                };
                tracing::info!(
                    threshold,
                    samples = print.samples,
                    "проверка голоса включена"
                );
                Some((print, threshold))
            }
            None => {
                tracing::info!(
                    "образец голоса не записан — Джарвис откликнется на любой голос; \
                     запишите свой: jarvis --enroll"
                );
                None
            }
        }
    } else {
        None
    };

    // К словам из конфига добавляем то, как распознаватель слышит именно вас.
    let mut words = config.wake.words.clone();
    if let Some((print, _)) = &voiceprint {
        words.extend(print.heard_as.iter().cloned());
    }
    let matcher = WakeMatcher::new(&words, config.wake.max_edit_distance);

    Ok(Hearing {
        models,
        voiceprint,
        matcher,
    })
}

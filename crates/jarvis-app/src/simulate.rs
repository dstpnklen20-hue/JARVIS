//! Прогон команды по записи — как в живом ядре: распознавание порциями по
//! 20 мс, раннее решение по частичному тексту, итог по концу фразы.
//! Для `jarvis --bench` и тестов на WAV.

use std::time::{Duration, Instant};

use jarvis_audio::bench::simulate_stream;
use jarvis_audio::stt::SpeechToText;

use crate::resolver::{CommandResolver, EarlyDecider, Resolution};

#[derive(Debug, Clone)]
pub struct CommandRun {
    /// Итоговый текст фразы.
    pub text: String,
    /// Что решило ядро (раннее решение или итог фразы).
    pub resolution: Resolution,
    /// Решение принято по частичному тексту, до конца фразы.
    pub early: bool,
    /// Момент решения относительно конца речи, секунды (меньше 0 — раньше конца).
    pub decided_after_speech: Option<f32>,
    pub speech_end: f32,
}

pub fn run_command(
    stt: &mut dyn SpeechToText,
    resolver: &CommandResolver,
    early_score: f32,
    early_stable: Duration,
    samples: &[f32],
) -> CommandRun {
    stt.reset();
    let base = Instant::now();
    let at = |t: f32| base + Duration::from_secs_f32(t.max(0.0) + 1.0);
    let mut early = EarlyDecider::new(early_score, early_stable);
    let mut decided: Option<(f32, Resolution, bool)> = None;
    let mut text = String::new();

    let speech_end = simulate_stream(samples, |t, chunk| {
        if let Some(tr) = stt.accept(chunk) {
            tracing::trace!(t, utterance = tr.utterance, is_final = tr.is_final, text = %tr.text, "распознано");
            let resolution = resolver.resolve(&tr.text);
            if tr.is_final {
                text.clone_from(&tr.text);
                if early.on_final(tr.utterance)
                    && decided.is_none()
                    && resolution != Resolution::Empty
                {
                    decided = Some((t, resolution, false));
                }
            } else if decided.is_none() {
                let instant = resolver.instant(&tr.text, &resolution);
                if let Some(id) =
                    early.on_partial(tr.utterance, &tr.text, &resolution, instant, at(t))
                {
                    decided = Some((t, Resolution::Command { id, score: 1.0 }, true));
                }
            }
        }
        if decided.is_none()
            && let Some((_, id)) = early.poll(at(t))
        {
            decided = Some((t, Resolution::Command { id, score: 1.0 }, true));
        }
    });

    let (time, resolution, was_early) = match decided {
        Some((t, r, e)) => (Some(t), r, e),
        None => (None, Resolution::Empty, false),
    };
    CommandRun {
        text,
        resolution,
        early: was_early,
        decided_after_speech: time.map(|t| t - speech_end),
        speech_end,
    }
}

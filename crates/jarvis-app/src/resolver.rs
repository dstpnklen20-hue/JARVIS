//! От распознанного текста к команде.
//!
//! - Слово активации в тексте убирается («джарвис открой ютуб» → «открой ютуб»).
//! - Текст сопоставляется с фразами команд (`jarvis_nlu`).
//! - [`EarlyDecider`] выполняет команду, не дожидаясь конца фразы, если
//!   частичный текст уверенно совпал и перестал меняться.

use std::time::{Duration, Instant};

use jarvis_audio::wake::{WakeMatcher, normalize_words};
use jarvis_core::commands::Registry;
use jarvis_core::config::Nlu;
use jarvis_nlu::{CommandMatcher, MatchResult, Thresholds};

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Command {
        id: String,
        score: f32,
    },
    /// Два кандидата почти равны.
    Ambiguous {
        first: String,
        second: String,
    },
    NotUnderstood {
        best: Option<(String, f32)>,
    },
    /// После удаления слова активации ничего не осталось («Джарвис.»).
    Empty,
}

pub struct CommandResolver {
    matcher: CommandMatcher,
    wake: WakeMatcher,
    ids: Vec<String>,
    /// 0 — решение по началу фразы выключено.
    prefix_min_ratio: f32,
}

/// Сходство, при котором частичный текст считается точной фразой команды.
const EXACT_SCORE: f32 = 0.99;

impl CommandResolver {
    pub fn new(registry: &Registry, wake: WakeMatcher, nlu: &Nlu) -> Self {
        let phrases: Vec<Vec<&str>> = registry
            .commands
            .iter()
            .map(|c| c.phrases.iter().map(String::as_str).collect())
            .collect();
        let thresholds = Thresholds {
            min_score: nlu.min_score,
            ambiguity_margin: nlu.ambiguity_margin,
        };
        Self {
            matcher: CommandMatcher::new(&phrases, thresholds),
            wake,
            ids: registry.commands.iter().map(|c| c.id.clone()).collect(),
            prefix_min_ratio: nlu.prefix_min_ratio,
        }
    }

    /// Команда, которую частичный текст определяет без ожидания: точная
    /// фраза или однозначное начало фразы (см. `CommandMatcher::unique_prefix`).
    pub fn instant(&self, text: &str, resolution: &Resolution) -> Option<String> {
        if let Resolution::Command { id, score } = resolution
            && *score >= EXACT_SCORE
        {
            return Some(id.clone());
        }
        if self.prefix_min_ratio <= 0.0 {
            return None;
        }
        let command = self.strip_wake(text);
        self.matcher
            .unique_prefix(&command, self.prefix_min_ratio)
            .and_then(|i| self.ids.get(i).cloned())
    }

    /// Текст без слов активации.
    pub fn strip_wake(&self, text: &str) -> String {
        normalize_words(text)
            .into_iter()
            .filter(|w| !self.wake.matches(w))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn resolve(&self, text: &str) -> Resolution {
        let command = self.strip_wake(text);
        if command.is_empty() {
            return Resolution::Empty;
        }
        let id = |i: usize| self.ids.get(i).cloned().unwrap_or_default();
        match self.matcher.find(&command) {
            MatchResult::Matched(c) => Resolution::Command {
                id: id(c.command),
                score: c.score,
            },
            MatchResult::Ambiguous(a, b) => Resolution::Ambiguous {
                first: id(a.command),
                second: id(b.command),
            },
            MatchResult::NoMatch(best) => Resolution::NotUnderstood {
                best: best.map(|c| (id(c.command), c.score)),
            },
        }
    }
}

/// Фразы для подсказок распознавателю: фразы команд и слова активации.
/// Только кириллица — латиницы в словаре русской модели нет, и sherpa-onnx
/// не смог бы закодировать такую подсказку.
pub fn hotword_phrases(registry: &Registry, wake_words: &[String]) -> Vec<String> {
    let mut phrases: Vec<String> = registry
        .commands
        .iter()
        .flat_map(|c| c.phrases.iter())
        .chain(wake_words)
        .map(|p| normalize_words(p).join(" "))
        .filter(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c == ' ' || ('а'..='я').contains(&c) || c == 'ё')
        })
        .collect();
    phrases.sort();
    phrases.dedup();
    phrases
}

/// Раннее решение по частичному тексту.
#[derive(Debug)]
pub struct EarlyDecider {
    min_score: f32,
    stable: Duration,
    candidate: Option<Candidate>,
    /// Фраза, по которой уже выполнена команда: её итог игнорируем.
    consumed: Option<u64>,
}

#[derive(Debug, Clone)]
struct Candidate {
    utterance: u64,
    id: String,
    text: String,
    since: Instant,
}

impl EarlyDecider {
    /// `stable` = 0 — раннее срабатывание выключено.
    pub fn new(min_score: f32, stable: Duration) -> Self {
        Self {
            min_score,
            stable,
            candidate: None,
            consumed: None,
        }
    }

    /// Новый частичный текст фразы `utterance`. `instant` — команда, которую
    /// текст определяет без ожидания ([`CommandResolver::instant`]): тогда
    /// возвращается сразу, и фраза помечается выполненной.
    pub fn on_partial(
        &mut self,
        utterance: u64,
        text: &str,
        resolution: &Resolution,
        instant: Option<String>,
        now: Instant,
    ) -> Option<String> {
        if self.stable.is_zero() || self.consumed == Some(utterance) {
            return None;
        }
        if let Some(id) = instant {
            self.candidate = None;
            self.consumed = Some(utterance);
            return Some(id);
        }
        self.track(utterance, text, resolution, now);
        None
    }

    fn track(&mut self, utterance: u64, text: &str, resolution: &Resolution, now: Instant) {
        match resolution {
            Resolution::Command { id, score } if *score >= self.min_score => {
                let same = self
                    .candidate
                    .as_ref()
                    .is_some_and(|c| c.utterance == utterance && c.id == *id && c.text == text);
                if !same {
                    self.candidate = Some(Candidate {
                        utterance,
                        id: id.clone(),
                        text: text.to_owned(),
                        since: now,
                    });
                }
            }
            _ => self.candidate = None,
        }
    }

    /// Пора ли выполнить: `(фраза, id команды)`. Фраза помечается выполненной.
    pub fn poll(&mut self, now: Instant) -> Option<(u64, String)> {
        let ready = self
            .candidate
            .as_ref()
            .is_some_and(|c| now.duration_since(c.since) >= self.stable);
        if !ready {
            return None;
        }
        let c = self.candidate.take()?;
        self.consumed = Some(c.utterance);
        Some((c.utterance, c.id))
    }

    /// Итог фразы: `false` — по ней уже выполнена команда, итог игнорировать.
    pub fn on_final(&mut self, utterance: u64) -> bool {
        if self
            .candidate
            .as_ref()
            .is_some_and(|c| c.utterance == utterance)
        {
            self.candidate = None;
        }
        self.consumed != Some(utterance)
    }

    pub fn clear(&mut self) {
        self.candidate = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn registry() -> Registry {
        Registry::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../commands"))
    }

    fn nlu() -> Nlu {
        Nlu {
            min_score: 0.75,
            ambiguity_margin: 0.05,
            early_score: 0.92,
            early_stable_ms: 300,
            prefix_min_ratio: 0.6,
        }
    }

    #[test]
    fn instant_on_exact_phrase_or_unique_prefix() {
        let r = resolver();
        let check = |text: &str| r.instant(text, &r.resolve(text));
        assert_eq!(check("тише").as_deref(), Some("media_volume_down"));
        assert_eq!(check("джарвис открой ю").as_deref(), Some("open_youtube"));
        assert_eq!(check("выключи зв").as_deref(), Some("media_mute"));
        // Неоднозначные начала — ждём.
        assert_eq!(check("сделай"), None);
        assert_eq!(check("открой"), None);
        assert_eq!(check("включи"), None);
    }

    #[test]
    fn instant_fires_once_per_utterance() {
        let t0 = Instant::now();
        let mut d = EarlyDecider::new(0.92, Duration::from_millis(300));
        let first = d.on_partial(1, "тише", &cmd("x", 1.0), Some("x".into()), t0);
        assert_eq!(first.as_deref(), Some("x"));
        assert_eq!(
            d.on_partial(1, "тише", &cmd("x", 1.0), Some("x".into()), t0),
            None
        );
        assert!(!d.on_final(1));
    }

    fn resolver() -> CommandResolver {
        CommandResolver::new(
            &registry(),
            WakeMatcher::new(["джарвис", "жарвис"], 1),
            &nlu(),
        )
    }

    fn id(text: &str) -> String {
        match resolver().resolve(text) {
            Resolution::Command { id, .. } => id,
            other => panic!("«{text}»: {other:?}"),
        }
    }

    #[test]
    fn wake_word_is_stripped() {
        assert_eq!(resolver().strip_wake("Джарвис, открой ютуб"), "открой ютуб");
        assert_eq!(
            resolver().strip_wake("слушай жарвиз включи музыку"),
            "слушай включи музыку"
        );
        assert_eq!(resolver().resolve("джарвис"), Resolution::Empty);
    }

    #[test]
    fn shipped_commands_resolve() {
        assert_eq!(id("джарвис открой ютуб"), "open_youtube");
        assert_eq!(id("но открой пожалуйста ютуб"), "open_youtube");
        assert_eq!(id("включи музыку"), "media_play_pause");
        assert_eq!(id("следующий трек"), "media_next");
        assert_eq!(id("сделай громче"), "media_volume_up");
        assert_eq!(id("тише"), "media_volume_down");
        assert_eq!(id("выключи звук"), "media_mute");
    }

    #[test]
    fn unknown_phrase_is_not_understood() {
        assert!(matches!(
            resolver().resolve("купи хлеба и молока"),
            Resolution::NotUnderstood { .. }
        ));
    }

    #[test]
    fn hotwords_keep_only_cyrillic() {
        let words = hotword_phrases(&registry(), &["джарвис".to_owned()]);
        assert!(words.contains(&"открой ютуб".to_owned()));
        assert!(words.contains(&"джарвис".to_owned()));
        assert!(!words.iter().any(|w| w.contains("youtube")));
    }

    fn cmd(id: &str, score: f32) -> Resolution {
        Resolution::Command {
            id: id.into(),
            score,
        }
    }

    #[test]
    fn early_decision_fires_after_stable_period() {
        let t0 = Instant::now();
        let mut d = EarlyDecider::new(0.92, Duration::from_millis(300));
        d.on_partial(1, "открой ютуб", &cmd("open_youtube", 1.0), None, t0);
        assert_eq!(d.poll(t0 + Duration::from_millis(200)), None);
        assert_eq!(
            d.poll(t0 + Duration::from_millis(300)),
            Some((1, "open_youtube".into()))
        );
        // Итог той же фразы — игнорировать.
        assert!(!d.on_final(1));
        // Следующая фраза — снова можно.
        assert!(d.on_final(2));
    }

    #[test]
    fn changing_text_restarts_the_clock() {
        let t0 = Instant::now();
        let mut d = EarlyDecider::new(0.92, Duration::from_millis(300));
        d.on_partial(1, "открой ютуб", &cmd("open_youtube", 0.95), None, t0);
        let t1 = t0 + Duration::from_millis(250);
        d.on_partial(1, "открой ютуб и", &cmd("open_youtube", 0.93), None, t1);
        assert_eq!(d.poll(t0 + Duration::from_millis(400)), None);
        assert!(d.poll(t1 + Duration::from_millis(300)).is_some());
    }

    #[test]
    fn low_score_or_other_result_clears_candidate() {
        let t0 = Instant::now();
        let mut d = EarlyDecider::new(0.92, Duration::from_millis(300));
        d.on_partial(1, "открой ютуб", &cmd("open_youtube", 1.0), None, t0);
        d.on_partial(
            1,
            "открой ютуб и включи",
            &cmd("open_youtube", 0.6),
            None,
            t0,
        );
        assert_eq!(d.poll(t0 + Duration::from_secs(1)), None);
        d.on_partial(
            2,
            "абв",
            &Resolution::NotUnderstood { best: None },
            None,
            t0,
        );
        assert_eq!(d.poll(t0 + Duration::from_secs(1)), None);
    }

    #[test]
    fn disabled_when_stable_is_zero() {
        let t0 = Instant::now();
        let mut d = EarlyDecider::new(0.92, Duration::ZERO);
        d.on_partial(1, "открой ютуб", &cmd("open_youtube", 1.0), None, t0);
        assert_eq!(d.poll(t0 + Duration::from_secs(5)), None);
        assert!(d.on_final(1));
    }
}

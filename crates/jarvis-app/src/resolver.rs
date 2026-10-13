//! От распознанного текста к команде.
//!
//! - Слово активации в тексте убирается («джарвис открой ютуб» → «открой ютуб»).
//! - Текст сопоставляется с фразами команд (`jarvis_nlu`), значения слотов
//!   проверяются по описанию в манифесте.
//! - [`EarlyDecider`] выполняет команду, не дожидаясь конца фразы, если
//!   частичный текст уверенно совпал и перестал меняться.

use std::time::{Duration, Instant};

use jarvis_audio::wake::{WakeMatcher, normalize_words};
use jarvis_core::commands::Registry;
use jarvis_core::config::Nlu;
use jarvis_core::slots::{SlotSpec, SlotValues};
use jarvis_nlu::{CommandMatcher, CommandPhrases, MatchResult, SlotKind, Thresholds};

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Command {
        id: String,
        score: f32,
        /// Значения слотов, проверенные по манифесту.
        slots: SlotValues,
    },
    /// Два кандидата почти равны.
    Ambiguous {
        first: String,
        second: String,
    },
    NotUnderstood {
        best: Option<(String, f32)>,
    },
    /// Команда узнана, но число в ней вне допустимого диапазона
    /// («громкость на 500» у слота без `clamp`).
    OutOfRange {
        id: String,
    },
    /// После удаления слова активации ничего не осталось («Джарвис.»).
    Empty,
}

impl Resolution {
    /// Команда без слотов и с этим id и уверенностью.
    pub fn command(id: impl Into<String>, score: f32) -> Self {
        Resolution::Command {
            id: id.into(),
            score,
            slots: SlotValues::new(),
        }
    }
}

pub struct CommandResolver {
    matcher: CommandMatcher,
    wake: WakeMatcher,
    ids: Vec<String>,
    /// Описания слотов команд (по тем же индексам, что `ids`).
    slot_specs: Vec<std::collections::BTreeMap<String, SlotSpec>>,
    /// 0 — решение по началу фразы выключено.
    prefix_min_ratio: f32,
}

/// Сходство, при котором частичный текст считается точной фразой команды.
pub const EXACT_SCORE: f32 = 0.99;

/// Встроенные команды Джарвиса. Двоеточие в id запрещено в манифестах —
/// пакет команд не может подменить встроенную команду.
pub const BUILTIN_STOP: &str = "builtin:stop";
pub const BUILTIN_FORGET_PERMISSIONS: &str = "builtin:forget_permissions";
pub const BUILTIN_RELOAD_COMMANDS: &str = "builtin:reload_commands";
pub const BUILTIN_DIALOG_ON: &str = "builtin:dialog_on";
pub const BUILTIN_DIALOG_OFF: &str = "builtin:dialog_off";

const BUILTINS: &[(&str, &[&str])] = &[
    (
        BUILTIN_STOP,
        &["стоп", "стой", "хватит", "замолчи", "отбой", "остановись"],
    ),
    (
        BUILTIN_FORGET_PERMISSIONS,
        &[
            "забудь разрешения",
            "сбрось разрешения",
            "забудь все разрешения",
        ],
    ),
    (
        BUILTIN_RELOAD_COMMANDS,
        &[
            "обнови команды",
            "перезагрузи команды",
            "обнови список команд",
        ],
    ),
    (
        BUILTIN_DIALOG_ON,
        &[
            "режим диалога",
            "включи режим диалога",
            "перейди в режим диалога",
            "давай поговорим",
        ],
    ),
    (
        BUILTIN_DIALOG_OFF,
        &[
            "выйди из режима диалога",
            "закончим диалог",
            "конец диалога",
        ],
    ),
];

pub fn is_builtin(id: &str) -> bool {
    id.starts_with("builtin:")
}

/// Как назвать команду вслух: первая фраза без мест для слотов
/// («громкость {level}» → «громкость»).
pub fn spoken_phrase(registry: &Registry, id: &str) -> String {
    let phrase = registry
        .get(id)
        .and_then(|c| c.phrases.first().map(String::as_str))
        .or_else(|| {
            BUILTINS
                .iter()
                .find(|(b, _)| *b == id)
                .and_then(|(_, phrases)| phrases.first().copied())
        })
        .unwrap_or(id);
    strip_placeholders(phrase)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn slot_kind(spec: &SlotSpec) -> SlotKind {
    match spec {
        // Сопоставление ловит любое число: вне диапазона — не «не понял»,
        // а прижать к границе или отказать (решает `accept_slots`).
        SlotSpec::Number { .. } => SlotKind::Number {
            min: i64::MIN,
            max: i64::MAX,
        },
        SlotSpec::Choice { values } => {
            SlotKind::Choice(values.spoken().into_iter().map(str::to_owned).collect())
        }
    }
}

impl CommandResolver {
    pub fn new(registry: &Registry, wake: WakeMatcher, nlu: &Nlu) -> Self {
        let mut entries: Vec<CommandPhrases> = BUILTINS
            .iter()
            .map(|(_, p)| CommandPhrases {
                phrases: p.iter().map(|s| (*s).to_owned()).collect(),
                slots: Vec::new(),
            })
            .collect();
        let mut ids: Vec<String> = BUILTINS.iter().map(|(id, _)| (*id).to_owned()).collect();
        let mut slot_specs = vec![Default::default(); BUILTINS.len()];
        for command in &registry.commands {
            entries.push(CommandPhrases {
                phrases: command.phrases.clone(),
                slots: command
                    .slots
                    .iter()
                    .map(|(name, spec)| (name.clone(), slot_kind(spec)))
                    .collect(),
            });
            ids.push(command.id.clone());
            slot_specs.push(command.slots.clone());
        }
        let thresholds = Thresholds {
            min_score: nlu.min_score,
            ambiguity_margin: nlu.ambiguity_margin,
        };
        Self {
            matcher: CommandMatcher::with_slots(&entries, thresholds),
            wake,
            ids,
            slot_specs,
            prefix_min_ratio: nlu.prefix_min_ratio,
        }
    }

    /// Команда, которую частичный текст определяет без ожидания: точная
    /// фраза или однозначное начало фразы (см. `CommandMatcher::unique_prefix`).
    /// Команды со слотами — никогда: «сорок» ещё может стать «сорок пять».
    pub fn instant(&self, text: &str, resolution: &Resolution) -> Option<String> {
        if let Resolution::Command { id, score, slots } = resolution
            && *score >= EXACT_SCORE
        {
            return slots.is_empty().then(|| id.clone());
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
            MatchResult::Matched(c) => match self.accept_slots(c.command, &c.slots) {
                Some(slots) => Resolution::Command {
                    id: id(c.command),
                    score: c.score,
                    slots,
                },
                None if self.out_of_range(c.command, &c.slots) => {
                    Resolution::OutOfRange { id: id(c.command) }
                }
                None => Resolution::NotUnderstood {
                    best: Some((id(c.command), c.score)),
                },
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

    /// Проверка значений слотов по манифесту и замена на подстановку.
    fn accept_slots(&self, command: usize, raw: &[(String, String)]) -> Option<SlotValues> {
        let specs = self.slot_specs.get(command)?;
        raw.iter()
            .map(|(name, value)| {
                let accepted = specs.get(name)?.accept(value)?;
                Some((name.clone(), accepted))
            })
            .collect()
    }

    fn out_of_range(&self, command: usize, raw: &[(String, String)]) -> bool {
        let Some(specs) = self.slot_specs.get(command) else {
            return false;
        };
        raw.iter()
            .any(|(name, value)| specs.get(name).is_some_and(|s| s.out_of_range(value)))
    }
}

/// Фразы для подсказок распознавателю: фразы команд и слова активации.
/// Только кириллица — латиницы в словаре русской модели нет, и sherpa-onnx
/// не смог бы закодировать такую подсказку.
pub fn hotword_phrases(registry: &Registry, wake_words: &[String]) -> Vec<String> {
    let builtin = BUILTINS
        .iter()
        .flat_map(|(_, p)| p.iter().map(|s| (*s).to_owned()));
    // Фразы со слотами — без мест слотов; значения выбора — отдельными подсказками.
    let choices = registry.commands.iter().flat_map(|c| {
        c.slots.values().flat_map(|spec| match spec {
            SlotSpec::Choice { values } => values.spoken().into_iter().map(str::to_owned).collect(),
            SlotSpec::Number { .. } => Vec::new(),
        })
    });
    let mut phrases: Vec<String> = registry
        .commands
        .iter()
        .flat_map(|c| c.phrases.iter().map(|p| strip_placeholders(p)))
        .chain(choices)
        .chain(wake_words.iter().cloned())
        .chain(builtin)
        .map(|p| normalize_words(&p).join(" "))
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

/// «громкость на {level}» → «громкость на ».
fn strip_placeholders(phrase: &str) -> String {
    let mut out = String::with_capacity(phrase.len());
    let mut depth = 0;
    for c in phrase.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
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
            Resolution::Command { id, score, slots }
                if *score >= self.min_score && slots.is_empty() =>
            {
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
    fn spoken_phrase_has_no_placeholders() {
        let r = registry();
        assert_eq!(spoken_phrase(&r, "media_volume_set"), "громкость на");
        assert_eq!(spoken_phrase(&r, BUILTIN_STOP), "стоп");
        assert_eq!(spoken_phrase(&r, "unknown_id"), "unknown_id");
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
        assert_eq!(id("открой браузер"), "open_browser");
        assert_eq!(id("открой календарь"), "open_calendar");
        assert_eq!(id("открой диспетчер задач"), "app_task_manager");
        assert_eq!(id("запусти клод"), "app_claude");
        assert_eq!(id("запусти хап"), "app_happ");
        assert_eq!(id("открой проводник"), "app_explorer");
    }

    #[test]
    fn builtin_commands_resolve() {
        assert_eq!(id("джарвис стоп"), BUILTIN_STOP);
        assert_eq!(id("хватит"), BUILTIN_STOP);
        assert_eq!(id("забудь разрешения"), BUILTIN_FORGET_PERMISSIONS);
        assert_eq!(id("джарвис режим диалога"), BUILTIN_DIALOG_ON);
        assert_eq!(id("включи режим диалога"), BUILTIN_DIALOG_ON);
        assert_eq!(id("выйди из режима диалога"), BUILTIN_DIALOG_OFF);
        assert_eq!(id("закончим диалог"), BUILTIN_DIALOG_OFF);
        assert!(is_builtin(BUILTIN_STOP));
        assert!(!is_builtin("open_youtube"));
    }

    #[test]
    fn volume_above_maximum_is_clamped() {
        for text in [
            "поставь громкость на сто двадцать",
            "сделай громкость двести",
        ] {
            match resolver().resolve(text) {
                Resolution::Command { id, slots, .. } => {
                    assert_eq!(id, "media_volume_set", "{text}");
                    assert_eq!(slots.get("level").map(String::as_str), Some("100"));
                }
                other => panic!("«{text}»: {other:?}"),
            }
        }
    }

    #[test]
    fn number_out_of_range_without_clamp_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("t");
        std::fs::create_dir(&pack).unwrap();
        std::fs::write(
            pack.join("command.toml"),
            "pack = \"t\"
version = 1
[[command]]
id = \"brightness\"
             phrases = [\"яркость на {level}\"]
description = \"яркость\"
             [command.slots.level]
type = \"number\"
min = 10
max = 90
             [command.action]
type = \"media\"
operation = \"volume_set\"
level = \"{level}\"
",
        )
        .unwrap();
        let registry = Registry::load(dir.path());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let r = CommandResolver::new(&registry, WakeMatcher::new(["джарвис"], 1), &nlu());
        assert_eq!(
            r.resolve("яркость на сто двадцать"),
            Resolution::OutOfRange {
                id: "brightness".into()
            }
        );
        assert!(matches!(
            r.resolve("яркость на пятьдесят"),
            Resolution::Command { .. }
        ));
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
        Resolution::command(id, score)
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

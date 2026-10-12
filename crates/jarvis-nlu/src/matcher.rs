//! Нечёткое сопоставление текста с фразами команд.
//!
//! Сходство двух нормализованных строк — нормированное расстояние
//! Левенштейна (0…1), считаемое и для исходного порядка слов, и для слов,
//! отсортированных по алфавиту («ютуб открой» = «открой ютуб»); берётся
//! лучшее. Ниже порога — «не понял». Два разных кандидата почти равны —
//! «вы имели в виду X или Y?».
//!
//! Слоты («громкость на {level}»): место слота во фразе — метка; в речи
//! перебираются подходящие значения (числа в диапазоне, слова из списка),
//! они заменяются меткой, и дальше сравнение обычное.

use crate::normalize::normalize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Ниже — «не понял».
    pub min_score: f32,
    /// Если лучший кандидат обгоняет второго (другую команду) меньше чем
    /// на столько — переспросить.
    pub ambiguity_margin: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_score: 0.75,
            ambiguity_margin: 0.05,
        }
    }
}

/// Какие значения слот принимает (для поиска в речи).
#[derive(Debug, Clone, PartialEq)]
pub enum SlotKind {
    Number {
        min: i64,
        max: i64,
    },
    /// Как значения произносятся.
    Choice(Vec<String>),
}

/// Фразы команды и её слоты.
#[derive(Debug, Clone)]
pub struct CommandPhrases {
    pub phrases: Vec<String>,
    pub slots: Vec<(String, SlotKind)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Индекс команды в том порядке, в каком их передали при создании.
    pub command: usize,
    /// Какая фраза команды подошла лучше всего (нормализованная).
    pub phrase: String,
    pub score: f32,
    /// Найденные значения слотов: имя → как сказано (число цифрами или
    /// значение выбора в том виде, как оно записано в манифесте).
    pub slots: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchResult {
    Matched(Candidate),
    /// Два разных кандидата почти равны.
    Ambiguous(Candidate, Candidate),
    /// Ничего не подошло; лучший кандидат — для журнала.
    NoMatch(Option<Candidate>),
}

impl MatchResult {
    pub fn best(&self) -> Option<&Candidate> {
        match self {
            MatchResult::Matched(c) | MatchResult::Ambiguous(c, _) => Some(c),
            MatchResult::NoMatch(c) => c.as_ref(),
        }
    }
}

#[derive(Debug, Clone)]
struct Phrase {
    command: usize,
    text: String,
    sorted: String,
    /// Слоты фразы по порядку меток (`zslotz0`, `zslotz1`…).
    slots: Vec<(String, SlotKind)>,
    /// Первое слово фразы — обычно действие («открой», «выключи»); см. [`LEAD_MIN_SIMILARITY`].
    lead: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommandMatcher {
    phrases: Vec<Phrase>,
    thresholds: Thresholds,
}

/// Метка места слота: латиница и цифры переживают нормализацию.
fn slot_mark(index: usize) -> String {
    format!("zslotz{index}")
}

impl CommandMatcher {
    /// `commands[i]` — фразы i-й команды без слотов (нормализуются здесь).
    pub fn new<P: AsRef<str>>(commands: &[Vec<P>], thresholds: Thresholds) -> Self {
        let entries: Vec<CommandPhrases> = commands
            .iter()
            .map(|phrases| CommandPhrases {
                phrases: phrases.iter().map(|p| p.as_ref().to_owned()).collect(),
                slots: Vec::new(),
            })
            .collect();
        Self::with_slots(&entries, thresholds)
    }

    /// Команды с фразами и слотами.
    pub fn with_slots(commands: &[CommandPhrases], thresholds: Thresholds) -> Self {
        let mut phrases = Vec::new();
        for (command, entry) in commands.iter().enumerate() {
            for raw in &entry.phrases {
                // «{level}» → метка; слоты фразы — по порядку появления.
                let mut template = raw.clone();
                let mut slots = Vec::new();
                for (name, kind) in &entry.slots {
                    let placeholder = format!("{{{name}}}");
                    if template.contains(&placeholder) {
                        template = template
                            .replace(&placeholder, &format!(" {} ", slot_mark(slots.len())));
                        slots.push((name.clone(), kind.clone()));
                    }
                }
                let text = normalize(&template);
                if !text.is_empty() {
                    phrases.push(Phrase {
                        command,
                        sorted: sorted_words(&text),
                        lead: lead_word(&text),
                        text,
                        slots,
                    });
                }
            }
        }
        Self {
            phrases,
            thresholds,
        }
    }

    pub fn thresholds(&self) -> Thresholds {
        self.thresholds
    }

    /// Сопоставляет распознанный текст (ненормализованный).
    pub fn find(&self, text: &str) -> MatchResult {
        let query = normalize(text);
        if query.is_empty() {
            return MatchResult::NoMatch(None);
        }
        let query_words: Vec<String> = query.split(' ').map(str::to_owned).collect();
        let query_sorted = sorted_words(&query);

        // Лучшая фраза для каждой команды.
        let mut best_per_command: Vec<Candidate> = Vec::new();
        for phrase in &self.phrases {
            let (score, slots) = if phrase.slots.is_empty() {
                let score =
                    similarity(&query, &phrase.text).max(similarity(&query_sorted, &phrase.sorted));
                (score, Vec::new())
            } else {
                match best_slot_fill(&query_words, phrase) {
                    Some(found) => found,
                    None => continue, // в речи нет подходящего значения слота
                }
            };
            // Действие в речи должно совпасть со словом фразы почти точно:
            // «закрой ютуб» не должно стать «открой ютуб» (сходство фраз 0.82).
            let score = if lead_heard(phrase, &query_words) {
                score
            } else {
                score * LEAD_MISMATCH_PENALTY
            };
            match best_per_command
                .iter_mut()
                .find(|c| c.command == phrase.command)
            {
                Some(c) if c.score >= score => {}
                Some(c) => {
                    c.score = score;
                    c.phrase = phrase.text.clone();
                    c.slots = slots;
                }
                None => best_per_command.push(Candidate {
                    command: phrase.command,
                    phrase: phrase.text.clone(),
                    score,
                    slots,
                }),
            }
        }
        best_per_command.sort_by(|a, b| b.score.total_cmp(&a.score));

        let mut ranked = best_per_command.into_iter();
        let Some(first) = ranked.next() else {
            return MatchResult::NoMatch(None);
        };
        if first.score < self.thresholds.min_score {
            return MatchResult::NoMatch(Some(first));
        }
        match ranked.next() {
            Some(second)
                if second.score >= self.thresholds.min_score
                    && first.score - second.score < self.thresholds.ambiguity_margin =>
            {
                MatchResult::Ambiguous(first, second)
            }
            _ => MatchResult::Matched(first),
        }
    }

    /// Команда, которую частичный текст определяет однозначно: текст —
    /// начало фраз только одной команды и покрывает не меньше `min_ratio`
    /// длины подходящей фразы («открой ю» → «открой ютуб»). Модель выдаёт
    /// последнее слово с задержкой до 0.6 с — так решение принимается раньше.
    /// Фразы со слотами не участвуют: «сорок» может стать «сорок пять».
    pub fn unique_prefix(&self, text: &str, min_ratio: f32) -> Option<usize> {
        let query = normalize(text);
        let query_len = query.chars().count();
        if query_len < MIN_PREFIX_CHARS {
            return None;
        }
        let mut found: Option<usize> = None;
        let mut covered_enough = false;
        for phrase in &self.phrases {
            let starts = if phrase.slots.is_empty() {
                phrase.text.starts_with(&query)
            } else {
                // Начало фразы до первого слота тоже может совпасть — тогда
                // решение по началу запрещено для всех.
                let fixed = phrase
                    .text
                    .split(&slot_mark(0))
                    .next()
                    .unwrap_or_default()
                    .trim_end();
                fixed.starts_with(&query) || query.starts_with(fixed)
            };
            if !starts {
                continue;
            }
            if !phrase.slots.is_empty() {
                return None;
            }
            match found {
                Some(c) if c != phrase.command => return None, // продолжений у разных команд
                _ => found = Some(phrase.command),
            }
            let ratio = query_len as f32 / phrase.text.chars().count() as f32;
            covered_enough |= ratio >= min_ratio;
        }
        found.filter(|_| covered_enough)
    }

    /// Нормализованные фразы разных команд, которые совпадают — для
    /// предупреждений при загрузке манифестов: `(команда a, команда b, фраза)`.
    pub fn conflicts(&self) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for (i, a) in self.phrases.iter().enumerate() {
            for b in &self.phrases[i + 1..] {
                if a.command != b.command && a.text == b.text {
                    out.push((a.command, b.command, a.text.clone()));
                }
            }
        }
        out
    }
}

/// Лучшее заполнение слотов фразы значениями из речи: (сходство, значения).
fn best_slot_fill(query: &[String], phrase: &Phrase) -> Option<(f32, Vec<(String, String)>)> {
    let mut best: Option<(f32, Vec<(String, String)>)> = None;
    fill_slot(query.to_vec(), phrase, 0, Vec::new(), &mut best);
    best
}

/// Перебор: слот `index` занимает подходящее окно слов, которое заменяется меткой.
fn fill_slot(
    words: Vec<String>,
    phrase: &Phrase,
    index: usize,
    values: Vec<(String, String)>,
    best: &mut Option<(f32, Vec<(String, String)>)>,
) {
    if index == phrase.slots.len() {
        let text = words.join(" ");
        let score =
            similarity(&text, &phrase.text).max(similarity(&sorted_words(&text), &phrase.sorted));
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            *best = Some((score, values));
        }
        return;
    }
    let (name, kind) = &phrase.slots[index];
    let mark = slot_mark(index);
    for (start, len, value) in slot_windows(&words, kind) {
        let mut next = words[..start].to_vec();
        next.push(mark.clone());
        next.extend_from_slice(&words[start + len..]);
        let mut next_values = values.clone();
        next_values.push((name.clone(), value));
        fill_slot(next, phrase, index + 1, next_values, best);
    }
}

/// Окна слов, которые могут быть значением слота: (начало, длина, значение).
fn slot_windows(words: &[String], kind: &SlotKind) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    match kind {
        SlotKind::Number { min, max } => {
            for (i, w) in words.iter().enumerate() {
                if let Ok(n) = w.parse::<i64>()
                    && (*min..=*max).contains(&n)
                {
                    out.push((i, 1, n.to_string()));
                }
            }
        }
        SlotKind::Choice(values) => {
            for value in values {
                let target = normalize(value);
                let len = target.split(' ').count();
                for start in 0..words.len().saturating_sub(len - 1) {
                    let window = words[start..start + len].join(" ");
                    if similarity(&window, &target) >= CHOICE_MIN_SIMILARITY {
                        out.push((start, len, value.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Насколько слово в речи должно быть похоже на первое слово фразы (действие).
/// «открою» → «открой» (0.83) проходит, «закрой» → «открой» (0.67) — нет.
const LEAD_MIN_SIMILARITY: f32 = 0.7;
/// Во сколько раз снижается сходство, если действия в речи нет.
const LEAD_MISMATCH_PENALTY: f32 = 0.8;

/// Первое слово фразы, если это не место для слота и не слишком короткое
/// (в коротких словах одна ошибка распознавания — уже большая доля).
fn lead_word(text: &str) -> Option<String> {
    let first = text.split(' ').next()?;
    (first.chars().count() >= 4 && !first.starts_with("zslotz")).then(|| first.to_owned())
}

/// Есть ли в речи действие фразы (на любом месте: порядок слов свободный).
fn lead_heard(phrase: &Phrase, query: &[String]) -> bool {
    phrase.lead.as_ref().is_none_or(|lead| {
        query
            .iter()
            .any(|w| similarity(w, lead) >= LEAD_MIN_SIMILARITY)
    })
}

/// Насколько слово в речи должно быть похоже на значение выбора.
const CHOICE_MIN_SIMILARITY: f32 = 0.75;

/// Короче — слишком мало информации для решения по началу фразы.
const MIN_PREFIX_CHARS: usize = 5;

fn sorted_words(text: &str) -> String {
    let mut words: Vec<&str> = text.split(' ').collect();
    words.sort_unstable();
    words.join(" ")
}

fn similarity(a: &str, b: &str) -> f32 {
    strsim::normalized_levenshtein(a, b) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher() -> CommandMatcher {
        let commands = vec![
            vec!["открой ютуб", "запусти ютуб", "включи ютуб"],
            vec!["открой гугл"],
            vec!["включи музыку", "поставь на паузу", "пауза", "продолжи"],
            vec!["следующий трек", "дальше"],
            vec!["выключи звук"],
            vec!["громче", "сделай громче"],
            vec!["тише", "сделай тише"],
        ];
        CommandMatcher::new(&commands, Thresholds::default())
    }

    fn matched(text: &str) -> usize {
        match matcher().find(text) {
            MatchResult::Matched(c) => c.command,
            other => panic!("«{text}»: ожидали совпадение, получили {other:?}"),
        }
    }

    #[test]
    fn exact_and_normalized_phrases() {
        assert_eq!(matched("открой ютуб"), 0);
        assert_eq!(matched("Ну открой, пожалуйста, ютуб!"), 0);
        assert_eq!(matched("Открой Гугл"), 1);
        assert_eq!(matched("тише"), 6);
    }

    #[test]
    fn word_order_does_not_matter() {
        assert_eq!(matched("ютуб открой"), 0);
        assert_eq!(matched("трек следующий"), 3);
    }

    #[test]
    fn small_misrecognitions_still_match() {
        assert_eq!(matched("открой ютю"), 0);
        assert_eq!(matched("следующей трек"), 3);
        assert_eq!(matched("включи музыка"), 2);
        assert_eq!(matched("сделай громчи"), 5);
    }

    #[test]
    fn similar_commands_are_told_apart() {
        assert_eq!(matched("включи музыку"), 2);
        assert_eq!(matched("выключи звук"), 4);
        assert_eq!(matched("открой гугл"), 1);
    }

    #[test]
    fn opposite_action_does_not_match() {
        // Одна буква меняет смысл: «закрой» — не «открой».
        for text in ["закрой ютуб", "закрой гугл"] {
            assert!(
                matches!(matcher().find(text), MatchResult::NoMatch(_)),
                "«{text}»: {:?}",
                matcher().find(text)
            );
        }
        // Ошибка распознавания в окончании действия — не помеха.
        assert_eq!(matched("открою ютуб"), 0);
    }

    #[test]
    fn unrelated_speech_is_rejected() {
        for text in [
            "купи хлеба и молока",
            "какая завтра погода",
            "сегодня хорошая погода",
            "",
            "ну",
        ] {
            assert!(
                matches!(matcher().find(text), MatchResult::NoMatch(_)),
                "«{text}» не должно совпасть: {:?}",
                matcher().find(text)
            );
        }
    }

    #[test]
    fn near_tie_between_commands_is_ambiguous() {
        let commands = vec![vec!["открой почту"], vec!["открой почты"]];
        let m = CommandMatcher::new(&commands, Thresholds::default());
        assert!(matches!(m.find("открой почте"), MatchResult::Ambiguous(..)));
    }

    #[test]
    fn same_command_phrases_never_ambiguous() {
        // «включи ютуб» и «открой ютуб» — одна команда: не переспрашиваем.
        assert_eq!(matched("включи ютуб"), 0);
    }

    #[test]
    fn best_candidate_reported_on_no_match() {
        let result = matcher().find("открой яндекс");
        let MatchResult::NoMatch(Some(best)) = result else {
            panic!("ожидали NoMatch с кандидатом");
        };
        assert!(best.score < 0.75);
    }

    #[test]
    fn unique_prefix_decides_early() {
        let m = matcher();
        assert_eq!(m.unique_prefix("открой ю", 0.6), Some(0));
        assert_eq!(m.unique_prefix("открой г", 0.6), Some(1));
        assert_eq!(m.unique_prefix("выключи зв", 0.6), Some(4));
        assert_eq!(m.unique_prefix("следующий т", 0.6), Some(3));
    }

    #[test]
    fn prefix_shared_by_commands_waits() {
        let m = matcher();
        // «сделай громче» и «сделай тише» — разные команды.
        assert_eq!(m.unique_prefix("сделай", 0.3), None);
        // «открой ютуб» и «открой гугл».
        assert_eq!(m.unique_prefix("открой", 0.3), None);
    }

    #[test]
    fn prefix_must_cover_enough_and_be_long_enough() {
        let m = matcher();
        // «следую» — 6 из 14 букв.
        assert_eq!(m.unique_prefix("следую", 0.6), None);
        assert_eq!(m.unique_prefix("след", 0.1), None, "короче 5 букв");
        assert_eq!(m.unique_prefix("купи хлеба", 0.1), None);
    }

    #[test]
    fn conflicts_are_found_after_normalization() {
        let commands = vec![
            vec!["Открой ютуб"],
            vec!["открой, пожалуйста, ютуб"],
            vec!["тише"],
        ];
        let m = CommandMatcher::new(&commands, Thresholds::default());
        assert_eq!(m.conflicts(), vec![(0, 1, "открой ютуб".to_owned())]);
    }

    fn slot_matcher() -> CommandMatcher {
        let commands = vec![
            CommandPhrases {
                phrases: vec![
                    "громкость на {level}".into(),
                    "поставь громкость {level}".into(),
                ],
                slots: vec![("level".into(), SlotKind::Number { min: 0, max: 100 })],
            },
            CommandPhrases {
                phrases: vec!["открой сайт {site}".into()],
                slots: vec![(
                    "site".into(),
                    SlotKind::Choice(vec!["ютуб".into(), "гугл".into(), "яндекс маркет".into()]),
                )],
            },
            CommandPhrases {
                phrases: vec!["громче".into()],
                slots: vec![],
            },
        ];
        CommandMatcher::with_slots(&commands, Thresholds::default())
    }

    fn slot_match(text: &str) -> (usize, Vec<(String, String)>) {
        match slot_matcher().find(text) {
            MatchResult::Matched(c) => (c.command, c.slots),
            other => panic!("«{text}»: {other:?}"),
        }
    }

    #[test]
    fn number_slot_from_digits_or_words() {
        assert_eq!(
            slot_match("громкость на 40"),
            (0, vec![("level".into(), "40".into())])
        );
        assert_eq!(
            slot_match("Громкость на сорок пять"),
            (0, vec![("level".into(), "45".into())])
        );
        assert_eq!(
            slot_match("поставь громкость ноль"),
            (0, vec![("level".into(), "0".into())])
        );
    }

    #[test]
    fn number_slot_out_of_range_does_not_match() {
        assert!(matches!(
            slot_matcher().find("громкость на 150"),
            MatchResult::NoMatch(_)
        ));
        assert!(matches!(
            slot_matcher().find("громкость на"),
            MatchResult::NoMatch(_)
        ));
    }

    #[test]
    fn choice_slot_with_small_misrecognition_and_multiword_values() {
        assert_eq!(
            slot_match("открой сайт ютуб"),
            (1, vec![("site".into(), "ютуб".into())])
        );
        assert_eq!(
            slot_match("открой сайт ютубе"),
            (1, vec![("site".into(), "ютуб".into())])
        );
        assert_eq!(
            slot_match("открой сайт яндекс маркет"),
            (1, vec![("site".into(), "яндекс маркет".into())])
        );
        assert!(matches!(
            slot_matcher().find("открой сайт вконтакте"),
            MatchResult::NoMatch(_)
        ));
    }

    #[test]
    fn plain_commands_unaffected_by_slots() {
        assert_eq!(slot_match("громче"), (2, vec![]));
    }

    #[test]
    fn slot_phrases_never_decide_by_prefix() {
        let m = slot_matcher();
        assert_eq!(m.unique_prefix("громкость на", 0.3), None);
        assert_eq!(m.unique_prefix("громкость на 40", 0.3), None);
    }
}

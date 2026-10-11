//! Нечёткое сопоставление текста с фразами команд.
//!
//! Сходство двух нормализованных строк — нормированное расстояние
//! Левенштейна (0…1), считаемое и для исходного порядка слов, и для слов,
//! отсортированных по алфавиту («ютуб открой» = «открой ютуб»); берётся
//! лучшее. Ниже порога — «не понял». Два разных кандидата почти равны —
//! «вы имели в виду X или Y?».

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

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Индекс команды в том порядке, в каком их передали в [`CommandMatcher::new`].
    pub command: usize,
    /// Какая фраза команды подошла лучше всего (нормализованная).
    pub phrase: String,
    pub score: f32,
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
}

#[derive(Debug, Clone)]
pub struct CommandMatcher {
    phrases: Vec<Phrase>,
    thresholds: Thresholds,
}

impl CommandMatcher {
    /// `commands[i]` — фразы i-й команды (как в манифесте; нормализуются здесь).
    pub fn new<P: AsRef<str>>(commands: &[Vec<P>], thresholds: Thresholds) -> Self {
        let phrases = commands
            .iter()
            .enumerate()
            .flat_map(|(command, phrases)| {
                phrases.iter().filter_map(move |p| {
                    let text = normalize(p.as_ref());
                    (!text.is_empty()).then(|| Phrase {
                        command,
                        sorted: sorted_words(&text),
                        text,
                    })
                })
            })
            .collect();
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
        let query_sorted = sorted_words(&query);

        // Лучшая фраза для каждой команды.
        let mut best_per_command: Vec<Candidate> = Vec::new();
        for phrase in &self.phrases {
            let score =
                similarity(&query, &phrase.text).max(similarity(&query_sorted, &phrase.sorted));
            match best_per_command
                .iter_mut()
                .find(|c| c.command == phrase.command)
            {
                Some(c) if c.score >= score => {}
                Some(c) => {
                    c.score = score;
                    c.phrase = phrase.text.clone();
                }
                None => best_per_command.push(Candidate {
                    command: phrase.command,
                    phrase: phrase.text.clone(),
                    score,
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
    pub fn unique_prefix(&self, text: &str, min_ratio: f32) -> Option<usize> {
        let query = normalize(text);
        let query_len = query.chars().count();
        if query_len < MIN_PREFIX_CHARS {
            return None;
        }
        let mut found: Option<usize> = None;
        let mut covered_enough = false;
        for phrase in &self.phrases {
            if !phrase.text.starts_with(&query) {
                continue;
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
}

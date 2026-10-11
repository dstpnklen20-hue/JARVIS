//! Ответ голосом на вопрос-разрешение (раздел 2.1 ТЗ): маленький словарь,
//! поэтому быстро и точно.
//!
//! Порядок проверки важен: «никогда» → отказ («нет», «не разрешаю») →
//! «всегда» → «да». Иначе «не разрешаю» сошло бы за «разрешаю», а
//! «нет, никогда» — за «нет».

use crate::normalize::words;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReply {
    Yes,
    Always,
    No,
    Never,
}

const NEVER: &[&str] = &["никогда"];
const NO: &[&str] = &[
    "нет",
    "отмена",
    "отмени",
    "отменить",
    "отменяю",
    "запрещаю",
    "стоп",
    "неа",
    "нельзя",
];
const ALWAYS: &[&str] = &["всегда", "навсегда", "постоянно"];
const YES: &[&str] = &[
    "да",
    "разрешаю",
    "давай",
    "можно",
    "выполняй",
    "выполни",
    "конечно",
    "ага",
    "угу",
    "хорошо",
    "ок",
    "окей",
    "подтверждаю",
    "согласен",
    "действуй",
    "валяй",
];

/// Распознаёт ответ. `None` — это не ответ (ждём дальше).
pub fn parse_reply(text: &str) -> Option<PermissionReply> {
    let words = words(text);
    let has = |list: &[&str]| words.iter().any(|w| list.iter().any(|t| close(w, t)));
    // «не разрешаю», «не надо», «не нужно»: частица «не» перед словом.
    let negated = words.windows(2).any(|w| w[0] == "не" && w[1] != "знаю");
    if has(NEVER) || words.windows(3).any(|w| w == ["ни", "за", "что"]) {
        return Some(PermissionReply::Never);
    }
    if has(NO) || negated {
        return Some(PermissionReply::No);
    }
    if has(ALWAYS) {
        return Some(PermissionReply::Always);
    }
    if has(YES) {
        return Some(PermissionReply::Yes);
    }
    None
}

/// Короткие слова — точно, длинные — с одной ошибкой («всигда»).
fn close(word: &str, target: &str) -> bool {
    if target.chars().count() < 5 {
        word == target
    } else {
        strsim::levenshtein(word, target) <= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionReply::*;

    #[test]
    fn basic_answers() {
        assert_eq!(parse_reply("да"), Some(Yes));
        assert_eq!(parse_reply("Разрешаю"), Some(Yes));
        assert_eq!(parse_reply("всегда"), Some(Always));
        assert_eq!(parse_reply("разрешаю всегда"), Some(Always));
        assert_eq!(parse_reply("нет"), Some(No));
        assert_eq!(parse_reply("отмена"), Some(No));
        assert_eq!(parse_reply("никогда"), Some(Never));
    }

    #[test]
    fn negation_wins_over_yes_words() {
        assert_eq!(parse_reply("не разрешаю"), Some(No));
        assert_eq!(parse_reply("не надо"), Some(No));
        assert_eq!(parse_reply("нет никогда"), Some(Never));
        assert_eq!(parse_reply("ни за что"), Some(Never));
        assert_eq!(parse_reply("да всегда"), Some(Always));
    }

    #[test]
    fn small_misrecognitions() {
        assert_eq!(parse_reply("всигда"), Some(Always));
        assert_eq!(parse_reply("никагда"), Some(Never));
        assert_eq!(parse_reply("разрешаю пожалуйста"), Some(Yes));
    }

    #[test]
    fn short_words_are_exact() {
        // «дай» не «да», «нести» не «нет».
        assert_eq!(parse_reply("дай подумать"), None);
        assert_eq!(parse_reply("нести"), None);
    }

    #[test]
    fn unrelated_text_is_not_an_answer() {
        assert_eq!(parse_reply("открой ютуб"), None);
        assert_eq!(parse_reply(""), None);
        assert_eq!(parse_reply("я не знаю"), None);
    }
}

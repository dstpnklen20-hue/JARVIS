//! Нормализация текста для сравнения: нижний регистр, ё→е, без знаков
//! препинания, без слов-паразитов, числа цифрами.
//!
//! Одной и той же функцией нормализуются и распознанная речь, и фразы из
//! манифестов — поэтому «Ну открой, пожалуйста, YouTube» и «открой ютуб»
//! сравниваются по существу.

use crate::numbers::words_to_digits;

/// Слова, которые ничего не меняют в команде. «но» — распознаватель часто
/// пишет так «ну».
const FILLERS: &[&str] = &[
    "ну",
    "но",
    "пожалуйста",
    "плиз",
    "давай",
    "давайте",
    "слушай",
    "послушай",
    "эээ",
    "ээ",
    "э",
    "эм",
    "мм",
    "ммм",
    "а",
    "так",
    "вот",
    "ка",
    "же",
    "уж",
    "сэр",
    "будь",
    "добр",
    "любезен",
];

/// Слова текста в нормальной форме (без удаления паразитов и чисел).
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase().replace('ё', "е"))
        .collect()
}

/// Полная нормализация: слова, без паразитов, числа цифрами, через пробел.
pub fn normalize(text: &str) -> String {
    let all = words(text);
    let meaningful: Vec<String> = all
        .iter()
        .filter(|w| !FILLERS.contains(&w.as_str()))
        .cloned()
        .collect();
    // Если фраза целиком из «паразитов» («так», «вот») — оставляем как есть.
    let kept = if meaningful.is_empty() {
        all
    } else {
        meaningful
    };
    words_to_digits(&kept).join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_punctuation_and_yo() {
        assert_eq!(normalize("Открой, ЮТУБ!"), "открой ютуб");
        assert_eq!(normalize("Ещё громче"), "еще громче");
        assert_eq!(normalize("  открой   ютуб  "), "открой ютуб");
    }

    #[test]
    fn fillers_removed_anywhere() {
        assert_eq!(normalize("Ну открой, пожалуйста, ютуб"), "открой ютуб");
        assert_eq!(normalize("открой-ка ютуб"), "открой ютуб");
        assert_eq!(normalize("эээ следующий трек"), "следующий трек");
    }

    #[test]
    fn only_fillers_are_kept() {
        assert_eq!(normalize("так вот"), "так вот");
    }

    #[test]
    fn numbers_become_digits() {
        assert_eq!(normalize("Громкость на сорок пять"), "громкость на 45");
        assert_eq!(normalize("громкость на 45"), "громкость на 45");
    }

    #[test]
    fn empty_text() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize(" ,.!"), "");
    }
}

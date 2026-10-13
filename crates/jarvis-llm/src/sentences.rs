//! Нарезка потокового текста на предложения для озвучки: каждое готовое
//! предложение сразу уходит в синтез, не дожидаясь конца ответа.
//!
//! - Конец предложения — `.`, `!`, `?`, `…` и пробел после них.
//! - Слишком короткие куски («т. е.», «А. С.») не отрезаются — копятся дальше.
//! - Длинный кусок без точки режется по запятой/тире/двоеточию, чтобы первое
//!   слово ответа не ждало конца длинного предложения.
//! - Разметка (`*`, `#`, `` ` ``) убирается — её незачем произносить.

/// Короче — не отдельное предложение (сокращение, инициалы).
const MIN_SENTENCE_CHARS: usize = 12;
/// Длиннее без конца предложения — резать по запятой.
const MAX_CHUNK_CHARS: usize = 160;

#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buffer: String,
}

impl SentenceSplitter {
    /// Добавить кусок текста; вернуть готовые предложения.
    pub fn push(&mut self, piece: &str) -> Vec<String> {
        self.buffer.push_str(piece);
        let mut out = Vec::new();
        while let Some(end) = self.sentence_end() {
            let sentence: String = self.buffer[..end].to_owned();
            self.buffer.drain(..end);
            if let Some(s) = clean(&sentence) {
                out.push(s);
            }
        }
        out
    }

    /// Ответ закончился — отдать остаток.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buffer);
        clean(&rest)
    }

    /// Байтовая позиция конца первого готового предложения (включая пробел).
    fn sentence_end(&self) -> Option<usize> {
        let chars: Vec<(usize, char)> = self.buffer.char_indices().collect();
        let mut soft = None;
        for (i, &(pos, c)) in chars.iter().enumerate() {
            let next = chars.get(i + 1).map(|&(_, n)| n);
            let len = i + 1;
            let ends = matches!(c, '.' | '!' | '?' | '…');
            if ends && next.is_some_and(char::is_whitespace) && len >= MIN_SENTENCE_CHARS {
                return Some(pos + c.len_utf8());
            }
            if c == '\n' && len >= MIN_SENTENCE_CHARS {
                return Some(pos + c.len_utf8());
            }
            if matches!(c, ',' | ';' | ':' | '—') && next.is_some_and(char::is_whitespace) {
                soft = Some(pos + c.len_utf8());
            }
            if len >= MAX_CHUNK_CHARS
                && let Some(cut) = soft
            {
                return Some(cut);
            }
        }
        None
    }
}

/// Убрать разметку и лишние пробелы; пустое — `None`.
fn clean(text: &str) -> Option<String> {
    let without: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '_'))
        .collect();
    let words: Vec<&str> = without.split_whitespace().collect();
    let joined = words.join(" ");
    let trimmed = joined.trim_start_matches(['-', '•']).trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(pieces: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::default();
        let mut out: Vec<String> = pieces.iter().flat_map(|p| s.push(p)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn splits_streamed_text_into_sentences() {
        assert_eq!(
            split(&[
                "Тони Старк — изобретатель",
                " и глава компании, сэр. Он же",
                " — Железный человек! Рад",
                " служить."
            ]),
            [
                "Тони Старк — изобретатель и глава компании, сэр.",
                "Он же — Железный человек!",
                "Рад служить."
            ]
        );
    }

    #[test]
    fn short_abbreviations_do_not_split() {
        assert_eq!(
            split(&["Это т. е. пример. А. С. Пушкин написал много стихов. Да."]),
            [
                "Это т. е. пример.",
                "А. С. Пушкин написал много стихов.",
                "Да."
            ]
        );
    }

    #[test]
    fn numbers_with_dots_stay_whole() {
        assert_eq!(
            split(&["Число пи примерно 3.14 и это важно знать."]),
            ["Число пи примерно 3.14 и это важно знать."]
        );
    }

    #[test]
    fn long_chunk_is_cut_at_comma() {
        let long = format!("{}, {}", "а".repeat(100), "б".repeat(80));
        let out = split(&[&long]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].ends_with(','));
    }

    #[test]
    fn markdown_is_removed() {
        assert_eq!(
            split(&["**Важно:** ядро *реактора* охлаждается водой.\n- Второй пункт списка тут."]),
            [
                "Важно: ядро реактора охлаждается водой.",
                "Второй пункт списка тут."
            ]
        );
    }
}

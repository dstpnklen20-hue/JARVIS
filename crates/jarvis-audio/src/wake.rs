//! Слово активации «Джарвис».
//!
//! [`WakeWordEngine`] — сменный движок (раздел 4.2 ТЗ). Основная реализация
//! [`SherpaWakeWord`] непрерывно распознаёт речь потоковой русской моделью и
//! ищет в распознанном тексте слово активации с допуском на неточность
//! ([`WakeMatcher`]). Срабатывает по частичному результату — не дожидаясь
//! конца фразы.

use std::sync::Arc;

use sherpa_onnx::{OnlineRecognizer, OnlineStream};

use crate::AudioError;
use crate::asr::{Endpointing, create_recognizer};
use crate::models::AsrModel;
use crate::resample::MODEL_RATE;

/// Сработавшее слово активации.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeDetection {
    /// Как распознаватель написал слово.
    pub heard: String,
    /// Сколько секунд назад (относительно последнего принятого отсчёта)
    /// началось слово, если движок это знает. Нужно, чтобы вырезать слово
    /// для проверки голоса.
    pub started_secs_ago: Option<f32>,
    /// Громкость фразы до срабатывания: самая громкая порция (~20 мс), dBFS.
    /// Для разбора промахов: сравнить с громкостью «почти Джарвис» в логе.
    pub level_db: f32,
}

/// Движок слова активации. Принимает 16 кГц моно.
pub trait WakeWordEngine: Send {
    fn accept(&mut self, samples: &[f32]) -> Option<WakeDetection>;
    /// Сбросить состояние (после паузы, после ответа).
    fn reset(&mut self);
    fn name(&self) -> &'static str;
}

/// Слова распознанного текста в нормальной форме: нижний регистр, ё→е,
/// только буквы.
pub fn normalize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase().replace('ё', "е"))
        .collect()
}

/// Ищет слово активации среди слов с допуском по расстоянию Левенштейна.
#[derive(Debug, Clone)]
pub struct WakeMatcher {
    targets: Vec<String>,
    max_distance: usize,
}

/// Короче этого слово не сравниваем: «да», «вас» и т. п. не должны
/// оказаться «похожими» на слово активации.
const MIN_WORD_CHARS: usize = 4;
/// «Почти слово активации» для лога: на столько букв больше допуска.
const NEAR_EXTRA: usize = 2;

impl WakeMatcher {
    pub fn new(words: impl IntoIterator<Item = impl AsRef<str>>, max_distance: usize) -> Self {
        let mut targets: Vec<String> = words
            .into_iter()
            .flat_map(|w| normalize_words(w.as_ref()))
            .collect();
        targets.sort();
        targets.dedup();
        Self {
            targets,
            max_distance,
        }
    }

    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    pub fn matches(&self, word: &str) -> bool {
        let len = word.chars().count();
        if len < MIN_WORD_CHARS {
            return false;
        }
        self.targets.iter().any(|t| {
            // Допуск не больше трети длины слова: для коротких целей — строже.
            let allowed = self.max_distance.min(t.chars().count() / 3);
            strsim::levenshtein(word, t) <= allowed
        })
    }

    /// Слово похоже на слово активации, но не засчитано: отличие на
    /// [`NEAR_EXTRA`] букв больше допуска. Только для лога промахов.
    pub fn near_miss<'w>(&self, words: &'w [String]) -> Option<&'w str> {
        words
            .iter()
            .filter(|w| w.chars().count() >= MIN_WORD_CHARS && !self.matches(w))
            .find(|w| {
                self.targets.iter().any(|t| {
                    let allowed = self.max_distance.min(t.chars().count() / 3);
                    strsim::levenshtein(w, t) <= allowed + NEAR_EXTRA
                })
            })
            .map(String::as_str)
    }

    /// Индекс первого подходящего слова начиная с `from`.
    pub fn find(&self, words: &[String], from: usize) -> Option<usize> {
        words
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, w)| self.matches(w))
            .map(|(i, _)| i)
    }
}

/// Слово активации на потоковой модели sherpa-onnx.
pub struct SherpaWakeWord {
    /// Может быть общим с распознаванием команд (один экземпляр модели в памяти).
    recognizer: Arc<OnlineRecognizer>,
    stream: OnlineStream,
    matcher: WakeMatcher,
    /// Слова текущей фразы, которые уже проверены и сработали.
    consumed_words: usize,
    last_text: String,
    /// Сколько отсчётов принято с создания потока (для времени слова).
    fed_samples: u64,
    /// Сколько отсчётов с последнего сброса потока.
    stream_samples: u64,
    /// Сколько последних отсчётов подряд — тишина.
    quiet_samples: u64,
    /// Самая громкая порция текущей фразы (RMS).
    phrase_peak: f32,
    /// В текущей фразе уже было слово активации.
    phrase_detected: bool,
}

impl SherpaWakeWord {
    /// Со своим распознавателем (жадный поиск, без подсказок).
    pub fn new(model: &AsrModel, matcher: WakeMatcher) -> Result<Self, AudioError> {
        let recognizer = create_recognizer(model, Endpointing::default())?;
        Ok(Self::with_recognizer(Arc::new(recognizer), matcher))
    }

    /// На общем распознавателе (см. [`crate::engines`]).
    pub fn with_recognizer(recognizer: Arc<OnlineRecognizer>, matcher: WakeMatcher) -> Self {
        let stream = recognizer.create_stream();
        let mut engine = Self {
            recognizer,
            stream,
            matcher,
            consumed_words: 0,
            last_text: String::new(),
            fed_samples: 0,
            stream_samples: 0,
            quiet_samples: 0,
            phrase_peak: 0.0,
            phrase_detected: false,
        };
        engine.warm_up();
        engine
    }

    /// Первый прогон модели медленнее остальных — делаем его на тишине
    /// при старте, чтобы первое «Джарвис» не ждало (раздел 3.2 ТЗ).
    fn warm_up(&mut self) {
        let silence = vec![0.0; MODEL_RATE as usize];
        self.accept(&silence);
        self.reset();
    }

    fn reset_stream(&mut self) {
        // Новый поток, а не recognizer.reset(): reset в sherpa-onnx не
        // сбрасывает состояние энкодера, и хвост прошлой фразы искажал начало
        // следующей («джарвис» → «орвис» в тестах).
        self.stream = self.recognizer.create_stream();
        self.consumed_words = 0;
        self.last_text.clear();
        self.stream_samples = 0;
        self.phrase_peak = 0.0;
        self.phrase_detected = false;
    }

    /// Фраза кончилась без слова активации, но в ней было похожее слово —
    /// записать в лог (что услышано и как громко), чтобы разобрать промах.
    fn log_near_miss(&self) {
        if self.phrase_detected {
            return;
        }
        let words = normalize_words(&self.last_text);
        if let Some(word) = self.matcher.near_miss(&words) {
            tracing::info!(
                heard = word,
                phrase = %self.last_text.trim(),
                level_db = format_args!("{:.0}", to_db(self.phrase_peak)),
                "похоже на «Джарвис», но не засчитано"
            );
        }
    }
}

/// RMS → dBFS (тишина — −100).
pub fn to_db(rms: f32) -> f32 {
    if rms > 1e-5 {
        20.0 * rms.log10()
    } else {
        -100.0
    }
}

/// Уровень (RMS), ниже которого звук считаем тишиной (≈ −46 dBFS).
pub(crate) const QUIET_RMS: f32 = 0.005;
/// Сброс на «пустой» тишине — только если поток живёт дольше…
const IDLE_RESET_SECS: f32 = 10.0;
/// …и последние столько секунд действительно тихо.
const QUIET_BEFORE_RESET_SECS: f32 = 0.3;
/// Длиннее поток не держим (ограничиваем память распознавателя)…
const MAX_STREAM_SECS: f32 = 60.0;
/// …а этот предел — даже если вокруг постоянный шум.
const HARD_STREAM_SECS: f32 = 180.0;

/// Когда сбрасывать поток распознавателя.
///
/// Встроенное правило sherpa-onnx «2 с тишины без слов» нельзя исполнять
/// вслепую: модель видит речь с задержкой, и сброс может прийтись на
/// первые 100–300 мс нового слова — его начало потеряется («джарвис» →
/// «орвис»). Поэтому на «пустой» тишине сбрасываем, только когда звук
/// действительно тихий, а после законченной фразы — сразу (после неё уже
/// 0.8 с тишины).
pub(crate) fn should_reset(
    endpoint: bool,
    has_text: bool,
    stream_secs: f32,
    quiet_secs: f32,
) -> bool {
    if stream_secs >= HARD_STREAM_SECS {
        return true;
    }
    if stream_secs >= MAX_STREAM_SECS && quiet_secs >= QUIET_BEFORE_RESET_SECS {
        return true;
    }
    if !endpoint {
        return false;
    }
    has_text || (stream_secs >= IDLE_RESET_SECS && quiet_secs >= QUIET_BEFORE_RESET_SECS)
}

pub(crate) fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

impl WakeWordEngine for SherpaWakeWord {
    fn accept(&mut self, samples: &[f32]) -> Option<WakeDetection> {
        self.stream.accept_waveform(MODEL_RATE as i32, samples);
        self.fed_samples += samples.len() as u64;
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }

        self.stream_samples += samples.len() as u64;
        let level = rms(samples);
        self.phrase_peak = self.phrase_peak.max(level);
        if level < QUIET_RMS {
            self.quiet_samples += samples.len() as u64;
        } else {
            self.quiet_samples = 0;
        }

        let mut detection = None;
        if let Some(result) = self.recognizer.get_result(&self.stream)
            && result.text != self.last_text
        {
            let words = normalize_words(&result.text);
            tracing::trace!(text = %result.text, "распознано");
            if let Some(index) = self.matcher.find(&words, self.consumed_words) {
                let now_secs = self.fed_samples as f32 / MODEL_RATE as f32;
                let started = word_start_secs(&result.tokens, result.timestamps.as_deref(), index)
                    .map(|t| result.start_time.unwrap_or(0.0) + t);
                detection = Some(WakeDetection {
                    heard: words[index].clone(),
                    started_secs_ago: started.map(|s| (now_secs - s).max(0.0)),
                    level_db: to_db(self.phrase_peak),
                });
                self.phrase_detected = true;
                // Слово может ещё дописываться («жарви» → «жарвис») —
                // повторно его не засчитываем.
                self.consumed_words = words.len();
            }
            self.last_text = result.text;
        }

        let secs = |n: u64| n as f32 / MODEL_RATE as f32;
        if should_reset(
            self.recognizer.is_endpoint(&self.stream),
            !self.last_text.trim().is_empty(),
            secs(self.stream_samples),
            secs(self.quiet_samples),
        ) {
            self.log_near_miss();
            self.reset_stream();
        }
        detection
    }

    fn reset(&mut self) {
        self.reset_stream();
    }

    fn name(&self) -> &'static str {
        "sherpa-zipformer"
    }
}

/// Время начала `word_index`-го слова по меткам токенов. Токен BPE,
/// начинающий слово, начинается с «▁».
fn word_start_secs(
    tokens: &[String],
    timestamps: Option<&[f32]>,
    word_index: usize,
) -> Option<f32> {
    let timestamps = timestamps?;
    let mut word = 0usize;
    let mut seen_any = false;
    for (i, token) in tokens.iter().enumerate() {
        // sherpa-onnx отдаёт «▁» в начале слова как пробел.
        let starts_word = token.starts_with('▁') || token.starts_with(' ');
        if starts_word && seen_any {
            word += 1;
        }
        if starts_word || !seen_any {
            seen_any = true;
            if word == word_index {
                return timestamps.get(i).copied();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        normalize_words(s)
    }

    #[test]
    fn normalization() {
        assert_eq!(
            words("Джарвис, ОТКРОЙ ютуб!"),
            ["джарвис", "открой", "ютуб"]
        );
        assert_eq!(words("ещё всё"), ["еще", "все"]);
        assert!(words(" ,.!").is_empty());
    }

    #[test]
    fn matches_typical_misrecognitions() {
        let m = WakeMatcher::new(["джарвис", "жарвис"], 1);
        for heard in ["джарвис", "жарвис", "жарвиз", "джарвиз", "джавис", "жарви"]
        {
            assert!(m.matches(heard), "{heard} должно сработать");
        }
    }

    #[test]
    fn rejects_unrelated_and_short_words() {
        let m = WakeMatcher::new(["джарвис", "жарвис"], 1);
        for heard in [
            "жар",
            "джаз",
            "арбуз",
            "сервис",
            "дарвин",
            "жарко",
            "да",
            "вас",
        ] {
            assert!(!m.matches(heard), "{heard} не должно сработать");
        }
    }

    #[test]
    fn near_miss_finds_similar_but_not_matching_words() {
        let m = WakeMatcher::new(["джарвис", "жарвис"], 1);
        let near = |s: &str| m.near_miss(&words(s)).map(str::to_owned);
        assert_eq!(near("сервис"), Some("сервис".into()));
        assert_eq!(near("ну дарвин открой"), Some("дарвин".into()));
        // Засчитанное слово — не промах; непохожие и короткие слова — тоже.
        assert_eq!(near("джарвис открой ютуб"), None);
        assert_eq!(near("открой ютуб"), None);
        assert_eq!(near("жар"), None);
    }

    #[test]
    fn db_of_level() {
        assert_eq!(to_db(1.0), 0.0);
        assert!((to_db(0.1) + 20.0).abs() < 1e-4);
        assert_eq!(to_db(0.0), -100.0);
    }

    #[test]
    fn zero_distance_is_exact() {
        let m = WakeMatcher::new(["джарвис"], 0);
        assert!(m.matches("джарвис"));
        assert!(!m.matches("жарвис"));
    }

    #[test]
    fn distance_is_capped_by_word_length() {
        // Для 4-буквенной цели допуск 2 превратится в 1.
        let m = WakeMatcher::new(["макс"], 2);
        assert!(m.matches("макс"));
        assert!(!m.matches("мост"));
    }

    #[test]
    fn find_respects_already_consumed_words() {
        let m = WakeMatcher::new(["джарвис"], 1);
        let w = words("джарвис открой джарвис");
        assert_eq!(m.find(&w, 0), Some(0));
        assert_eq!(m.find(&w, 1), Some(2));
        assert_eq!(m.find(&w, 3), None);
    }

    #[test]
    fn targets_are_normalized_and_deduplicated() {
        let m = WakeMatcher::new(["Джарвис", "джарвис", "ЖАРВИС"], 1);
        assert_eq!(m.targets(), ["джарвис", "жарвис"]);
    }

    #[test]
    fn reset_after_finished_phrase() {
        assert!(should_reset(true, true, 3.0, 0.0));
    }

    #[test]
    fn no_reset_on_short_empty_silence() {
        // Тот самый случай: 2 с «тишины», а пользователь уже начал говорить.
        assert!(!should_reset(true, false, 2.0, 0.0));
        assert!(!should_reset(true, false, 5.0, 1.0));
    }

    #[test]
    fn reset_on_long_quiet_silence_only() {
        assert!(should_reset(true, false, 12.0, 0.5));
        assert!(!should_reset(true, false, 12.0, 0.1));
        assert!(!should_reset(false, false, 12.0, 5.0));
    }

    #[test]
    fn stream_length_is_bounded() {
        assert!(should_reset(false, true, 61.0, 0.4));
        assert!(!should_reset(false, true, 61.0, 0.0));
        assert!(should_reset(false, true, 181.0, 0.0));
    }

    #[test]
    fn rms_of_signal() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[0.5, -0.5]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn word_start_from_token_timestamps() {
        let tokens: Vec<String> = ["▁от", "крой", "▁д", "жар", "вис"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ts = [0.1, 0.3, 0.6, 0.7, 0.8];
        assert_eq!(word_start_secs(&tokens, Some(&ts), 0), Some(0.1));
        assert_eq!(word_start_secs(&tokens, Some(&ts), 1), Some(0.6));
        assert_eq!(word_start_secs(&tokens, Some(&ts), 2), None);
        assert_eq!(word_start_secs(&tokens, None, 0), None);

        // Так токены приходят от sherpa-onnx на самом деле.
        let tokens: Vec<String> = [" д", "жа", "р", "ви", "з", " включи"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ts = [0.2, 0.3, 0.4, 0.5, 0.6, 0.9];
        assert_eq!(word_start_secs(&tokens, Some(&ts), 0), Some(0.2));
        assert_eq!(word_start_secs(&tokens, Some(&ts), 1), Some(0.9));
    }
}

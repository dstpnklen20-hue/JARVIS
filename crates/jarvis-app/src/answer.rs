//! Очередь озвучки ответа нейросети: предложения уходят в синтез по мере
//! прихода, а звучат строго по порядку и без наложения — следующее, когда
//! кончилось предыдущее (плеер новый звук не смешивает, а прерывает старый).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Через сколько без первого предложения сказать «Секунду, сэр».
pub const THINKING_CUE_AFTER: Duration = Duration::from_millis(1500);

#[derive(Debug)]
pub struct SpeechQueue<T> {
    /// Номера предложений по порядку; `Some` — синтез готов.
    items: VecDeque<(u64, Option<T>)>,
    playing_until: Option<Instant>,
}

impl<T> Default for SpeechQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            playing_until: None,
        }
    }
}

impl<T> SpeechQueue<T> {
    /// Предложение отправлено в синтез.
    pub fn expect(&mut self, id: u64) {
        self.items.push_back((id, None));
    }

    pub fn contains(&self, id: u64) -> bool {
        self.items.iter().any(|(i, _)| *i == id)
    }

    /// Синтез готов. `false` — такого предложения не ждали.
    pub fn ready(&mut self, id: u64, clip: T) -> bool {
        match self.items.iter_mut().find(|(i, _)| *i == id) {
            Some(item) => {
                item.1 = Some(clip);
                true
            }
            None => false,
        }
    }

    /// Синтез не удался — предложение пропускается.
    pub fn failed(&mut self, id: u64) {
        self.items.retain(|(i, _)| *i != id);
    }

    /// Что играть сейчас: первое по порядку, если оно готово и прошлое кончилось.
    pub fn next(&mut self, now: Instant) -> Option<T> {
        if self.busy(now) || !matches!(self.items.front(), Some((_, Some(_)))) {
            return None;
        }
        self.items.pop_front().and_then(|(_, clip)| clip)
    }

    /// Начали играть звук длительностью `duration`.
    pub fn playing(&mut self, now: Instant, duration: Duration) {
        self.playing_until = Some(now + duration);
    }

    /// Ещё звучит прошлое.
    pub fn busy(&self, now: Instant) -> bool {
        self.playing_until.is_some_and(|until| now < until)
    }

    /// Всё сказано и ничего не ждём.
    pub fn idle(&self, now: Instant) -> bool {
        self.items.is_empty() && !self.busy(now)
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.playing_until = None;
    }
}

/// Сказать ли «Секунду, сэр»: ответ ещё не начал звучать и задерживается.
pub fn thinking_cue_due(asked: Instant, now: Instant, anything_spoken: bool, done: bool) -> bool {
    !anything_spoken && !done && now.duration_since(asked) >= THINKING_CUE_AFTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_in_order_without_overlap() {
        let t0 = Instant::now();
        let mut q = SpeechQueue::default();
        q.expect(1);
        q.expect(2);
        // Второе готово раньше первого — ждём первое.
        assert!(q.ready(2, "второе"));
        assert_eq!(q.next(t0), None);
        assert!(q.ready(1, "первое"));
        assert_eq!(q.next(t0), Some("первое"));
        q.playing(t0, Duration::from_secs(2));
        // Пока звучит первое — второе ждёт.
        assert_eq!(q.next(t0 + Duration::from_secs(1)), None);
        assert!(!q.idle(t0 + Duration::from_secs(1)));
        let t2 = t0 + Duration::from_secs(2);
        assert_eq!(q.next(t2), Some("второе"));
        q.playing(t2, Duration::from_secs(1));
        assert!(q.idle(t2 + Duration::from_secs(1)));
    }

    #[test]
    fn failed_and_unknown_sentences_are_skipped() {
        let t0 = Instant::now();
        let mut q = SpeechQueue::default();
        q.expect(1);
        q.expect(2);
        q.failed(1);
        assert!(!q.ready(99, "чужое"));
        assert!(q.ready(2, "второе"));
        assert_eq!(q.next(t0), Some("второе"));
        q.expect(3);
        q.clear();
        assert!(q.idle(t0));
    }

    #[test]
    fn thinking_cue_only_when_answer_is_late() {
        let t0 = Instant::now();
        let late = t0 + THINKING_CUE_AFTER;
        assert!(!thinking_cue_due(t0, t0, false, false));
        assert!(thinking_cue_due(t0, late, false, false));
        assert!(!thinking_cue_due(t0, late, true, false), "уже звучит ответ");
        assert!(!thinking_cue_due(t0, late, false, true), "ответ уже пришёл");
    }
}

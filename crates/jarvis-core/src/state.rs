//! Конечный автомат состояний Джарвиса (раздел 3.1 ТЗ).
//!
//! Переходы — чистая функция [`transition`]: состояние + событие + текущее
//! время → новое состояние и список эффектов. Эффекты исполняет вызывающий
//! код (воспроизвести звук, остановить микрофон). Так все переходы
//! проверяются тестами без звука, потоков и часов.
//!
//! Этап 1: `PAUSED ⇄ IDLE → LISTENING → IDLE`. Остальные состояния
//! (ASKING_PERMISSION, EXECUTING, FOLLOW_UP, GAME_MODE) появятся на своих этапах.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Прослушивание выключено, микрофон освобождён.
    Paused,
    /// Ждём «Джарвис».
    Idle,
    /// Услышали «Джарвис», ждём команду до `until`.
    Listening { until: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Распознано слово активации (и, если включено, подтверждён голос).
    WakeWord,
    /// Периодический такт: проверка таймаутов.
    Tick,
    /// Пункт «Пауза» в трее (переключатель).
    TogglePause,
    Pause,
    Resume,
}

/// Готовые ответы из звукового пакета (раздел 4.5 ТЗ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reply {
    /// «Да, сэр» — отклик на слово активации.
    Wake,
    /// «Запрос выполнен, сэр».
    Done,
    /// «Не понял, сэр».
    NotUnderstood,
    /// «Не удалось, сэр».
    Failed,
}

impl Reply {
    pub const ALL: [Reply; 4] = [
        Reply::Wake,
        Reply::Done,
        Reply::NotUnderstood,
        Reply::Failed,
    ];

    /// Ключ в `pack.toml` звукового пакета.
    pub fn key(self) -> &'static str {
        match self {
            Reply::Wake => "wake",
            Reply::Done => "done",
            Reply::NotUnderstood => "not_understood",
            Reply::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    PlayReply(Reply),
    /// Включить или выключить прослушивание микрофона.
    SetListening(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub next: State,
    pub effects: Vec<Effect>,
}

impl Transition {
    fn stay(state: State) -> Self {
        Self {
            next: state,
            effects: Vec::new(),
        }
    }
}

/// Настройки, влияющие на переходы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// Сколько ждать команду после «Джарвис».
    pub listen: Duration,
}

pub fn transition(state: State, event: Event, now: Instant, timings: Timings) -> Transition {
    use Event as E;
    use State as S;

    let pause = || Transition {
        next: S::Paused,
        effects: vec![Effect::SetListening(false)],
    };

    match (state, event) {
        (S::Idle, E::WakeWord) => Transition {
            next: S::Listening {
                until: now + timings.listen,
            },
            effects: vec![Effect::PlayReply(Reply::Wake)],
        },
        // Повторное «Джарвис» во время ожидания команды: продлеваем ожидание
        // без повторного ответа, чтобы не перебивать пользователя.
        (S::Listening { .. }, E::WakeWord) => Transition::stay(S::Listening {
            until: now + timings.listen,
        }),
        (S::Listening { until }, E::Tick) if now >= until => Transition::stay(S::Idle),
        (S::Listening { .. } | S::Idle, E::Tick) => Transition::stay(state),

        (S::Idle | S::Listening { .. }, E::Pause | E::TogglePause) => pause(),
        (S::Idle | S::Listening { .. }, E::Resume) => Transition::stay(state),

        (S::Paused, E::Resume | E::TogglePause) => Transition {
            next: S::Idle,
            effects: vec![Effect::SetListening(true)],
        },
        // На паузе микрофон выключен: запоздавшее «Джарвис» из очереди игнорируем.
        (S::Paused, E::WakeWord | E::Tick | E::Pause) => Transition::stay(S::Paused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timings = Timings {
        listen: Duration::from_secs(6),
    };

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn idle_wake_starts_listening_and_answers() {
        let t0 = now();
        let tr = transition(State::Idle, Event::WakeWord, t0, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: t0 + T.listen
            }
        );
        assert_eq!(tr.effects, [Effect::PlayReply(Reply::Wake)]);
    }

    #[test]
    fn listening_wake_extends_without_answer() {
        let t0 = now();
        let state = State::Listening { until: t0 };
        let later = t0 + Duration::from_secs(2);
        let tr = transition(state, Event::WakeWord, later, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: later + T.listen
            }
        );
        assert!(tr.effects.is_empty());
    }

    #[test]
    fn listening_times_out_to_idle() {
        let t0 = now();
        let state = State::Listening {
            until: t0 + T.listen,
        };
        assert_eq!(transition(state, Event::Tick, t0, T).next, state);
        let at_deadline = transition(state, Event::Tick, t0 + T.listen, T);
        assert_eq!(at_deadline.next, State::Idle);
        assert!(at_deadline.effects.is_empty());
    }

    #[test]
    fn idle_tick_and_resume_change_nothing() {
        for event in [Event::Tick, Event::Resume] {
            let tr = transition(State::Idle, event, now(), T);
            assert_eq!(tr, Transition::stay(State::Idle), "{event:?}");
        }
        let listening = State::Listening {
            until: now() + T.listen,
        };
        assert_eq!(
            transition(listening, Event::Resume, now(), T),
            Transition::stay(listening)
        );
    }

    #[test]
    fn pause_from_any_active_state_stops_listening() {
        let listening = State::Listening {
            until: now() + T.listen,
        };
        for state in [State::Idle, listening] {
            for event in [Event::Pause, Event::TogglePause] {
                let tr = transition(state, event, now(), T);
                assert_eq!(tr.next, State::Paused, "{state:?} + {event:?}");
                assert_eq!(tr.effects, [Effect::SetListening(false)]);
            }
        }
    }

    #[test]
    fn resume_from_pause_returns_to_idle() {
        for event in [Event::Resume, Event::TogglePause] {
            let tr = transition(State::Paused, event, now(), T);
            assert_eq!(tr.next, State::Idle, "{event:?}");
            assert_eq!(tr.effects, [Effect::SetListening(true)]);
        }
    }

    #[test]
    fn paused_ignores_wake_tick_and_pause() {
        for event in [Event::WakeWord, Event::Tick, Event::Pause] {
            let tr = transition(State::Paused, event, now(), T);
            assert_eq!(tr, Transition::stay(State::Paused), "{event:?}");
        }
    }
}

//! Конечный автомат состояний Джарвиса (раздел 3.1 ТЗ).
//!
//! Переходы — чистая функция [`transition`]: состояние + событие + текущее
//! время → новое состояние и список эффектов. Эффекты исполняет вызывающий
//! код (воспроизвести звук, выполнить команду, остановить микрофон). Так
//! все переходы проверяются тестами без звука, потоков и часов.
//!
//! ```text
//! PAUSED ⇄ IDLE ──«Джарвис»──▶ LISTENING ──команда──▶ EXECUTING ──готово──▶ FOLLOW_UP
//!            ▲                     │ тайм-аут                                 │ команда → EXECUTING
//!            └─────────────────────┴──────────────── тайм-аут ────────────────┘
//! ```
//!
//! ASKING_PERMISSION появится на этапе 3, GAME_MODE — на этапе 6.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Прослушивание выключено, микрофон освобождён.
    Paused,
    /// Ждём «Джарвис».
    Idle,
    /// Услышали «Джарвис», ждём команду до `until`.
    Listening { until: Instant },
    /// Выполняется команда.
    Executing,
    /// После команды ещё ждём следующую без «Джарвис» до `until`.
    FollowUp { until: Instant },
}

impl State {
    /// Принимаются ли сейчас команды без слова активации.
    pub fn accepts_commands(&self) -> bool {
        matches!(self, State::Listening { .. } | State::FollowUp { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Распознано слово активации (и, если включено, подтверждён голос).
    WakeWord,
    /// Распознана команда с этим id.
    Command(String),
    /// Фраза не похожа ни на одну команду.
    NotUnderstood,
    /// Команда выполнена; `reply` — что сказать (уже выбрано ядром:
    /// ответ команды при успехе, «не удалось» при ошибке, ничего для `say`).
    ActionFinished {
        reply: Option<Reply>,
    },
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

    /// Ключ в `pack.toml` звукового пакета и в `reply` манифеста команды.
    pub fn key(self) -> &'static str {
        match self {
            Reply::Wake => "wake",
            Reply::Done => "ok",
            Reply::NotUnderstood => "not_understood",
            Reply::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    PlayReply(Reply),
    /// Включить или выключить микрофон.
    SetListening(bool),
    /// Включить или выключить распознавание команд (слово активации
    /// включает его само, без задержки на круг через ядро).
    SetTranscribing(bool),
    /// Выполнить команду с этим id.
    Execute(String),
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

    fn to(next: State, effects: Vec<Effect>) -> Self {
        Self { next, effects }
    }
}

/// Настройки, влияющие на переходы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// Сколько ждать команду после «Джарвис».
    pub listen: Duration,
    /// Сколько ждать следующую команду после выполненной (FOLLOW_UP).
    pub follow_up: Duration,
}

pub fn transition(state: State, event: Event, now: Instant, timings: Timings) -> Transition {
    use Event as E;
    use State as S;

    let listening = || S::Listening {
        until: now + timings.listen,
    };

    match (state, event) {
        // --- пауза: из любого активного состояния ---
        (S::Paused, E::Resume | E::TogglePause) => {
            Transition::to(S::Idle, vec![Effect::SetListening(true)])
        }
        (S::Paused, _) => Transition::stay(S::Paused),
        (_, E::Pause | E::TogglePause) => Transition::to(
            S::Paused,
            vec![Effect::SetTranscribing(false), Effect::SetListening(false)],
        ),
        (_, E::Resume) => Transition::stay(state),

        // --- слово активации ---
        (S::Idle | S::FollowUp { .. }, E::WakeWord) => {
            Transition::to(listening(), vec![Effect::PlayReply(Reply::Wake)])
        }
        // Повторное «Джарвис» во время ожидания команды: продлеваем без
        // повторного ответа, чтобы не перебивать пользователя.
        (S::Listening { .. }, E::WakeWord) => Transition::stay(listening()),
        (S::Executing, E::WakeWord) => Transition::stay(S::Executing),

        // --- команды ---
        (S::Listening { .. } | S::FollowUp { .. }, E::Command(id)) => {
            Transition::to(S::Executing, vec![Effect::Execute(id)])
        }
        // После «Джарвис» непонятная фраза — переспросить; в FOLLOW_UP это
        // скорее посторонний разговор — молчим.
        (S::Listening { .. }, E::NotUnderstood) => {
            Transition::to(listening(), vec![Effect::PlayReply(Reply::NotUnderstood)])
        }
        (S::FollowUp { .. } | S::Idle | S::Executing, E::NotUnderstood) => Transition::stay(state),
        // Команда без «Джарвис» в ожидании и вторая команда во время первой — игнор.
        (S::Idle | S::Executing, E::Command(_)) => Transition::stay(state),

        // --- окончание команды ---
        (S::Executing, E::ActionFinished { reply }) => Transition::to(
            S::FollowUp {
                until: now + timings.follow_up,
            },
            reply.map(Effect::PlayReply).into_iter().collect(),
        ),
        (_, E::ActionFinished { .. }) => Transition::stay(state),

        // --- таймауты ---
        (S::Listening { until } | S::FollowUp { until }, E::Tick) if now >= until => {
            Transition::to(S::Idle, vec![Effect::SetTranscribing(false)])
        }
        (_, E::Tick) => Transition::stay(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timings = Timings {
        listen: Duration::from_secs(6),
        follow_up: Duration::from_secs(12),
    };

    fn now() -> Instant {
        Instant::now()
    }

    fn cmd(id: &str) -> Event {
        Event::Command(id.into())
    }

    fn all_states(t0: Instant) -> [State; 5] {
        [
            State::Paused,
            State::Idle,
            State::Listening {
                until: t0 + T.listen,
            },
            State::Executing,
            State::FollowUp {
                until: t0 + T.follow_up,
            },
        ]
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
        let later = t0 + Duration::from_secs(2);
        let tr = transition(State::Listening { until: t0 }, Event::WakeWord, later, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: later + T.listen
            }
        );
        assert!(tr.effects.is_empty());
    }

    #[test]
    fn follow_up_wake_answers_and_listens() {
        let t0 = now();
        let tr = transition(State::FollowUp { until: t0 }, Event::WakeWord, t0, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: t0 + T.listen
            }
        );
        assert_eq!(tr.effects, [Effect::PlayReply(Reply::Wake)]);
    }

    #[test]
    fn command_while_listening_or_follow_up_executes() {
        let t0 = now();
        for state in [
            State::Listening {
                until: t0 + T.listen,
            },
            State::FollowUp {
                until: t0 + T.follow_up,
            },
        ] {
            let tr = transition(state, cmd("open_youtube"), t0, T);
            assert_eq!(tr.next, State::Executing, "{state:?}");
            assert_eq!(tr.effects, [Effect::Execute("open_youtube".into())]);
        }
    }

    #[test]
    fn command_ignored_when_idle_executing_or_paused() {
        let t0 = now();
        for state in [State::Idle, State::Executing, State::Paused] {
            assert_eq!(transition(state, cmd("x"), t0, T), Transition::stay(state));
        }
    }

    #[test]
    fn finished_action_goes_to_follow_up_with_reply() {
        let t0 = now();
        let tr = transition(
            State::Executing,
            Event::ActionFinished {
                reply: Some(Reply::Done),
            },
            t0,
            T,
        );
        assert_eq!(
            tr.next,
            State::FollowUp {
                until: t0 + T.follow_up
            }
        );
        assert_eq!(tr.effects, [Effect::PlayReply(Reply::Done)]);

        let silent = transition(
            State::Executing,
            Event::ActionFinished { reply: None },
            t0,
            T,
        );
        assert!(silent.effects.is_empty());
    }

    #[test]
    fn late_action_result_is_ignored_elsewhere() {
        let t0 = now();
        for state in all_states(t0) {
            if state == State::Executing {
                continue;
            }
            let tr = transition(
                state,
                Event::ActionFinished {
                    reply: Some(Reply::Done),
                },
                t0,
                T,
            );
            assert_eq!(tr, Transition::stay(state), "{state:?}");
        }
    }

    #[test]
    fn not_understood_answers_only_after_wake_word() {
        let t0 = now();
        let tr = transition(State::Listening { until: t0 }, Event::NotUnderstood, t0, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: t0 + T.listen
            }
        );
        assert_eq!(tr.effects, [Effect::PlayReply(Reply::NotUnderstood)]);
        for state in [
            State::FollowUp { until: t0 },
            State::Idle,
            State::Executing,
            State::Paused,
        ] {
            assert_eq!(
                transition(state, Event::NotUnderstood, t0, T),
                Transition::stay(state)
            );
        }
    }

    #[test]
    fn listening_and_follow_up_time_out_to_idle() {
        let t0 = now();
        for state in [
            State::Listening {
                until: t0 + T.listen,
            },
            State::FollowUp {
                until: t0 + T.listen,
            },
        ] {
            assert_eq!(
                transition(state, Event::Tick, t0, T),
                Transition::stay(state)
            );
            let tr = transition(state, Event::Tick, t0 + T.listen, T);
            assert_eq!(tr.next, State::Idle, "{state:?}");
            assert_eq!(tr.effects, [Effect::SetTranscribing(false)]);
        }
    }

    #[test]
    fn tick_does_not_change_idle_executing_paused() {
        let t0 = now();
        for state in [State::Idle, State::Executing, State::Paused] {
            assert_eq!(
                transition(state, Event::Tick, t0, T),
                Transition::stay(state)
            );
        }
    }

    #[test]
    fn executing_ignores_wake_word() {
        let tr = transition(State::Executing, Event::WakeWord, now(), T);
        assert_eq!(tr, Transition::stay(State::Executing));
    }

    #[test]
    fn pause_from_every_active_state() {
        let t0 = now();
        for state in all_states(t0) {
            if state == State::Paused {
                continue;
            }
            for event in [Event::Pause, Event::TogglePause] {
                let tr = transition(state, event.clone(), t0, T);
                assert_eq!(tr.next, State::Paused, "{state:?} + {event:?}");
                assert_eq!(
                    tr.effects,
                    [Effect::SetTranscribing(false), Effect::SetListening(false)]
                );
            }
        }
    }

    #[test]
    fn resume_from_pause_returns_to_idle() {
        for event in [Event::Resume, Event::TogglePause] {
            let tr = transition(State::Paused, event.clone(), now(), T);
            assert_eq!(tr.next, State::Idle, "{event:?}");
            assert_eq!(tr.effects, [Effect::SetListening(true)]);
        }
    }

    #[test]
    fn resume_elsewhere_changes_nothing() {
        let t0 = now();
        for state in all_states(t0) {
            if state == State::Paused {
                continue;
            }
            assert_eq!(
                transition(state, Event::Resume, t0, T),
                Transition::stay(state)
            );
        }
    }

    #[test]
    fn paused_ignores_everything_but_resume() {
        let t0 = now();
        for event in [
            Event::WakeWord,
            cmd("x"),
            Event::NotUnderstood,
            Event::ActionFinished { reply: None },
            Event::Tick,
            Event::Pause,
        ] {
            assert_eq!(
                transition(State::Paused, event.clone(), t0, T),
                Transition::stay(State::Paused),
                "{event:?}"
            );
        }
    }

    #[test]
    fn accepts_commands_only_when_waiting_for_them() {
        let t0 = now();
        let accepting: Vec<bool> = all_states(t0).iter().map(State::accepts_commands).collect();
        assert_eq!(accepting, [false, false, true, false, true]);
    }

    #[test]
    fn full_scenario_wake_command_follow_up_command_timeout() {
        // «Джарвис, открой ютуб», затем «включи музыку» без «Джарвис».
        let t0 = now();
        let mut s = State::Idle;
        let mut step = |event: Event, at: Instant| {
            let tr = transition(s, event, at, T);
            s = tr.next;
            tr.effects
        };
        assert_eq!(step(Event::WakeWord, t0), [Effect::PlayReply(Reply::Wake)]);
        assert_eq!(
            step(cmd("open_youtube"), t0),
            [Effect::Execute("open_youtube".into())]
        );
        assert_eq!(
            step(
                Event::ActionFinished {
                    reply: Some(Reply::Done)
                },
                t0
            ),
            [Effect::PlayReply(Reply::Done)]
        );
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(
            step(cmd("media_play_pause"), t1),
            [Effect::Execute("media_play_pause".into())]
        );
        assert!(step(Event::ActionFinished { reply: None }, t1).is_empty());
        assert_eq!(
            step(Event::Tick, t1 + T.follow_up),
            [Effect::SetTranscribing(false)]
        );
        assert_eq!(s, State::Idle);
    }
}

//! Конечный автомат состояний Джарвиса (раздел 3.1 ТЗ).
//!
//! Переходы — чистая функция [`transition`]: состояние + событие + текущее
//! время → новое состояние и список эффектов. Эффекты исполняет вызывающий
//! код (воспроизвести звук, выполнить команду, остановить микрофон). Так
//! все переходы проверяются тестами без звука, потоков и часов.
//!
//! ```text
//! PAUSED ⇄ IDLE ──«Джарвис»──▶ LISTENING ──команда──▶ EXECUTING ──готово──▶ FOLLOW_UP
//!            ▲                     │ нет разрешения        ▲                    │ команда → …
//!            │                     ▼                       │ да / всегда        │
//!            │              ASKING_PERMISSION ─────────────┘                    │
//!            │                     │ нет / никогда → FOLLOW_UP; тишина → IDLE   │
//!            └─────────────────────┴──────────────── тайм-аут ──────────────────┘
//! ```
//!
//! Аварийная остановка из любого состояния → PAUSED. GAME_MODE — этап 6.

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
    /// Спросили разрешение, ждём ответа до `until` (тишина — отказ).
    AskingPermission { until: Instant },
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
    /// Распознана команда с этим id, и она разрешена.
    Command(String),
    /// Распознана команда, но разрешения на неё ещё нет — спросить.
    NeedPermission(String),
    /// Пользователь разрешил («да» или «всегда»).
    PermissionGranted(String),
    /// Пользователь отказал («нет» или «никогда»).
    PermissionDenied,
    /// Горячая клавиша или «Джарвис, стоп».
    EmergencyStop,
    /// Фраза не похожа ни на одну команду.
    NotUnderstood,
    /// Команда выполнена; `reply` — что сказать (уже выбрано ядром:
    /// ответ команды при успехе, «не удалось» при ошибке, ничего для `say`).
    ActionFinished {
        reply: Option<Reply>,
    },
    /// Периодический такт: проверка таймаутов.
    Tick,
    /// Джарвис начал говорить фразу такой длительности (микрофон заглушён).
    Spoke(std::time::Duration),
    /// В речи появились слова (частичный текст) — человек говорит.
    UserSpeaking,
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
    /// «Сэр, разрешите выполнить?» — вопрос-разрешение (подробности — в окне;
    /// голосом их скажет синтез речи на этапе 5).
    Ask,
    /// «Как скажете, сэр» — после отказа.
    Denied,
}

impl Reply {
    pub const ALL: [Reply; 6] = [
        Reply::Wake,
        Reply::Done,
        Reply::NotUnderstood,
        Reply::Failed,
        Reply::Ask,
        Reply::Denied,
    ];

    /// Ключ в `pack.toml` звукового пакета и в `reply` манифеста команды.
    pub fn key(self) -> &'static str {
        match self {
            Reply::Wake => "wake",
            Reply::Done => "ok",
            Reply::NotUnderstood => "not_understood",
            Reply::Failed => "failed",
            Reply::Ask => "ask",
            Reply::Denied => "denied",
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
    /// Показать окно-вопрос о разрешении для команды с этим id.
    AskUser(String),
    /// Убрать окно-вопрос.
    CloseAsk,
    /// Аварийная остановка: замолчать, отменить действие, отпустить клавиши.
    StopAll,
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

/// Пока в речи появляются слова, ожидание команды не кончается раньше, чем
/// через столько после последнего слова: фраза успеет закончиться (конец
/// фразы — 0.8 с тишины) и не оборвётся по таймеру.
pub const SPEAKING_GRACE: Duration = Duration::from_millis(2500);

/// Настройки, влияющие на переходы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// Сколько ждать команду после «Джарвис».
    pub listen: Duration,
    /// Сколько ждать следующую команду после выполненной (FOLLOW_UP).
    pub follow_up: Duration,
    /// Сколько ждать ответа на вопрос-разрешение (раздел 2.1: 10 секунд).
    pub ask: Duration,
}

pub fn transition(state: State, event: Event, now: Instant, timings: Timings) -> Transition {
    use Event as E;
    use State as S;

    let listening = || S::Listening {
        until: now + timings.listen,
    };

    // Уходя из вопроса-разрешения, окно надо убрать.
    let close_ask = matches!(state, S::AskingPermission { .. });
    let with_close = |mut effects: Vec<Effect>| {
        if close_ask {
            effects.insert(0, Effect::CloseAsk);
        }
        effects
    };

    match (state, event) {
        // --- аварийная остановка: из любого состояния ---
        (S::Paused, E::EmergencyStop) => Transition::to(S::Paused, vec![Effect::StopAll]),
        (_, E::EmergencyStop) => Transition::to(
            S::Paused,
            with_close(vec![
                Effect::StopAll,
                Effect::SetTranscribing(false),
                Effect::SetListening(false),
            ]),
        ),

        // --- пауза: из любого активного состояния ---
        (S::Paused, E::Resume | E::TogglePause) => {
            Transition::to(S::Idle, vec![Effect::SetListening(true)])
        }
        (S::Paused, _) => Transition::stay(S::Paused),
        (_, E::Pause | E::TogglePause) => Transition::to(
            S::Paused,
            with_close(vec![
                Effect::SetTranscribing(false),
                Effect::SetListening(false),
            ]),
        ),
        (_, E::Resume) => Transition::stay(state),

        // --- своя фраза (синтез речи): пока она звучит, микрофон заглушён,
        // поэтому ожидание ответа продлевается на её длительность ---
        (S::AskingPermission { until }, E::Spoke(d)) => {
            Transition::stay(S::AskingPermission { until: until + d })
        }
        (S::Listening { until }, E::Spoke(d)) => {
            Transition::stay(S::Listening { until: until + d })
        }
        (S::FollowUp { until }, E::Spoke(d)) => Transition::stay(S::FollowUp { until: until + d }),
        (_, E::Spoke(_)) => Transition::stay(state),
        // Человек ещё говорит — не обрывать фразу на середине по таймеру.
        (S::Listening { until }, E::UserSpeaking) => Transition::stay(S::Listening {
            until: until.max(now + SPEAKING_GRACE),
        }),
        (S::FollowUp { until }, E::UserSpeaking) => Transition::stay(S::FollowUp {
            until: until.max(now + SPEAKING_GRACE),
        }),
        (_, E::UserSpeaking) => Transition::stay(state),

        // --- вопрос-разрешение ---
        (S::Listening { .. } | S::FollowUp { .. }, E::NeedPermission(id)) => Transition::to(
            S::AskingPermission {
                until: now + timings.ask,
            },
            vec![Effect::PlayReply(Reply::Ask), Effect::AskUser(id)],
        ),
        (S::AskingPermission { .. }, E::PermissionGranted(id)) => {
            Transition::to(S::Executing, vec![Effect::CloseAsk, Effect::Execute(id)])
        }
        (S::AskingPermission { .. }, E::PermissionDenied) => Transition::to(
            S::FollowUp {
                until: now + timings.follow_up,
            },
            vec![Effect::CloseAsk, Effect::PlayReply(Reply::Denied)],
        ),
        // Тишина 10 секунд — не выполнять (раздел 2.1).
        (S::AskingPermission { until }, E::Tick) if now >= until => Transition::to(
            S::Idle,
            vec![Effect::CloseAsk, Effect::SetTranscribing(false)],
        ),
        // Пока ждём ответа, всё остальное не отвлекает.
        (S::AskingPermission { .. }, _) => Transition::stay(state),
        (_, E::NeedPermission(_) | E::PermissionGranted(_) | E::PermissionDenied) => {
            Transition::stay(state)
        }

        // --- слово активации ---
        (S::Idle | S::FollowUp { .. }, E::WakeWord) => {
            Transition::to(listening(), vec![Effect::PlayReply(Reply::Wake)])
        }
        // Повторное «Джарвис» во время ожидания команды: ответить снова и
        // ждать команду заново — человек проверяет, слышат ли его.
        (S::Listening { .. }, E::WakeWord) => {
            Transition::to(listening(), vec![Effect::PlayReply(Reply::Wake)])
        }
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
        ask: Duration::from_secs(10),
    };

    fn asking(t0: Instant) -> State {
        State::AskingPermission { until: t0 + T.ask }
    }

    #[test]
    fn command_without_permission_asks() {
        let t0 = now();
        for state in [
            State::Listening {
                until: t0 + T.listen,
            },
            State::FollowUp {
                until: t0 + T.follow_up,
            },
        ] {
            let tr = transition(state, Event::NeedPermission("yt".into()), t0, T);
            assert_eq!(tr.next, asking(t0), "{state:?}");
            assert_eq!(
                tr.effects,
                [Effect::PlayReply(Reply::Ask), Effect::AskUser("yt".into())]
            );
        }
    }

    #[test]
    fn need_permission_ignored_outside_command_states() {
        let t0 = now();
        for state in [State::Idle, State::Executing, State::Paused, asking(t0)] {
            let tr = transition(state, Event::NeedPermission("yt".into()), t0, T);
            assert_eq!(tr, Transition::stay(state), "{state:?}");
        }
    }

    #[test]
    fn granted_permission_executes() {
        let t0 = now();
        let tr = transition(asking(t0), Event::PermissionGranted("yt".into()), t0, T);
        assert_eq!(tr.next, State::Executing);
        assert_eq!(tr.effects, [Effect::CloseAsk, Effect::Execute("yt".into())]);
    }

    #[test]
    fn denied_permission_goes_to_follow_up() {
        let t0 = now();
        let tr = transition(asking(t0), Event::PermissionDenied, t0, T);
        assert_eq!(
            tr.next,
            State::FollowUp {
                until: t0 + T.follow_up
            }
        );
        assert_eq!(
            tr.effects,
            [Effect::CloseAsk, Effect::PlayReply(Reply::Denied)]
        );
    }

    #[test]
    fn silence_means_no() {
        let t0 = now();
        assert_eq!(
            transition(asking(t0), Event::Tick, t0, T),
            Transition::stay(asking(t0))
        );
        let tr = transition(asking(t0), Event::Tick, t0 + T.ask, T);
        assert_eq!(tr.next, State::Idle);
        assert_eq!(
            tr.effects,
            [Effect::CloseAsk, Effect::SetTranscribing(false)]
        );
    }

    #[test]
    fn asking_ignores_other_events() {
        let t0 = now();
        for event in [
            Event::WakeWord,
            cmd("x"),
            Event::NotUnderstood,
            Event::ActionFinished { reply: None },
            Event::Resume,
        ] {
            assert_eq!(
                transition(asking(t0), event.clone(), t0, T),
                Transition::stay(asking(t0)),
                "{event:?}"
            );
        }
    }

    #[test]
    fn own_speech_extends_waiting() {
        let t0 = now();
        let d = Duration::from_secs(3);
        assert_eq!(
            transition(asking(t0), Event::Spoke(d), t0, T),
            Transition::stay(State::AskingPermission {
                until: t0 + T.ask + d
            })
        );
        let listening = State::Listening {
            until: t0 + T.listen,
        };
        assert_eq!(
            transition(listening, Event::Spoke(d), t0, T),
            Transition::stay(State::Listening {
                until: t0 + T.listen + d
            })
        );
        for state in [State::Idle, State::Executing, State::Paused] {
            assert_eq!(
                transition(state, Event::Spoke(d), t0, T),
                Transition::stay(state)
            );
        }
    }

    #[test]
    fn late_answers_are_ignored_elsewhere() {
        let t0 = now();
        for state in all_states(t0) {
            for event in [
                Event::PermissionGranted("x".into()),
                Event::PermissionDenied,
            ] {
                assert_eq!(
                    transition(state, event.clone(), t0, T),
                    Transition::stay(state),
                    "{state:?} {event:?}"
                );
            }
        }
    }

    #[test]
    fn pause_while_asking_closes_window() {
        let t0 = now();
        let tr = transition(asking(t0), Event::TogglePause, t0, T);
        assert_eq!(tr.next, State::Paused);
        assert_eq!(
            tr.effects,
            [
                Effect::CloseAsk,
                Effect::SetTranscribing(false),
                Effect::SetListening(false)
            ]
        );
    }

    #[test]
    fn emergency_stop_from_everywhere() {
        let t0 = now();
        for state in all_states(t0).into_iter().chain([asking(t0)]) {
            let tr = transition(state, Event::EmergencyStop, t0, T);
            assert_eq!(tr.next, State::Paused, "{state:?}");
            let mut expected = match state {
                State::Paused => vec![Effect::StopAll],
                _ => vec![
                    Effect::StopAll,
                    Effect::SetTranscribing(false),
                    Effect::SetListening(false),
                ],
            };
            if matches!(state, State::AskingPermission { .. }) {
                expected.insert(0, Effect::CloseAsk);
            }
            assert_eq!(tr.effects, expected, "{state:?}");
        }
    }

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
    fn repeated_wake_answers_again_and_restarts_waiting() {
        let t0 = now();
        let later = t0 + Duration::from_secs(2);
        let tr = transition(State::Listening { until: t0 }, Event::WakeWord, later, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: later + T.listen
            }
        );
        assert_eq!(tr.effects, [Effect::PlayReply(Reply::Wake)]);
    }

    #[test]
    fn speaking_keeps_waiting_open_until_phrase_ends() {
        let t0 = now();
        // Ожидание почти кончилось, а человек ещё говорит.
        let listening = State::Listening {
            until: t0 + Duration::from_millis(300),
        };
        let tr = transition(listening, Event::UserSpeaking, t0, T);
        assert_eq!(
            tr.next,
            State::Listening {
                until: t0 + SPEAKING_GRACE
            }
        );
        assert!(tr.effects.is_empty());
        // Запас больше оставшегося времени — ничего не укорачивается.
        let long = State::Listening {
            until: t0 + Duration::from_secs(5),
        };
        assert_eq!(
            transition(long, Event::UserSpeaking, t0, T),
            Transition::stay(long)
        );
        for state in [State::Idle, State::Executing, State::Paused] {
            assert_eq!(
                transition(state, Event::UserSpeaking, t0, T),
                Transition::stay(state)
            );
        }
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

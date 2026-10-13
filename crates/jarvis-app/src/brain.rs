//! «Мозг» в отдельном потоке `jarvis-llm`: держит разговор с нейросетью,
//! отправляет вопрос, режет ответ на предложения и отдаёт ядру — оно их
//! озвучивает по очереди — и сообщает, какие команды попросила модель
//! (выполняет их ядро, через разрешения).
//!
//! Ключ API берётся из хранилища ОС при первом вопросе: сохранили ключ —
//! он подхватится без перезапуска; сервер его не принял — ключ перечитается.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use jarvis_core::commands::{Command, Registry};
use jarvis_core::slots::{SlotSpec, SlotValues};
use jarvis_llm::conversation::{Conversation, ToolOutcome};
use jarvis_llm::sentences::SentenceSplitter;
use jarvis_llm::tools::{CommandRequest, ToolCommand, parse_call, tool_definition};
use jarvis_llm::{LanguageModel, LlmError, Request, persona};

use crate::resolver::spoken_phrase;

/// Что пошло не так — от этого зависит, что сказать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Ключ не сохранён: `jarvis --set-api-key`.
    NoKey,
    /// Сервер не принял ключ.
    BadKey,
    /// Нет интернета или сервер не ответил.
    Offline,
    /// Сервер перегружен / лимит.
    Busy,
    /// Модель отказалась отвечать по своим правилам.
    Refused,
    Other(String),
}

pub enum BrainEvent {
    /// Готовое предложение — озвучить.
    Sentence {
        generation: u64,
        text: String,
    },
    /// Весь текст ответа на сейчас — для окна.
    Text {
        generation: u64,
        text: String,
    },
    /// Ответ закончен; команды, которые попросила модель.
    Done {
        generation: u64,
        commands: Vec<CommandRequest>,
    },
    Failed {
        generation: u64,
        failure: Failure,
    },
}

pub type BrainSink = Box<dyn Fn(BrainEvent) + Send>;
pub type ModelFactory = Box<dyn Fn() -> Result<Box<dyn LanguageModel>, LlmError> + Send>;

enum BrainRequest {
    Ask { generation: u64, question: String },
    Outcome(ToolOutcome),
    Commands(Vec<ToolCommand>),
    Forget,
}

/// Ручка потока. При удалении поток завершается.
pub struct Brain {
    requests: Sender<BrainRequest>,
    cancel: Arc<AtomicBool>,
}

impl Brain {
    pub fn spawn(
        factory: ModelFactory,
        commands: Vec<ToolCommand>,
        memory: Duration,
        events: BrainSink,
    ) -> std::io::Result<Self> {
        let (requests, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("jarvis-llm".into())
            .spawn(move || worker(factory, commands, memory, events, rx, &flag))?;
        Ok(Self { requests, cancel })
    }

    pub fn ask(&self, generation: u64, question: String) {
        let _ = self.requests.send(BrainRequest::Ask {
            generation,
            question,
        });
    }

    /// Прервать текущий ответ (аварийная остановка).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn outcome(&self, outcome: ToolOutcome) {
        let _ = self.requests.send(BrainRequest::Outcome(outcome));
    }

    /// Команды перезагружены — новый список инструментов, разговор заново.
    pub fn set_commands(&self, commands: Vec<ToolCommand>) {
        let _ = self.requests.send(BrainRequest::Commands(commands));
    }

    /// Забыть разговор.
    pub fn forget(&self) {
        let _ = self.requests.send(BrainRequest::Forget);
    }
}

fn worker(
    factory: ModelFactory,
    mut commands: Vec<ToolCommand>,
    memory: Duration,
    events: BrainSink,
    rx: Receiver<BrainRequest>,
    cancel: &AtomicBool,
) {
    let mut model: Option<Box<dyn LanguageModel>> = None;
    let mut conversation = Conversation::new(memory);
    let mut tools = vec![tool_definition(&commands)];
    // Подсказка фиксируется на весь разговор (дата — при его начале).
    let mut system = String::new();
    while let Ok(request) = rx.recv() {
        match request {
            BrainRequest::Outcome(outcome) => conversation.record_outcome(outcome),
            BrainRequest::Commands(list) => {
                commands = list;
                tools = vec![tool_definition(&commands)];
                conversation.reset();
            }
            BrainRequest::Forget => conversation.reset(),
            BrainRequest::Ask {
                generation,
                question,
            } => {
                cancel.store(false, Ordering::Release);
                let now = Instant::now();
                if conversation.expired(now) {
                    conversation.reset();
                }
                if conversation.is_new() {
                    system = persona(&today());
                }
                if model.is_none() {
                    match factory() {
                        Ok(m) => model = Some(m),
                        Err(e) => {
                            events(BrainEvent::Failed {
                                generation,
                                failure: failure(&e),
                            });
                            continue;
                        }
                    }
                }
                let Some(engine) = model.as_ref() else {
                    continue;
                };
                let user = conversation.user_message(&question);
                let messages = conversation.request_messages(&user);
                let mut splitter = SentenceSplitter::default();
                let mut text = String::new();
                let started = Instant::now();
                let mut first_piece: Option<Duration> = None;
                let result = engine.respond(
                    &Request {
                        system: &system,
                        tools: &tools,
                        messages: &messages,
                    },
                    cancel,
                    &mut |piece| {
                        first_piece.get_or_insert_with(|| started.elapsed());
                        text.push_str(piece);
                        events(BrainEvent::Text {
                            generation,
                            text: text.clone(),
                        });
                        for sentence in splitter.push(piece) {
                            events(BrainEvent::Sentence {
                                generation,
                                text: sentence,
                            });
                        }
                    },
                );
                match result {
                    Ok(turn) if turn.refused() => {
                        tracing::info!(model = %turn.model, "нейросеть отказалась отвечать");
                        events(BrainEvent::Failed {
                            generation,
                            failure: Failure::Refused,
                        });
                    }
                    Ok(turn) => {
                        if let Some(rest) = splitter.finish() {
                            events(BrainEvent::Sentence {
                                generation,
                                text: rest,
                            });
                        }
                        let mut requested = Vec::new();
                        let mut rejected = Vec::new();
                        for call in &turn.tool_calls {
                            match parse_call(call, &commands) {
                                Ok(request) => requested.push(request),
                                Err(why) => {
                                    tracing::warn!(%why, "вызов команды отклонён");
                                    rejected.push(ToolOutcome {
                                        tool_use_id: call.id.clone(),
                                        text: format!("Отклонено: {why}."),
                                        is_error: true,
                                    });
                                }
                            }
                        }
                        tracing::info!(
                            model = %turn.model,
                            first_text_ms = first_piece.map(|d| d.as_millis() as u64),
                            total_ms = started.elapsed().as_millis() as u64,
                            commands = requested.len(),
                            stop = %turn.stop_reason,
                            "ответ нейросети"
                        );
                        // Сначала ход в историю (он открывает ожидание результатов),
                        // затем — результаты отклонённых вызовов.
                        conversation.commit(user, &turn, Instant::now());
                        for outcome in rejected {
                            conversation.record_outcome(outcome);
                        }
                        events(BrainEvent::Done {
                            generation,
                            commands: requested,
                        });
                    }
                    Err(LlmError::Cancelled) => {
                        tracing::info!("ответ нейросети прерван");
                    }
                    Err(e) => {
                        tracing::error!(error = %jarvis_core::error_chain(&e), "нейросеть не ответила");
                        if matches!(e, LlmError::BadKey { .. }) {
                            model = None; // ключ перечитается при следующем вопросе
                        }
                        events(BrainEvent::Failed {
                            generation,
                            failure: failure(&e),
                        });
                    }
                }
            }
        }
    }
}

fn failure(e: &LlmError) -> Failure {
    match e {
        LlmError::NoKey => Failure::NoKey,
        LlmError::BadKey { .. } => Failure::BadKey,
        LlmError::Busy => Failure::Busy,
        e if e.is_offline() => Failure::Offline,
        e => Failure::Other(jarvis_core::error_chain(e)),
    }
}

/// Команды для модели: все из пакетов (встроенные — нет), с одним
/// параметром или без. Команды с несколькими параметрами модели не даются.
pub fn tool_commands(registry: &Registry) -> Vec<ToolCommand> {
    registry
        .commands
        .iter()
        .filter(|c| c.slots.len() <= 1)
        .map(|c| ToolCommand {
            id: c.id.clone(),
            description: c.description.clone(),
            example: spoken_phrase(registry, &c.id),
            param: c.slots.values().next().map(|spec| match spec {
                SlotSpec::Number { min, max, .. } => format!("число от {min} до {max}"),
                SlotSpec::Choice { values } => format!("одно из: {}", values.spoken().join(", ")),
            }),
        })
        .collect()
}

/// Параметр от модели → значения слотов команды, проверенные по манифесту
/// (как у голосовой команды: число в диапазоне, значение из списка).
pub fn slot_values(command: &Command, value: Option<&str>) -> Result<SlotValues, String> {
    let mut slots = SlotValues::new();
    match (command.slots.iter().next(), value) {
        (None, _) => {}
        (Some((name, _)), None) => return Err(format!("не указан параметр {name}")),
        (Some((name, spec)), Some(raw)) => {
            let accepted = spec
                .accept(raw.trim())
                .ok_or_else(|| format!("недопустимое значение «{raw}»"))?;
            slots.insert(name.clone(), accepted);
        }
    }
    Ok(slots)
}

/// «13 октября 2026 года» — по местному времени.
pub fn today() -> String {
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    let stamp = jarvis_core::journal::now();
    let mut parts = stamp.get(..10).unwrap_or_default().split('-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return stamp;
    };
    let month = m
        .parse::<usize>()
        .ok()
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
        .copied()
        .unwrap_or(m);
    format!("{} {month} {y} года", d.trim_start_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_llm::{AssistantTurn, ToolCall};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    /// Поддельная модель: отдаёт заранее заданные ходы и запоминает запросы.
    struct Scripted {
        turns: Mutex<Vec<Result<AssistantTurn, LlmError>>>,
        seen: Seen,
    }

    impl LanguageModel for Scripted {
        fn respond(
            &self,
            request: &Request<'_>,
            _cancel: &AtomicBool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<AssistantTurn, LlmError> {
            self.seen.lock().unwrap().push(request.messages.to_vec());
            let turn = self.turns.lock().unwrap().remove(0)?;
            // Текст — кусками, как в потоке.
            for piece in turn.text.split_inclusive(' ') {
                on_text(piece);
            }
            Ok(turn)
        }
    }

    fn turn(text: &str, calls: &[(&str, Value)]) -> AssistantTurn {
        let mut blocks = vec![json!({"type": "text", "text": text})];
        blocks.extend(calls.iter().map(|(id, input)| {
            json!({"type": "tool_use", "id": id, "name": "run_command", "input": input})
        }));
        AssistantTurn {
            blocks,
            text: text.into(),
            tool_calls: calls
                .iter()
                .map(|(id, input)| ToolCall {
                    id: (*id).into(),
                    name: "run_command".into(),
                    input: Some(input.clone()),
                })
                .collect(),
            stop_reason: if calls.is_empty() {
                "end_turn"
            } else {
                "tool_use"
            }
            .into(),
            model: "fake".into(),
        }
    }

    fn commands() -> Vec<ToolCommand> {
        vec![ToolCommand {
            id: "open_youtube".into(),
            description: "откроет YouTube".into(),
            example: "открой ютуб".into(),
            param: None,
        }]
    }

    /// Какие сообщения модель получила в каждом запросе.
    type Seen = Arc<Mutex<Vec<Vec<Value>>>>;

    fn spawn(turns: Vec<Result<AssistantTurn, LlmError>>) -> (Brain, Receiver<BrainEvent>, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let model_seen = Arc::clone(&seen);
        let script = Mutex::new(Some(turns));
        let factory: ModelFactory = Box::new(move || {
            let turns = script.lock().unwrap().take().unwrap_or_default();
            Ok(Box::new(Scripted {
                turns: Mutex::new(turns),
                seen: Arc::clone(&model_seen),
            }) as Box<dyn LanguageModel>)
        });
        let (tx, rx) = mpsc::channel();
        let brain = Brain::spawn(
            factory,
            commands(),
            Duration::from_secs(300),
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .unwrap();
        (brain, rx, seen)
    }

    /// Собрать события до конца ответа.
    fn collect(rx: &Receiver<BrainEvent>) -> (Vec<String>, Result<Vec<CommandRequest>, Failure>) {
        let mut sentences = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                BrainEvent::Sentence { text, .. } => sentences.push(text),
                BrainEvent::Text { .. } => {}
                BrainEvent::Done { commands, .. } => return (sentences, Ok(commands)),
                BrainEvent::Failed { failure, .. } => return (sentences, Err(failure)),
            }
        }
    }

    #[test]
    fn answer_is_spoken_by_sentences_and_commands_returned() {
        let (brain, rx, seen) = spawn(vec![
            Ok(turn(
                "Тони Старк — изобретатель, сэр. Открываю ютуб для вас.",
                &[("t1", json!({"command": "open_youtube"}))],
            )),
            Ok(turn("Пожалуйста, сэр.", &[])),
        ]);
        brain.ask(1, "кто такой Тони Старк? и открой ютуб".into());
        let (sentences, commands) = collect(&rx);
        assert_eq!(
            sentences,
            ["Тони Старк — изобретатель, сэр.", "Открываю ютуб для вас."]
        );
        let commands = commands.unwrap();
        assert_eq!(commands[0].command, "open_youtube");

        // Результат команды уходит в начале следующего вопроса.
        brain.outcome(ToolOutcome {
            tool_use_id: "t1".into(),
            text: "Выполнено.".into(),
            is_error: false,
        });
        brain.ask(2, "спасибо".into());
        let _ = collect(&rx);
        let second = &seen.lock().unwrap()[1];
        assert_eq!(second.len(), 3, "вопрос, ответ, новый вопрос");
        assert_eq!(second[2]["content"][0]["type"], "tool_result");
        assert_eq!(second[2]["content"][0]["is_error"], false);
    }

    #[test]
    fn foreign_command_is_rejected_not_executed() {
        let (brain, rx, _) = spawn(vec![Ok(turn(
            "Хорошо.",
            &[("t1", json!({"command": "format_c"}))],
        ))]);
        brain.ask(1, "отформатируй диск".into());
        let (_, commands) = collect(&rx);
        assert!(commands.unwrap().is_empty());
    }

    #[test]
    fn failures_are_reported() {
        let (brain, rx, _) = spawn(vec![
            Err(LlmError::Timeout),
            Ok(AssistantTurn {
                stop_reason: "refusal".into(),
                ..AssistantTurn::default()
            }),
        ]);
        brain.ask(1, "вопрос".into());
        assert_eq!(collect(&rx).1.unwrap_err(), Failure::Offline);
        brain.ask(2, "другой вопрос".into());
        assert_eq!(collect(&rx).1.unwrap_err(), Failure::Refused);
    }

    #[test]
    fn missing_key_is_reported() {
        let (tx, rx) = mpsc::channel();
        let brain = Brain::spawn(
            Box::new(|| Err(LlmError::NoKey)),
            commands(),
            Duration::from_secs(300),
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .unwrap();
        brain.ask(1, "вопрос".into());
        assert_eq!(collect(&rx).1.unwrap_err(), Failure::NoKey);
    }

    #[test]
    fn shipped_commands_become_tools_with_checked_params() {
        let registry = Registry::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../commands"),
        );
        let tools = tool_commands(&registry);
        let volume = tools.iter().find(|t| t.id == "media_volume_set").unwrap();
        assert_eq!(volume.param.as_deref(), Some("число от 0 до 100"));
        assert_eq!(volume.example, "громкость на");
        assert!(!tools.iter().any(|t| t.id.starts_with("builtin:")));

        let command = registry.get("media_volume_set").unwrap();
        assert_eq!(
            slot_values(command, Some("40"))
                .unwrap()
                .get("level")
                .map(String::as_str),
            Some("40")
        );
        // clamp у громкости: 150 → 100.
        assert_eq!(
            slot_values(command, Some("150"))
                .unwrap()
                .get("level")
                .map(String::as_str),
            Some("100")
        );
        assert!(slot_values(command, Some("громко")).is_err());
        assert!(slot_values(command, None).is_err());
        let youtube = registry.get("open_youtube").unwrap();
        assert!(slot_values(youtube, None).unwrap().is_empty());
    }

    #[test]
    fn today_is_russian_date() {
        let t = today();
        assert!(t.ends_with(" года"), "{t}");
        assert!(t.split(' ').count() == 4, "{t}");
    }
}

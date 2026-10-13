//! История разговора — только дописывается (так требует API: блоки раздумий
//! действительны, пока всё до них не менялось). Вопрос попадает в историю
//! вместе с ответом, после успешного ответа: сбой связи не оставляет вопрос
//! без пары. Новый разговор — после тишины или длинной беседы: начало
//! разговора не обрезается, а начинается заново.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::turn::AssistantTurn;

/// Больше обменов — начать заново (растёт стоимость каждого запроса).
pub const MAX_TURNS: usize = 20;

/// Результат команды, которую попросила модель.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub tool_use_id: String,
    pub text: String,
    pub is_error: bool,
}

#[derive(Debug)]
pub struct Conversation {
    messages: Vec<Value>,
    turns: usize,
    last_activity: Option<Instant>,
    memory: Duration,
    /// Вызовы инструментов из последнего ответа, ждущие результата.
    open_calls: Vec<String>,
    outcomes: Vec<ToolOutcome>,
    /// Когда начат разговор (для даты в подсказке).
    started: Option<Instant>,
}

impl Conversation {
    pub fn new(memory: Duration) -> Self {
        Self {
            messages: Vec::new(),
            turns: 0,
            last_activity: None,
            memory,
            open_calls: Vec::new(),
            outcomes: Vec::new(),
            started: None,
        }
    }

    /// Начать ли новый разговор перед вопросом в момент `now`.
    pub fn expired(&self, now: Instant) -> bool {
        self.turns >= MAX_TURNS
            || self
                .last_activity
                .is_some_and(|t| now.duration_since(t) > self.memory)
    }

    /// Начать заново (тишина, смена команд, аварийная остановка).
    pub fn reset(&mut self) {
        self.messages.clear();
        self.turns = 0;
        self.open_calls.clear();
        self.outcomes.clear();
        self.started = None;
    }

    pub fn is_new(&self) -> bool {
        self.messages.is_empty()
    }

    /// Сообщение пользователя с вопросом: впереди — результаты команд,
    /// которые модель просила в прошлый раз (без них API не примет вопрос).
    pub fn user_message(&self, question: &str) -> Value {
        let mut content: Vec<Value> = self
            .open_calls
            .iter()
            .map(|id| {
                let outcome = self.outcomes.iter().find(|o| &o.tool_use_id == id);
                json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": outcome.map_or("Команда не выполнялась.", |o| o.text.as_str()),
                    "is_error": outcome.is_none_or(|o| o.is_error),
                })
            })
            .collect();
        content.push(json!({"type": "text", "text": question}));
        json!({"role": "user", "content": content})
    }

    /// Сообщения для запроса: история и новый вопрос.
    pub fn request_messages(&self, user: &Value) -> Vec<Value> {
        let mut all = self.messages.clone();
        all.push(user.clone());
        all
    }

    /// Ответ получен — вопрос и ответ становятся историей.
    pub fn commit(&mut self, user: Value, turn: &AssistantTurn, now: Instant) {
        if turn.blocks.is_empty() {
            return; // пустой ответ (отказ до начала) — не в историю
        }
        if self.started.is_none() {
            self.started = Some(now);
        }
        self.messages.push(user);
        self.messages
            .push(json!({"role": "assistant", "content": turn.blocks}));
        self.turns += 1;
        self.last_activity = Some(now);
        self.open_calls = turn.tool_calls.iter().map(|c| c.id.clone()).collect();
        self.outcomes.clear();
    }

    /// Результат команды из последнего ответа.
    pub fn record_outcome(&mut self, outcome: ToolOutcome) {
        if self.open_calls.contains(&outcome.tool_use_id) {
            self.outcomes
                .retain(|o| o.tool_use_id != outcome.tool_use_id);
            self.outcomes.push(outcome);
        }
    }

    pub fn messages(&self) -> &[Value] {
        &self.messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::ToolCall;

    fn turn(text: &str, calls: &[&str]) -> AssistantTurn {
        let mut blocks = vec![json!({"type": "text", "text": text})];
        blocks.extend(calls.iter().map(|id| {
            json!({"type": "tool_use", "id": id, "name": "run_command", "input": {"command": "x"}})
        }));
        AssistantTurn {
            blocks,
            text: text.into(),
            tool_calls: calls
                .iter()
                .map(|id| ToolCall {
                    id: (*id).into(),
                    name: "run_command".into(),
                    input: Some(json!({"command": "x"})),
                })
                .collect(),
            stop_reason: "end_turn".into(),
            model: "m".into(),
        }
    }

    #[test]
    fn history_is_append_only_and_committed_after_answer() {
        let t0 = Instant::now();
        let mut c = Conversation::new(Duration::from_secs(300));
        let q1 = c.user_message("кто такой Тони Старк?");
        assert_eq!(c.request_messages(&q1).len(), 1);
        assert!(c.is_new(), "вопрос без ответа — ещё не история");
        c.commit(q1, &turn("Изобретатель, сэр.", &[]), t0);
        let q2 = c.user_message("а подробнее?");
        let req = c.request_messages(&q2);
        assert_eq!(req.len(), 3);
        assert_eq!(req[1]["role"], "assistant");
        assert_eq!(req[2]["content"][0]["text"], "а подробнее?");
    }

    #[test]
    fn tool_results_come_first_in_next_question() {
        let t0 = Instant::now();
        let mut c = Conversation::new(Duration::from_secs(300));
        let q = c.user_message("сделай тише и открой ютуб");
        c.commit(q, &turn("Сделаю, сэр.", &["a", "b"]), t0);
        c.record_outcome(ToolOutcome {
            tool_use_id: "a".into(),
            text: "Выполнено.".into(),
            is_error: false,
        });
        c.record_outcome(ToolOutcome {
            tool_use_id: "чужой".into(),
            text: "x".into(),
            is_error: false,
        });
        let next = c.user_message("спасибо");
        let content = next["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["tool_use_id"], "a");
        assert_eq!(content[0]["is_error"], false);
        // Без результата — честно «не выполнялась».
        assert_eq!(content[1]["tool_use_id"], "b");
        assert_eq!(content[1]["is_error"], true);
        assert_eq!(content[2]["type"], "text");
    }

    #[test]
    fn silence_or_long_talk_starts_over() {
        let t0 = Instant::now();
        let mut c = Conversation::new(Duration::from_secs(300));
        assert!(!c.expired(t0));
        let q = c.user_message("вопрос");
        c.commit(q, &turn("ответ", &[]), t0);
        assert!(!c.expired(t0 + Duration::from_secs(299)));
        assert!(c.expired(t0 + Duration::from_secs(301)));
        for _ in 0..MAX_TURNS {
            let q = c.user_message("ещё");
            c.commit(q, &turn("да", &[]), t0);
        }
        assert!(c.expired(t0));
        c.reset();
        assert!(c.is_new());
    }

    #[test]
    fn empty_refusal_is_not_history() {
        let t0 = Instant::now();
        let mut c = Conversation::new(Duration::from_secs(300));
        let q = c.user_message("вопрос");
        let refused = AssistantTurn {
            stop_reason: "refusal".into(),
            ..AssistantTurn::default()
        };
        c.commit(q, &refused, t0);
        assert!(c.is_new());
    }
}

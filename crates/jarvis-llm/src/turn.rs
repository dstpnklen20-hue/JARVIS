//! Сборка ответа модели из событий потока: текст (сразу — для озвучки),
//! вызовы инструментов, блоки раздумий и всё, что нужно вернуть в историю.
//!
//! Правила истории (Claude Sonnet 5.5):
//! - блоки `thinking` возвращаются без изменений (с подписью);
//! - если в ответе есть блок `fallback` (запрос отказал и его продолжила
//!   другая модель), блоки раздумий и вызовов инструментов до последнего
//!   такого блока в историю не возвращаются, текст — возвращается.

use serde_json::{Map, Value, json};

use crate::LlmError;

/// Вызов инструмента моделью.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// `None` — модель прислала неразборчивые параметры.
    pub input: Option<Value>,
}

/// Готовый ход ассистента.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantTurn {
    /// Блоки для истории разговора — в том виде, как их нужно вернуть.
    pub blocks: Vec<Value>,
    /// Весь текст ответа (то, что озвучивается и показывается).
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: String,
    /// Модель, которая ответила (после запасного варианта — другая).
    pub model: String,
}

impl AssistantTurn {
    pub fn refused(&self) -> bool {
        self.stop_reason == "refusal"
    }
}

#[derive(Debug)]
struct Block {
    value: Value,
    /// Части JSON параметров инструмента, пока блок не закрыт.
    partial_json: String,
}

/// Собирает ход из событий потока.
#[derive(Debug, Default)]
pub struct TurnBuilder {
    blocks: Vec<Block>,
    text: String,
    stop_reason: String,
    model: String,
}

impl TurnBuilder {
    /// Принимает событие. Возвращает новый кусок текста ответа, если он есть.
    pub fn accept(&mut self, event: &str, data: &str) -> Result<Option<String>, LlmError> {
        if event == "ping" || data.is_empty() {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(data).map_err(|e| LlmError::Protocol {
            what: format!("событие {event}: {e}"),
        })?;
        match v["type"].as_str().unwrap_or(event) {
            "message_start" => {
                self.model = v["message"]["model"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
            }
            "content_block_start" => {
                let index = index(&v)?;
                let mut block = v["content_block"].clone();
                // Параметры инструмента придут по частям.
                if block["type"] == "tool_use" {
                    block["input"] = json!({});
                }
                while self.blocks.len() <= index {
                    self.blocks.push(Block {
                        value: Value::Null,
                        partial_json: String::new(),
                    });
                }
                self.blocks[index] = Block {
                    value: block,
                    partial_json: String::new(),
                };
            }
            "content_block_delta" => {
                let index = index(&v)?;
                let Some(block) = self.blocks.get_mut(index) else {
                    return Err(LlmError::Protocol {
                        what: format!("часть блока {index} без начала"),
                    });
                };
                let delta = &v["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let piece = delta["text"].as_str().unwrap_or_default();
                        append(&mut block.value, "text", piece);
                        self.text.push_str(piece);
                        return Ok((!piece.is_empty()).then(|| piece.to_owned()));
                    }
                    "input_json_delta" => {
                        block
                            .partial_json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    "thinking_delta" => {
                        append(
                            &mut block.value,
                            "thinking",
                            delta["thinking"].as_str().unwrap_or_default(),
                        );
                    }
                    "signature_delta" => {
                        block.value["signature"] = delta["signature"].clone();
                    }
                    _ => {} // цитаты и прочее — для голоса не нужны
                }
            }
            "content_block_stop" => {
                let index = index(&v)?;
                if let Some(block) = self.blocks.get_mut(index)
                    && block.value["type"] == "tool_use"
                {
                    let raw = block.partial_json.trim();
                    block.value["input"] = if raw.is_empty() {
                        json!({})
                    } else {
                        // Неразборчивые параметры (обрыв на max_tokens) — пометка,
                        // а не паника: вызов отклонится при проверке.
                        serde_json::from_str(raw).unwrap_or(Value::Null)
                    };
                }
            }
            "message_delta" => {
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = reason.to_owned();
                }
            }
            "message_stop" => {}
            "error" => {
                return Err(LlmError::Api {
                    status: 0,
                    kind: v["error"]["type"].as_str().unwrap_or_default().to_owned(),
                    message: v["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            _ => {}
        }
        Ok(None)
    }

    pub fn finish(self) -> AssistantTurn {
        let blocks: Vec<Value> = self
            .blocks
            .into_iter()
            .map(|b| b.value)
            .filter(|v| !v.is_null())
            .collect();
        let last_fallback = blocks.iter().rposition(|b| b["type"] == "fallback");
        let history: Vec<Value> = blocks
            .iter()
            .enumerate()
            .filter(|(i, b)| match last_fallback {
                Some(fb) if *i < fb => !matches!(
                    b["type"].as_str(),
                    Some("thinking" | "redacted_thinking" | "tool_use" | "server_tool_use")
                ),
                _ => true,
            })
            .map(|(_, b)| b.clone())
            .collect();
        let tool_calls = blocks
            .iter()
            .enumerate()
            .filter(|(i, b)| b["type"] == "tool_use" && last_fallback.is_none_or(|fb| *i > fb))
            .map(|(_, b)| ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_owned(),
                name: b["name"].as_str().unwrap_or_default().to_owned(),
                input: (!b["input"].is_null()).then(|| b["input"].clone()),
            })
            .collect();
        AssistantTurn {
            blocks: history,
            text: self.text,
            tool_calls,
            stop_reason: self.stop_reason,
            model: self.model,
        }
    }
}

fn index(v: &Value) -> Result<usize, LlmError> {
    v["index"]
        .as_u64()
        .map(|i| i as usize)
        .ok_or_else(|| LlmError::Protocol {
            what: "событие без index".into(),
        })
}

fn append(block: &mut Value, field: &str, piece: &str) {
    let mut current = block[field].as_str().unwrap_or_default().to_owned();
    current.push_str(piece);
    if let Value::Object(map) = block {
        map.insert(field.to_owned(), Value::String(current));
    } else {
        let mut map = Map::new();
        map.insert(field.to_owned(), Value::String(current));
        *block = Value::Object(map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseReader;

    fn run(raw: &str) -> (Vec<String>, AssistantTurn) {
        let mut reader = SseReader::new(raw.as_bytes());
        let mut builder = TurnBuilder::default();
        let mut pieces = Vec::new();
        while let Some(e) = reader.next_event().unwrap() {
            if let Some(p) = builder.accept(&e.event, &e.data).unwrap() {
                pieces.push(p);
            }
        }
        (pieces, builder.finish())
    }

    #[test]
    fn text_streams_and_thinking_is_kept_for_history() {
        let (pieces, turn) = run(include_str!("../tests/fixtures/text_answer.sse"));
        assert_eq!(pieces.concat(), turn.text);
        assert!(turn.text.starts_with("Тони Старк"), "{}", turn.text);
        assert_eq!(turn.stop_reason, "end_turn");
        assert_eq!(turn.model, "claude-sonnet-5-5");
        assert_eq!(turn.blocks[0]["type"], "thinking");
        assert_eq!(turn.blocks[0]["signature"], "sig-123");
        assert_eq!(turn.blocks[1]["type"], "text");
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn tool_call_input_arrives_in_parts() {
        let (_, turn) = run(include_str!("../tests/fixtures/tool_call.sse"));
        assert_eq!(turn.stop_reason, "tool_use");
        assert_eq!(
            turn.tool_calls,
            [ToolCall {
                id: "toolu_1".into(),
                name: "run_command".into(),
                input: Some(json!({"command": "media_volume_set", "value": "30"})),
            }]
        );
        assert_eq!(turn.blocks[1]["input"]["command"], "media_volume_set");
    }

    #[test]
    fn blocks_before_fallback_are_not_echoed() {
        let (_, turn) = run(include_str!("../tests/fixtures/fallback.sse"));
        let types: Vec<&str> = turn
            .blocks
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect();
        // Раздумья отказавшей модели — нет; текст и всё после `fallback` — есть.
        assert_eq!(types, ["text", "fallback", "text"]);
        assert_eq!(turn.model, "claude-sonnet-5");
    }

    #[test]
    fn stream_error_event_is_an_error() {
        let mut b = TurnBuilder::default();
        let err = b
            .accept(
                "error",
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            )
            .unwrap_err();
        assert!(matches!(err, LlmError::Api { ref kind, .. } if kind == "overloaded_error"));
    }

    #[test]
    fn refusal_is_reported() {
        let mut b = TurnBuilder::default();
        b.accept(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#,
        )
        .unwrap();
        assert!(b.finish().refused());
    }
}

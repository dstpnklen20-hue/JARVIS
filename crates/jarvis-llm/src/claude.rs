//! Claude Messages API: потоковый запрос через `reqwest` (блокирующий — у
//! нас свой поток), разбор SSE и сборка хода.
//!
//! - `fallbacks: "default"` (beta `server-side-fallback-2026-07-01`): если
//!   модель откажет по своим правилам, сервер сам продолжит на другой модели.
//! - Подсказка и инструменты помечены для кэша — повторные вопросы дешевле.

use std::io::BufReader;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::sse::SseReader;
use crate::turn::{AssistantTurn, TurnBuilder};
use crate::{LanguageModel, LlmError, Request};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Сколько ждать соединения (нет интернета — понятное сообщение быстро).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Предел на весь ответ: голосовой ответ короткий.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(90);

/// Настройки запроса.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSettings {
    pub model: String,
    /// `low`, `medium`, `high`.
    pub effort: String,
    pub max_tokens: u32,
    /// `adaptive` (по умолчанию) или `between_tools` (без раздумий, Sonnet 5.5).
    pub thinking: String,
    pub fallback: bool,
}

pub struct ClaudeClient {
    http: reqwest::blocking::Client,
    api_key: String,
    settings: ClaudeSettings,
    url: String,
}

impl ClaudeClient {
    pub fn new(api_key: String, settings: ClaudeSettings) -> Result<Self, LlmError> {
        Self::with_url(api_key, settings, API_URL.to_owned())
    }

    /// Другой адрес — для тестов с локальным сервером.
    pub fn with_url(
        api_key: String,
        settings: ClaudeSettings,
        url: String,
    ) -> Result<Self, LlmError> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .build()
            .map_err(LlmError::Client)?;
        Ok(Self {
            http,
            api_key,
            settings,
            url,
        })
    }

    pub fn body(&self, request: &Request<'_>) -> Value {
        request_body(&self.settings, request)
    }
}

/// Тело запроса (отдельно — для тестов).
pub fn request_body(settings: &ClaudeSettings, request: &Request<'_>) -> Value {
    let mut body = json!({
        "model": settings.model,
        "max_tokens": settings.max_tokens,
        "stream": true,
        // Порядок для кэша: инструменты → подсказка → сообщения; метка на
        // подсказке кэширует и инструменты.
        "system": [{"type": "text", "text": request.system, "cache_control": {"type": "ephemeral"}}],
        "messages": request.messages,
        "output_config": {"effort": settings.effort},
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.to_vec());
    }
    if settings.thinking == "between_tools" {
        body["thinking"] = json!({"type": "between_tools"});
    }
    if settings.fallback {
        body["fallbacks"] = json!("default");
    }
    body
}

impl LanguageModel for ClaudeClient {
    fn respond(
        &self,
        request: &Request<'_>,
        cancel: &AtomicBool,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<AssistantTurn, LlmError> {
        let mut call = self
            .http
            .post(&self.url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json");
        if self.settings.fallback {
            call = call.header("anthropic-beta", FALLBACK_BETA);
        }
        let response = call
            .body(self.body(request).to_string())
            .send()
            .map_err(classify)?;
        let status = response.status().as_u16();
        if status != 200 {
            let text = response.text().unwrap_or_default();
            return Err(api_error(status, &text));
        }
        let mut reader = SseReader::new(BufReader::new(response));
        let mut builder = TurnBuilder::default();
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(LlmError::Cancelled);
            }
            let Some(event) = reader.next_event().map_err(LlmError::Read)? else {
                break;
            };
            if let Some(piece) = builder.accept(&event.event, &event.data)? {
                on_text(&piece);
            }
        }
        Ok(builder.finish())
    }
}

/// Ошибка соединения: нет сети / тайм-аут / прочее.
fn classify(e: reqwest::Error) -> LlmError {
    if e.is_timeout() {
        LlmError::Timeout
    } else if e.is_connect() {
        LlmError::Offline(e)
    } else {
        LlmError::Client(e)
    }
}

/// Ответ сервера с ошибкой: `{"type":"error","error":{"type":..,"message":..}}`.
pub fn api_error(status: u16, body: &str) -> LlmError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = v["error"]["type"].as_str().unwrap_or_default().to_owned();
    let message = v["error"]["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status {
        401 | 403 => LlmError::BadKey,
        429 | 529 => LlmError::Busy,
        _ if kind == "overloaded_error" => LlmError::Busy,
        _ => LlmError::Api {
            status,
            kind,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ClaudeSettings {
        ClaudeSettings {
            model: "claude-sonnet-5-5".into(),
            effort: "low".into(),
            max_tokens: 1024,
            thinking: "adaptive".into(),
            fallback: true,
        }
    }

    #[test]
    fn body_has_model_stream_cache_and_fallback() {
        let messages = vec![json!({"role": "user", "content": "привет"})];
        let tools = vec![json!({"name": "run_command"})];
        let body = request_body(
            &settings(),
            &Request {
                system: "Ты — Джарвис.",
                tools: &tools,
                messages: &messages,
            },
        );
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["tools"][0]["name"], "run_command");
        // Адаптивные раздумья — по умолчанию, поле не шлём.
        assert!(body.get("thinking").is_none());
        let off = request_body(
            &ClaudeSettings {
                thinking: "between_tools".into(),
                fallback: false,
                ..settings()
            },
            &Request {
                system: "x",
                tools: &[],
                messages: &messages,
            },
        );
        assert_eq!(off["thinking"]["type"], "between_tools");
        assert!(off.get("fallbacks").is_none());
        assert!(off.get("tools").is_none());
    }

    #[test]
    fn api_errors_are_classified() {
        assert!(matches!(api_error(401, "{}"), LlmError::BadKey));
        assert!(matches!(api_error(529, "{}"), LlmError::Busy));
        let e = api_error(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad field"}}"#,
        );
        assert!(
            matches!(e, LlmError::Api { status: 400, ref kind, ref message } if kind == "invalid_request_error" && message == "bad field")
        );
    }
}

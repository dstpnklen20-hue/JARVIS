#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты: падение = провал теста

//! Весь HTTP-путь клиента Claude на локальном поддельном сервере: заголовки,
//! потоковое чтение, ошибки ключа и отсутствие связи.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use jarvis_llm::claude::{ClaudeClient, ClaudeSettings};
use jarvis_llm::{LanguageModel, LlmError, Request};
use serde_json::json;

fn settings() -> ClaudeSettings {
    ClaudeSettings {
        model: "claude-sonnet-5-5".into(),
        effort: "low".into(),
        max_tokens: 512,
        thinking: "adaptive".into(),
        fallback: true,
        bearer: false,
    }
}

/// Сервер на один запрос: отдаёт `status` и `body`, присылает текст запроса.
fn serve_once(
    status: &str,
    content_type: &str,
    body: &'static str,
) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let status = status.to_owned();
    let content_type = content_type.to_owned();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 8192];
        // Заголовки и тело (Content-Length).
        loop {
            let n = stream.read(&mut buf).unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= head_end + 4 + length {
                    break;
                }
            }
        }
        tx.send(String::from_utf8_lossy(&request).to_string())
            .unwrap();
        // Ответ частями — как настоящий поток.
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).unwrap();
        for chunk in body.as_bytes().chunks(97) {
            stream.write_all(chunk).unwrap();
            stream.flush().unwrap();
        }
    });
    (url, rx)
}

#[test]
fn streams_answer_over_http_with_headers() {
    let (url, request) = serve_once(
        "200 OK",
        "text/event-stream",
        include_str!("fixtures/text_answer.sse"),
    );
    let client = ClaudeClient::with_url("sk-ant-test-key".into(), settings(), url).unwrap();
    let messages = vec![json!({"role": "user", "content": "кто такой Тони Старк?"})];
    let mut pieces = Vec::new();
    let turn = client
        .respond(
            &Request {
                system: "Ты — Джарвис.",
                tools: &[],
                messages: &messages,
            },
            &AtomicBool::new(false),
            &mut |p| pieces.push(p.to_owned()),
        )
        .unwrap();
    assert_eq!(pieces.concat(), turn.text);
    assert!(turn.text.contains("Железный человек"));
    let sent = request.recv().unwrap();
    let lower = sent.to_ascii_lowercase();
    assert!(lower.contains("x-api-key: sk-ant-test-key"), "{sent}");
    assert!(lower.contains("anthropic-version: 2023-06-01"));
    assert!(lower.contains("anthropic-beta: server-side-fallback-2026-07-01"));
    assert!(sent.contains("\"stream\":true"));
}

#[test]
fn server_key_goes_as_bearer_token() {
    let (url, request) = serve_once(
        "200 OK",
        "text/event-stream",
        include_str!("fixtures/text_answer.sse"),
    );
    let bearer = ClaudeSettings {
        bearer: true,
        ..settings()
    };
    let client = ClaudeClient::with_url("sk-cvc-test-key".into(), bearer, url).unwrap();
    client
        .respond(
            &Request {
                system: "x",
                tools: &[],
                messages: &[json!({"role": "user", "content": "привет"})],
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
    let lower = request.recv().unwrap().to_ascii_lowercase();
    assert!(
        lower.contains("authorization: bearer sk-cvc-test-key"),
        "{lower}"
    );
    assert!(!lower.contains("x-api-key"));
}

#[test]
fn rejected_key_is_reported() {
    // Получатель держим до конца: иначе сервер не сможет отчитаться и не ответит.
    let (url, _request) = serve_once(
        "401 Unauthorized",
        "application/json",
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    );
    let client = ClaudeClient::with_url("sk-ant-bad".into(), settings(), url).unwrap();
    let err = client
        .respond(
            &Request {
                system: "x",
                tools: &[],
                messages: &[json!({"role": "user", "content": "привет"})],
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        matches!(err, LlmError::BadKey { status: 401, ref message } if message == "invalid x-api-key"),
        "{err:?}"
    );
}

#[test]
fn closed_port_means_offline() {
    // Порт заняли и отпустили — соединение будет отклонено.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client = ClaudeClient::with_url(
        "sk-ant-x".into(),
        settings(),
        format!("http://127.0.0.1:{port}/v1/messages"),
    )
    .unwrap();
    let err = client
        .respond(
            &Request {
                system: "x",
                tools: &[],
                messages: &[json!({"role": "user", "content": "привет"})],
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(err.is_offline(), "{err:?}");
}

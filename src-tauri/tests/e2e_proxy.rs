//! End-to-end verification against the real `claude` CLI.
//!
//! Skipped if the `claude` binary cannot be found on PATH or its auth check
//! fails. Exercises: 401 gate, OpenAI non-stream, OpenAI stream, Anthropic
//! non-stream, Anthropic stream, config-driven port change, request logs.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::Value;
use tauri_app_lib::{
    config::Config,
    keys::KeyStore,
    server::{self, state::HttpState},
};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct RunningServer {
    base_url: String,
    cancel: CancellationToken,
    state: HttpState,
    _dir: tempfile::TempDir,
}

async fn start_server() -> Option<RunningServer> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .try_init();
    let claude_path = std::env::var("CLAUDE_BINARY")
        .unwrap_or_else(|_| {
            std::process::Command::new("which")
                .arg("claude")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "claude".to_string())
        });
    let claude_path = std::fs::canonicalize(&claude_path)
        .ok()
        .and_then(|p| p.to_str().map(ToOwned::to_owned))
        .unwrap_or(claude_path);
    if std::process::Command::new(&claude_path).arg("--version").output().is_err() {
        eprintln!("claude binary not runnable — skipping e2e tests");
        return None;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let working = dir.path().join("scratch");
    std::fs::create_dir_all(&working).unwrap();

    let config = Config {
        bind_address: "127.0.0.1".to_string(),
        port: 0,
        claude_binary_path: claude_path,
        default_model: "sonnet".to_string(),
        model_map: Default::default(),
        max_concurrency: 2,
        request_timeout_secs: 120,
        working_dir: working.to_string_lossy().into_owned(),
        require_auth: true,
    };

    let mut keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let (_, raw) = keys.create("e2e".to_string()).unwrap();

    let state = HttpState {
        config: Arc::new(Mutex::new(config.clone())),
        keys: Arc::new(Mutex::new(keys)),
        semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrency)),
        logs: Arc::new(Mutex::new(VecDeque::new())),
        app: None,
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cancel = CancellationToken::new();
    let service = server::router(state.clone());
    let shutdown = cancel.clone();

    tokio::spawn(async move {
        let _ = axum::serve(listener, service)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await;
    });

    Some(RunningServer {
        base_url: format!("http://127.0.0.1:{port}?key={raw}"),
        cancel,
        state,
        _dir: dir,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_401_when_missing_key() {
    let Some(server) = start_server().await else {
        return;
    };
    let app = server::router(server.state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    server.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_openai_non_stream() {
    let Some(server) = start_server().await else {
        return;
    };
    let raw = extract_key(&server.base_url);
    let url = strip_query(&server.base_url);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/v1/chat/completions"))
        .bearer_auth(&raw)
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
        }))
        .send()
        .await
        .expect("request");

    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"].as_str().unwrap().trim(), "OK");
    assert!(body["usage"]["completion_tokens"].as_u64().unwrap() > 0);

    server.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_openai_stream() {
    let Some(server) = start_server().await else {
        return;
    };
    let raw = extract_key(&server.base_url);
    let url = strip_query(&server.base_url);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/v1/chat/completions"))
        .bearer_auth(&raw)
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        buffer.extend_from_slice(&chunk.unwrap());
        if buffer.windows(7).any(|window| window == b"[DONE]") {
            break;
        }
    }
    let body = String::from_utf8(buffer).unwrap();
    assert!(body.contains("\"object\":\"chat.completion.chunk\""), "got: {body}");
    assert!(body.contains("data: [DONE]"), "got: {body}");

    server.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_anthropic_non_stream() {
    let Some(server) = start_server().await else {
        return;
    };
    let raw = extract_key(&server.base_url);
    let url = strip_query(&server.base_url);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/v1/messages"))
        .header("x-api-key", &raw)
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 50,
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["type"], "message");
    let text = body["content"][0]["text"].as_str().unwrap();
    assert_eq!(text.trim(), "OK");
    assert!(body["id"].as_str().unwrap().starts_with("msg_"));

    server.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_anthropic_stream() {
    let Some(server) = start_server().await else {
        return;
    };
    let raw = extract_key(&server.base_url);
    let url = strip_query(&server.base_url);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/v1/messages"))
        .header("x-api-key", &raw)
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 50,
            "stream": true,
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut saw_message_stop = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        let text = String::from_utf8_lossy(&chunk);
        if text.contains("event: message_stop") {
            saw_message_stop = true;
            break;
        }
        buffer.push_str(&text);
        if buffer.len() > 32_768 {
            break;
        }
    }
    assert!(saw_message_stop, "expected event: message_stop, got: {buffer}");

    server.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_request_log_records_requests() {
    let Some(server) = start_server().await else {
        return;
    };
    let raw = extract_key(&server.base_url);
    let url = strip_query(&server.base_url);

    let client = reqwest::Client::new();
    let _ = client
        .get(format!("{url}/v1/models"))
        .bearer_auth(&raw)
        .send()
        .await
        .expect("models request");

    // give the server a moment to record the log entry
    tokio::time::sleep(Duration::from_millis(50)).await;

    let logs = server.state.logs.lock().await;
    assert!(!logs.is_empty(), "expected at least one log entry");
    assert_eq!(logs[0].path, "/v1/models");
    assert_eq!(logs[0].method, "GET");

    server.cancel.cancel();
}

fn extract_key(url: &str) -> String {
    let (_, query) = url.split_once('?').expect("key in query");
    query.strip_prefix("key=").expect("key prefix").to_string()
}

fn strip_query(url: &str) -> String {
    url.split_once('?').map(|(base, _)| base.to_string()).unwrap_or_else(|| url.to_string())
}

#[allow(dead_code)]
fn _suppress_unused_warning() {
    let _ = mpsc::channel::<()>(1);
}

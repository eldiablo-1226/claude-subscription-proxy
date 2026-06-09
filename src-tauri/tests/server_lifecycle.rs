use std::{collections::VecDeque, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::Value;
use tauri_app_lib::{
    config::Config,
    keys::KeyStore,
    server::{self, state::{HttpState, RequestLogEntry, ServerRuntime}},
};
use tokio::sync::Mutex;
use tower::ServiceExt;

fn test_state(keys: KeyStore) -> HttpState {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());
    HttpState {
        config: Arc::new(config),
        keys: Arc::new(Mutex::new(keys)),
        runtime: ServerRuntime::new(4),
        rate_limit: Arc::new(Mutex::new(None)),
        logs: Arc::new(Mutex::new(VecDeque::new())),
        app: None,
    }
}

#[tokio::test]
async fn router_rejects_missing_key_with_openai_error_shape() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(KeyStore::load(dir.path().join("keys.json")).unwrap());
    let app = server::router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn router_accepts_bearer_key_for_models() {
    let dir = tempfile::tempdir().unwrap();
    let mut keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let (_, raw) = keys.create("client".to_string()).unwrap();
    let state = test_state(keys);
    let app = server::router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .method("GET")
                .header("authorization", format!("Bearer {raw}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["object"], "list");
    let logs = state.logs.lock().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].method, "GET");
    assert_eq!(logs[0].path, "/v1/models");
    assert_eq!(logs[0].status, 200);
}

#[tokio::test]
async fn openai_missing_model_returns_openai_shaped_400() {
    let dir = tempfile::tempdir().unwrap();
    let mut keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let (_, raw) = keys.create("client".to_string()).unwrap();
    let app = server::router(test_state(keys));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("authorization", format!("Bearer {raw}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hi"}]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn anthropic_missing_model_returns_anthropic_shaped_400() {
    let dir = tempfile::tempdir().unwrap();
    let mut keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let (_, raw) = keys.create("client".to_string()).unwrap();
    let app = server::router(test_state(keys));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/messages")
                .method("POST")
                .header("x-api-key", raw)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hi"}]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["type"], "error");
    assert_eq!(json["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn anthropic_bad_request_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    let mut keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let (_, raw) = keys.create("client".to_string()).unwrap();
    let state = test_state(keys);
    let app = server::router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/messages")
                .method("POST")
                .header("x-api-key", raw)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude-sonnet-4-5","messages":[{"role":"assistant","content":"prefill"}]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let logs = state.logs.lock().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].method, "POST");
    assert_eq!(logs[0].path, "/v1/messages");
    assert_eq!(logs[0].status, 400);
}

#[tokio::test]
async fn request_log_ring_buffer_keeps_last_500_entries() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(KeyStore::load(dir.path().join("keys.json")).unwrap());

    for i in 0..510 {
        state.record_log(RequestLogEntry {
            ts: i,
            method: "GET".to_string(),
            path: "/v1/models".to_string(),
            client_model: None,
            mapped_model: None,
            status: 200,
            duration_ms: 1,
            usage: None,
        }).await;
    }

    let logs = state.logs.lock().await;
    assert_eq!(logs.len(), 500);
    assert_eq!(logs.front().unwrap().ts, 10);
    assert_eq!(logs.back().unwrap().ts, 509);
}

use std::{convert::Infallible, time::{Instant, SystemTime, UNIX_EPOCH}};

use async_stream::stream;
use axum::{
    extract::State,
    http::StatusCode,
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Config;

use super::{
    claude::{self, ClaudeEvent, ClaudeRequest, CompletedTurn},
    state::{epoch_millis, HttpState, RequestLogEntry},
    translate,
};

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

pub async fn chat_completions(
    State(state): State<HttpState>,
    Json(request): Json<ChatCompletionsRequest>,
) -> Response {
    let started = Instant::now();
    let config = state.config.lock().await.clone();
    let flattened = match translate::flatten_openai_chat(&request.messages) {
        Ok(flattened) => flattened,
        Err(error) => {
            record_openai_log(
                &state,
                "/v1/chat/completions",
                Some(request.model.clone()),
                None,
                StatusCode::BAD_REQUEST,
                started,
                None,
            )
            .await;
            return openai_error_response(StatusCode::BAD_REQUEST, error.message);
        }
    };
    let mapped_model = resolve_model(&config, &request.model);
    let claude_request = ClaudeRequest {
        final_user_text: flattened.final_user_text,
        system_text: non_empty(flattened.system_text),
        history_stdin: flattened.history_stdin,
        mapped_model: mapped_model.clone(),
        stream: request.stream,
    };

    if request.stream {
        let include_usage = request
            .stream_options
            .as_ref()
            .map(|options| options.include_usage)
            .unwrap_or(false);
        match claude::stream(config, state.semaphore.clone(), claude_request).await {
            Ok(rx) => Sse::new(openai_sse_stream(
                rx,
                state,
                request.model,
                mapped_model,
                include_usage,
                started,
            ))
            .into_response(),
            Err(error) => {
                let status = error.status_code();
                record_openai_log(
                    &state,
                    "/v1/chat/completions",
                    Some(request.model),
                    Some(mapped_model),
                    status,
                    started,
                    None,
                )
                .await;
                openai_error_response(status, error.client_message())
            }
        }
    } else {
        match claude::collect(config, state.semaphore.clone(), claude_request).await {
            Ok(completed) => {
                let usage = Some(openai_usage(&completed.usage));
                let response = completion_response(&request.model, completed);
                record_openai_log(
                    &state,
                    "/v1/chat/completions",
                    Some(request.model),
                    Some(mapped_model),
                    StatusCode::OK,
                    started,
                    usage,
                )
                .await;
                Json(response).into_response()
            }
            Err(error) => {
                let status = error.status_code();
                record_openai_log(
                    &state,
                    "/v1/chat/completions",
                    Some(request.model),
                    Some(mapped_model),
                    status,
                    started,
                    None,
                )
                .await;
                openai_error_response(status, error.client_message())
            }
        }
    }
}

pub async fn models(State(state): State<HttpState>) -> Response {
    let started = Instant::now();
    let config = state.config.lock().await.clone();
    let response = Json(models_response(&config)).into_response();
    record_openai_log(
        &state,
        "/v1/models",
        None,
        None,
        StatusCode::OK,
        started,
        None,
    )
    .await;
    response
}

pub fn resolve_model(config: &Config, requested: &str) -> String {
    config
        .model_map
        .get(requested)
        .cloned()
        .or_else(|| requested.starts_with("claude").then(|| requested.to_string()))
        .unwrap_or_else(|| config.default_model.clone())
}

pub fn completion_response(requested_model: &str, completed: CompletedTurn) -> Value {
    json!({
        "id": format!("chatcmpl-{}", Uuid::new_v4()),
        "object": "chat.completion",
        "created": epoch_seconds(),
        "model": requested_model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": completed.text },
            "finish_reason": map_finish_reason(completed.stop_reason.as_deref())
        }],
        "usage": openai_usage(&completed.usage),
    })
}

pub fn models_response(config: &Config) -> Value {
    let mut ids = std::collections::BTreeSet::from([
        "sonnet".to_string(),
        "opus".to_string(),
        "haiku".to_string(),
    ]);
    ids.extend(config.model_map.keys().cloned());

    json!({
        "object": "list",
        "data": ids.into_iter().map(|id| json!({
            "id": id,
            "object": "model",
            "owned_by": "anthropic",
        })).collect::<Vec<_>>()
    })
}

pub fn openai_error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": "invalid_request_error",
                "code": null
            }
        })),
    )
        .into_response()
}

pub fn map_finish_reason(stop_reason: Option<&str>) -> &'static str {
    match stop_reason {
        Some("max_tokens") => "length",
        Some("stop_sequence") | Some("end_turn") => "stop",
        _ => "stop",
    }
}

pub fn openai_usage(usage: &Value) -> Value {
    let prompt = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();

    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    })
}

fn openai_sse_stream(
    mut rx: claude::ClaudeEventReceiver,
    state: HttpState,
    requested_model: String,
    mapped_model: String,
    include_usage: bool,
    started: Instant,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = epoch_seconds();

    stream! {
        yield Ok(Event::default().data(openai_chunk(&id, created, &requested_model, json!({ "role": "assistant" }), Value::Null, Value::Null).to_string()));
        let mut saw_result = false;

        while let Some(event) = rx.recv().await {
            match event {
                Ok(ClaudeEvent::TextDelta(text)) => {
                    yield Ok(Event::default().data(openai_chunk(&id, created, &requested_model, json!({ "content": text }), Value::Null, Value::Null).to_string()));
                }
                Ok(ClaudeEvent::Result { usage, stop_reason, is_error, text, api_error_status, .. }) => {
                    saw_result = true;
                    if is_error {
                        let status = StatusCode::from_u16(api_error_status.unwrap_or(StatusCode::BAD_GATEWAY.as_u16()))
                            .unwrap_or(StatusCode::BAD_GATEWAY);
                        yield Ok(Event::default().data(json!({
                            "error": {
                                "message": text,
                                "type": "upstream_error",
                                "code": status.as_u16(),
                            }
                        }).to_string()));
                        record_openai_log(
                            &state,
                            "/v1/chat/completions",
                            Some(requested_model.clone()),
                            Some(mapped_model.clone()),
                            status,
                            started,
                            None,
                        )
                        .await;
                    } else {
                        let finish_reason = Value::String(map_finish_reason(stop_reason.as_deref()).to_string());
                        let usage_value = openai_usage(&usage);
                        yield Ok(Event::default().data(openai_chunk(&id, created, &requested_model, json!({}), finish_reason, Value::Null).to_string()));
                        if include_usage {
                            yield Ok(Event::default().data(json!({
                                "id": &id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": &requested_model,
                                "choices": [],
                                "usage": usage_value.clone(),
                            }).to_string()));
                        }
                        record_openai_log(
                            &state,
                            "/v1/chat/completions",
                            Some(requested_model.clone()),
                            Some(mapped_model.clone()),
                            StatusCode::OK,
                            started,
                            Some(usage_value),
                        )
                        .await;
                    }
                    break;
                }
                Err(error) => {
                    let status = error.status_code();
                    yield Ok(Event::default().data(json!({
                        "error": {
                            "message": error.client_message(),
                            "type": "upstream_error",
                            "code": status.as_u16(),
                        }
                    }).to_string()));
                    record_openai_log(
                        &state,
                        "/v1/chat/completions",
                        Some(requested_model.clone()),
                        Some(mapped_model.clone()),
                        status,
                        started,
                        None,
                    )
                    .await;
                    saw_result = true;
                    break;
                }
                _ => {}
            }
        }

        if !saw_result {
            yield Ok(Event::default().data(json!({
                "error": {
                    "message": "Claude CLI stream ended before a result line",
                    "type": "upstream_error",
                    "code": StatusCode::BAD_GATEWAY.as_u16(),
                }
            }).to_string()));
            record_openai_log(
                &state,
                "/v1/chat/completions",
                Some(requested_model.clone()),
                Some(mapped_model.clone()),
                StatusCode::BAD_GATEWAY,
                started,
                None,
            )
            .await;
        }

        yield Ok(Event::default().data("[DONE]"));
    }
}

fn openai_chunk(
    id: &str,
    created: u64,
    requested_model: &str,
    delta: Value,
    finish_reason: Value,
    usage: Value,
) -> Value {
    json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": requested_model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    })
}


async fn record_openai_log(
    state: &HttpState,
    path: &str,
    client_model: Option<String>,
    mapped_model: Option<String>,
    status: StatusCode,
    started: Instant,
    usage: Option<Value>,
) {
    state
        .record_log(RequestLogEntry {
            ts: epoch_millis(),
            method: if path == "/v1/models" { "GET" } else { "POST" }.to_string(),
            path: path.to_string(),
            client_model,
            mapped_model,
            status: status.as_u16(),
            duration_ms: started.elapsed().as_millis(),
            usage,
        })
        .await;
}
fn non_empty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

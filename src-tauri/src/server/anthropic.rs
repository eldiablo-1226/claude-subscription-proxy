use std::{convert::Infallible, time::Instant};

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
pub struct MessagesRequest {
    pub model: String,
    #[serde(default)]
    pub system: Option<Value>,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<Value>,
}

pub async fn messages(State(state): State<HttpState>, Json(request): Json<MessagesRequest>) -> Response {
    let started = Instant::now();
    let _ = &request.max_tokens;
    let config = state.config.lock().await.clone();
    let flattened = match translate::flatten_anthropic_messages(
        &request.messages,
        request.system.as_ref(),
    ) {
        Ok(flattened) => flattened,
        Err(error) => {
            record_anthropic_log(
                &state,
                Some(request.model.clone()),
                None,
                StatusCode::BAD_REQUEST,
                started,
                None,
            )
            .await;
            return anthropic_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", error.message);
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
    let message_id = format!("msg_{}", Uuid::new_v4().simple());

    if request.stream {
        match claude::stream(config, state.semaphore.clone(), claude_request).await {
            Ok(rx) => Sse::new(anthropic_sse_stream(
                rx,
                state,
                message_id,
                request.model,
                mapped_model,
                started,
            ))
            .into_response(),
            Err(error) => {
                let status = error.status_code();
                record_anthropic_log(
                    &state,
                    Some(request.model),
                    Some(mapped_model),
                    status,
                    started,
                    None,
                )
                .await;
                anthropic_error_response(status, "api_error", error.client_message())
            }
        }
    } else {
        match claude::collect(config, state.semaphore.clone(), claude_request).await {
            Ok(completed) => {
                let usage = Some(completed.usage.clone());
                let response = message_response(&request.model, &message_id, completed);
                record_anthropic_log(
                    &state,
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
                record_anthropic_log(
                    &state,
                    Some(request.model),
                    Some(mapped_model),
                    status,
                    started,
                    None,
                )
                .await;
                anthropic_error_response(status, "api_error", error.client_message())
            }
        }
    }
}

pub fn resolve_model(config: &Config, requested: &str) -> String {
    requested
        .starts_with("claude")
        .then(|| requested.to_string())
        .or_else(|| config.model_map.get(requested).cloned())
        .unwrap_or_else(|| config.default_model.clone())
}

pub fn message_response(requested_model: &str, id: &str, completed: CompletedTurn) -> Value {
    if let Some(mut assistant) = completed.assistant {
        if let Some(object) = assistant.as_object_mut() {
            object.insert("id".to_string(), Value::String(id.to_string()));
            object.insert("model".to_string(), Value::String(requested_model.to_string()));
        }
        return assistant;
    }

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": requested_model,
        "content": [{ "type": "text", "text": completed.text }],
        "stop_reason": completed.stop_reason.unwrap_or_else(|| "end_turn".to_string()),
        "usage": completed.usage,
    })
}

pub fn rewrite_message_start(mut event: Value, id: &str, requested_model: &str) -> Value {
    if event.get("type").and_then(Value::as_str) == Some("message_start") {
        if let Some(message) = event.get_mut("message").and_then(Value::as_object_mut) {
            message.insert("id".to_string(), Value::String(id.to_string()));
            message.insert("model".to_string(), Value::String(requested_model.to_string()));
        }
    }
    event
}

pub fn anthropic_error_response(
    status: StatusCode,
    error_type: impl Into<String>,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": error_type.into(),
                "message": message.into(),
            }
        })),
    )
        .into_response()
}

fn anthropic_sse_stream(
    mut rx: claude::ClaudeEventReceiver,
    state: HttpState,
    message_id: String,
    requested_model: String,
    mapped_model: String,
    started: Instant,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream! {
        let mut saw_message_stop = false;
        let mut saw_result = false;

        while let Some(event) = rx.recv().await {
            match event {
                Ok(ClaudeEvent::AnthropicEvent(event)) => {
                    let event = rewrite_message_start(event, &message_id, &requested_model);
                    let event_type = event
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("message")
                        .to_string();
                    if event_type == "message_stop" {
                        saw_message_stop = true;
                    }
                    yield Ok(Event::default().event(event_type).data(event.to_string()));
                }
                Ok(ClaudeEvent::Result { is_error, text, api_error_status, usage, .. }) => {
                    saw_result = true;
                    if is_error {
                        let status = StatusCode::from_u16(api_error_status.unwrap_or(StatusCode::BAD_GATEWAY.as_u16()))
                            .unwrap_or(StatusCode::BAD_GATEWAY);
                        yield Ok(Event::default().event("error").data(json!({
                            "type": "error",
                            "error": {
                                "type": "api_error",
                                "message": text,
                                "status": status.as_u16(),
                            }
                        }).to_string()));
                        record_anthropic_log(
                            &state,
                            Some(requested_model.clone()),
                            Some(mapped_model.clone()),
                            status,
                            started,
                            None,
                        )
                        .await;
                    } else {
                        record_anthropic_log(
                            &state,
                            Some(requested_model.clone()),
                            Some(mapped_model.clone()),
                            StatusCode::OK,
                            started,
                            Some(usage),
                        )
                        .await;
                    }
                    break;
                }
                Err(error) => {
                    saw_result = true;
                    let status = error.status_code();
                    yield Ok(Event::default().event("error").data(json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": error.client_message(),
                            "status": status.as_u16(),
                        }
                    }).to_string()));
                    record_anthropic_log(
                        &state,
                        Some(requested_model.clone()),
                        Some(mapped_model.clone()),
                        status,
                        started,
                        None,
                    )
                    .await;
                    break;
                }
                _ => {}
            }
        }

        if !saw_result && !saw_message_stop {
            yield Ok(Event::default().event("error").data(json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": "Claude CLI stream ended before a result line",
                    "status": StatusCode::BAD_GATEWAY.as_u16(),
                }
            }).to_string()));
            record_anthropic_log(
                &state,
                Some(requested_model.clone()),
                Some(mapped_model.clone()),
                StatusCode::BAD_GATEWAY,
                started,
                None,
            )
            .await;
        } else if !saw_result {
            record_anthropic_log(
                &state,
                Some(requested_model.clone()),
                Some(mapped_model.clone()),
                StatusCode::OK,
                started,
                None,
            )
            .await;
        }
    }
}


async fn record_anthropic_log(
    state: &HttpState,
    client_model: Option<String>,
    mapped_model: Option<String>,
    status: StatusCode,
    started: Instant,
    usage: Option<Value>,
) {
    state
        .record_log(RequestLogEntry {
            ts: epoch_millis(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
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

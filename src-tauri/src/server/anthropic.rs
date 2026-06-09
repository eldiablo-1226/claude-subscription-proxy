use std::{convert::Infallible, time::Instant};

use async_stream::stream;
use axum::{
    extract::State,
    http::StatusCode,
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Config;

use super::{
    claude::{self, ClaudeError, ClaudeEvent, ClaudeRequest, CompletedTurn},
    state::{epoch_millis, HttpState, RequestLogEntry},
    translate,
};

const PATH: &str = "/v1/messages";

pub async fn messages(State(state): State<HttpState>, Json(body): Json<Value>) -> Response {
    let started = Instant::now();

    let Some(model) = body.get("model").and_then(Value::as_str).map(ToOwned::to_owned) else {
        return anthropic_fail(&state, started, None, None, StatusCode::BAD_REQUEST, "invalid_request_error", "you must provide a model parameter").await;
    };
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return anthropic_fail(&state, started, Some(model), None, StatusCode::BAD_REQUEST, "invalid_request_error", "you must provide a messages parameter").await;
    };
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let flattened = match translate::flatten_anthropic_messages(messages, body.get("system")) {
        Ok(flattened) => flattened,
        Err(error) => {
            return anthropic_fail(&state, started, Some(model), None, StatusCode::BAD_REQUEST, "invalid_request_error", error.message).await;
        }
    };
    let mapped_model = resolve_model(&state.config, &model);
    let claude_request = ClaudeRequest {
        final_user_text: flattened.final_user_text,
        system_text: non_empty(flattened.system_text),
        history_stdin: flattened.history_stdin,
        mapped_model: mapped_model.clone(),
        stream,
    };
    let message_id = format!("msg_{}", Uuid::new_v4().simple());

    if stream {
        let mut rx = match claude::stream(state.config.clone(), state.semaphore.clone(), claude_request).await {
            Ok(rx) => rx,
            Err(error) => {
                return anthropic_fail(&state, started, Some(model), Some(mapped_model), error.status_code(), "api_error", error.client_message()).await;
            }
        };
        // Peek the first event so a pre-stream failure maps to a real HTTP
        // status rather than HTTP 200 with an in-band error event.
        match rx.recv().await {
            Some(Err(error)) => {
                anthropic_fail(&state, started, Some(model), Some(mapped_model), error.status_code(), "api_error", error.client_message()).await
            }
            None => {
                anthropic_fail(&state, started, Some(model), Some(mapped_model), StatusCode::BAD_GATEWAY, "api_error", "Claude CLI produced no output").await
            }
            Some(first) => {
                let events = claude::into_event_stream(first, rx);
                Sse::new(anthropic_sse_stream(events, state, message_id, model, mapped_model, started)).into_response()
            }
        }
    } else {
        match claude::collect(state.config.clone(), state.semaphore.clone(), claude_request).await {
            Ok(completed) => {
                let usage = Some(completed.usage.clone());
                let response = message_response(&model, &message_id, completed);
                record_anthropic_log(&state, Some(model), Some(mapped_model), StatusCode::OK, started, usage).await;
                Json(response).into_response()
            }
            Err(error) => {
                anthropic_fail(&state, started, Some(model), Some(mapped_model), error.status_code(), "api_error", error.client_message()).await
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn anthropic_fail(
    state: &HttpState,
    started: Instant,
    client_model: Option<String>,
    mapped_model: Option<String>,
    status: StatusCode,
    error_type: &str,
    message: impl Into<String>,
) -> Response {
    record_anthropic_log(state, client_model, mapped_model, status, started, None).await;
    anthropic_error_response(status, error_type, message)
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
    events: impl Stream<Item = Result<ClaudeEvent, ClaudeError>>,
    state: HttpState,
    message_id: String,
    requested_model: String,
    mapped_model: String,
    started: Instant,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream! {
        futures::pin_mut!(events);
        let mut saw_message_stop = false;
        let mut saw_result = false;

        while let Some(event) = events.next().await {
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
                Ok(ClaudeEvent::Result { is_error, text, api_error_status, usage, subtype, .. }) => {
                    saw_result = true;
                    if claude::result_is_failure(is_error, &subtype) {
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
            path: PATH.to_string(),
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

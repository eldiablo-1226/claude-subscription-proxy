use std::{process::Stdio, sync::Arc, time::Duration};

use axum::http::StatusCode;
use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, Semaphore},
};

use crate::config::Config;

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    Init { model: Option<String> },
    TextDelta(String),
    AnthropicEvent(Value),
    Assistant(Value),
    Result {
        text: String,
        usage: Value,
        stop_reason: Option<String>,
        is_error: bool,
        subtype: String,
        api_error_status: Option<u16>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRequest {
    pub final_user_text: String,
    pub system_text: Option<String>,
    pub history_stdin: String,
    pub mapped_model: String,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletedTurn {
    pub text: String,
    pub usage: Value,
    pub stop_reason: Option<String>,
    pub assistant: Option<Value>,
    pub subtype: String,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ClaudeError {
    #[error("timed out waiting for Claude CLI")]
    Timeout,
    #[error("failed to spawn Claude CLI: {0}")]
    Spawn(String),
    #[error("Claude CLI I/O failed: {0}")]
    Io(String),
    #[error("failed to parse Claude CLI JSON: {0}")]
    Json(String),
    #[error("Claude CLI exited without a result: {stderr}")]
    ProcessFailed { stderr: String },
    #[error("Claude CLI returned an upstream error: {message}")]
    Upstream {
        status: u16,
        message: String,
        subtype: String,
    },
    #[error("Claude CLI stream ended before a result line")]
    NoResult,
    #[error("Claude concurrency limiter closed")]
    ConcurrencyClosed,
}

impl ClaudeError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Upstream { status, .. } => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
            Self::ConcurrencyClosed => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_GATEWAY,
        }
    }

    pub fn client_message(&self) -> String {
        self.to_string()
    }
}

pub type ClaudeEventReceiver = mpsc::Receiver<Result<ClaudeEvent, ClaudeError>>;

pub async fn stream(
    config: Config,
    semaphore: Arc<Semaphore>,
    request: ClaudeRequest,
) -> Result<ClaudeEventReceiver, ClaudeError> {
    let permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| ClaudeError::ConcurrencyClosed)?;
    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = run_child(config, request, tx.clone()).await {
            let _ = tx.send(Err(error)).await;
        }
    });

    Ok(rx)
}

pub async fn collect(
    config: Config,
    semaphore: Arc<Semaphore>,
    mut request: ClaudeRequest,
) -> Result<CompletedTurn, ClaudeError> {
    request.stream = false;
    let mut rx = stream(config, semaphore, request).await?;
    let mut assistant = None;

    while let Some(event) = rx.recv().await {
        match event? {
            ClaudeEvent::Assistant(message) => assistant = Some(message),
            ClaudeEvent::Result {
                text,
                usage,
                stop_reason,
                is_error,
                subtype,
                api_error_status,
            } => {
                if is_error {
                    return Err(ClaudeError::Upstream {
                        status: api_error_status.unwrap_or(StatusCode::BAD_GATEWAY.as_u16()),
                        message: text,
                        subtype,
                    });
                }

                return Ok(CompletedTurn {
                    text,
                    usage,
                    stop_reason,
                    assistant,
                    subtype,
                });
            }
            _ => {}
        }
    }

    Err(ClaudeError::NoResult)
}

pub fn build_command_parts(
    config: &Config,
    final_user_text: &str,
    system_text: Option<&str>,
    mapped_model: &str,
    stream: bool,
) -> (String, Vec<String>) {
    let mut args = vec![
        "-p".to_string(),
        final_user_text.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        mapped_model.to_string(),
        "--tools".to_string(),
        String::new(),
        "--max-turns".to_string(),
        "1".to_string(),
        "--no-session-persistence".to_string(),
        "--fallback-model".to_string(),
        "sonnet".to_string(),
        "--strict-mcp-config".to_string(),
        "--mcp-config".to_string(),
        r#"{"mcpServers":{}}"#.to_string(),
    ];

    if stream {
        args.push("--include-partial-messages".to_string());
    }

    if let Some(system_text) = system_text.filter(|value| !value.trim().is_empty()) {
        args.push("--append-system-prompt".to_string());
        args.push(system_text.to_string());
    }

    (config.claude_binary_path.clone(), args)
}

pub fn parse_sdk_message(value: Value) -> Result<Option<ClaudeEvent>, ClaudeError> {
    Ok(parse_sdk_messages(value)?.into_iter().next())
}

pub fn parse_sdk_messages(value: Value) -> Result<Vec<ClaudeEvent>, ClaudeError> {
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            Ok(vec![ClaudeEvent::Init {
                model: value.get("model").and_then(Value::as_str).map(ToOwned::to_owned),
            }])
        }
        Some("stream_event") => {
            let Some(event) = value.get("event").cloned() else {
                return Ok(Vec::new());
            };

            let mut events = Vec::with_capacity(2);
            if let Some(text) = text_delta(&event) {
                events.push(ClaudeEvent::TextDelta(text.to_string()));
            }
            events.push(ClaudeEvent::AnthropicEvent(event));
            Ok(events)
        }
        Some("assistant") => Ok(value
            .get("message")
            .cloned()
            .map(ClaudeEvent::Assistant)
            .into_iter()
            .collect()),
        Some("result") => Ok(vec![ClaudeEvent::Result {
            text: value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            usage: value.get("usage").cloned().unwrap_or(Value::Null),
            stop_reason: value
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            subtype: value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("success")
                .to_string(),
            api_error_status: value
                .get("api_error_status")
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok()),
        }]),
        _ => Ok(Vec::new()),
    }
}

async fn run_child(
    config: Config,
    request: ClaudeRequest,
    tx: mpsc::Sender<Result<ClaudeEvent, ClaudeError>>,
) -> Result<(), ClaudeError> {
    let (program, args) = build_command_parts(
        &config,
        &request.final_user_text,
        request.system_text.as_deref(),
        &request.mapped_model,
        request.stream,
    );

    let mut child = Command::new(program)
        .args(args)
        .current_dir(&config.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| ClaudeError::Spawn(err.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        if !request.history_stdin.is_empty() {
            stdin
                .write_all(request.history_stdin.as_bytes())
                .await
                .map_err(|err| ClaudeError::Io(err.to_string()))?;
        }
        stdin
            .shutdown()
            .await
            .map_err(|err| ClaudeError::Io(err.to_string()))?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ClaudeError::Io("missing stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ClaudeError::Io("missing stderr pipe".to_string()))?;
    let stderr_task = tokio::spawn(read_stderr(stderr));
    let mut lines = BufReader::new(stdout).lines();
    let mut saw_result = false;
    let timeout = tokio::time::sleep(Duration::from_secs(config.request_timeout_secs));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                let _ = child.kill().await;
                return Err(ClaudeError::Timeout);
            }
            line = lines.next_line() => {
                let Some(line) = line.map_err(|err| ClaudeError::Io(err.to_string()))? else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&line).map_err(|err| ClaudeError::Json(err.to_string()))?;
                for event in parse_sdk_messages(value)? {
                    saw_result |= matches!(event, ClaudeEvent::Result { .. });
                    if tx.send(Ok(event)).await.is_err() {
                        let _ = child.kill().await;
                        return Ok(());
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|err| ClaudeError::Io(err.to_string()))?;
    let stderr = stderr_task
        .await
        .map_err(|err| ClaudeError::Io(err.to_string()))?
        .map_err(|err| ClaudeError::Io(err.to_string()))?;

    if !status.success() && !saw_result {
        return Err(ClaudeError::ProcessFailed { stderr });
    }

    if !saw_result {
        return Err(ClaudeError::NoResult);
    }

    Ok(())
}

async fn read_stderr(mut stderr: tokio::process::ChildStderr) -> std::io::Result<String> {
    let mut buffer = String::new();
    stderr.read_to_string(&mut buffer).await?;
    Ok(buffer)
}

fn text_delta(event: &Value) -> Option<&str> {
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    delta.get("text").and_then(Value::as_str)
}

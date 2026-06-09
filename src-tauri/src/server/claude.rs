use std::{process::Stdio, sync::Arc, time::Duration};

use axum::http::StatusCode;
use futures::Stream;
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
    config: Arc<Config>,
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

/// Re-yield an already-dequeued first event, then drain the rest of the
/// receiver. Lets a handler peek the first event (to map a pre-stream failure
/// to a real HTTP status) without losing it from the SSE body.
pub fn into_event_stream(
    first: Result<ClaudeEvent, ClaudeError>,
    mut rx: ClaudeEventReceiver,
) -> impl Stream<Item = Result<ClaudeEvent, ClaudeError>> {
    async_stream::stream! {
        yield first;
        while let Some(item) = rx.recv().await {
            yield item;
        }
    }
}

pub async fn collect(
    config: Arc<Config>,
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
                if result_is_failure(is_error, &subtype) {
                    return Err(ClaudeError::Upstream {
                        status: api_error_status.unwrap_or(StatusCode::BAD_GATEWAY.as_u16()),
                        message: if text.is_empty() { subtype.clone() } else { text },
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
        // Load no user/project/local settings: the proxy must not inherit the
        // operator's personal hooks, output styles, or system-prompt additions,
        // which would otherwise leak into every proxied API response.
        "--setting-sources".to_string(),
        String::new(),
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

/// Parse one NDJSON line of `claude --output-format stream-json` output into
/// zero or more events. Blank or non-JSON lines (the CLI occasionally prints
/// stray notices to stdout) are skipped rather than aborting the turn.
pub fn parse_line(line: &str) -> Vec<ClaudeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => parse_sdk_messages(value).unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "skipping unparseable claude stdout line");
            Vec::new()
        }
    }
}

/// A `result` line is a failure when the CLI flags it OR its subtype is an
/// `error_*` variant (the boolean can be absent/false while subtype reports
/// `error_max_turns`, `error_during_execution`, etc.).
pub fn result_is_failure(is_error: bool, subtype: &str) -> bool {
    is_error || subtype.starts_with("error")
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

enum ReadOutcome {
    Eof,
    ConsumerGone,
}

async fn run_child(
    config: Arc<Config>,
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

    let mut child = Command::new(&program)
        .args(args)
        .current_dir(&config.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| ClaudeError::Spawn(err.to_string()))?;

    let stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ClaudeError::Io("missing stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ClaudeError::Io("missing stderr pipe".to_string()))?;

    // Write the prior-conversation transcript to the child CONCURRENTLY with
    // draining stdout. Writing it all up front and only then reading would
    // deadlock once the transcript exceeds the OS pipe buffer (~64KB): the
    // child stops reading stdin while its own stdout pipe is full, and the
    // write_all never returns.
    let history = request.history_stdin;
    let stdin_task = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            if !history.is_empty() {
                let _ = stdin.write_all(history.as_bytes()).await;
            }
            let _ = stdin.shutdown().await;
        }
    });

    let stderr_task = tokio::spawn(read_stderr(stderr));
    let mut lines = BufReader::new(stdout).lines();
    let mut saw_result = false;

    // The entire read loop is bounded by request_timeout_secs and aborts the
    // instant the consumer (e.g. a disconnected SSE client) drops the receiver,
    // so a stalled or abandoned request never pins its concurrency permit.
    let read = tokio::time::timeout(Duration::from_secs(config.request_timeout_secs), async {
        loop {
            tokio::select! {
                _ = tx.closed() => return Ok::<ReadOutcome, ClaudeError>(ReadOutcome::ConsumerGone),
                line = lines.next_line() => {
                    let Some(line) = line.map_err(|err| ClaudeError::Io(err.to_string()))? else {
                        return Ok(ReadOutcome::Eof);
                    };
                    for event in parse_line(&line) {
                        saw_result |= matches!(event, ClaudeEvent::Result { .. });
                        if tx.send(Ok(event)).await.is_err() {
                            return Ok(ReadOutcome::ConsumerGone);
                        }
                    }
                }
            }
        }
    })
    .await;

    stdin_task.abort();

    let outcome = match read {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            let _ = child.start_kill();
            stderr_task.abort();
            return Err(error);
        }
        Err(_elapsed) => {
            let _ = child.start_kill();
            stderr_task.abort();
            return Err(ClaudeError::Timeout);
        }
    };

    if matches!(outcome, ReadOutcome::ConsumerGone) {
        let _ = child.start_kill();
        stderr_task.abort();
        return Ok(());
    }

    // stdout hit EOF: reap the child and stderr under a short bound so a child
    // that closes stdout without exiting cannot hold the permit indefinitely.
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let stderr = match tokio::time::timeout(Duration::from_secs(2), stderr_task).await {
        Ok(Ok(Ok(text))) => text,
        _ => String::new(),
    };

    match status {
        Ok(Ok(status)) if !status.success() && !saw_result => {
            Err(ClaudeError::ProcessFailed { stderr })
        }
        Err(_elapsed) => {
            let _ = child.start_kill();
            if saw_result {
                Ok(())
            } else {
                Err(ClaudeError::ProcessFailed { stderr })
            }
        }
        _ if !saw_result => Err(ClaudeError::NoResult),
        _ => Ok(()),
    }
}

async fn read_stderr(mut stderr: tokio::process::ChildStderr) -> std::io::Result<String> {
    const MAX: usize = 16 * 1024;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stderr.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        // Keep draining past the cap (so the child never blocks on a full
        // stderr pipe) but retain at most MAX bytes.
        if buffer.len() < MAX {
            let take = read.min(MAX - buffer.len());
            buffer.extend_from_slice(&chunk[..take]);
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
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

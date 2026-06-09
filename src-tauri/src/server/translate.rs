use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlattenedInput {
    pub system_text: String,
    pub final_user_text: String,
    pub history_stdin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslateError {
    pub message: String,
}

impl TranslateError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn flatten_openai_chat(messages: &[Value]) -> Result<FlattenedInput, TranslateError> {
    flatten_messages(messages, None)
}

pub fn flatten_anthropic_messages(
    messages: &[Value],
    system: Option<&Value>,
) -> Result<FlattenedInput, TranslateError> {
    flatten_messages(messages, system)
}

fn flatten_messages(
    messages: &[Value],
    top_level_system: Option<&Value>,
) -> Result<FlattenedInput, TranslateError> {
    let Some(last) = messages.last() else {
        return Err(TranslateError::new("conversation must end with a user message"));
    };
    if role(last) != Some("user") {
        return Err(TranslateError::new("conversation must end with a user message"));
    }

    let mut system_parts = Vec::new();
    if let Some(system) = top_level_system {
        let text = content_text(system)?;
        if !text.is_empty() {
            system_parts.push(text);
        }
    }

    for message in messages {
        if role(message) == Some("system") {
            let text = message_content_text(message)?;
            if !text.is_empty() {
                system_parts.push(text);
            }
        }
    }

    let final_user_text = message_content_text(last)?;
    let mut history = String::new();
    for message in &messages[..messages.len().saturating_sub(1)] {
        let Some(role) = role(message) else {
            return Err(TranslateError::new("message role is required"));
        };
        if role == "system" {
            continue;
        }

        let text = message_content_text(message)?;
        if history.is_empty() {
            history.push_str("Prior conversation (continue it):\n\n");
        }
        history.push('[');
        history.push_str(role);
        history.push_str("]:\n");
        history.push_str(&text);
        history.push_str("\n\n");
    }

    Ok(FlattenedInput {
        system_text: system_parts.join("\n\n"),
        final_user_text,
        history_stdin: history,
    })
}

fn role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn message_content_text(message: &Value) -> Result<String, TranslateError> {
    let Some(content) = message.get("content") else {
        return Ok(String::new());
    };
    content_text(content)
}

fn content_text(content: &Value) -> Result<String, TranslateError> {
    match content {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => parts.iter().map(part_text).collect::<Result<Vec<_>, _>>().map(|parts| parts.join("")),
        Value::Null => Ok(String::new()),
        _ => Err(TranslateError::new(
            "image and non-text content is not supported by this proxy",
        )),
    }
}

fn part_text(part: &Value) -> Result<String, TranslateError> {
    let part_type = part.get("type").and_then(Value::as_str);
    match part_type {
        Some("text") | Some("input_text") => part
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| TranslateError::new("text content part is missing text")),
        _ => Err(TranslateError::new(
            "image and non-text content is not supported by this proxy",
        )),
    }
}

use serde_json::json;
use tauri_app_lib::{
    config::Config,
    server::{
        anthropic,
        claude::CompletedTurn,
        translate::{self, FlattenedInput},
    },
};

#[test]
fn flatten_anthropic_uses_top_level_system_and_final_user() {
    let system = json!([{ "type": "text", "text": "Be exact" }]);
    let messages = json!([
        {"role":"user","content":"Earlier"},
        {"role":"assistant","content":[{"type":"text","text":"Ack"}]},
        {"role":"user","content":"Now"}
    ]);

    let flattened = translate::flatten_anthropic_messages(messages.as_array().unwrap(), Some(&system)).unwrap();

    assert_eq!(flattened, FlattenedInput {
        system_text: "Be exact".to_string(),
        final_user_text: "Now".to_string(),
        history_stdin: "Prior conversation (continue it):\n\n[user]:\nEarlier\n\n[assistant]:\nAck\n\n".to_string(),
    });
}

#[test]
fn anthropic_resolve_model_uses_passthrough_then_map_then_default() {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());

    assert_eq!(anthropic::resolve_model(&config, "claude-sonnet-4-5"), "claude-sonnet-4-5");
    assert_eq!(anthropic::resolve_model(&config, "gpt-4o-mini"), "haiku");
    assert_eq!(anthropic::resolve_model(&config, "unknown"), "sonnet");
}

#[test]
fn anthropic_response_prefers_assistant_message_with_rewritten_id_and_model() {
    let completed = CompletedTurn {
        text: "fallback".to_string(),
        usage: json!({ "input_tokens": 1, "output_tokens": 2 }),
        stop_reason: Some("end_turn".to_string()),
        assistant: Some(json!({
            "id": "msg_from_cli",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-from-cli",
            "content": [{"type":"text", "text":"OK"}],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        })),
        subtype: "success".to_string(),
    };

    let response = anthropic::message_response("claude-sonnet-4-5", "msg_local", completed);

    assert_eq!(response["id"], "msg_local");
    assert_eq!(response["model"], "claude-sonnet-4-5");
    assert_eq!(response["content"][0]["text"], "OK");
}

#[test]
fn anthropic_response_synthesizes_when_assistant_missing() {
    let completed = CompletedTurn {
        text: "OK".to_string(),
        usage: json!({ "input_tokens": 1, "output_tokens": 2 }),
        stop_reason: Some("end_turn".to_string()),
        assistant: None,
        subtype: "success".to_string(),
    };

    let response = anthropic::message_response("claude-sonnet-4-5", "msg_local", completed);

    assert_eq!(response["type"], "message");
    assert_eq!(response["role"], "assistant");
    assert_eq!(response["content"][0], json!({"type":"text", "text":"OK"}));
    assert_eq!(response["usage"], json!({ "input_tokens": 1, "output_tokens": 2 }));
}

#[test]
fn message_start_event_rewrites_nested_message_identity() {
    let event = json!({
        "type": "message_start",
        "message": {
            "id": "msg_cli",
            "model": "claude-cli",
            "type": "message",
            "role": "assistant",
            "content": []
        }
    });

    let rewritten = anthropic::rewrite_message_start(event, "msg_local", "requested-model");

    assert_eq!(rewritten["message"]["id"], "msg_local");
    assert_eq!(rewritten["message"]["model"], "requested-model");
}

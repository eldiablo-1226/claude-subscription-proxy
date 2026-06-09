use serde_json::json;
use tauri_app_lib::{
    config::Config,
    server::{
        claude::CompletedTurn,
        openai,
        translate::{self, FlattenedInput},
    },
};

#[test]
fn flatten_openai_chat_splits_system_history_and_final_user() {
    let messages = json!([
        {"role":"system","content":"Be terse"},
        {"role":"user","content":"Hello"},
        {"role":"assistant","content":"Hi"},
        {"role":"user","content":[{"type":"text","text":"Continue"}]}
    ]);

    let flattened = translate::flatten_openai_chat(messages.as_array().unwrap()).unwrap();

    assert_eq!(flattened, FlattenedInput {
        system_text: "Be terse".to_string(),
        final_user_text: "Continue".to_string(),
        history_stdin: "Prior conversation (continue it):\n\n[user]:\nHello\n\n[assistant]:\nHi\n\n".to_string(),
    });
}

#[test]
fn flatten_rejects_assistant_prefill() {
    let messages = json!([
        {"role":"user","content":"Hello"},
        {"role":"assistant","content":"Hi"}
    ]);

    let error = translate::flatten_openai_chat(messages.as_array().unwrap()).unwrap_err();

    assert_eq!(error.message, "conversation must end with a user message");
}

#[test]
fn flatten_rejects_images_and_non_text_parts() {
    let messages = json!([
        {"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}]}
    ]);

    let error = translate::flatten_openai_chat(messages.as_array().unwrap()).unwrap_err();

    assert_eq!(error.message, "image and non-text content is not supported by this proxy");
}

#[test]
fn resolve_model_uses_map_then_claude_passthrough_then_default() {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());

    assert_eq!(openai::resolve_model(&config, "gpt-4o"), "opus");
    assert_eq!(openai::resolve_model(&config, "claude-sonnet-4-5"), "claude-sonnet-4-5");
    assert_eq!(openai::resolve_model(&config, "unknown-model"), "sonnet");
}

#[test]
fn completion_response_maps_usage_and_finish_reason() {
    let completed = CompletedTurn {
        text: "OK".to_string(),
        usage: json!({ "input_tokens": 7, "output_tokens": 2 }),
        stop_reason: Some("max_tokens".to_string()),
        assistant: None,
        subtype: "success".to_string(),
    };

    let response = openai::completion_response("gpt-4o", completed);

    assert_eq!(response["object"], "chat.completion");
    assert_eq!(response["model"], "gpt-4o");
    assert_eq!(response["choices"][0]["message"]["content"], "OK");
    assert_eq!(response["choices"][0]["finish_reason"], "length");
    assert_eq!(response["usage"]["prompt_tokens"], 7);
    assert_eq!(response["usage"]["completion_tokens"], 2);
    assert_eq!(response["usage"]["total_tokens"], 9);
}

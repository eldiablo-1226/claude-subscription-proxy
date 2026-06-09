use serde_json::json;
use tauri_app_lib::{config::Config, server::claude::{self, ClaudeEvent}};

#[test]
fn build_command_uses_subscription_safe_cli_flags() {
    let mut config = Config::default_for_data_dir("/tmp/csp-data".into());
    config.claude_binary_path = "/usr/local/bin/claude".to_string();

    let (program, args) = claude::build_command_parts(
        &config,
        "Say hi",
        Some("client system"),
        "opus",
        true,
    );

    assert_eq!(program, "/usr/local/bin/claude");
    assert_eq!(args[0], "-p");
    assert_eq!(args[1], "Say hi");
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(args.contains(&"--verbose".to_string()));
    assert!(args.contains(&"--tools".to_string()));
    assert!(args.contains(&"".to_string()));
    assert!(args.contains(&"--max-turns".to_string()));
    assert!(args.contains(&"1".to_string()));
    assert!(args.contains(&"--no-session-persistence".to_string()));
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    assert!(args.contains(&"--mcp-config".to_string()));
    assert!(args.contains(&r#"{"mcpServers":{}}"#.to_string()));
    assert!(args.contains(&"--include-partial-messages".to_string()));
    assert!(args.contains(&"--append-system-prompt".to_string()));
    assert!(args.contains(&"client system".to_string()));
    assert!(!args.contains(&"--bare".to_string()));
    assert!(!args.contains(&"--system-prompt".to_string()));
}

#[test]
fn build_command_omits_streaming_and_system_flags_when_unused() {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());

    let (_, args) = claude::build_command_parts(&config, "Say hi", None, "haiku", false);

    assert!(!args.contains(&"--include-partial-messages".to_string()));
    assert!(!args.contains(&"--append-system-prompt".to_string()));
    assert!(args.windows(2).any(|pair| pair == ["--model", "haiku"]));
}

#[test]
fn parse_stream_event_text_delta_from_sdk_line() {
    let line = json!({
        "type": "stream_event",
        "event": {
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "Hello" }
        }
    });

    assert_eq!(claude::parse_sdk_message(line).unwrap(), Some(ClaudeEvent::TextDelta("Hello".to_string())));
}

#[test]
fn parse_result_and_usage_from_sdk_line() {
    let line = json!({
        "type": "result",
        "subtype": "success",
        "result": "OK",
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 3, "output_tokens": 2 },
        "is_error": false
    });

    let event = claude::parse_sdk_message(line).unwrap().unwrap();
    assert_eq!(event, ClaudeEvent::Result {
        text: "OK".to_string(),
        usage: json!({ "input_tokens": 3, "output_tokens": 2 }),
        stop_reason: Some("end_turn".to_string()),
        is_error: false,
        subtype: "success".to_string(),
        api_error_status: None,
    });
}

#[test]
fn build_command_isolates_user_settings_and_hooks() {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());

    let (_, args) = claude::build_command_parts(&config, "hi", None, "sonnet", false);

    let idx = args
        .iter()
        .position(|arg| arg == "--setting-sources")
        .expect("--setting-sources flag present");
    assert_eq!(args[idx + 1], "");
}

#[test]
fn parse_line_skips_blank_and_invalid_lines() {
    assert!(claude::parse_line("").is_empty());
    assert!(claude::parse_line("   ").is_empty());
    assert!(claude::parse_line("not json at all").is_empty());

    let line = r#"{"type":"result","subtype":"success","result":"OK","is_error":false}"#;
    let events = claude::parse_line(line);
    assert!(matches!(events.as_slice(), [ClaudeEvent::Result { .. }]));
}

#[test]
fn result_is_failure_treats_error_subtype_as_failure() {
    assert!(claude::result_is_failure(true, "success"));
    assert!(claude::result_is_failure(false, "error_max_turns"));
    assert!(claude::result_is_failure(false, "error_during_execution"));
    assert!(!claude::result_is_failure(false, "success"));
}

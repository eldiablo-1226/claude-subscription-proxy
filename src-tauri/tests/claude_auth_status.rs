use serde_json::json;
use tauri_app_lib::claude_auth::{self, ClaudeAuthStatus};

#[test]
fn parse_auth_status_extracts_subscription_and_account() {
    let status = claude_auth::parse_status_output(
        true,
        r#"{"loggedIn":true,"subscriptionType":"max","email":"user@example.com"}"#,
        "",
    );

    assert_eq!(status, ClaudeAuthStatus {
        logged_in: true,
        subscription_type: Some("max".to_string()),
        account: Some("user@example.com".to_string()),
        raw: json!({"loggedIn":true,"subscriptionType":"max","email":"user@example.com"}),
    });
}

#[test]
fn parse_auth_status_reports_not_logged_in_on_failed_exit() {
    let status = claude_auth::parse_status_output(false, "", "not logged in");

    assert!(!status.logged_in);
    assert_eq!(status.subscription_type, None);
    assert_eq!(status.account, None);
    assert_eq!(status.raw["error"], "not logged in");
}

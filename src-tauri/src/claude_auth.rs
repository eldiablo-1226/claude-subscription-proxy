use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    pub subscription_type: Option<String>,
    pub account: Option<String>,
    pub raw: Value,
}

pub async fn get_claude_auth_status(binary: &str) -> ClaudeAuthStatus {
    match Command::new(binary).args(["auth", "status"]).output().await {
        Ok(output) => parse_status_output(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) => ClaudeAuthStatus {
            logged_in: false,
            subscription_type: None,
            account: None,
            raw: json!({ "error": format!("failed to run {binary} auth status: {error}") }),
        },
    }
}

pub fn parse_status_output(success: bool, stdout: &str, stderr: &str) -> ClaudeAuthStatus {
    let raw = serde_json::from_str(stdout).unwrap_or_else(|_| {
        let error = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        json!({ "error": error })
    });

    ClaudeAuthStatus {
        logged_in: success,
        subscription_type: string_at_any(&raw, &["subscriptionType", "subscription_type", "plan"]),
        account: string_at_any(&raw, &["email", "account"]),
        raw,
    }
}

pub async fn start_claude_login(binary: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell app \"Terminal\" to do script \"{} auth login\"",
            escape_for_applescript_double_quotes(binary)
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = binary;
        Err("run `claude auth login` manually on this platform".to_string())
    }
}

fn string_at_any(raw: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| raw.get(*key).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

#[cfg(target_os = "macos")]
fn escape_for_applescript_double_quotes(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

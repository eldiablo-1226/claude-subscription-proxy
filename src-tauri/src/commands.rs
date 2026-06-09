use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Semaphore;

use crate::{
    claude_auth::{self, ClaudeAuthStatus},
    config::Config,
    keys::KeyInfo,
    server::{
        self,
        claude::{self, ClaudeRequest},
        state::{self, AppState, RateLimitInfo, RequestLogEntry, ServerMetrics},
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerStatus {
    pub running: bool,
    pub bind: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreatedKey {
    pub id: String,
    pub label: String,
    pub raw_key: String,
    pub created_at: i64,
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    Ok(state.config.lock().await.clone())
}

#[tauri::command]
pub async fn set_config(app: AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    if state.server.lock().await.is_some() {
        return Err("stop the server before changing config".to_string());
    }

    config.save(&app)?;
    *state.config.lock().await = config;
    Ok(())
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    Ok(state.server_status().await)
}

#[tauri::command]
pub async fn start_server(app: AppHandle, state: State<'_, AppState>) -> Result<ServerStatus, String> {
    if state.server.lock().await.is_some() {
        let status = state.server_status().await;
        let _ = app.emit("server_status", status.clone());
        return Ok(status);
    }

    let config = state.config.lock().await.clone();
    let handle = server::start(
        app.clone(),
        config,
        state.keys.clone(),
        state.logs.clone(),
        state.rate_limit.clone(),
    )
    .await?;
    {
        let mut server = state.server.lock().await;
        *server = Some(handle);
    }
    let status = state.server_status().await;
    let _ = app.emit("server_status", status.clone());
    Ok(status)
}

#[tauri::command]
pub async fn stop_server(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.server.lock().await.take() {
        handle.cancel.cancel();
    }
    let status = state.server_status().await;
    let _ = app.emit("server_status", status);
    Ok(())
}

#[tauri::command]
pub async fn list_api_keys(state: State<'_, AppState>) -> Result<Vec<KeyInfo>, String> {
    Ok(state.keys.lock().await.list())
}

#[tauri::command]
pub async fn create_api_key(state: State<'_, AppState>, label: String) -> Result<CreatedKey, String> {
    let (stored, raw_key) = state.keys.lock().await.create(label)?;
    Ok(CreatedKey {
        id: stored.id,
        label: stored.label,
        raw_key,
        created_at: stored.created_at,
    })
}

#[tauri::command]
pub async fn revoke_api_key(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.keys.lock().await.revoke(&id)
}

#[tauri::command]
pub async fn get_claude_auth_status(state: State<'_, AppState>) -> Result<ClaudeAuthStatus, String> {
    let binary = state.config.lock().await.claude_binary_path.clone();
    Ok(claude_auth::get_claude_auth_status(&binary).await)
}

#[tauri::command]
pub async fn start_claude_login(state: State<'_, AppState>) -> Result<(), String> {
    let binary = state.config.lock().await.claude_binary_path.clone();
    claude_auth::start_claude_login(&binary).await
}

#[tauri::command]
pub async fn get_logs(state: State<'_, AppState>) -> Result<Vec<RequestLogEntry>, String> {
    Ok(state.logs.lock().await.iter().cloned().collect())
}

#[tauri::command]
pub async fn get_server_metrics(state: State<'_, AppState>) -> Result<ServerMetrics, String> {
    Ok(state.server_metrics().await)
}

#[tauri::command]
pub async fn get_subscription_limits(state: State<'_, AppState>) -> Result<Option<RateLimitInfo>, String> {
    Ok(state.rate_limit.lock().await.clone())
}

/// Run a minimal one-off `claude -p` turn purely to capture the current
/// subscription rate-limit window, then store + broadcast it. Lets the UI show
/// limits on demand without waiting for proxied client traffic.
#[tauri::command]
pub async fn refresh_subscription_limits(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<RateLimitInfo>, String> {
    let config = Arc::new(state.config.lock().await.clone());
    let model = config.default_model.clone();
    let request = ClaudeRequest {
        final_user_text: "hi".to_string(),
        system_text: None,
        history_stdin: String::new(),
        mapped_model: model,
        stream: false,
    };

    let completed = claude::collect(config, Arc::new(Semaphore::new(1)), request)
        .await
        .map_err(|err| err.client_message())?;

    match completed.rate_limit {
        Some(raw) => Ok(state::store_rate_limit(&state.rate_limit, Some(&app), raw).await),
        None => Ok(state.rate_limit.lock().await.clone()),
    }
}

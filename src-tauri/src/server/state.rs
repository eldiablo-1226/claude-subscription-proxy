use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};

use crate::{config::Config, keys::KeyStore};

const MAX_LOG_ENTRIES: usize = 500;

/// Live, per-run server state shared between the axum handlers (`HttpState`) and
/// the command layer (`AppState` via `ServerHandle`). Recreated on every start.
pub struct ServerRuntime {
    pub started_at: i64,
    pub max_concurrency: usize,
    pub semaphore: Arc<Semaphore>,
    pub total_requests: AtomicU64,
}

impl ServerRuntime {
    pub fn new(max_concurrency: usize) -> Arc<Self> {
        Arc::new(Self {
            started_at: epoch_millis(),
            max_concurrency,
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
            total_requests: AtomicU64::new(0),
        })
    }

    /// In-flight requests = permits currently checked out of the semaphore.
    pub fn active_requests(&self) -> u64 {
        self.max_concurrency
            .saturating_sub(self.semaphore.available_permits()) as u64
    }
}

#[derive(Clone)]
pub struct HttpState {
    pub config: Arc<Config>,
    pub keys: Arc<Mutex<KeyStore>>,
    pub runtime: Arc<ServerRuntime>,
    pub logs: Arc<Mutex<VecDeque<RequestLogEntry>>>,
    pub rate_limit: Arc<Mutex<Option<RateLimitInfo>>>,
    pub app: Option<AppHandle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestLogEntry {
    pub ts: i64,
    pub method: String,
    pub path: String,
    pub client_model: Option<String>,
    pub mapped_model: Option<String>,
    pub status: u16,
    pub duration_ms: u128,
    pub usage: Option<Value>,
}

/// The most recent subscription rate-limit snapshot the CLI reported (via its
/// `rate_limit_event` line). `captured_at` is when the proxy observed it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateLimitInfo {
    pub status: Option<String>,
    pub rate_limit_type: Option<String>,
    pub resets_at: Option<i64>,
    pub overage_status: Option<String>,
    pub overage_resets_at: Option<i64>,
    pub is_using_overage: Option<bool>,
    pub captured_at: i64,
}

impl RateLimitInfo {
    pub fn from_value(value: &Value) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        Some(Self {
            status: string_field(value, "status"),
            rate_limit_type: string_field(value, "rateLimitType"),
            resets_at: value.get("resetsAt").and_then(Value::as_i64),
            overage_status: string_field(value, "overageStatus"),
            overage_resets_at: value.get("overageResetsAt").and_then(Value::as_i64),
            is_using_overage: value.get("isUsingOverage").and_then(Value::as_bool),
            captured_at: epoch_millis(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerMetrics {
    pub running: bool,
    pub bind: String,
    pub port: u16,
    pub started_at: Option<i64>,
    pub uptime_secs: u64,
    pub total_requests: u64,
    pub active_requests: u64,
    pub max_concurrency: u64,
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub server: Mutex<Option<super::ServerHandle>>,
    pub keys: Arc<Mutex<KeyStore>>,
    pub logs: Arc<Mutex<VecDeque<RequestLogEntry>>>,
    pub rate_limit: Arc<Mutex<Option<RateLimitInfo>>>,
}

impl AppState {
    pub fn load(app: &AppHandle) -> Result<Self, String> {
        let config = Config::load(app)?;
        let keys = KeyStore::load_for_app(app)?;
        Ok(Self::new(config, keys))
    }

    pub fn new_for_test(config: Config, keys: KeyStore) -> Self {
        Self::new(config, keys)
    }

    fn new(config: Config, keys: KeyStore) -> Self {
        Self {
            config: Mutex::new(config),
            server: Mutex::new(None),
            keys: Arc::new(Mutex::new(keys)),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            rate_limit: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn server_status(&self) -> crate::commands::ServerStatus {
        if let Some(handle) = self.server.lock().await.as_ref() {
            return crate::commands::ServerStatus {
                running: true,
                bind: handle.bind.clone(),
                port: handle.port,
            };
        }

        let config = self.config.lock().await;
        crate::commands::ServerStatus {
            running: false,
            bind: config.bind_address.clone(),
            port: config.port,
        }
    }

    pub async fn server_metrics(&self) -> ServerMetrics {
        if let Some(handle) = self.server.lock().await.as_ref() {
            let runtime = &handle.runtime;
            return ServerMetrics {
                running: true,
                bind: handle.bind.clone(),
                port: handle.port,
                started_at: Some(runtime.started_at),
                uptime_secs: uptime_secs(runtime.started_at, epoch_millis()),
                total_requests: runtime.total_requests.load(Ordering::Relaxed),
                active_requests: runtime.active_requests(),
                max_concurrency: runtime.max_concurrency as u64,
            };
        }

        let config = self.config.lock().await;
        ServerMetrics {
            running: false,
            bind: config.bind_address.clone(),
            port: config.port,
            started_at: None,
            uptime_secs: 0,
            total_requests: 0,
            active_requests: 0,
            max_concurrency: config.max_concurrency as u64,
        }
    }
}

impl HttpState {
    pub async fn record_log(&self, entry: RequestLogEntry) {
        self.runtime.total_requests.fetch_add(1, Ordering::Relaxed);
        {
            let mut logs = self.logs.lock().await;
            while logs.len() >= MAX_LOG_ENTRIES {
                logs.pop_front();
            }
            logs.push_back(entry.clone());
        }

        if let Some(app) = &self.app {
            let _ = app.emit("request_log", entry);
            let _ = app.emit("server_metrics", self.metrics_snapshot());
        }
    }

    pub async fn set_rate_limit(&self, raw: Value) {
        store_rate_limit(&self.rate_limit, self.app.as_ref(), raw).await;
    }

    fn metrics_snapshot(&self) -> ServerMetrics {
        ServerMetrics {
            running: true,
            bind: self.config.bind_address.clone(),
            port: self.config.port,
            started_at: Some(self.runtime.started_at),
            uptime_secs: uptime_secs(self.runtime.started_at, epoch_millis()),
            total_requests: self.runtime.total_requests.load(Ordering::Relaxed),
            active_requests: self.runtime.active_requests(),
            max_concurrency: self.runtime.max_concurrency as u64,
        }
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

/// Parse, persist, and broadcast a rate-limit snapshot. Shared by the request
/// path (`HttpState::set_rate_limit`) and the on-demand probe command. Returns
/// the parsed snapshot, or `None` when the value is not a rate-limit object.
pub async fn store_rate_limit(
    slot: &Mutex<Option<RateLimitInfo>>,
    app: Option<&AppHandle>,
    raw: Value,
) -> Option<RateLimitInfo> {
    let info = RateLimitInfo::from_value(&raw)?;
    *slot.lock().await = Some(info.clone());
    if let Some(app) = app {
        let _ = app.emit("subscription_limits", info.clone());
    }
    Some(info)
}

pub fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

pub fn uptime_secs(started_at_ms: i64, now_ms: i64) -> u64 {
    (now_ms.saturating_sub(started_at_ms).max(0) / 1000) as u64
}

use std::{collections::VecDeque, sync::Arc, time::{SystemTime, UNIX_EPOCH}};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};

use crate::{config::Config, keys::KeyStore};

#[derive(Clone)]
pub struct HttpState {
    pub config: Arc<Mutex<Config>>,
    pub keys: Arc<Mutex<KeyStore>>,
    pub semaphore: Arc<Semaphore>,
    pub logs: Arc<Mutex<VecDeque<RequestLogEntry>>>,
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

pub fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub server: Mutex<Option<super::ServerHandle>>,
    pub keys: Arc<Mutex<KeyStore>>,
    pub logs: Arc<Mutex<VecDeque<RequestLogEntry>>>,
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
}

impl HttpState {
    pub async fn record_log(&self, entry: RequestLogEntry) {
        {
            let mut logs = self.logs.lock().await;
            if logs.len() == 500 {
                logs.pop_front();
            }
            logs.push_back(entry.clone());
        }

        if let Some(app) = &self.app {
            let _ = app.emit("request_log", entry);
        }
    }
}

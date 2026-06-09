use std::{collections::HashMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub bind_address: String,
    pub port: u16,
    pub claude_binary_path: String,
    pub default_model: String,
    pub model_map: HashMap<String, String>,
    pub max_concurrency: usize,
    pub request_timeout_secs: u64,
    pub working_dir: String,
    pub require_auth: bool,
}

impl Config {
    pub fn default_for_data_dir(data_dir: PathBuf) -> Self {
        Self {
            bind_address: "0.0.0.0".to_string(),
            port: 8787,
            claude_binary_path: "claude".to_string(),
            default_model: "sonnet".to_string(),
            model_map: default_model_map(),
            max_concurrency: 4,
            request_timeout_secs: 600,
            working_dir: data_dir.join("scratch").to_string_lossy().into_owned(),
            require_auth: true,
        }
    }

    pub fn load(app: &tauri::AppHandle) -> Result<Self, String> {
        let config_dir = app.path().app_config_dir().map_err(|err| err.to_string())?;
        let data_dir = app.path().app_data_dir().map_err(|err| err.to_string())?;
        fs::create_dir_all(&config_dir).map_err(|err| err.to_string())?;
        fs::create_dir_all(&data_dir).map_err(|err| err.to_string())?;

        let path = config_path(app)?;
        let config = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            serde_json::from_str(&raw).map_err(|err| err.to_string())?
        } else {
            Self::default_for_data_dir(data_dir)
        };

        fs::create_dir_all(&config.working_dir).map_err(|err| err.to_string())?;
        Ok(config)
    }

    pub fn save(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let path = config_path(app)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::create_dir_all(&self.working_dir).map_err(|err| err.to_string())?;
        let raw = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;
        fs::write(path, raw).map_err(|err| err.to_string())
    }
}

fn default_model_map() -> HashMap<String, String> {
    HashMap::from([
        ("gpt-4o".to_string(), "opus".to_string()),
        ("gpt-4o-mini".to_string(), "haiku".to_string()),
        ("gpt-4".to_string(), "opus".to_string()),
        ("gpt-3.5-turbo".to_string(), "haiku".to_string()),
    ])
}

pub fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|err| err.to_string())?
        .join("config.json"))
}

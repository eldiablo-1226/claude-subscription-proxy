use std::{fs, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredKey {
    pub id: String,
    pub label: String,
    pub hash: String,
    pub prefix: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyInfo {
    pub id: String,
    pub label: String,
    pub prefix: String,
    pub created_at: i64,
}

#[derive(Debug)]
pub struct KeyStore {
    path: PathBuf,
    keys: Vec<StoredKey>,
}

impl KeyStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let keys = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            serde_json::from_str(&raw).map_err(|err| err.to_string())?
        } else {
            Vec::new()
        };

        Ok(Self { path, keys })
    }

    pub fn load_for_app(app: &tauri::AppHandle) -> Result<Self, String> {
        Self::load(keys_path(app)?)
    }

    pub fn create(&mut self, label: String) -> Result<(StoredKey, String), String> {
        let raw = format!("csp-{}", random_base62(40));
        let key = StoredKey {
            id: Uuid::new_v4().to_string(),
            label,
            hash: sha256_hex(&raw),
            prefix: raw.chars().take(8).collect(),
            created_at: epoch_millis(),
        };

        self.keys.push(key.clone());
        self.save()?;
        Ok((key, raw))
    }

    pub fn verify(&self, presented: &str) -> bool {
        let presented_hash = sha256_hex(presented);
        self.keys
            .iter()
            .any(|stored| constant_time_eq(stored.hash.as_bytes(), presented_hash.as_bytes()))
    }

    pub fn list(&self) -> Vec<KeyInfo> {
        self.keys
            .iter()
            .map(|stored| KeyInfo {
                id: stored.id.clone(),
                label: stored.label.clone(),
                prefix: stored.prefix.clone(),
                created_at: stored.created_at,
            })
            .collect()
    }

    pub fn revoke(&mut self, id: &str) -> Result<(), String> {
        self.keys.retain(|stored| stored.id != id);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let raw = serde_json::to_string_pretty(&self.keys).map_err(|err| err.to_string())?;
        fs::write(&self.path, raw).map_err(|err| err.to_string())
    }
}

pub fn keys_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|err| err.to_string())?
        .join("keys.json"))
}

fn random_base62(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

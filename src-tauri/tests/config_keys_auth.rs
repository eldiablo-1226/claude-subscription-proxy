use std::{collections::HashMap, fs};

use axum::http::HeaderMap;
use tauri_app_lib::{config::Config, keys::KeyStore, server::auth};

#[test]
fn config_defaults_match_proxy_contract() {
    let config = Config::default_for_data_dir("/tmp/csp-data".into());

    assert_eq!(config.bind_address, "0.0.0.0");
    assert_eq!(config.port, 8787);
    assert_eq!(config.claude_binary_path, "claude");
    assert_eq!(config.default_model, "sonnet");
    assert_eq!(config.max_concurrency, 4);
    assert_eq!(config.request_timeout_secs, 600);
    assert_eq!(config.working_dir, "/tmp/csp-data/scratch");
    assert!(config.require_auth);

    let expected = HashMap::from([
        ("gpt-4o".to_string(), "opus".to_string()),
        ("gpt-4o-mini".to_string(), "haiku".to_string()),
        ("gpt-4".to_string(), "opus".to_string()),
        ("gpt-3.5-turbo".to_string(), "haiku".to_string()),
    ]);
    assert_eq!(config.model_map, expected);
}

#[test]
fn key_store_returns_raw_key_once_and_persists_only_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let mut store = KeyStore::load(&path).unwrap();

    let (created, raw) = store.create("laptop".to_string()).unwrap();

    assert!(raw.starts_with("csp-"));
    assert_eq!(raw.len(), 44);
    assert_eq!(created.prefix, raw.chars().take(8).collect::<String>());
    assert!(store.verify(&raw));
    assert!(!store.verify("csp-wrong"));

    let disk = fs::read_to_string(&path).unwrap();
    assert!(!disk.contains(&raw));
    assert!(!disk.contains("csp-wrong"));

    let reloaded = KeyStore::load(&path).unwrap();
    assert!(reloaded.verify(&raw));
    let listed = reloaded.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
    assert_eq!(listed[0].label, "laptop");
    assert_eq!(listed[0].prefix, created.prefix);
}

#[test]
fn key_store_revoke_removes_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    let mut store = KeyStore::load(&path).unwrap();
    let (created, raw) = store.create("phone".to_string()).unwrap();

    store.revoke(&created.id).unwrap();

    assert!(!store.verify(&raw));
    assert!(store.list().is_empty());
}

#[test]
fn auth_accepts_bearer_or_x_api_key_headers() {
    let mut bearer = HeaderMap::new();
    bearer.insert("authorization", "Bearer csp-secret".parse().unwrap());
    assert_eq!(auth::presented_key(&bearer).as_deref(), Some("csp-secret"));

    let mut api_key = HeaderMap::new();
    api_key.insert("x-api-key", "csp-secret-2".parse().unwrap());
    assert_eq!(auth::presented_key(&api_key).as_deref(), Some("csp-secret-2"));

    let mut lower = HeaderMap::new();
    lower.insert("authorization", "bearer csp-secret-3".parse().unwrap());
    assert_eq!(auth::presented_key(&lower).as_deref(), Some("csp-secret-3"));
}

#[test]
fn auth_error_shape_follows_route_family() {
    let openai = auth::unauthorized_json("/v1/chat/completions");
    assert_eq!(openai["error"]["type"], "invalid_request_error");
    assert_eq!(openai["error"]["code"], "invalid_api_key");

    let anthropic = auth::unauthorized_json("/v1/messages");
    assert_eq!(anthropic["type"], "error");
    assert_eq!(anthropic["error"]["type"], "authentication_error");
}

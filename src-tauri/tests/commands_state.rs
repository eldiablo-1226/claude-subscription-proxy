use std::path::PathBuf;

use tauri_app_lib::{
    config::Config,
    keys::KeyStore,
    server::state::AppState,
    commands::ServerStatus,
};

#[tokio::test]
async fn app_state_reports_stopped_status_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default_for_data_dir(PathBuf::from("/tmp/csp-data"));
    config.bind_address = "127.0.0.1".to_string();
    config.port = 9999;
    let keys = KeyStore::load(dir.path().join("keys.json")).unwrap();
    let state = AppState::new_for_test(config, keys);

    assert_eq!(state.server_status().await, ServerStatus {
        running: false,
        bind: "127.0.0.1".to_string(),
        port: 9999,
    });
}

pub mod claude_auth;
pub mod commands;
pub mod config;
pub mod keys;
pub mod server;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt::try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = server::state::AppState::load(&app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::get_server_status,
            commands::start_server,
            commands::stop_server,
            commands::list_api_keys,
            commands::create_api_key,
            commands::revoke_api_key,
            commands::get_claude_auth_status,
            commands::start_claude_login,
            commands::get_logs,
            commands::get_server_metrics,
            commands::get_subscription_limits,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

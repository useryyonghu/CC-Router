pub mod app_paths;
pub mod claude;
pub mod commands;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod preset;
pub mod routing;

use crate::commands::AppState;
use crate::config::store::ConfigStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = ConfigStore::load(crate::app_paths::config_path())
        .expect("无法加载 config.json：请检查 %APPDATA%\\cc-router\\config.json");
    let state = AppState::new(store).shared();

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::agent_create,
            commands::agent_delete,
            commands::agent_set_model,
            commands::agents_list,
            commands::gateway_start,
            commands::gateway_status,
            commands::gateway_stop,
            commands::get_config,
            commands::recent_logs,
            commands::save_config,
            commands::takeover_apply,
            commands::takeover_restore,
            commands::takeover_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

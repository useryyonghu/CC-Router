pub mod app_paths;
pub mod claude;
pub mod commands;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod preset;
pub mod provider;
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
            commands::backups_list,
            commands::gateway_restart,
            commands::gateway_start,
            commands::gateway_status,
            commands::gateway_stop,
            commands::get_config,
            commands::model_add,
            commands::model_remove,
            commands::models_add_many,
            commands::models_fetch,
            commands::presets_list,
            commands::provider_add,
            commands::provider_remove,
            commands::provider_test,
            commands::provider_update,
            commands::recent_logs,
            commands::role_set,
            commands::save_config,
            commands::settings_paths,
            commands::takeover_apply,
            commands::takeover_restore,
            commands::takeover_status,
            commands::token_regenerate,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

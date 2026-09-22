pub mod app_paths;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod routing;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

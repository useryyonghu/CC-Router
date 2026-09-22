use std::path::PathBuf;

/// `%APPDATA%\cc-router`。刻意不用 Tauri 的 appDataDir（那会带上 bundle id）。
pub fn app_data_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(base) => PathBuf::from(base).join("cc-router"),
        None => PathBuf::from(".cc-router"),
    }
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.json")
}

pub fn backups_dir() -> PathBuf {
    app_data_dir().join("backups")
}

pub fn logs_dir() -> PathBuf {
    app_data_dir().join("logs")
}

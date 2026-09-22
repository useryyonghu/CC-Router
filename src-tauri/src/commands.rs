use crate::config::{store::ConfigStore, Config};
use crate::gateway::server::Gateway;
use crate::logging::{LogEntry, RequestLog};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

pub struct AppState {
    pub store: ConfigStore,
    pub log: RequestLog,
    pub gateway: Mutex<Option<Gateway>>,
}

impl AppState {
    pub fn new(store: ConfigStore) -> Self {
        let log = RequestLog::new(500);
        AppState { store, log, gateway: Mutex::new(None) }
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub requests_served: u64,
}

#[tauri::command]
pub async fn gateway_status(state: State<'_, Arc<AppState>>) -> Result<GatewayStatus, String> {
    let guard = state.gateway.lock().await;
    Ok(match guard.as_ref() {
        Some(g) => GatewayStatus {
            running: true,
            port: Some(g.bound_port),
            requests_served: g.requests_served(),
        },
        None => GatewayStatus { running: false, port: None, requests_served: 0 },
    })
}

#[tauri::command]
pub async fn gateway_start(state: State<'_, Arc<AppState>>) -> Result<u16, String> {
    let mut guard = state.gateway.lock().await;
    if let Some(g) = guard.as_ref() {
        return Ok(g.bound_port);
    }
    let config = state.store.shared();
    let gw = crate::gateway::server::start(config, state.log.clone())
        .await
        .map_err(|e| e.to_string())?;
    let port = gw.bound_port;
    *guard = Some(gw);
    Ok(port)
}

#[tauri::command]
pub async fn gateway_stop(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut guard = state.gateway.lock().await;
    if let Some(gw) = guard.take() {
        gw.shutdown().await;
    }
    Ok(())
}

#[tauri::command]
pub fn get_config(state: State<'_, Arc<AppState>>) -> Config {
    state.store.snapshot()
}

#[tauri::command]
pub fn save_config(state: State<'_, Arc<AppState>>, config: Config) -> Result<(), String> {
    state.store.save(config).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recent_logs(state: State<'_, Arc<AppState>>, limit: usize) -> Vec<LogEntry> {
    state.log.recent(limit.clamp(1, 500))
}

use crate::claude::{agents, settings as claude_settings};
use crate::claude::agents::ModelChoice;
use crate::config::{store::ConfigStore, validate::validate, Config, TakeoverState};
use crate::gateway::server::Gateway;
use crate::logging::{LogEntry, RequestLog};
use std::path::{Path, PathBuf};
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

    /// Claude Code 侧的两个路径：`~/.claude/settings.json` 与 `%APPDATA%\cc-router\backups`。
    pub fn claude_paths(&self) -> (PathBuf, PathBuf) {
        (claude_settings::claude_settings_path(), claude_settings::backups_dir())
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

// ---------------------------------------------------------------- 接管（spec §7）

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverStatusDto {
    /// `"applied"` | `"stale"` | `"not_applied"`。
    pub state: String,
    pub gateway_url: String,
    /// 状态为 `stale` 时，settings.json 里实际写着的 baseUrl。
    pub found: Option<String>,
    pub applied_at: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDto {
    /// `"verbatim"`（逐字节回放原始字节）| `"merged_manifest"`（按清单合并式回放）。
    pub path: String,
    pub changed_keys: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDto {
    pub path: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
}

fn status_dto(cfg: &Config, settings_path: &Path) -> TakeoverStatusDto {
    use claude_settings::TakeoverState as FileState;
    let (state, found) = match claude_settings::takeover_state(cfg, settings_path) {
        FileState::Applied => ("applied".to_string(), None),
        FileState::Stale { found } => ("stale".to_string(), Some(found)),
        FileState::NotApplied => ("not_applied".to_string(), None),
    };
    TakeoverStatusDto {
        state,
        gateway_url: claude_settings::gateway_url(cfg),
        found,
        applied_at: cfg.takeover.applied_at.clone(),
    }
}

#[tauri::command]
pub fn takeover_status(state: State<'_, Arc<AppState>>) -> TakeoverStatusDto {
    let cfg = state.store.snapshot();
    let (settings_path, _) = state.claude_paths();
    status_dto(&cfg, &settings_path)
}

#[tauri::command]
pub fn takeover_apply(state: State<'_, Arc<AppState>>) -> Result<TakeoverStatusDto, String> {
    let mut cfg = state.store.snapshot();

    // 前置检查（spec §7.2 步骤 1）：配置校验通过 + 三个角色槽位都已绑定。
    let missing: Vec<&str> = [
        ("main", cfg.roles.main.is_some()),
        ("fast", cfg.roles.fast.is_some()),
        ("subagent", cfg.roles.subagent.is_some()),
    ]
    .iter()
    .filter(|(_, bound)| !bound)
    .map(|(name, _)| *name)
    .collect();
    if !missing.is_empty() {
        return Err(format!(
            "以下角色槽位尚未绑定：{}。请先在「角色路由」里为它们选择模型",
            missing.join("、")
        ));
    }
    validate(&cfg).map_err(|errs| {
        errs.iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    let (settings_path, backups_dir) = state.claude_paths();
    let manifest = claude_settings::apply_takeover(&cfg, &settings_path, &backups_dir)
        .map_err(|e| e.to_string())?;

    cfg.takeover.enabled = true;
    cfg.takeover.applied_at = Some(manifest.applied_at.clone());
    cfg.takeover.backup_file = Some(manifest.pre_bytes_file.clone());
    cfg.takeover.manifest = Some(serde_json::to_value(&manifest).map_err(|e| e.to_string())?);

    if let Err(e) = state.store.save(cfg.clone()) {
        // 清单没落盘 = 用户将无法精确还原。立即把 settings.json 滚回接管前，
        // 绝不留下"已改文件、无还原依据"的状态。
        let rollback = claude_settings::restore(&manifest, &settings_path, &backups_dir);
        return Err(match rollback {
            Ok(_) => format!("保存接管清单失败，已回滚 settings.json：{e}"),
            Err(rb) => format!("保存接管清单失败（{e}），且回滚 settings.json 也失败（{rb}）：请用 {backups_dir:?} 里的 *.pre.bak 手工还原"),
        });
    }

    Ok(status_dto(&cfg, &settings_path))
}

#[tauri::command]
pub fn takeover_restore(state: State<'_, Arc<AppState>>) -> Result<RestoreDto, String> {
    let mut cfg = state.store.snapshot();
    let (settings_path, backups_dir) = state.claude_paths();

    let manifest = match cfg.takeover.manifest.clone() {
        Some(value) => serde_json::from_value::<claude_settings::TakeoverManifest>(value)
            .map_err(|e| format!("接管清单已损坏（{e}）：请用 backups 目录里的 *.pre.bak 手工还原"))?,
        None if cfg.takeover.enabled => {
            return Err("接管清单缺失，请用备份文件手动还原".to_string())
        }
        None => return Err("当前没有接管记录，无需还原".to_string()),
    };

    let outcome = claude_settings::restore(&manifest, &settings_path, &backups_dir)
        .map_err(|e| e.to_string())?;

    cfg.takeover = TakeoverState::default();
    state.store.save(cfg).map_err(|e| e.to_string())?;

    Ok(RestoreDto {
        path: match outcome.path {
            claude_settings::RestorePath::Verbatim => "verbatim".to_string(),
            claude_settings::RestorePath::MergedManifest => "merged_manifest".to_string(),
        },
        changed_keys: outcome.changed_keys,
    })
}

// ---------------------------------------------------------------- 子 Agent（spec §8）

#[tauri::command]
pub fn agents_list() -> Vec<AgentDto> {
    // 注意：本命令按计划返回裸 `Vec`（不返回 Result），列目录失败时只能给出空表。
    agents::list_agents(&agents::agents_dir())
        .unwrap_or_default()
        .into_iter()
        .map(|a| AgentDto {
            path: a.path.to_string_lossy().to_string(),
            name: a.name,
            description: a.description,
            model: a.model,
        })
        .collect()
}

fn parse_choice<'a>(choice: &str, alias: Option<&'a str>) -> Result<ModelChoice<'a>, String> {
    match choice {
        "inherit" => Ok(ModelChoice::Inherit),
        "subagent_default" => Ok(ModelChoice::SubagentDefault),
        "alias" => alias
            .filter(|a| !a.trim().is_empty())
            .map(ModelChoice::Alias)
            .ok_or_else(|| "choice=alias 时必须提供非空的 alias".to_string()),
        other => Err(format!(
            "未知的 model 选择：{other}（只能是 inherit | subagent_default | alias）"
        )),
    }
}

#[tauri::command]
pub fn agent_set_model(
    state: State<'_, Arc<AppState>>,
    path: String,
    choice: String,
    alias: Option<String>,
) -> Result<(), String> {
    let (_, backups_dir) = state.claude_paths();
    let choice = parse_choice(&choice, alias.as_deref())?;
    agents::set_model(&PathBuf::from(path), choice, &backups_dir).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agent_create(
    state: State<'_, Arc<AppState>>,
    name: String,
    description: String,
    choice: String,
    alias: Option<String>,
    body: String,
) -> Result<String, String> {
    let (_, backups_dir) = state.claude_paths();
    let choice = parse_choice(&choice, alias.as_deref())?;
    agents::create_agent(
        &agents::agents_dir(),
        &name,
        &description,
        choice,
        &body,
        &backups_dir,
    )
    .map(|p| p.to_string_lossy().to_string())
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agent_delete(state: State<'_, Arc<AppState>>, path: String) -> Result<(), String> {
    let (_, backups_dir) = state.claude_paths();
    agents::delete_agent(&PathBuf::from(path), &backups_dir).map_err(|e| e.to_string())
}

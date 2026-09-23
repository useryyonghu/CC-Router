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

// ---------------------------------------------------------------- 预设目录（spec §5.6 / A1）

/// IPC 视图与领域类型同构（`Preset` 已 `Serialize` + camelCase），因此不手抄 19 个字段。
pub type PresetDto = crate::preset::Preset;

#[tauri::command]
pub fn presets_list() -> Vec<PresetDto> {
    match crate::preset::load_presets_on_disk() {
        Ok(list) => list,
        Err(e) => {
            // 计划把签名钉成裸 `Vec`（无 Result），所以这里只能回退 + 在 stderr 留痕：
            // 一份写坏的 presets.user.json 不应该让界面变成空列表却不留任何线索。
            eprintln!("[cc-router] 预设目录加载失败（{e}），本次回退到内置目录");
            crate::preset::load_presets(crate::preset::presets_builtin(), None).unwrap_or_default()
        }
    }
}

// ---------------------------------------------------------------- 服务商与模型（spec §5.8 / A2）

pub type ProviderDto = crate::config::Provider;
pub type TargetDto = crate::config::Target;
pub type NewProviderDto = crate::provider::NewProvider;
pub type TestResultDto = crate::provider::TestResult;

/// 所有写命令的统一节奏：快照 → 改 → 经 `ConfigStore::save` 校验落盘。
/// 校验失败时内存与磁盘都不变（`save` 的既有语义），因此命令层不必自己回滚。
fn mutate<F>(store: &ConfigStore, edit: F) -> Result<(), String>
where
    F: FnOnce(&mut Config) -> Result<(), String>,
{
    let mut cfg = store.snapshot();
    edit(&mut cfg)?;
    store.save(cfg).map_err(|e| e.to_string())
}

/// 同 [`mutate`]，但把新建对象的返回值（provider id / model 别名）带出来。
fn mutate_returning<T, F>(store: &ConfigStore, edit: F) -> Result<T, String>
where
    F: FnOnce(&mut Config) -> Result<T, String>,
{
    let mut cfg = store.snapshot();
    let value = edit(&mut cfg)?;
    store.save(cfg).map_err(|e| e.to_string())?;
    Ok(value)
}

/// 新增服务商。**不自动绑定角色**（spec §5.8：由 UI 询问后再调 `role_set`）。
#[tauri::command]
pub fn provider_add(state: State<'_, Arc<AppState>>, new: NewProviderDto) -> Result<String, String> {
    mutate_returning(&state.store, |cfg| {
        crate::provider::add_provider(cfg, new).map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub fn provider_update(
    state: State<'_, Arc<AppState>>,
    provider: ProviderDto,
) -> Result<(), String> {
    mutate(&state.store, |cfg| {
        crate::provider::update_provider(cfg, provider).map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub fn provider_remove(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    mutate(&state.store, |cfg| {
        crate::provider::remove_provider(cfg, &id).map_err(|e| e.to_string())
    })
}

/// 新增模型，返回自动生成的别名（`[1M]` 后缀剥离并置 `context1m`）。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn model_add(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
    name: Option<String>,
    context_1m: bool,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
) -> Result<String, String> {
    mutate_returning(&state.store, |cfg| {
        crate::provider::add_model(
            cfg,
            &provider_id,
            &model_id,
            name.as_deref(),
            context_1m,
            context_window,
            max_tokens,
        )
        .map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub fn model_remove(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
) -> Result<(), String> {
    mutate(&state.store, |cfg| {
        crate::provider::remove_model(cfg, &provider_id, &model_id).map_err(|e| e.to_string())
    })
}

/// `role = "main" | "fast" | "subagent"`；`target = null` 表示解绑。
#[tauri::command]
pub fn role_set(
    state: State<'_, Arc<AppState>>,
    role: String,
    target: Option<TargetDto>,
) -> Result<(), String> {
    let slot = crate::provider::RoleSlot::parse(&role).map_err(|e| e.to_string())?;
    mutate(&state.store, |cfg| {
        crate::provider::set_role(cfg, slot, target).map_err(|e| e.to_string())
    })
}

/// 测试连接：实际发一条最小 `POST /v1/messages`（15s 超时）。
/// 刻意不写请求日志、不计入 `requestsServed`（spec §9.3：那不是网关转发的请求）。
#[tauri::command]
pub async fn provider_test(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: Option<String>,
) -> Result<TestResultDto, String> {
    let cfg = state.store.snapshot();
    crate::provider::test_connection(&cfg, &provider_id, model_id.as_deref())
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 一键获取模型（spec §5.7 / A3）

pub type FetchOutcomeDto = crate::provider::fetch::FetchOutcome;

/// `models_add_many` 的单条入参（前端把拉取结果原样回传；`displayName` 作为 `name` 的别名兼容）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchPickDto {
    pub id: String,
    #[serde(default, alias = "displayName")]
    pub name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

/// 用 `ConfigStore` 里的 provider 拉取模型列表；`modelsUrl` 只用于**失败后的手动重试**
/// （临时覆盖候选，不写回 `provider.modelsUrl`）。成功时把来源与数量记进 `modelsFetch`。
#[tauri::command]
pub async fn models_fetch(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    models_url: Option<String>,
) -> Result<FetchOutcomeDto, String> {
    let mut cfg = state.store.snapshot();
    let mut provider = cfg
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .cloned()
        .ok_or_else(|| format!("provider '{provider_id}' 不存在"))?;
    if let Some(url) = models_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
    {
        provider.models_url = Some(url);
    }

    let client = reqwest::Client::builder()
        .connect_timeout(crate::provider::REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
    let outcome = crate::provider::fetch::fetch_models(&client, &provider)
        .await
        .map_err(|e| e.to_string())?;

    // spec §5.7：结果写入 modelsFetch —— UI 显示"上次拉取：时间 / 来源 URL / 数量"，
    // 且 `candidate_urls` 下次会优先试这个地址。
    if let Some(slot) = cfg.providers.iter_mut().find(|p| p.id == provider_id) {
        slot.models_fetch = Some(crate::config::ModelsFetch {
            last_at: chrono::Local::now().to_rfc3339(),
            last_url: outcome.used_url.clone(),
            count: outcome.models.len(),
        });
    }
    if let Err(e) = state.store.save(cfg) {
        // 拉取本身已经成功：不能因为"记不上时间戳"就让 UI 以为失败。
        eprintln!("[cc-router] 记录 modelsFetch 失败（本次拉取结果仍然有效）: {e}");
    }
    Ok(outcome)
}

/// 批量加入模型（逐条走 `provider::add_model`，最后一次落盘）；返回新别名。
#[tauri::command]
pub fn models_add_many(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    models: Vec<FetchPickDto>,
) -> Result<Vec<String>, String> {
    mutate_returning(&state.store, |cfg| {
        let mut aliases = Vec::with_capacity(models.len());
        for pick in &models {
            let alias = crate::provider::add_model(
                cfg,
                &provider_id,
                &pick.id,
                pick.name.as_deref(),
                false,
                pick.context_window,
                pick.max_tokens,
            )
            .map_err(|e| e.to_string())?;
            aliases.push(alias);
        }
        Ok(aliases)
    })
}

/// 重启网关：先停（若在跑）再用当前配置启动，返回新的绑定端口（spec §5.5 改端口后立即生效）。
#[tauri::command]
pub async fn gateway_restart(state: State<'_, Arc<AppState>>) -> Result<u16, String> {
    let mut guard = state.gateway.lock().await;
    if let Some(gw) = guard.take() {
        gw.shutdown().await;
    }
    let config = state.store.shared();
    let gw = crate::gateway::server::start(config, state.log.clone())
        .await
        .map_err(|e| e.to_string())?;
    let port = gw.bound_port;
    *guard = Some(gw);
    Ok(port)
}

// ---------------------------------------------------------------- 令牌 / 备份 / 路径（spec §9.2 / §10.5）

/// 重新生成本地令牌；若已接管则同步重写 `settings.json`（属于接管更新，**不触发新备份**）。
///
/// 网关与配置共享同一个 `Arc<RwLock<Config>>`，所以 `save` 之后新令牌立即生效，无需重启网关。
#[tauri::command]
pub fn token_regenerate(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let mut cfg = state.store.snapshot();
    let token = crate::config::generate_local_token();
    cfg.gateway.local_token = token.clone();

    let refresh = if cfg.takeover.enabled {
        let value = cfg.takeover.manifest.clone().ok_or_else(|| {
            "已标记接管但缺少还原清单，拒绝只改一半：请先「一键还原」再重新接管".to_string()
        })?;
        let manifest: claude_settings::TakeoverManifest = serde_json::from_value(value)
            .map_err(|e| format!("接管清单已损坏（{e}）：请先「一键还原」再重新接管"))?;
        let (settings_path, backups_dir) = state.claude_paths();
        Some((manifest, settings_path, backups_dir))
    } else {
        None
    };

    if let Some((manifest, settings_path, backups_dir)) = refresh.as_ref() {
        // 先改文件再落配置：`refresh_takeover` 是"先校验后原子写"，失败时两边都没变；
        // 而落配置失败时用**内存里的旧配置**把文件回滚，绝不留下
        // "config.json 是新令牌、Claude Code 拿旧令牌" 的错配（那会让每个请求都 401）。
        refresh_takeover_token(&cfg, manifest, settings_path, backups_dir)?;
    }
    if let Err(e) = state.store.save(cfg) {
        if let Some((manifest, settings_path, backups_dir)) = refresh.as_ref() {
            let previous = state.store.snapshot();
            let _ = refresh_takeover_token(&previous, manifest, settings_path, backups_dir);
        }
        return Err(format!("保存新令牌失败（已把 Claude Code 配置回滚为旧令牌）：{e}"));
    }
    Ok(token)
}

/// 就地刷新已接管 `settings.json` 里的令牌与模型别名（spec §9.2）。
///
/// 逻辑上属于 `claude::settings`，但那里由并行的另一个 agent 正在改，故暂放在命令层：
/// 只依赖它的公开 API（`build_env_updates` / `TakeoverManifest::post_bytes_file` / `atomic_write_bytes`）。
/// 只写己方拥有的键（`build_env_updates`），外部新增的键与顶层结构原样保留；
/// 只覆盖 post 快照，**不新建 pre 备份**，因此精确还原（spec §7.3）依旧成立。
fn refresh_takeover_token(
    cfg: &Config,
    manifest: &claude_settings::TakeoverManifest,
    settings_path: &Path,
    backups_dir: &Path,
) -> Result<(), String> {
    let bytes = std::fs::read(settings_path)
        .map_err(|e| format!("读取 {} 失败: {e}", settings_path.display()))?;
    let mut doc: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} 不是合法 JSON: {e}", settings_path.display()))?;
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| format!("{} 顶层必须是 JSON 对象", settings_path.display()))?;
    if !obj.contains_key("env") {
        obj.insert("env".to_string(), serde_json::Value::Object(serde_json::Map::new()));
    }
    let env = obj
        .get_mut("env")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| format!("{} 的 env 必须是 JSON 对象", settings_path.display()))?;
    for (name, value) in claude_settings::build_env_updates(cfg) {
        env.insert(name, serde_json::Value::String(value));
    }

    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| format!("序列化失败: {e}"))?;
    text.push('\n');
    let after = text.into_bytes();
    crate::claude::atomic_write_bytes(settings_path, &after).map_err(|e| e.to_string())?;
    if !manifest.post_bytes_file.is_empty() {
        crate::claude::atomic_write_bytes(&backups_dir.join(&manifest.post_bytes_file), &after)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupDto {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

/// `%APPDATA%\cc-router\backups` 下的文件（新的在前）。目录不存在时返回空表，不报错。
#[tauri::command]
pub fn backups_list() -> Vec<BackupDto> {
    let dir = crate::app_paths::backups_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<BackupDto> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(BackupDto {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry.path().to_string_lossy().to_string(),
                size_bytes: meta.len(),
                modified_at: meta
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<chrono::Local>::from(t).to_rfc3339()),
            })
        })
        .collect();
    out.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    out
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPathsDto {
    pub config_path: String,
    pub settings_path: String,
    pub backups_dir: String,
    pub agents_dir: String,
    pub presets_user_path: String,
    pub logs_dir: String,
}

/// 供 UI 的「打开配置文件 / 备份目录」使用（`settingsPath` 即 `~/.claude/settings.json`）。
#[tauri::command]
pub fn settings_paths() -> SettingsPathsDto {
    let s = |p: PathBuf| p.to_string_lossy().to_string();
    SettingsPathsDto {
        config_path: s(crate::app_paths::config_path()),
        settings_path: s(claude_settings::claude_settings_path()),
        backups_dir: s(claude_settings::backups_dir()),
        agents_dir: s(agents::agents_dir()),
        presets_user_path: s(crate::preset::presets_user_path()),
        logs_dir: s(crate::app_paths::logs_dir()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthStyle, GatewayConfig, ModelsFetch, Provider as ConfigProvider, Roles, Target, UiConfig,
        UnknownModelPolicy, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };
    use crate::provider::RoleSlot;

    fn provider(id: &str, base: &str, models: &[(&str, &str)]) -> ConfigProvider {
        ConfigProvider {
            id: id.into(),
            name: id.into(),
            base_url: base.into(),
            api_key: format!("{id}-key"),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models: models
                .iter()
                .map(|(mid, alias)| crate::config::ModelSpec {
                    id: (*mid).into(),
                    name: (*mid).into(),
                    alias: (*alias).into(),
                    context_1m: false,
                    context_window: None,
                    max_tokens: None,
                })
                .collect(),
        }
    }

    fn cfg() -> Config {
        Config {
            version: 1,
            gateway: GatewayConfig {
                bind: DEFAULT_BIND.into(),
                port: 8787,
                local_token: "sk-ccr-test-token".into(),
                max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
                connect_timeout_ms: 10_000,
                idle_timeout_ms: 300_000,
            },
            providers: vec![
                provider("kimi", "https://api.moonshot.cn/anthropic", &[("k3", "ccr-kimi-k3")]),
                provider("deepseek", "https://api.deepseek.com/anthropic", &[("ds-pro", "ccr-deepseek-ds-pro")]),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            },
            extra_routes: Vec::new(),
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    /// 命令层刻意不再抄一遍校验规则：`provider_add` 的入参校验就是 `provider::add_provider` 的职责，
    /// 本用例把这个契约钉住（空/非 http/空密钥都必须被拒，且不得留下半个 provider）。
    #[test]
    fn provider_add_rejects_empty_and_invalid_input() {
        let mut c = cfg();
        let err = crate::provider::add_provider(&mut c, NewProviderDto::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("baseUrl"), "{err}");

        let err = crate::provider::add_provider(
            &mut c,
            NewProviderDto { base_url: "api.example.com".into(), api_key: "sk-1".into(), ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("baseUrl"), "{err}");

        let err = crate::provider::add_provider(
            &mut c,
            NewProviderDto { base_url: "https://api.example.com".into(), api_key: "   ".into(), ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("apiKey"), "{err}");
        assert_eq!(c.providers.len(), 2, "校验失败时不得改动配置");

        let id = crate::provider::add_provider(
            &mut c,
            NewProviderDto { base_url: "https://api.example.com".into(), api_key: "sk-1".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(id, "example");
        assert_eq!(
            c.roles.main,
            Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
            "provider_add 不自动绑定角色（spec §5.8：由 UI 追问后再调 role_set）"
        );
    }

    #[test]
    fn role_set_rejects_unknown_role_strings() {
        for good in ["main", "fast", "subagent", " MAIN "] {
            assert!(RoleSlot::parse(good).is_ok(), "{good} 应当可解析");
        }
        let err = RoleSlot::parse("primary").unwrap_err().to_string();
        assert!(err.contains("primary"), "{err}");
        assert!(err.contains("main | fast | subagent"), "错误信息要给出合法取值：{err}");
    }

    #[test]
    fn token_regeneration_changes_the_token_and_stays_valid() {
        let mut c = cfg();
        let before = c.gateway.local_token.clone();
        c.gateway.local_token = crate::config::generate_local_token();
        assert_ne!(c.gateway.local_token, before, "重新生成必须真的换掉令牌");
        assert!(c.gateway.local_token.starts_with("sk-ccr-"));
        assert_eq!(c.gateway.local_token.len(), "sk-ccr-".len() + 32);
        assert_eq!(
            crate::config::validate::validate(&c),
            Ok(()),
            "新令牌必须仍能通过校验，否则 save 会失败"
        );
    }

    /// 令牌变更后同步重写 `settings.json`（spec §9.2）：只动己方的键、保留外部键、
    /// **不新建 pre 备份**，且精确还原（spec §7.3 的逐字节回放）依然可用。
    #[test]
    fn refresh_takeover_token_rewrites_settings_and_keeps_restore_exact() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let backups = dir.path().join("backups");
        std::fs::write(
            &settings_path,
            br#"{"env":{"KEEP_ME":"1"},"permissions":{"allow":["Bash"]}}"#,
        )
        .unwrap();

        let mut c = cfg();
        let manifest =
            claude_settings::apply_takeover_at(&c, &settings_path, &backups, "STAMP").unwrap();
        let pre_before = std::fs::read(backups.join(&manifest.pre_bytes_file)).unwrap();
        let taken_over = std::fs::read_to_string(&settings_path).unwrap();
        assert!(taken_over.contains(&c.gateway.local_token), "接管时必须写入令牌");
        assert!(taken_over.contains("ccr-kimi-k3"), "接管时必须写入别名");

        c.gateway.local_token = "sk-ccr-brand-new-token".into();
        refresh_takeover_token(&c, &manifest, &settings_path, &backups).unwrap();

        let after = std::fs::read_to_string(&settings_path).unwrap();
        assert!(after.contains("sk-ccr-brand-new-token"), "{after}");
        assert!(!after.contains("sk-ccr-test-token"), "旧令牌必须被替换：{after}");
        assert!(after.contains("KEEP_ME"), "外部 env 键必须保留：{after}");
        assert!(after.contains("permissions"), "外部顶层键必须保留：{after}");
        assert_eq!(
            std::fs::read(backups.join(&manifest.pre_bytes_file)).unwrap(),
            pre_before,
            "刷新不触发新备份：pre 快照必须逐字节不变"
        );

        // post 快照已更新 → 还原仍走 Verbatim（逐字节回放）
        let outcome = claude_settings::restore(&manifest, &settings_path, &backups).unwrap();
        assert_eq!(outcome.path, claude_settings::RestorePath::Verbatim);
        assert_eq!(
            std::fs::read(&settings_path).unwrap(),
            pre_before,
            "还原后必须与接管前逐字节相同（spec §7.3）"
        );
    }

    /// `models_fetch` 成功后会写 `modelsFetch`（spec §5.7），UI 的"上次拉取"与
    /// `candidate_urls` 的优先项都依赖它。
    #[test]
    fn models_fetch_record_shape_is_serializable_and_camel_case() {
        let record = ModelsFetch {
            last_at: chrono::Local::now().to_rfc3339(),
            last_url: "https://api.deepseek.com/models".into(),
            count: 12,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["lastUrl"], "https://api.deepseek.com/models");
        assert_eq!(json["count"], 12);
        assert!(json.get("lastAt").is_some());
    }

    /// 前端 `pickFromFetched` 只带 `id`/`name`/`contextWindow`/`maxTokens`；
    /// 也容忍直接把拉取结果（`displayName`）回传。
    #[test]
    fn fetch_pick_accepts_both_name_shapes() {
        let pick: FetchPickDto =
            serde_json::from_str(r#"{"id":"m1","name":"M1","contextWindow":200000,"maxTokens":8192}"#)
                .unwrap();
        assert_eq!(pick.name.as_deref(), Some("M1"));
        assert_eq!(pick.context_window, Some(200_000));
        assert_eq!(pick.max_tokens, Some(8192));

        let pick: FetchPickDto = serde_json::from_str(r#"{"id":"m2","displayName":"M2"}"#).unwrap();
        assert_eq!(pick.name.as_deref(), Some("M2"), "displayName 是 name 的别名");
        assert_eq!(pick.context_window, None);

        let pick: FetchPickDto = serde_json::from_str(r#"{"id":"m3"}"#).unwrap();
        assert_eq!(pick.name, None);
    }
}

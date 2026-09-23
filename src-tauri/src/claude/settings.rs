//! `~/.claude/settings.json` 的接管与**逐字节精确还原**（spec §7）。
//!
//! # 为什么能逐字节还原（AC8）
//!
//! 重新序列化 JSON 会重排键序、重写空白，永远做不到"字节级相等"。因此接管时保存**两份原始
//! 字节快照**：接管前的 `*.pre.bak` 与接管后写出的 `*.post.bak`。还原时若当前文件字节仍等于
//! post 快照（说明接管后没有别的程序动过它），就**直接写回 pre 快照的原始字节**
//! （[`RestorePath::Verbatim`]）——这是唯一能保证字节级相等的方式。
//!
//! 若文件被外部改过（例如 Claude Code 自己往 `env` 里加了键），则退化为按清单合并式回放
//! （[`RestorePath::MergedManifest`]）：己方的键恢复原值/删除，外部新增的键保留。

use super::{atomic_write_bytes, backup_stamp};
use crate::config::{Config, ModelSpec, Provider, Target};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const BASE_URL_KEY: &str = "ANTHROPIC_BASE_URL";
pub const AUTH_TOKEN_KEY: &str = "ANTHROPIC_AUTH_TOKEN";
pub const SUBAGENT_MODEL_KEY: &str = "CLAUDE_CODE_SUBAGENT_MODEL";

/// 本应用可能**写入**的全部键（spec §7.1，存在则覆盖）。
/// 实际写哪些取决于角色槽位是否绑定：未绑定的槽位对应的键一个都不写。
///
/// `CLAUDE_CODE_SUBAGENT_MODEL` 没有配套的 `*_MODEL_NAME`：spec §7.1 的写入清单里没有它，
/// 凭空写一个 Claude Code 不认识的键会违反"只动自己拥有的键"。
pub const OWNED_KEYS: [&str; 11] = [
    BASE_URL_KEY,
    AUTH_TOKEN_KEY,
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    SUBAGENT_MODEL_KEY,
];

/// 接管时**移除**的旧键（spec §7.1）：它们会把请求钉死在单一模型、绕过别名路由，或与新键冲突。
pub const REMOVED_KEYS: [&str; 3] =
    ["ANTHROPIC_MODEL", "ANTHROPIC_API_KEY", "ANTHROPIC_SMALL_FAST_MODEL"];

/// 本网关的 Anthropic 兼容入口地址（spec §7.1 第一行）。
pub fn gateway_url(cfg: &Config) -> String {
    format!("http://127.0.0.1:{}", cfg.gateway.port)
}

/// `~/.claude`（Windows 上取 `USERPROFILE`，与 `app_paths` 取 `APPDATA` 同一风格）。
pub fn claude_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
}

pub fn claude_settings_path() -> PathBuf {
    claude_dir().join("settings.json")
}

/// 备份根目录：`%APPDATA%\cc-router\backups`。
pub fn backups_dir() -> PathBuf {
    crate::app_paths::backups_dir()
}

/// 单个键的原值快照（spec §7.2 步骤 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySnapshot {
    pub existed: bool,
    #[serde(default)]
    pub value: Option<String>,
}

/// 一次接管的还原清单。**存进 `config.json` 的 `takeover.manifest`**，
/// 因此应用重启后仍能精确还原。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverManifest {
    pub applied_at: String,
    /// 接管前 `settings.json` 是否存在。为 false 时还原必须**删除**该文件（spec §7.3）：
    /// 只看 `env_existed` 无法区分"文件不存在"与"文件存在但没有 env"。
    #[serde(default)]
    pub file_existed: bool,
    /// pre 快照文件名（相对 `backups_dir`）。接管前文件不存在时为空串。
    pub pre_bytes_file: String,
    /// post 快照文件名（相对 `backups_dir`）。
    pub post_bytes_file: String,
    /// 接管前顶层是否就有 `env` 键：没有时还原要把 `env` 整体删除。
    pub env_existed: bool,
    /// 所有被写入或被移除的键的原值。
    pub keys: BTreeMap<String, KeySnapshot>,
}

/// 还原走的是哪条路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorePath {
    /// 文件自接管后未被外部改动 → 直接写回 pre 快照的原始字节（逐字节相等）。
    Verbatim,
    /// 文件被外部改过 → 按清单合并式回放，外部新增的键保留。
    MergedManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub path: RestorePath,
    /// 真正被改回的键（含整体增删的 `env`）。
    pub changed_keys: Vec<String>,
}

/// 接管状态（用于 spec §7.4 的防呆横幅）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverState {
    Applied,
    /// `settings.json` 的 baseUrl 已不是本网关地址 —— 被其它程序改写。
    Stale { found: String },
    NotApplied,
}

/// 只计算**要写入的键**（未绑定槽位不产生键）。`*_MODEL_NAME` 是 spec §7.1 要求成对写入的
/// 人类可读展示名（`"<provider名> / <模型显示名>"`），它不参与路由。
pub fn build_env_updates(cfg: &Config) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(BASE_URL_KEY.to_string(), gateway_url(cfg));
    out.insert(AUTH_TOKEN_KEY.to_string(), cfg.gateway.local_token.clone());

    // spec §7.1：fable → opus → default 是 main 家族的回填链，三者都写 main 的别名。
    for prefix in ["ANTHROPIC_DEFAULT_OPUS", "ANTHROPIC_DEFAULT_SONNET", "ANTHROPIC_DEFAULT_FABLE"] {
        if let Some((alias, display)) = alias_and_display(cfg, &cfg.roles.main) {
            out.insert(format!("{prefix}_MODEL"), alias);
            out.insert(format!("{prefix}_MODEL_NAME"), display);
        }
    }
    if let Some((alias, display)) = alias_and_display(cfg, &cfg.roles.fast) {
        out.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), alias);
        out.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME".to_string(), display);
    }
    if let Some((alias, _)) = alias_and_display(cfg, &cfg.roles.subagent) {
        out.insert(SUBAGENT_MODEL_KEY.to_string(), alias);
    }
    // 不变量：写入的键必须都在 OWNED_KEYS 里（新增键时忘了登记会在这里炸掉测试）。
    debug_assert!(
        out.keys().all(|k| OWNED_KEYS.contains(&k.as_str())),
        "build_env_updates 产出了 OWNED_KEYS 之外的键: {:?}",
        out.keys().collect::<Vec<_>>()
    );
    out
}

fn target_of<'a>(cfg: &'a Config, slot: &Option<Target>) -> Option<(&'a Provider, &'a ModelSpec)> {
    let t = slot.as_ref()?;
    let p = cfg.providers.iter().find(|p| p.id == t.provider_id)?;
    let m = p.models.iter().find(|m| m.id == t.model_id)?;
    Some((p, m))
}

fn alias_and_display(cfg: &Config, slot: &Option<Target>) -> Option<(String, String)> {
    let (p, m) = target_of(cfg, slot)?;
    Some((m.alias.clone(), format!("{} / {}", p.name, m.name)))
}

/// 接管。时间戳取当前本地时间，用于备份文件名。
pub fn apply_takeover(
    cfg: &Config,
    settings_path: &Path,
    backups_dir: &Path,
) -> Result<TakeoverManifest> {
    apply_takeover_at(cfg, settings_path, backups_dir, &backup_stamp())
}

/// 与 [`apply_takeover`] 相同，但可显式指定备份时间戳。
/// 测试靠它稳定地复现"同一 stamp 下第二次接管"这一场景（spec §7.2 步骤 3）。
pub fn apply_takeover_at(
    cfg: &Config,
    settings_path: &Path,
    backups_dir: &Path,
    stamp: &str,
) -> Result<TakeoverManifest> {
    let file_existed = settings_path.exists();
    let before: Vec<u8> = if file_existed {
        std::fs::read(settings_path).map_err(|e| Error::io(settings_path.to_path_buf(), e))?
    } else {
        b"{}".to_vec()
    };

    // 先把一切能失败的事情做完，再动盘（spec §7 关键设计 7：JSON 非法时不写任何东西）。
    let mut doc: Value = serde_json::from_slice(&before).map_err(|e| Error::Json {
        path: settings_path.to_path_buf(),
        source: e,
    })?;
    let obj = doc.as_object_mut().ok_or_else(|| {
        Error::ConfigInvalid(format!("{} 顶层必须是 JSON 对象", settings_path.display()))
    })?;

    let env_existed = obj.contains_key("env");
    if env_existed && !obj["env"].is_object() {
        return Err(Error::ConfigInvalid(format!(
            "{} 的 env 必须是 JSON 对象",
            settings_path.display()
        )));
    }
    let env_map: Map<String, Value> = obj
        .get("env")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let updates = build_env_updates(cfg);
    let mut keys: BTreeMap<String, KeySnapshot> = BTreeMap::new();
    for name in updates.keys().map(String::as_str).chain(REMOVED_KEYS.iter().copied()) {
        let existing = env_map.get(name);
        if let Some(v) = existing {
            if !v.is_string() {
                // 只有字符串才能进清单；否则还原时会静默丢值。宁可不接管。
                return Err(Error::ConfigInvalid(format!(
                    "env.{name} 不是字符串，无法安全接管（还原时会丢失原值，请先手工修正 {}）",
                    settings_path.display()
                )));
            }
        }
        keys.insert(
            name.to_string(),
            KeySnapshot {
                existed: existing.is_some(),
                value: existing.and_then(|v| v.as_str()).map(str::to_string),
            },
        );
    }

    if !env_existed {
        obj.insert("env".to_string(), Value::Object(Map::new()));
    }
    let env = obj
        .get_mut("env")
        .and_then(|v| v.as_object_mut())
        .expect("env 已确保为对象");
    for (name, value) in &updates {
        env.insert(name.clone(), Value::String(value.clone()));
    }
    for name in REMOVED_KEYS {
        env.remove(name);
    }

    let mut text = serde_json::to_string_pretty(&doc)
        .map_err(|e| Error::ConfigInvalid(format!("序列化 {} 失败: {e}", settings_path.display())))?;
    text.push('\n');
    let after = text.into_bytes();

    // 备份：**同一个 stamp 只备份一次 pre**。第二次接管若覆盖 pre，就会把"已接管的内容"
    // 当成原始状态，精确还原当场失效。
    let file_name = settings_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "settings.json".to_string());
    let pre_bytes_file = if file_existed { format!("{file_name}.{stamp}.pre.bak") } else { String::new() };
    let post_bytes_file = format!("{file_name}.{stamp}.post.bak");
    if file_existed {
        write_if_absent(&backups_dir.join(&pre_bytes_file), &before)?;
    }
    // **post 必须先于 settings.json 落盘**：post 既是"我们写出的字节"的凭据，也是还原判据。
    // 若反过来，settings.json 写成功后 post 写失败，就会留下"文件已被接管、却没有任何还原
    // 依据"的状态 —— 清单要到本函数返回 Ok 之后才由上层落盘。post 总是覆盖。
    atomic_write_bytes(&backups_dir.join(&post_bytes_file), &after)?;
    atomic_write_bytes(settings_path, &after)?;

    Ok(TakeoverManifest {
        applied_at: chrono::Local::now().to_rfc3339(),
        file_existed,
        pre_bytes_file,
        post_bytes_file,
        env_existed,
        keys,
    })
}

/// 还原（spec §7.3）。返回实际走的路径与被改回的键。
pub fn restore(
    manifest: &TakeoverManifest,
    settings_path: &Path,
    backups_dir: &Path,
) -> Result<RestoreOutcome> {
    let current: Vec<u8> = if settings_path.exists() {
        std::fs::read(settings_path).map_err(|e| Error::io(settings_path.to_path_buf(), e))?
    } else {
        Vec::new()
    };
    let post = read_backup(backups_dir, &manifest.post_bytes_file);
    let pre = read_backup(backups_dir, &manifest.pre_bytes_file);

    let unmodified = post.as_deref() == Some(current.as_slice());
    let (target, path) = if !manifest.file_existed {
        // 接管前整个文件都不存在 → 这个文件完全是我们创建的，还原就是"删掉它"（spec §7.3）。
        // 但若接管后被外部改动过，删除就会毁掉别人的内容 —— 那种情况必须**拒绝**，
        // 且绝不能把它谎报成"合并式回放（MergedManifest）"。
        if settings_path.exists() && !unmodified {
            return Err(Error::ConfigInvalid(format!(
                "接管前 {} 并不存在，但接管后它被外部改动过；还原会删除这些内容，已拒绝执行。\
                 请先人工核对（我们写出的版本在 {}），确认可以丢弃后再删除该文件并重试。",
                settings_path.display(),
                backups_dir.join(&manifest.post_bytes_file).display()
            )));
        }
        (None, RestorePath::Verbatim)
    } else if unmodified && pre.is_some() {
        (pre, RestorePath::Verbatim)
    } else {
        (Some(merged_replay(&current, manifest, settings_path)?), RestorePath::MergedManifest)
    };

    let changed_keys = diff_keys(&current, target.as_deref(), &manifest.keys);

    match target {
        Some(bytes) => atomic_write_bytes(settings_path, &bytes)?,
        None => {
            if settings_path.exists() {
                std::fs::remove_file(settings_path)
                    .map_err(|e| Error::io(settings_path.to_path_buf(), e))?;
            }
        }
    }
    Ok(RestoreOutcome { path, changed_keys })
}

/// spec §7.4：`settings.json` 的 `ANTHROPIC_BASE_URL` 是否仍等于本网关地址。
/// 文件缺失或无法解析时一律返回 [`TakeoverState::NotApplied`]——绝不宣称"已接管"。
pub fn takeover_state(cfg: &Config, settings_path: &Path) -> TakeoverState {
    let Ok(bytes) = std::fs::read(settings_path) else {
        return TakeoverState::NotApplied;
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return TakeoverState::NotApplied;
    };
    match doc.get("env").and_then(|e| e.get(BASE_URL_KEY)).and_then(Value::as_str) {
        None => TakeoverState::NotApplied,
        Some(found) if found == gateway_url(cfg) => TakeoverState::Applied,
        Some(found) => TakeoverState::Stale { found: found.to_string() },
    }
}

fn read_backup(backups_dir: &Path, file_name: &str) -> Option<Vec<u8>> {
    if file_name.is_empty() {
        return None;
    }
    std::fs::read(backups_dir.join(file_name)).ok()
}

fn write_if_absent(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    atomic_write_bytes(path, bytes)
}

/// 按清单把己方的键合并回当前内容：`existed=true` 写回原值，`existed=false` 删除；
/// 外部新增的键一律保留。
fn merged_replay(
    current: &[u8],
    manifest: &TakeoverManifest,
    settings_path: &Path,
) -> Result<Vec<u8>> {
    let mut doc: Value = if current.is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_slice(current).map_err(|e| Error::Json {
            path: settings_path.to_path_buf(),
            source: e,
        })?
    };
    let obj = doc.as_object_mut().ok_or_else(|| {
        Error::ConfigInvalid(format!("{} 顶层必须是 JSON 对象", settings_path.display()))
    })?;

    let needs_env = manifest.keys.values().any(|s| s.existed && s.value.is_some());
    // `env` 存在但不是对象：合并式回放**无法**执行。绝不能静默跳过 —— 那会让调用方
    // 以为"所有键都还原了"（changed_keys 还会显示"无事发生"），而实际上是**一个键都没还原**。
    if let Some(value) = obj.get("env") {
        if !value.is_object() {
            return Err(Error::ConfigInvalid(format!(
                "{} 的 env 不是 JSON 对象，无法做合并式还原；请用 backups 目录里的 *.pre.bak 手工恢复",
                settings_path.display()
            )));
        }
    }
    if needs_env && !obj.contains_key("env") {
        obj.insert("env".to_string(), Value::Object(Map::new()));
    }
    if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
        for (name, snap) in &manifest.keys {
            match (&snap.value, snap.existed) {
                (Some(value), _) => {
                    env.insert(name.clone(), Value::String(value.clone()));
                }
                (None, _) => {
                    env.remove(name);
                }
            }
        }
    }

    // 原本没有 env：还原时要把 env 整体删除。但若外部往 env 里加了键（env 非空），
    // 删除会毁掉别人的数据 —— 此时保留 env，只删己方的键（路径已标记为 MergedManifest）。
    if !manifest.env_existed {
        let empty = obj.get("env").and_then(Value::as_object).map(Map::is_empty).unwrap_or(false);
        if empty {
            obj.remove("env");
        }
    }

    let mut text = serde_json::to_string_pretty(&doc)
        .map_err(|e| Error::ConfigInvalid(format!("序列化 {} 失败: {e}", settings_path.display())))?;
    text.push('\n');
    Ok(text.into_bytes())
}

/// 比较还原前后己方键的取值，列出真正被改回的键（含整体增删的 `env`）。
fn diff_keys(
    current: &[u8],
    target: Option<&[u8]>,
    keys: &BTreeMap<String, KeySnapshot>,
) -> Vec<String> {
    let before: Option<Value> = serde_json::from_slice(current).ok();
    let after: Option<Value> = target.and_then(|b| serde_json::from_slice(b).ok());

    // 整个文件被删除：己方键与 env 都算变化。
    if target.is_none() && !current.is_empty() {
        let mut out: Vec<String> = keys.keys().cloned().collect();
        out.push("env".to_string());
        out.sort();
        out.dedup();
        return out;
    }

    let env_at = |doc: &Option<Value>| doc.as_ref().and_then(|d| d.get("env")).cloned();
    let (env_before, env_after) = (env_at(&before), env_at(&after));

    let mut out: Vec<String> = keys
        .keys()
        .filter(|name| {
            let a = env_before.as_ref().and_then(|e| e.get(*name));
            let b = env_after.as_ref().and_then(|e| e.get(*name));
            a != b
        })
        .cloned()
        .collect();
    let has_env = |doc: &Option<Value>| doc.as_ref().map(|d| d.get("env").is_some()).unwrap_or(false);
    if before.is_some() && has_env(&before) != has_env(&after) {
        out.push("env".to_string());
    }
    out.sort();
    out.dedup();
    out
}

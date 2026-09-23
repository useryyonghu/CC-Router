pub mod store;
pub mod validate;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 8787;
pub const DEFAULT_BIND: &str = "127.0.0.1";
pub const DEFAULT_MAX_BODY_BYTES: usize = 134_217_728; // 128 MiB
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub extra_routes: Vec<ExtraRoute>,
    pub on_unknown_model: UnknownModelPolicy,
    #[serde(default)]
    pub default_target: Option<Target>,
    #[serde(default)]
    pub takeover: TakeoverState,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    pub bind: String,
    pub port: u16,
    pub local_token: String,
    pub max_request_body_bytes: usize,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub models_url: Option<String>,
    #[serde(default)]
    pub models_fetch: Option<ModelsFetch>,
    #[serde(default)]
    pub models: Vec<ModelSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    Both,
    XApiKey,
    Bearer,
}

impl Default for AuthStyle {
    fn default() -> Self {
        AuthStyle::Both
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsFetch {
    pub last_at: String,
    pub last_url: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub alias: String,
    #[serde(default)]
    pub context_1m: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Roles {
    #[serde(default)]
    pub main: Option<Target>,
    #[serde(default)]
    pub fast: Option<Target>,
    #[serde(default)]
    pub subagent: Option<Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtraRoute {
    pub alias: String,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UnknownModelPolicy {
    Default,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    pub close_to_tray: bool,
    pub autostart: bool,
    pub request_log_to_file: bool,
    pub restore_on_exit: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            close_to_tray: true,
            autostart: true,
            request_log_to_file: false,
            restore_on_exit: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverState {
    pub enabled: bool,
    #[serde(default)]
    pub applied_at: Option<String>,
    #[serde(default)]
    pub backup_file: Option<String>,
    #[serde(default)]
    pub settings_keys: serde_json::Value,
    #[serde(default)]
    pub agent_files: serde_json::Value,
    /// `claude::settings::TakeoverManifest` 的序列化形式：**精确还原的唯一依据**。
    ///
    /// 存进配置而不仅放内存，是为了让还原在应用重启后仍然可用；`serde(default)` 保证
    /// 没有该字段的旧 config.json（本字段加入之前写的）仍能正常反序列化。
    #[serde(default)]
    pub manifest: Option<serde_json::Value>,
}

impl Config {
    /// 首次运行用的默认配置：无 provider、无角色绑定、策略 default、令牌随机生成。
    pub fn first_run() -> Self {
        Config {
            version: 1,
            gateway: GatewayConfig {
                bind: DEFAULT_BIND.to_string(),
                port: DEFAULT_PORT,
                local_token: generate_local_token(),
                max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
                connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
                idle_timeout_ms: DEFAULT_IDLE_TIMEOUT_MS,
            },
            providers: Vec::new(),
            roles: Roles::default(),
            extra_routes: Vec::new(),
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: None,
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }
}

pub fn generate_local_token() -> String {
    use rand::Rng;
    const HEX: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    let mut s = String::from("sk-ccr-");
    for _ in 0..32 {
        s.push(HEX[rng.gen_range(0..16)] as char);
    }
    s
}

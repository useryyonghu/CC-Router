//! 服务商与模型的增删改（spec §5.8、§10.2 / Plan 3 A2）。
//!
//! 全部是**作用于 `Config` 的纯函数**（除 [`test_connection`] 需要发一次网络请求外），
//! 因此可以完全脱离 Tauri 单测：命令层只负责参数校验与 `ConfigStore::save`。
//!
//! 本模块不依赖 Tauri（硬约束）；`add_model` 的别名生成复用 `routing::alias`，
//! `test_connection` 的鉴权头复用 `gateway::rewrite`（both 同发两种头）——避免第二套实现漂移。

#[cfg(test)]
mod mock;

use crate::config::{AuthStyle, Config, ModelSpec, Provider, Target};
use crate::error::{Error, Result};
use crate::routing::alias::{derive_provider_id, generate_alias, strip_1m_marker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// 角色槽位（spec §7.1 的三个槽）。字符串解析只在命令层边界发生。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleSlot {
    Main,
    Fast,
    Subagent,
}

impl RoleSlot {
    pub const ALL: [RoleSlot; 3] = [RoleSlot::Main, RoleSlot::Fast, RoleSlot::Subagent];

    /// 大小写与首尾空白容错；非法值给出可选清单，而不是静默忽略。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_lowercase().as_str() {
            "main" => Ok(RoleSlot::Main),
            "fast" => Ok(RoleSlot::Fast),
            "subagent" => Ok(RoleSlot::Subagent),
            other => Err(Error::ConfigInvalid(format!(
                "未知的角色 '{other}'（只能是 main | fast | subagent）"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RoleSlot::Main => "main",
            RoleSlot::Fast => "fast",
            RoleSlot::Subagent => "subagent",
        }
    }

    fn slot_mut<'a>(self, cfg: &'a mut Config) -> &'a mut Option<Target> {
        match self {
            RoleSlot::Main => &mut cfg.roles.main,
            RoleSlot::Fast => &mut cfg.roles.fast,
            RoleSlot::Subagent => &mut cfg.roles.subagent,
        }
    }
}

/// 新增服务商的最小输入（spec §5.8：最少必填 `baseUrl` + `apiKey`）。
///
/// 同时充当 IPC 的 `NewProviderDto`：`#[serde(rename_all = "camelCase")]` 与前端调用一致。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProvider {
    pub base_url: String,
    pub api_key: String,
    /// 缺省由 `baseUrl` 域名推导（`api.moonshot.cn` → `api.moonshot.cn`）。
    #[serde(default)]
    pub name: Option<String>,
    /// 缺省 `both`（spec §5.8：用户不必理解该概念）。
    #[serde(default)]
    pub auth_style: Option<AuthStyle>,
    /// 缺省 `"custom"`（spec §5.8 的自定义路径）；从预设创建时由前端传预设 id。
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub models_url: Option<String>,
}

/// 测试连接的结果（spec §10.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    #[serde(default)]
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub message: String,
}

/// 测试连接与拉取模型的统一超时（spec §5.7 / §10.2：15s）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// HTTP 状态码的常见原因（spec §5.7「常见原因在 UI 直接解释」）。
/// 拉取失败信息与测试连接共用同一套措辞，避免两处说法不一致。
pub fn status_hint(status: u16) -> Option<&'static str> {
    match status {
        401 => Some("密钥无效或无权限"),
        403 => Some("地区或账号限制"),
        404 => Some("厂商不提供该接口，或地址/模型名不对"),
        429 => Some("触发限流"),
        _ => None,
    }
}

/// 新增服务商。返回新 `id`；`name` 缺省取 `baseUrl` 的主机名。
pub fn add_provider(cfg: &mut Config, new: NewProvider) -> Result<String> {
    let base_url = new.base_url.trim().to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(Error::ConfigInvalid(format!(
            "baseUrl 必须以 http:// 或 https:// 开头（收到 {base_url:?}）"
        )));
    }
    if new.api_key.trim().is_empty() {
        return Err(Error::ConfigInvalid("apiKey 不能为空".to_string()));
    }

    let taken: HashSet<String> = cfg.providers.iter().map(|p| p.id.clone()).collect();
    let id = derive_provider_id(&base_url, &taken);
    let name = new
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| host_of(&base_url));

    cfg.providers.push(Provider {
        id: id.clone(),
        name,
        base_url,
        api_key: new.api_key,
        auth_style: new.auth_style.unwrap_or_default(),
        preset_id: Some(new.preset_id.unwrap_or_else(|| "custom".to_string())),
        models_url: new.models_url,
        models_fetch: None,
        models: Vec::new(),
    });
    Ok(id)
}

/// 覆写已有服务商（按 `id` 定位）。找不到则报错，绝不悄悄追加一条同 id 的重复项。
pub fn update_provider(cfg: &mut Config, provider: Provider) -> Result<()> {
    let Some(slot) = cfg.providers.iter_mut().find(|p| p.id == provider.id) else {
        return Err(Error::ConfigInvalid(format!(
            "provider '{}' 不存在",
            provider.id
        )));
    };
    *slot = provider;
    // 覆写可能删掉了某个模型：指向它的角色/默认目标/额外规则必须一并清掉，
    // 否则配置会因"目标不存在"而整份保存失败，而用户只改了个名字。
    prune_dangling(cfg);
    Ok(())
}

/// 删除服务商，并清掉指向它的角色绑定、默认目标与额外规则（spec §A2 规则）。
pub fn remove_provider(cfg: &mut Config, id: &str) -> Result<()> {
    let before = cfg.providers.len();
    cfg.providers.retain(|p| p.id != id);
    if cfg.providers.len() == before {
        return Err(Error::ConfigInvalid(format!("provider '{id}' 不存在")));
    }
    prune_dangling(cfg);
    Ok(())
}

/// 新增模型：`[1M]` 后缀剥离并置 `context1m`；别名经 `routing::alias::generate_alias` 生成并返回。
pub fn add_model(
    cfg: &mut Config,
    provider_id: &str,
    model_id: &str,
    name: Option<&str>,
    context_1m: bool,
    context_window: Option<u64>,
    max_tokens: Option<u64>,
) -> Result<String> {
    let (clean, had_marker) = strip_1m_marker(model_id.trim());
    let clean = clean.trim().to_string();
    if clean.is_empty() {
        return Err(Error::ConfigInvalid("模型 id 不能为空".to_string()));
    }

    let taken = taken_aliases(cfg);
    let provider = cfg
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| Error::ConfigInvalid(format!("provider '{provider_id}' 不存在")))?;
    if provider.models.iter().any(|m| m.id == clean) {
        return Err(Error::ConfigInvalid(format!(
            "provider '{provider_id}' 下已存在模型 '{clean}'"
        )));
    }

    let alias = generate_alias(provider_id, &clean, &taken);
    let display = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| clean.clone());
    provider.models.push(ModelSpec {
        id: clean,
        name: display,
        alias: alias.clone(),
        context_1m: context_1m || had_marker,
        context_window,
        max_tokens,
    });
    Ok(alias)
}

/// 删除模型，并清掉指向它的角色绑定、默认目标与额外规则。
pub fn remove_model(cfg: &mut Config, provider_id: &str, model_id: &str) -> Result<()> {
    {
        let provider = cfg
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| Error::ConfigInvalid(format!("provider '{provider_id}' 不存在")))?;
        let before = provider.models.len();
        provider.models.retain(|m| m.id != model_id);
        if provider.models.len() == before {
            return Err(Error::ConfigInvalid(format!(
                "provider '{provider_id}' 下不存在模型 '{model_id}'"
            )));
        }
    }
    prune_dangling(cfg);
    Ok(())
}

/// 绑定/解绑角色槽位（`target = None` 表示未绑定，spec §7.2 允许）。
/// 绑定时必须指向真实存在的 provider + model，避免留下需要等到保存才暴露的坏引用。
pub fn set_role(cfg: &mut Config, role: RoleSlot, target: Option<Target>) -> Result<()> {
    if let Some(t) = &target {
        if !target_resolves(cfg, t) {
            return Err(Error::ConfigInvalid(format!(
                "provider '{}' 下不存在模型 '{}'",
                t.provider_id, t.model_id
            )));
        }
    }
    *role.slot_mut(cfg) = target;
    Ok(())
}

/// 测试连接：发一条最小 `POST /v1/messages`（`max_tokens: 16`、单轮 user），15s 超时。
///
/// 刻意**不写请求日志、不计入 `requestsServed`**（spec §9.3：本应用只转发 Claude Code 的请求
/// 与「测试连接」按钮；测试连接不是网关转发的请求，混进日志会让统计失真）。
pub async fn test_connection(
    cfg: &Config,
    provider_id: &str,
    model_id: Option<&str>,
) -> Result<TestResult> {
    let provider = cfg
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| Error::ConfigInvalid(format!("provider '{provider_id}' 不存在")))?;

    let model = match model_id {
        Some(wanted) => provider
            .models
            .iter()
            .find(|m| m.id == wanted)
            .map(|m| m.id.clone())
            .ok_or_else(|| {
                Error::ConfigInvalid(format!("provider '{provider_id}' 下不存在模型 '{wanted}'"))
            })?,
        None => provider.models.first().map(|m| m.id.clone()).ok_or_else(|| {
            Error::ConfigInvalid(format!(
                "provider '{provider_id}' 下还没有模型：请先添加模型，或指定 modelId"
            ))
        })?,
    };

    // 复用网关的 URL 拼接与鉴权头注入（both 同发 x-api-key 与 Bearer）。
    let target = crate::routing::resolve::ResolvedTarget {
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        auth_style: provider.auth_style,
        upstream_model: model.clone(),
        alias: String::new(),
        matched_by: crate::routing::resolve::MatchedBy::Passthrough,
        roles: Vec::new(),
        context_1m: false,
    };
    let url = crate::gateway::rewrite::upstream_url(&provider.base_url, "/v1/messages", None);
    let headers = crate::gateway::rewrite::build_headers(&target, &hyper::HeaderMap::new()).headers;
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "ping" }],
    });

    let client = reqwest::Client::builder()
        .connect_timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| Error::Gateway(format!("构造 HTTP 客户端失败: {e}")))?;
    let mut request = client.post(&url).json(&body).timeout(REQUEST_TIMEOUT);
    for (name, value) in headers.iter() {
        request = request.header(name.as_str(), value.as_bytes());
    }

    let started = Instant::now();
    let result = match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let mut message = format!("HTTP {status}");
            if let Some(hint) = status_hint(status) {
                message.push_str(&format!("（{hint}）"));
            }
            let detail = summarize(&text);
            if !detail.is_empty() {
                message.push_str(&format!(": {detail}"));
            }
            TestResult {
                ok: (200..300).contains(&status),
                status: Some(status),
                latency_ms: started.elapsed().as_millis() as u64,
                message,
            }
        }
        Err(e) => TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            message: if e.is_timeout() {
                format!("请求超时（{}s）: {e}", REQUEST_TIMEOUT.as_secs())
            } else {
                format!("请求失败: {e}")
            },
        },
    };
    Ok(result)
}

/// `baseUrl` 的主机名（含端口），作为缺省显示名。
fn host_of(base_url: &str) -> String {
    let rest = base_url
        .trim()
        .strip_prefix("https://")
        .or_else(|| base_url.trim().strip_prefix("http://"))
        .unwrap_or(base_url.trim());
    let host = rest.split('/').next().unwrap_or("").trim();
    if host.is_empty() {
        base_url.trim().to_string()
    } else {
        host.to_string()
    }
}

/// 全局已占用别名（含 `extraRoutes`），比较键与校验、路由一致：`trim().to_lowercase()`。
fn taken_aliases(cfg: &Config) -> HashSet<String> {
    cfg.providers
        .iter()
        .flat_map(|p| p.models.iter().map(|m| m.alias.trim().to_lowercase()))
        .chain(cfg.extra_routes.iter().map(|r| r.alias.trim().to_lowercase()))
        .collect()
}

fn target_resolves(cfg: &Config, t: &Target) -> bool {
    cfg.providers
        .iter()
        .any(|p| p.id == t.provider_id && p.models.iter().any(|m| m.id == t.model_id))
}

/// 清掉所有指向不存在的 provider/model 的角色绑定、默认目标与额外规则。
/// 这样"删掉一条模型"不会让整份配置因为悬空引用而保存失败（spec §A2）。
fn prune_dangling(cfg: &mut Config) {
    let valid: HashSet<(String, String)> = cfg
        .providers
        .iter()
        .flat_map(|p| p.models.iter().map(move |m| (p.id.clone(), m.id.clone())))
        .collect();
    let ok = |t: &Target| valid.contains(&(t.provider_id.clone(), t.model_id.clone()));

    for slot in [
        &mut cfg.roles.main,
        &mut cfg.roles.fast,
        &mut cfg.roles.subagent,
    ] {
        if slot.as_ref().is_some_and(|t| !ok(t)) {
            *slot = None;
        }
    }
    if cfg.default_target.as_ref().is_some_and(|t| !ok(t)) {
        cfg.default_target = None;
    }
    cfg.extra_routes
        .retain(|r| valid.contains(&(r.provider_id.clone(), r.model_id.clone())));
}

/// 错误正文摘要：去掉换行、截断，避免把整页 HTML 塞进 UI 提示。
fn summarize(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 300 {
        flat
    } else {
        let cut: String = flat.chars().take(300).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ExtraRoute, GatewayConfig, Roles, TakeoverState, UiConfig, UnknownModelPolicy,
        DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };

    fn model(id: &str, alias: &str) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            name: id.into(),
            alias: alias.into(),
            context_1m: false,
            context_window: None,
            max_tokens: None,
        }
    }

    fn provider(id: &str, base: &str, models: Vec<ModelSpec>) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: base.into(),
            api_key: format!("{id}-key"),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models,
        }
    }

    /// 与 `config::validate` 的测试夹具同构：kimi(main) + deepseek(fast/subagent/default)。
    fn cfg() -> Config {
        Config {
            version: 1,
            gateway: GatewayConfig {
                bind: DEFAULT_BIND.into(),
                port: 8787,
                local_token: "sk-ccr-test".into(),
                max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
                connect_timeout_ms: 10_000,
                idle_timeout_ms: 300_000,
            },
            providers: vec![
                provider("kimi", "https://api.moonshot.cn/anthropic", vec![model("k3", "ccr-kimi-k3")]),
                provider("deepseek", "https://api.deepseek.com/anthropic", vec![model("ds-pro", "ccr-deepseek-ds-pro")]),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            },
            extra_routes: vec![ExtraRoute {
                alias: "ccr-extra".into(),
                provider_id: "kimi".into(),
                model_id: "k3".into(),
            }],
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    fn new_provider(base: &str, key: &str) -> NewProvider {
        NewProvider { base_url: base.into(), api_key: key.into(), ..Default::default() }
    }

    #[test]
    fn add_provider_derives_id_and_defaults() {
        let mut c = cfg();
        let id = add_provider(&mut c, new_provider("https://api.moonshot.ai/anthropic", "sk-new")).unwrap();
        assert_eq!(id, "moonshot");
        let added = c.providers.iter().find(|p| p.id == id).unwrap();
        assert_eq!(added.name, "api.moonshot.ai", "name 缺省取主机名");
        assert_eq!(added.auth_style, AuthStyle::Both, "authStyle 缺省 both");
        assert_eq!(added.preset_id.as_deref(), Some("custom"), "spec §5.8：自定义路径 presetId=custom");
        assert!(added.models.is_empty());
        assert_eq!(added.api_key, "sk-new");

        let dup = add_provider(&mut c, new_provider("https://api.moonshot.cn/anthropic", "sk-dup")).unwrap();
        assert_eq!(dup, "moonshot-2", "推导出的 id 冲突时必须加后缀");
        let id2 = add_provider(&mut c, new_provider("https://api.deepseek.com/anthropic", "sk-2")).unwrap();
        assert_eq!(id2, "deepseek-2");

        let named = add_provider(
            &mut c,
            NewProvider {
                base_url: "https://example.com".into(),
                api_key: "sk-3".into(),
                name: Some("  My Provider  ".into()),
                auth_style: Some(AuthStyle::XApiKey),
                preset_id: Some("some-preset".into()),
                models_url: Some("https://example.com/models".into()),
            },
        )
        .unwrap();
        let added = c.providers.iter().find(|p| p.id == named).unwrap();
        assert_eq!(added.name, "My Provider", "首尾空白必须清掉");
        assert_eq!(added.auth_style, AuthStyle::XApiKey);
        assert_eq!(added.preset_id.as_deref(), Some("some-preset"));
        assert_eq!(added.models_url.as_deref(), Some("https://example.com/models"));
    }

    #[test]
    fn add_provider_rejects_empty_or_non_http_input() {
        let mut c = cfg();
        for bad in ["", "   ", "api.moonshot.cn", "ftp://api.moonshot.cn"] {
            let err = add_provider(&mut c, new_provider(bad, "sk-x")).unwrap_err();
            assert!(err.to_string().contains("baseUrl"), "{bad:?} → {err}");
        }
        let err = add_provider(&mut c, new_provider("https://ok.example.com", "  ")).unwrap_err();
        assert!(err.to_string().contains("apiKey"), "{err}");
        assert_eq!(c.providers.len(), 2, "失败时不得留下半个 provider");
    }

    #[test]
    fn remove_provider_clears_dangling_references() {
        let mut c = cfg();
        assert_eq!(c.roles.main, Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }));
        remove_provider(&mut c, "kimi").unwrap();

        assert_eq!(c.roles.main, None, "指向被删 provider 的角色必须解绑");
        assert!(c.extra_routes.is_empty(), "指向被删 provider 的额外规则必须删除");
        assert_eq!(
            c.roles.fast,
            Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            "其它 provider 的绑定不受影响"
        );
        assert!(c.default_target.is_some());

        let mut c2 = cfg();
        c2.default_target = Some(Target { provider_id: "kimi".into(), model_id: "k3".into() });
        remove_provider(&mut c2, "kimi").unwrap();
        assert_eq!(c2.default_target, None, "悬空的默认目标必须置 None");

        assert!(remove_provider(&mut c2, "kimi").is_err(), "删除不存在的 provider 必须报错");
    }

    #[test]
    fn add_model_generates_unique_alias_and_strips_1m_marker() {
        let mut c = cfg();
        let alias = add_model(&mut c, "kimi", "k3-pro[1M]", Some("K3 Pro"), false, Some(200_000), None).unwrap();
        assert_eq!(alias, "ccr-kimi-k3-pro");
        let added = c.providers[0].models.iter().find(|m| m.id == "k3-pro").unwrap();
        assert!(added.context_1m, "[1M] 后缀必须置 context1m=true");
        assert_eq!(added.id, "k3-pro", "id 必须剥离 [1M]");
        assert_eq!(added.name, "K3 Pro");
        assert_eq!(added.context_window, Some(200_000));

        // 同名模型重复加入 → 明确报错，而不是插入两条同 id
        assert!(add_model(&mut c, "kimi", "k3-pro", None, false, None, None).is_err());
        assert!(add_model(&mut c, "nope", "x", None, false, None, None).is_err());
        assert!(add_model(&mut c, "kimi", "   ", None, false, None, None).is_err());

        // 别名冲突（slug 规范化后同名）→ 追加 -2
        let alias2 = add_model(&mut c, "kimi", "K3 Pro", None, false, None, None).unwrap();
        assert_eq!(alias2, "ccr-kimi-k3-pro-2", "K3 Pro 与 k3-pro 的 slug 相同，别名必须避让");

        // context1m 显式传入也要生效；name 缺省取模型 id
        let alias3 = add_model(&mut c, "deepseek", "ds-flash", None, true, None, Some(8192)).unwrap();
        assert_eq!(alias3, "ccr-deepseek-ds-flash");
        let ds = c.providers[1].models.iter().find(|m| m.id == "ds-flash").unwrap();
        assert!(ds.context_1m);
        assert_eq!(ds.name, "ds-flash");
        assert_eq!(ds.max_tokens, Some(8192));
    }

    #[test]
    fn alias_pool_includes_extra_routes_and_normalizes() {
        let mut c = cfg();
        c.extra_routes[0].alias = "  CCR-Kimi-K3-Pro  ".into();
        let taken = taken_aliases(&c);
        assert!(taken.contains("ccr-kimi-k3-pro"), "额外规则的别名也必须占用别名池");
        assert!(taken.contains("ccr-kimi-k3"), "模型别名按 trim+lowercase 规范化后入池");
        assert!(!taken.contains("  CCR-Kimi-K3-Pro  "));
    }

    #[test]
    fn remove_model_clears_references_and_reports_missing() {
        let mut c = cfg();
        remove_model(&mut c, "deepseek", "ds-pro").unwrap();
        assert_eq!(c.roles.fast, None, "指向被删模型的角色必须解绑");
        assert_eq!(c.roles.subagent, None);
        assert_eq!(c.default_target, None);
        assert_eq!(c.roles.main, Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }));

        assert!(remove_model(&mut c, "deepseek", "ds-pro").is_err());
        assert!(remove_model(&mut c, "nope", "x").is_err());
    }

    #[test]
    fn set_role_binds_unbinds_and_rejects_unknown_targets() {
        let mut c = cfg();
        set_role(&mut c, RoleSlot::Main, None).unwrap();
        assert_eq!(c.roles.main, None, "None = 未绑定");

        let t = Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() };
        set_role(&mut c, RoleSlot::Main, Some(t.clone())).unwrap();
        assert_eq!(c.roles.main, Some(t));

        let bad = Target { provider_id: "deepseek".into(), model_id: "nope".into() };
        let err = set_role(&mut c, RoleSlot::Subagent, Some(bad)).unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        assert_eq!(c.roles.subagent, Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }));

        let bad_provider = Target { provider_id: "ghost".into(), model_id: "ds-pro".into() };
        assert!(set_role(&mut c, RoleSlot::Fast, Some(bad_provider)).is_err());
    }

    #[test]
    fn role_slot_parse_is_tolerant_but_rejects_unknown() {
        assert_eq!(RoleSlot::parse(" MAin ").unwrap(), RoleSlot::Main);
        assert_eq!(RoleSlot::parse("fast").unwrap(), RoleSlot::Fast);
        assert_eq!(RoleSlot::parse("SubAgent").unwrap(), RoleSlot::Subagent);
        for slot in RoleSlot::ALL {
            assert_eq!(RoleSlot::parse(slot.as_str()).unwrap(), slot);
        }
        let err = RoleSlot::parse("primary").unwrap_err();
        assert!(err.to_string().contains("primary"));
        assert!(err.to_string().contains("main | fast | subagent"));
    }

    #[test]
    fn update_provider_replaces_and_prunes_removed_models() {
        let mut c = cfg();
        let mut updated = c.providers[1].clone();
        updated.name = "DeepSeek 新版".into();
        updated.models.clear();
        update_provider(&mut c, updated).unwrap();

        assert_eq!(c.providers[1].name, "DeepSeek 新版");
        assert_eq!(c.providers.len(), 2, "更新不得追加新 provider");
        assert_eq!(c.roles.fast, None, "被移除模型上的绑定必须清掉");
        assert_eq!(c.default_target, None);

        let ghost = provider("ghost", "https://ghost.example.com", vec![]);
        assert!(update_provider(&mut c, ghost).is_err());
    }

    #[tokio::test]
    async fn test_connection_sends_a_minimal_message_with_provider_credentials() {
        let mock = mock::MockUpstream::start(|_path| {
            (200, r#"{"id":"msg_1","type":"message","content":[]}"#.to_string())
        })
        .await;
        let mut c = cfg();
        c.providers[0].base_url = mock.base_url.clone();

        let result = test_connection(&c, "kimi", Some("k3")).await.unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.status, Some(200));
        assert!(result.message.contains("200"), "{}", result.message);

        let sent = mock.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, "POST");
        assert_eq!(sent[0].path, "/v1/messages");
        assert_eq!(sent[0].body["model"], "k3", "测试用的必须是上游模型 id（不是别名）");
        assert_eq!(sent[0].body["max_tokens"], 16);
        assert_eq!(sent[0].body["messages"][0]["role"], "user");
        assert_eq!(sent[0].api_key.as_deref(), Some("kimi-key"), "authStyle=both 两种头都发");
        assert_eq!(sent[0].authorization.as_deref(), Some("Bearer kimi-key"));
        mock.shutdown().await;
    }

    #[tokio::test]
    async fn test_connection_reports_upstream_error_status_with_hint() {
        let mock = mock::MockUpstream::start(|_path| {
            (401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.to_string())
        })
        .await;
        let mut c = cfg();
        c.providers[0].base_url = mock.base_url.clone();

        let result = test_connection(&c, "kimi", None).await.unwrap();
        assert!(!result.ok);
        assert_eq!(result.status, Some(401));
        assert!(result.message.contains("401"), "{}", result.message);
        assert!(result.message.contains("密钥无效或无权限"), "{}", result.message);
        assert!(result.message.contains("invalid x-api-key"), "正文摘要必须带上：{}", result.message);
        assert!(mock.requests().len() == 1, "modelId=None 时用第一个模型，仍要发出请求");
        mock.shutdown().await;
    }

    #[tokio::test]
    async fn test_connection_requires_a_known_model() {
        let c = cfg();
        assert!(test_connection(&c, "nope", None).await.is_err());
        let err = test_connection(&c, "kimi", Some("ghost")).await.unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");

        let mut empty = cfg();
        empty.providers[0].models.clear();
        let err = test_connection(&empty, "kimi", None).await.unwrap_err();
        assert!(err.to_string().contains("还没有模型"), "{err}");
    }
}

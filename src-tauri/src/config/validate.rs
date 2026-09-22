use super::{Config, Target};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: String,
    pub message: String,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError { field: field.into(), message: message.into() }
    }
}

fn valid_provider_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0] as char;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| matches!(*b as char, 'a'..='z' | '0'..='9' | '-'))
}

pub fn validate(cfg: &Config) -> Result<(), Vec<ConfigError>> {
    let mut errs = Vec::new();

    if !(1024..=65535).contains(&cfg.gateway.port) {
        errs.push(ConfigError::new("gateway.port", "端口必须在 1024..=65535"));
    }
    if cfg.gateway.bind != "127.0.0.1" && cfg.gateway.bind != "::1" && cfg.gateway.bind != "localhost" {
        errs.push(ConfigError::new(
            "gateway.bind",
            "只允许监听回环地址（127.0.0.1 / ::1 / localhost）",
        ));
    }
    if cfg.gateway.local_token.trim().is_empty() {
        errs.push(ConfigError::new("gateway.localToken", "本地令牌不能为空"));
    }
    if cfg.gateway.max_request_body_bytes == 0 {
        errs.push(ConfigError::new("gateway.maxRequestBodyBytes", "请求体上限必须大于 0"));
    }

    let mut provider_ids: HashSet<&str> = HashSet::new();
    let mut aliases: HashSet<&str> = HashSet::new();

    for (pi, p) in cfg.providers.iter().enumerate() {
        let pf = |f: &str| format!("providers[{pi}].{f}");
        if !valid_provider_id(&p.id) {
            errs.push(ConfigError::new(pf("id"), "必须是 ^[a-z0-9][a-z0-9-]{0,31}$"));
        }
        if !provider_ids.insert(p.id.as_str()) {
            errs.push(ConfigError::new(pf("id"), "provider id 重复"));
        }
        if !(p.base_url.starts_with("http://") || p.base_url.starts_with("https://")) {
            errs.push(ConfigError::new(pf("baseUrl"), "必须以 http:// 或 https:// 开头"));
        } else if is_self_loop(&p.base_url, &cfg.gateway) {
            errs.push(ConfigError::new(
                pf("baseUrl"),
                "不允许指向本网关自身（会自环）",
            ));
        }
        if let Some(mu) = &p.models_url {
            if !(mu.starts_with("http://") || mu.starts_with("https://")) {
                errs.push(ConfigError::new(pf("modelsUrl"), "只能是完整 http(s):// URL 或 null"));
            }
        }
        let mut model_ids: HashSet<&str> = HashSet::new();
        for (mi, m) in p.models.iter().enumerate() {
            let mf = |f: &str| format!("providers[{pi}].models[{mi}].{f}");
            if m.id.trim().is_empty() {
                errs.push(ConfigError::new(mf("id"), "模型 id 不能为空"));
            }
            if !model_ids.insert(m.id.as_str()) {
                errs.push(ConfigError::new(mf("id"), "同一 provider 内模型 id 重复"));
            }
            if m.alias.trim().is_empty() {
                errs.push(ConfigError::new(mf("alias"), "别名不能为空"));
            }
            if !aliases.insert(m.alias.as_str()) {
                errs.push(ConfigError::new(mf("alias"), "别名全局重复"));
            }
        }
    }

    for r in cfg.extra_routes.iter() {
        let ef = |f: &str| format!("extraRoutes[{}].{f}", r.alias);
        if r.alias.trim().is_empty() {
            errs.push(ConfigError::new("extraRoutes[].alias", "别名不能为空"));
        } else if !aliases.insert(r.alias.as_str()) {
            errs.push(ConfigError::new(ef("alias"), "别名与已有别名冲突"));
        }
        check_target(cfg, &ef(""), &Target { provider_id: r.provider_id.clone(), model_id: r.model_id.clone() }, &mut errs);
    }

    for (name, slot) in [
        ("roles.main", &cfg.roles.main),
        ("roles.fast", &cfg.roles.fast),
        ("roles.subagent", &cfg.roles.subagent),
    ] {
        if let Some(t) = slot {
            check_target(cfg, name, t, &mut errs);
        }
    }

    if let Some(t) = &cfg.default_target {
        check_target(cfg, "defaultTarget", t, &mut errs);
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

fn check_target(cfg: &Config, field: &str, t: &Target, errs: &mut Vec<ConfigError>) {
    match cfg.providers.iter().find(|p| p.id == t.provider_id) {
        None => errs.push(ConfigError::new(
            field.to_string(),
            format!("providerId '{}' 不存在", t.provider_id),
        )),
        Some(p) => {
            if !p.models.iter().any(|m| m.id == t.model_id) {
                errs.push(ConfigError::new(
                    field.to_string(),
                    format!("provider '{}' 下不存在模型 '{}'", t.provider_id, t.model_id),
                ));
            }
        }
    }
}

/// 防自环：baseUrl 的 host:port 命中本网关监听地址即视为自环。
fn is_self_loop(base_url: &str, gw: &super::GatewayConfig) -> bool {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let host_port = rest.split('/').next().unwrap_or("");
    let host_port = host_port.split('?').next().unwrap_or(host_port);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()),
        None => (host_port, None),
    };
    let host_is_loopback = matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if !host_is_loopback {
        return false;
    }
    match port {
        Some(p) => p == gw.port,
        // 未写端口时按 scheme 默认端口比较
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthStyle, ExtraRoute, GatewayConfig, ModelSpec, Provider, Roles, TakeoverState, UiConfig,
        UnknownModelPolicy, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };

    fn gw() -> GatewayConfig {
        GatewayConfig {
            bind: DEFAULT_BIND.to_string(),
            port: 8787,
            local_token: "sk-ccr-test".to_string(),
            max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 300_000,
        }
    }

    fn provider(id: &str, base: &str, models: &[(&str, &str)]) -> Provider {
        Provider {
            id: id.to_string(),
            name: id.to_string(),
            base_url: base.to_string(),
            api_key: "k".to_string(),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models: models
                .iter()
                .map(|(mid, alias)| ModelSpec {
                    id: mid.to_string(),
                    name: mid.to_string(),
                    alias: alias.to_string(),
                    context_1m: false,
                    context_window: None,
                    max_tokens: None,
                })
                .collect(),
        }
    }

    fn base_cfg() -> Config {
        Config {
            version: 1,
            gateway: gw(),
            providers: vec![
                provider("kimi", "https://api.moonshot.cn/anthropic", &[("k3", "ccr-kimi-k3")]),
                provider("deepseek", "https://api.deepseek.com/anthropic", &[("ds-flash", "ccr-deepseek-ds-flash")]),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
            },
            extra_routes: Vec::new(),
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    #[test]
    fn accepts_a_well_formed_config() {
        assert_eq!(validate(&base_cfg()), Ok(()));
    }

    #[test]
    fn allows_unbound_roles_and_null_default_target() {
        let mut cfg = base_cfg();
        cfg.roles.main = None;
        cfg.roles.fast = None;
        cfg.roles.subagent = None;
        cfg.default_target = None;
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let mut cfg = base_cfg();
        cfg.gateway.bind = "0.0.0.0".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "gateway.bind"));
    }

    #[test]
    fn rejects_self_loop_base_url() {
        let mut cfg = base_cfg();
        cfg.providers[0].base_url = "http://127.0.0.1:8787/anthropic".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].baseUrl"));
    }

    #[test]
    fn allows_loopback_base_url_on_a_different_port() {
        let mut cfg = base_cfg();
        cfg.providers[0].base_url = "http://127.0.0.1:9999/anthropic".to_string();
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn rejects_duplicate_aliases_across_providers() {
        let mut cfg = base_cfg();
        cfg.providers[1].models[0].alias = "ccr-kimi-k3".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field.ends_with(".alias")));
    }

    #[test]
    fn rejects_extra_route_alias_colliding_with_model_alias() {
        let mut cfg = base_cfg();
        cfg.extra_routes.push(ExtraRoute {
            alias: "ccr-kimi-k3".to_string(),
            provider_id: "kimi".to_string(),
            model_id: "k3".to_string(),
        });
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field.contains("extraRoutes")));
    }

    #[test]
    fn rejects_role_pointing_at_missing_model() {
        let mut cfg = base_cfg();
        cfg.roles.main = Some(Target { provider_id: "kimi".into(), model_id: "nope".into() });
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "roles.main"));
    }

    #[test]
    fn rejects_bad_provider_id_and_relative_models_url() {
        let mut cfg = base_cfg();
        cfg.providers[0].id = "Kimi_1".to_string();
        cfg.providers[0].models_url = Some("/v1/models".to_string());
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].id"));
        assert!(errs.iter().any(|e| e.field == "providers[0].modelsUrl"));
    }

    #[test]
    fn rejects_port_out_of_range() {
        let mut cfg = base_cfg();
        cfg.gateway.port = 80;
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "gateway.port"));
    }
}

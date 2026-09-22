use super::alias::strip_1m_marker;
use crate::config::{AuthStyle, Config, UnknownModelPolicy};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    Alias,
    Family,
    Fallback,
    /// spec §6.1：非 messages 路径的 catch-all 转发，与"未知模型走 policy default"的
    /// `Fallback` 是两回事，日志必须能区分是哪条规则生效。
    Passthrough,
    /// spec §6.7：路由阶段就失败（当前为未知模型 + policy=error），没有 `ResolvedTarget`。
    Error,
}

impl MatchedBy {
    /// 日志 `matchedBy` 字段的唯一取值来源（spec §6.7）。
    pub fn as_str(self) -> &'static str {
        match self {
            MatchedBy::Alias => "alias",
            MatchedBy::Family => "family",
            MatchedBy::Fallback => "fallback",
            MatchedBy::Passthrough => "passthrough",
            MatchedBy::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Main,
    Fast,
    Subagent,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Fast => "fast",
            Role::Subagent => "subagent",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTarget {
    pub provider_id: String,
    pub provider_name: String,
    pub base_url: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    pub upstream_model: String,
    pub alias: String,
    pub matched_by: MatchedBy,
    /// 别名 → 引用该别名的角色集合（反向查，已排序）。spec §6.7 要求多值时逗号连接；
    /// 别名被清空（fallback/passthrough）时本集合也必须为空，否则日志会说"角色 fast"
    /// 却又没有别名可依。
    pub roles: Vec<Role>,
    pub context_1m: bool,
}

impl ResolvedTarget {
    /// spec §6.7：`role` 字段是别名 → 角色集合的反向查，多值逗号连接（如 `main,subagent`）；
    /// 无角色时为 `None`。`x-ccr-role`（spec §6.3）与本字段同源。
    pub fn role_label(&self) -> Option<String> {
        if self.roles.is_empty() {
            None
        } else {
            Some(self.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(","))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("模型 '{requested}' 未被 CC Router 路由：请在角色路由中为它添加规则，或设置默认目标")]
    UnknownModel { requested: String },
}

#[derive(Debug, Clone)]
struct Entry {
    provider_id: String,
    provider_name: String,
    base_url: String,
    api_key: String,
    auth_style: AuthStyle,
    upstream_model: String,
    alias: String,
    context_1m: bool,
}

#[derive(Debug, Clone)]
pub struct RouteTable {
    aliases: HashMap<String, Entry>,
    role_aliases: HashMap<Role, String>,
    default_entry: Option<Entry>,
    policy: UnknownModelPolicy,
}

impl RouteTable {
    pub fn build(cfg: &Config) -> Self {
        let mut aliases: HashMap<String, Entry> = HashMap::new();
        for p in &cfg.providers {
            for m in &p.models {
                aliases.insert(
                    m.alias.trim().to_lowercase(),
                    Entry {
                        provider_id: p.id.clone(),
                        provider_name: p.name.clone(),
                        base_url: p.base_url.clone(),
                        api_key: p.api_key.clone(),
                        auth_style: p.auth_style,
                        upstream_model: m.id.clone(),
                        alias: m.alias.clone(),
                        context_1m: m.context_1m,
                    },
                );
            }
        }
        for r in &cfg.extra_routes {
            if let Some(p) = cfg.providers.iter().find(|p| p.id == r.provider_id) {
                if let Some(m) = p.models.iter().find(|m| m.id == r.model_id) {
                    aliases.insert(
                        r.alias.trim().to_lowercase(),
                        Entry {
                            provider_id: p.id.clone(),
                            provider_name: p.name.clone(),
                            base_url: p.base_url.clone(),
                            api_key: p.api_key.clone(),
                            auth_style: p.auth_style,
                            upstream_model: m.id.clone(),
                            alias: r.alias.clone(),
                            context_1m: m.context_1m,
                        },
                    );
                }
            }
        }

        let lookup = |t: &Option<crate::config::Target>| -> Option<String> {
            let t = t.as_ref()?;
            let p = cfg.providers.iter().find(|p| p.id == t.provider_id)?;
            let m = p.models.iter().find(|m| m.id == t.model_id)?;
            Some(m.alias.clone())
        };

        let mut role_aliases = HashMap::new();
        for (role, slot) in [
            (Role::Main, &cfg.roles.main),
            (Role::Fast, &cfg.roles.fast),
            (Role::Subagent, &cfg.roles.subagent),
        ] {
            if let Some(alias) = lookup(slot) {
                role_aliases.insert(role, alias.trim().to_lowercase());
            }
        }

        let default_entry = cfg.default_target.as_ref().and_then(|t| {
            let p = cfg.providers.iter().find(|p| p.id == t.provider_id)?;
            let m = p.models.iter().find(|m| m.id == t.model_id)?;
            Some(Entry {
                provider_id: p.id.clone(),
                provider_name: p.name.clone(),
                base_url: p.base_url.clone(),
                api_key: p.api_key.clone(),
                auth_style: p.auth_style,
                upstream_model: m.id.clone(),
                alias: m.alias.clone(),
                context_1m: m.context_1m,
            })
        });

        RouteTable { aliases, role_aliases, default_entry, policy: cfg.on_unknown_model }
    }

    /// 反向查：别名 → 引用它的角色集合（用于日志展示，spec §6.7）。
    pub fn roles_for_alias(&self, alias: &str) -> Vec<Role> {
        let key = alias.trim().to_lowercase();
        let mut roles: Vec<Role> = self
            .role_aliases
            .iter()
            .filter(|(_, a)| **a == key)
            .map(|(r, _)| *r)
            .collect();
        roles.sort_by_key(|r| match r {
            Role::Main => 0,
            Role::Fast => 1,
            Role::Subagent => 2,
        });
        roles
    }

    pub fn resolve(&self, requested_model: &str) -> Result<ResolvedTarget, ResolveError> {
        let (without_marker, forced_1m) = strip_1m_marker(requested_model.trim());
        let key = without_marker.trim().to_lowercase();

        if let Some(entry) = self.aliases.get(&key) {
            return Ok(self.finish(entry.clone(), forced_1m, MatchedBy::Alias));
        }

        let family_role = if key.contains("haiku") {
            Some(Role::Fast)
        } else if key.contains("opus") || key.contains("sonnet") || key.contains("fable") {
            Some(Role::Main)
        } else {
            None
        };
        if let Some(role) = family_role {
            if let Some(alias) = self.role_aliases.get(&role) {
                if let Some(entry) = self.aliases.get(alias) {
                    // 别名命中的角色集合已在 `finish` 里反查好；家族角色必然在其中
                    // （`role_aliases[role]` 就是这个条目的别名键），无需再覆写。
                    return Ok(self.finish(entry.clone(), forced_1m, MatchedBy::Family));
                }
            }
        }

        match self.policy {
            UnknownModelPolicy::Default => match &self.default_entry {
                Some(entry) => {
                    let mut resolved = self.finish(entry.clone(), forced_1m, MatchedBy::Fallback);
                    // 别名被清空 → 角色反向查失去依据，日志里 role 必须同为 null，
                    // 否则会出现"alias 为空、role=fast"这种自相矛盾的一行。
                    resolved.alias = String::new();
                    resolved.roles.clear();
                    Ok(resolved)
                }
                None => Err(ResolveError::UnknownModel { requested: without_marker }),
            },
            UnknownModelPolicy::Error => {
                Err(ResolveError::UnknownModel { requested: without_marker })
            }
        }
    }

    /// 直接解析一个显式 Target。用于 catch-all 路径转发到 defaultTarget，
    /// 以及"未绑定角色但需要取该目标别名"的场景。返回值的 `alias` 为空、`roles` 为空
    /// （因为它不是通过别名命中的），`matched_by` 为 `Passthrough`（spec §6.1）。
    pub fn resolve_target(&self, t: &crate::config::Target) -> Option<ResolvedTarget> {
        let entry = self
            .aliases
            .values()
            .find(|e| e.provider_id == t.provider_id && e.upstream_model == t.model_id)
            .cloned()?;
        let mut r = self.finish(entry, false, MatchedBy::Passthrough);
        r.alias = String::new();
        r.roles.clear();
        Some(r)
    }

    fn finish(&self, entry: Entry, forced_1m: bool, matched_by: MatchedBy) -> ResolvedTarget {
        let roles = self.roles_for_alias(&entry.alias);
        ResolvedTarget {
            provider_id: entry.provider_id,
            provider_name: entry.provider_name,
            base_url: entry.base_url,
            api_key: entry.api_key,
            auth_style: entry.auth_style,
            upstream_model: entry.upstream_model,
            context_1m: forced_1m || entry.context_1m,
            alias: entry.alias,
            matched_by,
            roles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthStyle, ExtraRoute, GatewayConfig, ModelSpec, Provider, Roles, Target, TakeoverState,
        UiConfig, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };

    fn model(id: &str, alias: &str, context_1m: bool) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            name: id.into(),
            alias: alias.into(),
            context_1m,
            context_window: None,
            max_tokens: None,
        }
    }

    fn provider(id: &str, name: &str, base: &str, models: Vec<ModelSpec>) -> Provider {
        Provider {
            id: id.into(),
            name: name.into(),
            base_url: base.into(),
            api_key: format!("{id}-key"),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models,
        }
    }

    fn cfg_with(policy: UnknownModelPolicy, extra: Vec<ExtraRoute>) -> Config {
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
                provider("kimi", "Kimi", "https://api.moonshot.cn/anthropic", vec![model("k3", "ccr-kimi-k3", false)]),
                provider(
                    "deepseek",
                    "DeepSeek",
                    "https://api.deepseek.com/anthropic",
                    vec![model("ds-pro", "ccr-deepseek-ds-pro", true)],
                ),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            },
            extra_routes: extra,
            on_unknown_model: policy,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    #[test]
    fn resolves_alias_exactly() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let r = t.resolve("ccr-kimi-k3").unwrap();
        assert_eq!(r.provider_id, "kimi");
        assert_eq!(r.upstream_model, "k3");
        assert_eq!(r.matched_by, MatchedBy::Alias);
        assert_eq!(r.api_key, "kimi-key");
        assert!(!r.context_1m, "context1m=false on the model spec");
    }

    #[test]
    fn alias_matching_tolerates_whitespace_case_and_1m_suffix() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        for input in ["  CCR-Kimi-K3  ", "ccr-kimi-k3[1M]", "ccr-kimi-k3[1m]"] {
            let r = t.resolve(input).unwrap_or_else(|e| panic!("{input} should resolve: {e}"));
            assert_eq!(r.provider_id, "kimi", "input={input}");
        }
    }

    #[test]
    fn forced_1m_marker_enables_context_1m_even_when_model_says_false() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        assert!(t.resolve("ccr-kimi-k3[1M]").unwrap().context_1m);
        assert!(t.resolve("ccr-deepseek-ds-pro").unwrap().context_1m, "model spec sets context1m=true");
    }

    #[test]
    fn every_configured_model_is_routable_without_role_binding() {
        let mut cfg = cfg_with(UnknownModelPolicy::Error, vec![]);
        cfg.roles = Roles::default();
        let t = RouteTable::build(&cfg);
        assert_eq!(t.resolve("ccr-deepseek-ds-pro").unwrap().provider_id, "deepseek");
        assert_eq!(t.resolve("ccr-kimi-k3").unwrap().provider_id, "kimi");
    }

    #[test]
    fn family_fallback_maps_haiku_to_fast_and_opus_sonnet_fable_to_main() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        assert_eq!(t.resolve("claude-haiku-4-5-20251001").unwrap().provider_id, "deepseek");
        for m in ["claude-opus-4-5-20251101", "claude-3-5-sonnet-20241022", "claude-fable-1"] {
            let r = t.resolve(m).unwrap();
            assert_eq!(r.provider_id, "kimi", "{m} should map to main");
            assert_eq!(r.matched_by, MatchedBy::Family);
            assert_eq!(r.roles, vec![Role::Main]);
            assert_eq!(r.upstream_model, "k3");
        }
    }

    #[test]
    fn family_fallback_is_skipped_when_role_unbound_and_falls_through() {
        let mut cfg = cfg_with(UnknownModelPolicy::Default, vec![]);
        cfg.roles.main = None;
        let t = RouteTable::build(&cfg);
        let r = t.resolve("claude-opus-4-5-20251101").unwrap();
        assert_eq!(r.matched_by, MatchedBy::Fallback);
        assert_eq!(r.alias, "", "fallback responses carry no alias");
        assert!(r.roles.is_empty(), "no alias → nothing to reverse-look-up (spec §6.7)");
        assert_eq!(r.role_label(), None);
    }

    /// spec §6.2 规则 1 必须先于规则 2：别名精确匹配优先于 Claude 家族兜底，
    /// 即使别名本身含家族词（`haiku`）也必须走别名。
    #[test]
    fn alias_match_wins_over_family_fallback_even_when_the_alias_contains_a_family_word() {
        let mut cfg = cfg_with(UnknownModelPolicy::Error, vec![]);
        cfg.providers[0].models.push(model("haiku-x", "ccr-haiku-x", false));
        let t = RouteTable::build(&cfg);

        let r = t.resolve("ccr-haiku-x").unwrap();
        assert_eq!(
            r.matched_by,
            MatchedBy::Alias,
            "alias match (rule 1) must precede the family fallback (rule 2)"
        );
        assert_eq!(r.provider_id, "kimi", "family fallback would have gone to roles.fast = deepseek");
        assert_eq!(r.upstream_model, "haiku-x");
        assert_eq!(r.alias, "ccr-haiku-x");
        assert!(r.roles.is_empty(), "no role references this alias");
    }

    #[test]
    fn unknown_model_uses_default_target_when_policy_is_default() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Default, vec![]));
        let r = t.resolve("totally-unknown-model").unwrap();
        assert_eq!(r.matched_by, MatchedBy::Fallback);
        assert_eq!(r.provider_id, "deepseek");
    }

    #[test]
    fn unknown_model_is_an_error_when_policy_is_error() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let err = t.resolve("totally-unknown-model").unwrap_err();
        assert!(matches!(err, ResolveError::UnknownModel { .. }));
        assert!(err.to_string().contains("totally-unknown-model"));
    }

    #[test]
    fn unknown_model_errors_when_policy_default_but_no_default_target() {
        let mut cfg = cfg_with(UnknownModelPolicy::Default, vec![]);
        cfg.default_target = None;
        let t = RouteTable::build(&cfg);
        assert!(t.resolve("nope").is_err());
    }

    #[test]
    fn extra_route_alias_resolves_to_its_target() {
        let extra = vec![ExtraRoute {
            alias: "ccr-big".into(),
            provider_id: "kimi".into(),
            model_id: "k3".into(),
        }];
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, extra));
        assert_eq!(t.resolve("ccr-big").unwrap().provider_id, "kimi");
    }

    #[test]
    fn reports_all_roles_sharing_one_alias() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let roles = t.roles_for_alias("ccr-deepseek-ds-pro");
        assert_eq!(roles, vec![Role::Fast, Role::Subagent]);
        assert_eq!(t.roles_for_alias("ccr-kimi-k3"), vec![Role::Main]);
        assert!(t.roles_for_alias("ccr-unknown").is_empty());
    }

    #[test]
    fn resolve_target_returns_entry_without_alias_or_role() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let target = crate::config::Target {
            provider_id: "deepseek".into(),
            model_id: "ds-pro".into(),
        };
        let r = t.resolve_target(&target).unwrap();
        assert_eq!(r.provider_id, "deepseek");
        assert_eq!(r.upstream_model, "ds-pro");
        assert_eq!(r.api_key, "deepseek-key");
        assert_eq!(r.alias, "", "explicit-target resolution carries no alias");
        assert!(r.roles.is_empty());
        assert_eq!(
            r.matched_by,
            MatchedBy::Passthrough,
            "spec §6.1: catch-all forwarding must be labelled passthrough, not fallback"
        );

        let missing = crate::config::Target { provider_id: "nope".into(), model_id: "x".into() };
        assert!(t.resolve_target(&missing).is_none());
    }
}

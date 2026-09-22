use crate::config::AuthStyle;
use crate::routing::resolve::ResolvedTarget;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

pub const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// 透传到上游的请求头白名单（spec §6.3）。
const PASSTHROUGH_HEADERS: &[&str] = &[
    "anthropic-version",
    "anthropic-beta",
    "content-type",
    "accept",
    "user-agent",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RewriteError {
    #[error("请求体不是合法的 JSON 对象: {0}")]
    NotJsonObject(String),
    #[error("请求体缺少字符串类型的 model 字段")]
    MissingModel,
}

pub fn rewrite_body(body: &[u8], upstream_model: &str) -> Result<Vec<u8>, RewriteError> {
    let mut value: Value = serde_json::from_slice(body)
        .map_err(|e| RewriteError::NotJsonObject(e.to_string()))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| RewriteError::NotJsonObject("顶层不是对象".to_string()))?;
    match obj.get("model") {
        Some(Value::String(_)) => {}
        _ => return Err(RewriteError::MissingModel),
    }
    obj.insert("model".to_string(), Value::String(upstream_model.to_string()));
    serde_json::to_vec(&value).map_err(|e| RewriteError::NotJsonObject(e.to_string()))
}

/// 上游 URL = `{baseUrl}/v1/messages`，保留入站 query string。
pub fn upstream_url(base_url: &str, path: &str, query: Option<&str>) -> String {
    let base = base_url.trim_end_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamHeaders {
    pub headers: HeaderMap,
}

pub fn build_headers(target: &ResolvedTarget, incoming: &HeaderMap) -> UpstreamHeaders {
    let mut out = HeaderMap::new();

    for name in PASSTHROUGH_HEADERS {
        let hn = HeaderName::from_static(name);
        if let Some(v) = incoming.get(&hn) {
            out.insert(hn, v.clone());
        }
    }
    if !out.contains_key("anthropic-version") {
        out.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
        );
    }

    // 客户端鉴权头一律剥离，改为注入 provider 的真实密钥
    out.remove("authorization");
    out.remove("x-api-key");
    let key = HeaderValue::from_str(&target.api_key)
        .unwrap_or_else(|_| HeaderValue::from_static(""));
    match target.auth_style {
        AuthStyle::Both => {
            out.insert(HeaderName::from_static("x-api-key"), key.clone());
            out.insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {}", target.api_key))
                    .unwrap_or_else(|_| HeaderValue::from_static("Bearer ")),
            );
        }
        AuthStyle::XApiKey => {
            out.insert(HeaderName::from_static("x-api-key"), key);
        }
        AuthStyle::Bearer => {
            out.insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {}", target.api_key))
                    .unwrap_or_else(|_| HeaderValue::from_static("Bearer ")),
            );
        }
    }

    if target.context_1m {
        let existing = out
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mut parts: Vec<String> = existing
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.iter().any(|p| p == CONTEXT_1M_BETA) {
            parts.push(CONTEXT_1M_BETA.to_string());
        }
        if let Ok(v) = HeaderValue::from_str(&parts.join(",")) {
            out.insert(HeaderName::from_static("anthropic-beta"), v);
        }
    }

    if let Ok(v) = HeaderValue::from_str(&target.alias) {
        if !target.alias.is_empty() {
            out.insert(HeaderName::from_static("x-ccr-alias"), v);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&target.provider_id) {
        out.insert(HeaderName::from_static("x-ccr-provider"), v);
    }
    // spec §6.3：角色也随请求带上（上游会忽略未知头，这只是给上游日志排查用）。
    // 无角色时与 `x-ccr-alias` 一样省略；多角色共享同一别名时按 §6.7 逗号连接。
    if let Some(label) = target.role_label() {
        if let Ok(v) = HeaderValue::from_str(&label) {
            out.insert(HeaderName::from_static("x-ccr-role"), v);
        }
    }

    // host / content-length / accept-encoding / connection 不在此白名单内，天然被丢弃
    UpstreamHeaders { headers: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthStyle;
    use crate::routing::resolve::{MatchedBy, ResolvedTarget, Role};

    fn target(auth_style: AuthStyle, context_1m: bool) -> ResolvedTarget {
        ResolvedTarget {
            provider_id: "kimi".into(),
            provider_name: "Kimi".into(),
            base_url: "https://api.moonshot.cn/anthropic".into(),
            api_key: "real-secret".into(),
            auth_style,
            upstream_model: "k3".into(),
            alias: "ccr-kimi-k3".into(),
            matched_by: MatchedBy::Alias,
            roles: Vec::new(),
            context_1m,
        }
    }

    fn incoming() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        h.insert("anthropic-beta", HeaderValue::from_static("prompt-caching-2024-07-31"));
        h.insert("content-type", HeaderValue::from_static("application/json"));
        h.insert("authorization", HeaderValue::from_static("Bearer sk-ccr-localtoken"));
        h.insert("x-api-key", HeaderValue::from_static("sk-ccr-localtoken"));
        h.insert("accept-encoding", HeaderValue::from_static("gzip, br"));
        h.insert("host", HeaderValue::from_static("127.0.0.1:8787"));
        h.insert("content-length", HeaderValue::from_static("123"));
        h
    }

    #[test]
    fn rewrites_only_the_model_field() {
        let body = br#"{"model":"ccr-kimi-k3","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"system":[{"type":"text","text":"s","cache_control":{"type":"ephemeral"}}]}"#;
        let out = rewrite_body(body, "k3").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "k3");
        assert_eq!(v["max_tokens"], 16);
        assert_eq!(v["messages"][0]["content"], "hi");
        assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn rejects_non_json_body_and_non_object_toplevel() {
        assert!(matches!(rewrite_body(b"<html>502</html>", "k3"), Err(RewriteError::NotJsonObject(_))));
        assert!(matches!(rewrite_body(b"[1,2,3]", "k3"), Err(RewriteError::NotJsonObject(_))));
        assert!(matches!(rewrite_body(b"\"just a string\"", "k3"), Err(RewriteError::NotJsonObject(_))));
    }

    #[test]
    fn rejects_missing_or_non_string_model() {
        assert!(matches!(rewrite_body(br#"{"max_tokens":1}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":123}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":null}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":{}}"#, "k3"), Err(RewriteError::MissingModel)));
    }

    #[test]
    fn builds_upstream_url_with_and_without_query() {
        assert_eq!(
            upstream_url("https://api.moonshot.cn/anthropic/", "/v1/messages", None),
            "https://api.moonshot.cn/anthropic/v1/messages"
        );
        assert_eq!(
            upstream_url("https://api.moonshot.cn/anthropic", "/v1/messages", Some("beta=true")),
            "https://api.moonshot.cn/anthropic/v1/messages?beta=true"
        );
    }

    #[test]
    fn strips_client_auth_and_injects_provider_key_for_both_style() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert_eq!(h.get("x-api-key").unwrap(), "real-secret");
        assert_eq!(h.get("authorization").unwrap(), "Bearer real-secret");
        assert!(!h.get("authorization").unwrap().to_str().unwrap().contains("localtoken"));
    }

    #[test]
    fn honours_each_auth_style() {
        let x = build_headers(&target(AuthStyle::XApiKey, false), &incoming()).headers;
        assert_eq!(x.get("x-api-key").unwrap(), "real-secret");
        assert!(x.get("authorization").is_none());

        let b = build_headers(&target(AuthStyle::Bearer, false), &incoming()).headers;
        assert!(b.get("x-api-key").is_none());
        assert_eq!(b.get("authorization").unwrap(), "Bearer real-secret");
    }

    #[test]
    fn drops_host_content_length_and_accept_encoding() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert!(h.get("host").is_none());
        assert!(h.get("content-length").is_none());
        assert!(h.get("accept-encoding").is_none());
    }

    #[test]
    fn defaults_anthropic_version_when_absent() {
        let h = build_headers(&target(AuthStyle::Both, false), &HeaderMap::new()).headers;
        assert_eq!(h.get("anthropic-version").unwrap(), DEFAULT_ANTHROPIC_VERSION);
    }

    #[test]
    fn appends_context_1m_beta_without_duplicating() {
        let h = build_headers(&target(AuthStyle::Both, true), &incoming()).headers;
        let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains("prompt-caching-2024-07-31"), "existing beta kept: {beta}");
        assert!(beta.contains(CONTEXT_1M_BETA), "1m beta appended: {beta}");
        assert_eq!(beta.matches(CONTEXT_1M_BETA).count(), 1);

        let mut twice = incoming();
        twice.insert("anthropic-beta", HeaderValue::from_str(CONTEXT_1M_BETA).unwrap());
        let h2 = build_headers(&target(AuthStyle::Both, true), &twice).headers;
        assert_eq!(
            h2.get("anthropic-beta").unwrap().to_str().unwrap().matches(CONTEXT_1M_BETA).count(),
            1,
            "must not duplicate an already-present 1m beta"
        );
    }

    #[test]
    fn does_not_add_context_1m_beta_when_disabled() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert!(!h.get("anthropic-beta").unwrap().to_str().unwrap().contains(CONTEXT_1M_BETA));
    }

    /// M6 / spec §6.3：`x-ccr-role` 只在解析出角色时发送，且与 §6.7 的日志字段同源
    /// （共享别名时给出逗号连接的多值）。
    #[test]
    fn emits_x_ccr_role_only_when_a_role_is_resolved() {
        let no_role = target(AuthStyle::Both, false);
        assert!(
            build_headers(&no_role, &incoming()).headers.get("x-ccr-role").is_none(),
            "no role → header omitted, like x-ccr-alias when the alias is empty"
        );

        let mut single = target(AuthStyle::Both, false);
        single.roles = vec![Role::Main];
        assert_eq!(build_headers(&single, &incoming()).headers.get("x-ccr-role").unwrap(), "main");

        let mut shared = target(AuthStyle::Both, false);
        shared.roles = vec![Role::Fast, Role::Subagent];
        assert_eq!(
            build_headers(&shared, &incoming()).headers.get("x-ccr-role").unwrap(),
            "fast,subagent",
            "an alias referenced by several roles reports the full sorted set"
        );
    }
}

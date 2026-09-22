mod support;

use support::{ok_message_body, start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

async fn post(port: u16, path: &str, body: &str) -> String {
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    out
}

/// 无 body 的 GET（catch-all 路径用）。
async fn get(port: u16, path: &str) -> String {
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    out
}

#[tokio::test]
async fn logs_alias_match_with_provider_and_role() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;

    let entries = log.recent(10);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let e = &entries[0];
    assert_eq!(e.requested_model, "ccr-kimi-k3");
    assert_eq!(e.matched_by, "alias");
    assert_eq!(e.provider_id, "kimi");
    assert_eq!(e.upstream_model, "k3");
    assert_eq!(e.status, Some(200));
    assert_eq!(e.role.as_deref(), Some("main"));
    assert!(e.error.is_none());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn logs_family_fallback_and_reports_all_sharing_roles() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"claude-opus-4-5-20251101","messages":[]}"#).await;
    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-deepseek-ds-pro","messages":[]}"#).await;

    let entries = log.recent(10);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].matched_by, "family");
    assert_eq!(entries[1].provider_id, "kimi");
    assert_eq!(entries[1].role.as_deref(), Some("main"));
    assert_eq!(
        entries[0].role.as_deref(),
        Some("fast,subagent"),
        "spec §6.7: roles sharing one alias are joined, not first-wins"
    );
    assert_eq!(entries[0].alias, "ccr-deepseek-ds-pro");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// I1 / spec §6.1：catch-all 请求的日志标签必须是 `passthrough`，
/// 不能与"未知模型走 policy default"的 `fallback` 混为一谈（它是诊断"哪条规则生效"的凭据）。
#[tokio::test]
async fn catch_all_request_is_logged_as_passthrough() {
    let upstream = MockUpstream::start_json(200, ok_message_body("ds-pro")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = get(gw.bound_port, "/v1/organizations").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let entries = log.recent(10);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let e = &entries[0];
    assert_eq!(e.matched_by, "passthrough", "spec §6.1 catch-all label: {e:?}");
    assert_eq!(e.path, "/v1/organizations");
    assert_eq!(e.provider_id, "deepseek");
    assert_eq!(e.upstream_model, "ds-pro");
    assert_eq!(e.status, Some(200));
    assert_eq!(e.alias, "", "passthrough carries no alias");
    assert_eq!(e.role, None, "no alias → no reverse-looked-up role");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// I3 / spec §6.7：路由失败（未知模型 + policy=error）必须留痕 `matchedBy="error"`，
/// 否则一个打错的别名只留下 400，在日志里与"请求根本没到网关"无法区分。
#[tokio::test]
async fn unknown_model_is_logged_as_error() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-typo","messages":[]}"#).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");

    let entries = log.recent(10);
    assert_eq!(entries.len(), 1, "a routing failure must still leave exactly one entry: {entries:?}");
    let e = &entries[0];
    assert_eq!(e.matched_by, "error");
    assert_eq!(e.requested_model, "ccr-typo");
    assert_eq!(e.status, Some(400), "the status returned to the client must be recorded");
    assert!(
        e.error.as_deref().unwrap_or("").contains("ccr-typo"),
        "the error message must name the requested model: {e:?}"
    );
    assert_eq!(e.path, "/v1/messages");
    assert!(upstream.requests().is_empty(), "the upstream must not be contacted");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn logs_streaming_flag_and_upstream_failure_with_alias_and_provider() {
    let upstream = MockUpstream::start_sse(
        vec!["event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()],
        std::time::Duration::from_millis(0),
        std::time::Duration::from_millis(0),
    )
    .await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;
    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#).await;
    assert!(log.recent(1)[0].stream, "streaming request must be flagged");
    gw.shutdown().await;
    upstream.shutdown().await;

    let dead = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let dead_url = dead.base_url.clone();
    dead.shutdown().await;
    let (gw2, log2) = start_gateway(test_config(&dead_url, 0)).await;
    let body = post(gw2.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;
    assert!(body.contains("provider=kimi"));
    let e = &log2.recent(1)[0];
    assert!(e.error.as_deref().unwrap_or("").contains("kimi"), "error must name provider: {e:?}");
    assert_eq!(e.provider_id, "kimi");
    assert!(e.status.is_none(), "failed requests have no upstream status");
    gw2.shutdown().await;
}

/// 端到端确认容量被强制。真正的淘汰顺序断言在 `logging` 的单元测试里
/// （`ring_buffer_drops_the_oldest_and_keeps_the_newest`）——这里用可区分的模型名，
/// 让"丢最旧"与"丢最新"在结果上真的不同：序列 [k,k,k,d,d]、容量 3 时，
/// 丢最旧 → 保留第 3/4/5 条 = [k,d,d]（倒序 [d,d,k]）；丢最新 → 保留第 1/2/5 条 = [k,k,d]
/// （倒序 [d,k,k]）；"永远保留前 3 条" → [k,k,k]。
#[tokio::test]
async fn ring_buffer_drops_oldest_entries() {
    let log = cc_router::logging::RequestLog::new(3);
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let cfg = test_config(&upstream.base_url, 0);
    let state_log = log.clone();
    let config = std::sync::Arc::new(std::sync::RwLock::new(cfg));
    let gw = cc_router::gateway::server::start(config, state_log).await.unwrap();

    for i in 0..5 {
        let alias = if i >= 3 { "ccr-deepseek-ds-pro" } else { "ccr-kimi-k3" };
        let body = format!(r#"{{"model":"{alias}","pad":{i},"messages":[]}}"#);
        let _ = post(gw.bound_port, "/v1/messages", &body).await;
    }
    assert_eq!(log.len(), 3, "capacity must be enforced");
    let models: Vec<String> = log.recent(10).into_iter().map(|e| e.requested_model).collect();
    assert_eq!(
        models,
        vec!["ccr-deepseek-ds-pro", "ccr-deepseek-ds-pro", "ccr-kimi-k3"],
        "the two oldest entries must be dropped, newest first"
    );

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn subscribers_receive_entries() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;
    let mut rx = log.subscribe();

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;

    let entry = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("broadcast must deliver within 2s")
        .expect("channel must not be closed");
    assert_eq!(entry.provider_id, "kimi");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn models_endpoint_lists_all_aliases_and_extra_routes() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.extra_routes.push(cc_router::config::ExtraRoute {
        alias: "ccr-big".into(),
        provider_id: "kimi".into(),
        model_id: "k3".into(),
    });
    let (gw, _log) = start_gateway(cfg).await;

    let req = format!(
        "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let json_start = out.find("\r\n\r\n").unwrap() + 4;
    let v: serde_json::Value = serde_json::from_str(out[json_start..].trim()).unwrap();
    let ids: Vec<String> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"ccr-kimi-k3".to_string()), "{ids:?}");
    assert!(ids.contains(&"ccr-deepseek-ds-pro".to_string()), "{ids:?}");
    assert!(ids.contains(&"ccr-big".to_string()), "extra route alias must be listed: {ids:?}");
    assert_eq!(v["has_more"], false);
    let first = &v["data"][0];
    assert!(first["display_name"].as_str().unwrap().contains('/'), "display name is 'provider / model'");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// M9：列表里只有 extraRoutes 时（providers 下没有任何模型），`first_id`/`last_id`
/// 必须回落到 extra 别名，而不是双双为 null。
#[tokio::test]
async fn models_endpoint_first_and_last_ids_cover_extra_routes() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    for p in &mut cfg.providers {
        p.models.clear();
    }
    cfg.extra_routes.push(cc_router::config::ExtraRoute {
        alias: "ccr-big".into(),
        provider_id: "kimi".into(),
        model_id: "k3".into(),
    });
    let (gw, _log) = start_gateway(cfg).await;

    let req = format!(
        "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;

    let json_start = out.find("\r\n\r\n").unwrap() + 4;
    let v: serde_json::Value = serde_json::from_str(out[json_start..].trim()).unwrap();
    assert_eq!(v["data"][0]["id"], "ccr-big");
    assert_eq!(v["first_id"], "ccr-big", "an extra-route-only list must not report first_id=null");
    assert_eq!(v["last_id"], "ccr-big", "an extra-route-only list must not report last_id=null");

    gw.shutdown().await;
    upstream.shutdown().await;
}

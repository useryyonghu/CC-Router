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
    assert_eq!(entries[0].role.as_deref(), Some("fast"), "first role wins in display order");
    assert_eq!(entries[0].alias, "ccr-deepseek-ds-pro");

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

#[tokio::test]
async fn ring_buffer_drops_oldest_entries() {
    let log = cc_router::logging::RequestLog::new(3);
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let cfg = test_config(&upstream.base_url, 0);
    let state_log = log.clone();
    let config = std::sync::Arc::new(std::sync::RwLock::new(cfg));
    let gw = cc_router::gateway::server::start(config, state_log).await.unwrap();

    for i in 0..5 {
        let body = format!(r#"{{"model":"ccr-kimi-k3","pad":{i},"messages":[]}}"#);
        let _ = post(gw.bound_port, "/v1/messages", &body).await;
    }
    assert_eq!(log.len(), 3, "capacity must be enforced");
    let entries = log.recent(10);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].requested_model, "ccr-kimi-k3", "newest first");

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

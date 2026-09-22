mod support;

use support::{ok_message_body, start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

async fn raw_request(port: u16, request: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).to_string()
}

fn messages_request(token: Option<&str>, body: &str) -> String {
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 无 body 的 GET（catch-all 路径用；不发送 Content-Length）。
fn get_request(path: &str, token: Option<&str>) -> String {
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n")
}

/// 只用 `x-api-key` 携带本地令牌的 POST（不带 Authorization）。
fn messages_request_with_api_key(key: &str, body: &str) -> String {
    format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nx-api-key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn health_endpoint_requires_no_token() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(
        gw.bound_port,
        "GET /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("\"status\":\"ok\""));
    assert!(resp.contains(&format!("\"port\":{}", gw.bound_port)));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn messages_without_token_is_rejected_with_anthropic_error() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(None, r#"{"model":"ccr-kimi-k3"}"#)).await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(resp.contains("\"type\":\"authentication_error\""), "{resp}");
    assert!(upstream.requests().is_empty(), "upstream must not be contacted");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(Some("sk-ccr-wrong"), r#"{"model":"ccr-kimi-k3"}"#)).await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn routes_alias_to_upstream_with_rewritten_model_and_injected_key() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("pong"), "{resp}");

    let seen = upstream.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/messages");
    assert_eq!(seen[0].body["model"], "k3", "alias must be rewritten to upstream model");
    assert_eq!(seen[0].body["max_tokens"], 16, "other fields untouched");
    assert_eq!(seen[0].header("x-api-key"), Some("kimi-real-key"));
    assert_eq!(seen[0].header("authorization"), Some("Bearer kimi-real-key"));
    assert_eq!(seen[0].header("x-ccr-alias"), Some("ccr-kimi-k3"));
    assert_eq!(seen[0].header("x-ccr-provider"), Some("kimi"));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn two_aliases_reach_two_providers_with_their_own_keys() {
    let upstream = MockUpstream::start_json(200, ok_message_body("ds-pro")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let b1 = r#"{"model":"ccr-kimi-k3","max_tokens":8,"messages":[]}"#;
    let b2 = r#"{"model":"ccr-deepseek-ds-pro","max_tokens":8,"messages":[]}"#;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b1)).await;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b2)).await;

    let seen = upstream.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].body["model"], "k3");
    assert_eq!(seen[0].header("x-api-key"), Some("kimi-real-key"));
    assert_eq!(seen[1].body["model"], "ds-pro");
    assert_eq!(seen[1].header("authorization"), Some("Bearer deepseek-real-key"));
    assert!(seen[1].header("x-api-key").is_none(), "bearer style must not send x-api-key");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn non_json_body_returns_400_not_500() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), "<html>oops</html>")).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "must be 400, got: {resp}");
    assert!(resp.contains("\"type\":\"invalid_request_error\""));
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn body_without_model_field_returns_400() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    for body in [r#"{"max_tokens":5}"#, r#"{"model":123}"#, r#"{"model":null}"#] {
        let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
        assert!(resp.starts_with("HTTP/1.1 400"), "body={body} got: {resp}");
        assert!(resp.contains("\"type\":\"invalid_request_error\""), "body={body}");
    }
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unknown_model_is_400_when_policy_is_error() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"totally-unknown","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
    assert!(resp.contains("totally-unknown"), "error must name the model: {resp}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn upstream_non_json_error_body_is_passed_through_verbatim() {
    let upstream = MockUpstream::start_raw(502, "text/html", b"<html><body>Bad Gateway</body></html>").await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    assert!(resp.contains("<html><body>Bad Gateway</body></html>"), "body must pass through: {resp}");
    assert!(!resp.contains("\"type\":\"api_error\""), "must not replace upstream body");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unreachable_provider_yields_502_naming_provider_and_alias() {
    // 关掉上游，模拟连接被拒
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let dead_url = upstream.base_url.clone();
    upstream.shutdown().await;

    let (gw, _log) = start_gateway(test_config(&dead_url, 0)).await;
    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    assert!(resp.contains("\"type\":\"api_error\""));
    assert!(resp.contains("provider=kimi"), "must name provider: {resp}");
    assert!(resp.contains("ccr-kimi-k3"), "must name alias: {resp}");

    gw.shutdown().await;
}

#[tokio::test]
async fn binding_an_occupied_port_returns_error_instead_of_panicking() {
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = occupied.local_addr().unwrap().port();

    let log = cc_router::logging::RequestLog::new(10);
    let config = std::sync::Arc::new(std::sync::RwLock::new(test_config("http://127.0.0.1:1", port)));
    let err = cc_router::gateway::server::start(config, log).await.unwrap_err();
    assert!(matches!(err, cc_router::error::Error::Bind { .. }), "{err:?}");
    assert!(err.to_string().contains(&port.to_string()));
}

#[tokio::test]
async fn oversized_body_returns_413() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.gateway.max_request_body_bytes = 64;
    let (gw, _log) = start_gateway(cfg).await;

    let big = format!(
        r#"{{"model":"ccr-kimi-k3","pad":"{}"}}"#,
        "x".repeat(500)
    );
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), &big)).await;
    assert!(resp.starts_with("HTTP/1.1 413"), "{resp}");
    assert!(resp.contains("\"type\":\"invalid_request_error\""));
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// spec §6.1 的免鉴权面是 **GET** `/ccr/health`；其它方法必须走令牌闸门。
#[tokio::test]
async fn post_to_health_requires_token() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(
        gw.bound_port,
        "POST /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(resp.contains("\"type\":\"authentication_error\""), "{resp}");
    assert!(upstream.requests().is_empty(), "upstream must not be contacted");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// catch-all 路径必须保留客户端的原始方法，且无 body 的请求不得被塞进一个空 body。
#[tokio::test]
async fn catch_all_preserves_method_and_forwards_bodyless_get() {
    let upstream = MockUpstream::start_json(200, ok_message_body("ds-pro")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &get_request("/v1/organizations", Some(TOKEN))).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let seen = upstream.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET", "catch-all must not flatten the method to POST");
    assert_eq!(seen[0].path, "/v1/organizations");
    assert!(
        seen[0].body.is_null(),
        "bodyless GET must not gain a body: {:?}",
        seen[0].body
    );

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// 本地令牌也可以只通过 `x-api-key` 携带；且它绝不能泄漏到上游（会被换成 provider 密钥）。
#[tokio::test]
async fn token_accepts_x_api_key_header() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request_with_api_key(TOKEN, body)).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let seen = upstream.requests();
    assert_eq!(seen.len(), 1, "x-api-key must authenticate the request");
    assert_eq!(seen[0].body["model"], "k3");
    assert_eq!(
        seen[0].header("x-api-key"),
        Some("kimi-real-key"),
        "local token must be replaced by the provider key, never forwarded"
    );
    assert_eq!(seen[0].header("authorization"), Some("Bearer kimi-real-key"));

    gw.shutdown().await;
    upstream.shutdown().await;
}

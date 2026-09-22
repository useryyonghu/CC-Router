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
    assert_eq!(seen[0].header("x-ccr-role"), Some("main"), "spec §6.3: x-ccr-role must reach the wire");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// AC3：两个别名必须各自到达**各自的** provider——所以这里起两个 mock 上游分别取证
/// （共用一个 base_url 时，"永远用第一个 provider 的 baseUrl" 这类回归测不出来）。
/// 同时覆盖 AC6：`context1m=true` 的模型必须在上游看到 1M beta 头，且上游模型名
/// 永远不含 `[1M]` 后缀。
#[tokio::test]
async fn two_aliases_reach_two_providers_with_their_own_keys() {
    let kimi_up = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let ds_up = MockUpstream::start_json(200, ok_message_body("ds-pro")).await;
    let mut cfg = test_config(&kimi_up.base_url, 0);
    cfg.providers[1].base_url = ds_up.base_url.clone();
    let (gw, _log) = start_gateway(cfg).await;

    let b1 = r#"{"model":"ccr-kimi-k3","max_tokens":8,"messages":[]}"#;
    let b2 = r#"{"model":"ccr-deepseek-ds-pro","max_tokens":8,"messages":[]}"#;
    // 带 `[1M]` 后缀：它是网关内部的强制开关，绝不能出现在上游模型名里。
    let b3 = r#"{"model":"ccr-deepseek-ds-pro[1M]","max_tokens":8,"messages":[]}"#;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b1)).await;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b2)).await;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b3)).await;

    let kimi = kimi_up.requests();
    let ds = ds_up.requests();
    assert_eq!(kimi.len(), 1, "the kimi alias must reach only the kimi upstream");
    assert_eq!(ds.len(), 2, "the deepseek aliases must reach only the deepseek upstream");

    assert_eq!(kimi[0].body["model"], "k3");
    assert_eq!(kimi[0].header("x-api-key"), Some("kimi-real-key"));
    assert_eq!(kimi[0].header("authorization"), Some("Bearer kimi-real-key"));
    assert_eq!(kimi[0].header("x-ccr-provider"), Some("kimi"));
    assert_eq!(kimi[0].header("x-ccr-role"), Some("main"));

    assert_eq!(ds[0].body["model"], "ds-pro");
    assert_eq!(ds[0].header("authorization"), Some("Bearer deepseek-real-key"));
    assert!(ds[0].header("x-api-key").is_none(), "bearer style must not send x-api-key");
    assert_eq!(ds[0].header("x-ccr-provider"), Some("deepseek"));
    assert_eq!(
        ds[0].header("x-ccr-role"),
        Some("fast,subagent"),
        "spec §6.7: an alias referenced by several roles reports the full sorted set"
    );

    // AC6 正向半边：context1m=true 的模型，beta 头必须真的到线上（不只是单元测试）。
    let ds_beta = ds[0].header("anthropic-beta").unwrap_or("");
    assert!(
        ds_beta.contains("context-1m-2025-08-07"),
        "context1m=true must put the 1M beta on the wire: {ds_beta:?}"
    );
    // AC6 负向半边：context1m=false 的模型不得出现该 beta。
    let kimi_beta = kimi[0].header("anthropic-beta").unwrap_or("");
    assert!(
        !kimi_beta.contains("context-1m-2025-08-07"),
        "context1m=false must not add the 1M beta: {kimi_beta:?}"
    );
    // AC6 第二半：`[1M]` 后缀只影响 beta 头，不进上游模型名。
    assert_eq!(ds[1].body["model"], "ds-pro", "the upstream model name must carry no [1M] suffix");
    let forced_beta = ds[1].header("anthropic-beta").unwrap_or("");
    assert!(
        forced_beta.contains("context-1m-2025-08-07"),
        "the [1M] suffix must force the 1M beta header: {forced_beta:?}"
    );
    assert_eq!(
        forced_beta.matches("context-1m-2025-08-07").count(),
        1,
        "the 1M beta must not be duplicated: {forced_beta:?}"
    );

    gw.shutdown().await;
    kimi_up.shutdown().await;
    ds_up.shutdown().await;
}

/// I2 / spec §6.5 的 504 行：上游收下连接、声明了 `Content-Length` 却永远不把正文发完时，
/// 非流式请求必须靠空闲超时收场，而不是永久挂住。
/// 上游用裸 `tokio::net::TcpListener` 才能构造"响应永不完成"的状态。
#[tokio::test]
async fn stalled_nonstream_response_body_yields_504() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let contacted = Arc::new(AtomicBool::new(false));
    let flag = contacted.clone();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        flag.store(true, Ordering::SeqCst);
        // 读掉请求头即可；本用例只关心响应体读取。
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = match sock.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // content-type 是 application/json（走非流式分支），声明 100 字节却只发一个前缀，
        // 然后既不补完正文、也不关闭连接。
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"partial\":")
            .await;
        let _ = sock.flush().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(sock);
    });

    let base = format!("http://127.0.0.1:{port}");
    let mut cfg = test_config(&base, 0);
    cfg.gateway.idle_timeout_ms = 300;
    let (gw, log) = start_gateway(cfg).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;

    assert!(resp.starts_with("HTTP/1.1 504"), "a stalled body read must become 504, got: {resp}");
    assert!(resp.contains("\"type\":\"api_error\""), "must be an Anthropic api_error: {resp}");
    assert!(resp.contains("provider=kimi"), "error must name the provider: {resp}");
    assert!(resp.contains("ccr-kimi-k3"), "error must name the alias: {resp}");
    assert!(
        contacted.load(Ordering::SeqCst),
        "the upstream must actually have been contacted (this is not a pre-flight rejection)"
    );
    // I2 的第二半：超时不允许被记成一次成功的 200。
    let e = log.recent(1)[0].clone();
    assert!(e.error.is_some(), "a timed-out body read must be logged as an error: {e:?}");
    assert_ne!(e.status, Some(200), "a timed-out body read must not be logged as ok: {e:?}");
    assert_eq!(e.provider_id, "kimi");
    assert_eq!(e.alias, "ccr-kimi-k3");

    gw.shutdown().await;
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

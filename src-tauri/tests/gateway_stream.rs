mod support;

use std::time::{Duration, Instant};
use support::{start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

fn sse_chunks(n: usize) -> Vec<String> {
    let mut v = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"k3\"}}\n\n".to_string(),
    ];
    for i in 0..n {
        v.push(format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"chunk{i}\"}}}}\n\n"
        ));
    }
    v.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
    v
}

fn sse_request(body: &str) -> String {
    format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn streams_chunks_without_buffering() {
    // 上游首块延迟 300ms，之后每块间隔 250ms，共 5 块 → 总时长 ≥ 1.3s
    let upstream = MockUpstream::start_sse(
        sse_chunks(5),
        Duration::from_millis(250),
        Duration::from_millis(300),
    )
    .await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"max_tokens":64,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    // 读到第一个数据块的时间必须远早于整体结束时间
    let started = Instant::now();
    let mut buf = vec![0u8; 4096];
    let mut first_chunk_at: Option<Duration> = None;
    let mut total = String::new();
    loop {
        let read = stream.read(&mut buf).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        total.push_str(&String::from_utf8_lossy(&buf[..read]));
        if first_chunk_at.is_none() && total.contains("message_start") {
            first_chunk_at = Some(started.elapsed());
        }
    }
    let finished_at = started.elapsed();

    assert!(total.contains("HTTP/1.1 200"), "{total}");
    assert!(total.contains("text/event-stream"), "content-type must be preserved: {total}");
    let first = first_chunk_at.expect("must observe message_start");
    assert!(
        first < finished_at - Duration::from_millis(800),
        "first chunk at {first:?} vs finished at {finished_at:?}: looks buffered"
    );
    // 对齐 spec AC4：首块到达应约等于上游首块延迟（300ms）+ 很小的转发开销。
    // 留 700ms 余量以容忍 CI 抖动；若真发生缓冲，first 会接近 finished（≥1.3s）。
    assert!(
        first < Duration::from_millis(300 + 700),
        "first chunk arrival {first:?} exceeds upstream first-byte delay budget (AC4)"
    );
    assert!(total.contains("chunk4"), "all chunks must arrive: {total}");
    assert!(total.contains("message_stop"));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn preserves_chunk_boundaries_in_order() {
    let chunks = sse_chunks(3);
    let upstream = MockUpstream::start_sse(chunks.clone(), Duration::from_millis(20), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut total = String::new();
    let _ = stream.read_to_string(&mut total).await;

    let mut last = 0;
    for (i, _) in chunks.iter().enumerate() {
        let needle = if i == 0 {
            "message_start".to_string()
        } else if i == chunks.len() - 1 {
            "message_stop".to_string()
        } else {
            format!("chunk{}", i - 1)
        };
        let pos = total.find(&needle).unwrap_or_else(|| panic!("missing {needle} in {total}"));
        assert!(pos >= last, "{needle} out of order in {total}");
        last = pos;
    }

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn handles_sixteen_concurrent_streams_without_crosstalk() {
    let upstream = MockUpstream::start_sse(sse_chunks(2), Duration::from_millis(30), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut handles = Vec::new();
    for _ in 0..16 {
        let port = gw.bound_port;
        handles.push(tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
            stream.write_all(sse_request(body).as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out).await;
            out
        }));
    }
    let results = futures_util::future::join_all(handles).await;
    for r in results {
        let text = r.unwrap();
        // 断言对象是 SSE **事件帧**而非裸子串：`sse_chunks` 的首块同时含
        // `event: message_start` 与 `"type":"message_start"`，裸子串计数恒为 2（brief 原文如此，
        // 见 task-7-report.md 的偏差说明），故改用帧头计数，强度不变。
        assert_eq!(text.matches("event: message_start").count(), 1, "crosstalk detected: {text}");
        assert_eq!(text.matches("event: message_stop").count(), 1, "crosstalk detected: {text}");
    }
    assert_eq!(upstream.requests().len(), 16);

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn client_disconnect_does_not_panic_the_gateway() {
    let upstream = MockUpstream::start_sse(sse_chunks(50), Duration::from_millis(50), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
        let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
        stream.write_all(sse_request(body).as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = vec![0u8; 128];
        let _ = stream.read(&mut buf).await;
        // 直接 drop 连接，模拟客户端中断
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 网关仍然可用
    let mut healthy = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    healthy
        .write_all(b"GET /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    let _ = healthy.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 200"), "gateway died after client disconnect: {out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn upstream_stream_that_ends_early_still_completes_the_response() {
    // 只发 3 块就结束（不是正常的 message_stop 结尾），客户端仍应收到完整已转发内容并正常收流
    let chunks = vec![
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"delta\":{\"text\":\"partial\"}}\n\n".to_string(),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n".to_string(),
    ];
    let upstream = MockUpstream::start_sse(chunks, Duration::from_millis(10), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("partial"), "already-forwarded chunks must not be lost: {out}");
    assert!(out.contains("overloaded_error"), "mid-stream error frame must pass through: {out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn count_tokens_is_routed_but_not_streamed() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"input_tokens": 42})).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-deepseek-ds-pro","messages":[]}"#;
    let req = format!(
        "POST /v1/messages/count_tokens HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("\"input_tokens\":42"), "{out}");
    let seen = upstream.requests();
    assert_eq!(seen[0].path, "/v1/messages/count_tokens");
    assert_eq!(seen[0].body["model"], "ds-pro");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn query_string_is_preserved_upstream() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"ok": true})).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","messages":[]}"#;
    let req = format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(upstream.requests()[0].query.as_deref() == Some("beta=true"), "query must be forwarded");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unmatched_path_without_default_target_returns_404() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"ok": true})).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.default_target = None;
    let (gw, _log) = start_gateway(cfg).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let req = format!(
        "GET /v1/organizations HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 404"), "{out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

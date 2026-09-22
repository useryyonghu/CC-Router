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

/// 本任务唯一的行为改动：块间空闲超时必须结束停滞的流，且**不得**丢弃已转发的块。
/// 上游第 1 块立即到达、第 2 块要等 2000ms，而网关空闲超时只有 300ms。
#[tokio::test]
async fn idle_timeout_ends_stalled_stream() {
    let chunks = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"k3\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"too_late_1\"}}\n\n".to_string(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
    ];
    let upstream = MockUpstream::start_sse(chunks, Duration::from_millis(2000), Duration::from_millis(0)).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.gateway.idle_timeout_ms = 300;
    let (gw, _log) = start_gateway(cfg).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    // 没有空闲保护时这里会一直等到上游发完（本用例 ≥ 4000ms）；read_to_string 能返回本身就是
    // "流被结束、而不是挂住"的证据。
    let started = Instant::now();
    let mut out = String::new();
    let read = stream.read_to_string(&mut out).await;
    let elapsed = started.elapsed();

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("content-type: text/event-stream"), "content-type must be preserved: {out}");
    // 已经转发的第 1 块必须完整保留（spec：已经转发的块保持完整），不能被超时截断或吞掉。
    assert!(out.contains("event: message_start"), "already-forwarded chunk must survive: {out}");
    assert!(out.contains("\"id\":\"msg_1\""), "already-forwarded chunk must not be truncated: {out}");
    // 2000ms 之后才到达的块必须没有出现：网关在 300ms 空闲后就已经收流了。
    assert!(!out.contains("too_late_1"), "idle timeout must cut the stream: {out}");
    assert!(!out.contains("message_stop"), "idle timeout must cut the stream: {out}");
    // 下游是"正常结束"（chunked 终止符），没有被挂起、也没有错误地重置连接。
    assert!(read.is_ok(), "downstream read must complete cleanly, got {read:?}");
    assert!(out.contains("0\r\n\r\n"), "stream must end with a chunked terminator: {out:?}");
    // 远早于上游 2000ms 的块间隔：证明是空闲保护收的流，而不是傻等上游把块发完。
    assert!(elapsed < Duration::from_millis(1500), "idle timeout did not fire; waited {elapsed:?}");
    eprintln!("idle timeout: downstream ended after {elapsed:?} (idle=300ms, upstream gap=2000ms, read={read:?})");

    gw.shutdown().await;
    upstream.shutdown().await;
}

/// 客户端断连必须**真正取消上游请求**，而不只是让网关存活下来。
/// 只有裸 TCP 上游能观测到这一点：网关一旦 drop 掉上游 reqwest 流，
/// 这条上游连接就会被关闭，上游读半部随即读到 EOF。
/// 若实现改为"后台任务继续把上游读完"，本用例会超时失败。
#[tokio::test]
async fn client_disconnect_cancels_upstream() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (eof_tx, eof_rx) = tokio::sync::oneshot::channel::<Instant>();
    let flag = cancelled.clone();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // 请求内容与本用例无关，读掉请求头即可。
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        let (mut rd, mut wr) = sock.into_split();
        // 观测"网关是否关闭了这条上游连接"（= 上游请求被取消）。
        tokio::spawn(async move {
            let mut b = [0u8; 64];
            loop {
                match rd.read(&mut b).await {
                    Ok(0) | Err(_) => {
                        flag.store(true, Ordering::SeqCst);
                        let _ = eof_tx.send(Instant::now());
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
        // 慢速供块：不取消的话要 ~2.5s 才能发完，足以与"已取消"区分开。
        for i in 0..50 {
            let chunk = format!("event: content_block_delta\ndata: {{\"text\":\"c{i}\"}}\n\n");
            let framed = format!("{:X}\r\n{chunk}\r\n", chunk.len());
            if wr.write_all(framed.as_bytes()).await.is_err() {
                return;
            }
            let _ = wr.flush().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // 写完整 chunked 终止符，保证这条裸上游响应本身是良构的。
        let _ = wr.write_all(b"0\r\n\r\n").await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let (gw, _log) = start_gateway(test_config(&base, 0)).await;

    {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
        let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
        stream.write_all(sse_request(body).as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = vec![0u8; 256];
        let _ = stream.read(&mut buf).await;
        // 直接 drop 连接，模拟客户端中断
    }
    let dropped_at = Instant::now();

    let eof_at = tokio::time::timeout(Duration::from_secs(2), eof_rx)
        .await
        .expect("upstream did not observe cancellation within 2s: gateway kept draining the upstream stream")
        .expect("upstream EOF watcher dropped without reporting");
    let elapsed = eof_at.duration_since(dropped_at);
    assert!(
        cancelled.load(Ordering::SeqCst),
        "upstream connection was not closed by the gateway"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "cancellation took too long: {elapsed:?}"
    );
    eprintln!("client disconnect: upstream saw EOF after {elapsed:?} (50 chunks x 50ms would take ~2.5s)");

    // 网关仍然可用（原有的存活断言仍然有价值）。
    let mut healthy = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    healthy
        .write_all(b"GET /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    let _ = healthy.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 200"), "gateway died after client disconnect: {out}");

    gw.shutdown().await;
}

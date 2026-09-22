use crate::config::{Config, DEFAULT_BIND};
use crate::gateway::body::{body_full, body_stream, BoxedBody};
use crate::gateway::error::{anthropic_error, ErrorKind};
use crate::gateway::rewrite::{build_headers, rewrite_body, upstream_url, RewriteError};
use crate::routing::resolve::{ResolveError, RouteTable};
use crate::error::{Error, Result};
use futures_util::StreamExt;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct GatewayState {
    pub config: Arc<RwLock<Config>>,
    pub client: reqwest::Client,
    pub started_at: Instant,
    pub requests_served: Arc<AtomicU64>,
    /// 实际绑定的端口（配置里写 0 时由 OS 分配，health 必须报告真实值）。
    pub bound_port: u16,
    pub log: crate::logging::RequestLog,
}

#[derive(Debug)]
pub struct Gateway {
    pub bound_port: u16,
    requests_served: Arc<AtomicU64>,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl Gateway {
    /// 已服务请求数（含未到达上游的失败请求）。
    pub fn requests_served(&self) -> u64 {
        self.requests_served.load(Ordering::Relaxed)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

pub async fn start(config: Arc<RwLock<Config>>, log: crate::logging::RequestLog) -> Result<Gateway> {
    let (bind, port, connect_timeout_ms) = {
        let cfg = config.read().expect("config lock poisoned");
        (cfg.gateway.bind.clone(), cfg.gateway.port, cfg.gateway.connect_timeout_ms)
    };
    start_on(config, log, &bind, port, connect_timeout_ms).await
}

/// `port = 0` 时由操作系统分配端口，便于测试。
pub async fn start_on(
    config: Arc<RwLock<Config>>,
    log: crate::logging::RequestLog,
    bind: &str,
    port: u16,
    connect_timeout_ms: u64,
) -> Result<Gateway> {
    let bind = if bind.is_empty() { DEFAULT_BIND } else { bind };
    let listener = TcpListener::bind((bind, port))
        .await
        .map_err(|e| Error::Bind { addr: format!("{bind}:{port}"), source: e })?;
    let bound_port = listener
        .local_addr()
        .map_err(|e| Error::Bind { addr: format!("{bind}:{port}"), source: e })?
        .port();

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(connect_timeout_ms))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| Error::Gateway(format!("构造 HTTP 客户端失败: {e}")))?;

    let state = GatewayState {
        config,
        client,
        started_at: Instant::now(),
        requests_served: Arc::new(AtomicU64::new(0)),
        bound_port,
        log,
    };

    let requests_served = state.requests_served.clone();
    let (tx, mut rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(v) => v,
                        Err(e) => {
                            tracing_less_warn(&format!("accept 失败: {e}"));
                            continue;
                        }
                    };
                    let io = TokioIo::new(stream);
                    let st = state.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| {
                            let st = st.clone();
                            async move { Ok::<_, std::convert::Infallible>(handle(req, st, peer).await) }
                        });
                        if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                            .serve_connection(io, svc)
                            .await
                        {
                            tracing_less_warn(&format!("连接结束: {e}"));
                        }
                    });
                }
            }
        }
    });

    Ok(Gateway { bound_port, requests_served, shutdown: Some(tx), join })
}

fn tracing_less_warn(msg: &str) {
    eprintln!("[cc-router] {msg}");
}

pub async fn handle(req: Request<Incoming>, state: GatewayState, _peer: SocketAddr) -> Response<BoxedBody> {
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    let method = req.method().as_str().to_string();

    // 免鉴权面**仅限** `GET /ccr/health`（spec §6.1）；其它方法落到下面的令牌闸门。
    if path == "/ccr/health" && req.method() == hyper::Method::GET {
        return health(&state);
    }

    if !token_ok(&req, &state) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            ErrorKind::Authentication,
            "本地令牌无效：请在请求中携带 Authorization: Bearer <localToken> 或 x-api-key: <localToken>",
        );
    }

    if path == "/v1/models" && req.method() == hyper::Method::GET {
        let cfg = state.config.read().expect("config lock poisoned");
        let payload = models_payload(&cfg);
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(body_full(serde_json::to_vec(&payload).unwrap()))
            .expect("static response is valid");
    }

    let is_messages = path == "/v1/messages";
    let is_count = path == "/v1/messages/count_tokens";

    // 必须在 into_body() 之前克隆入站头：改写阶段要用它们决定透传白名单。
    let incoming_headers = req.headers().clone();

    let (max_body, idle_timeout_ms) = {
        let cfg = state.config.read().expect("config lock poisoned");
        (cfg.gateway.max_request_body_bytes, cfg.gateway.idle_timeout_ms)
    };

    // 用 Limited 包住 body，防止超大请求体打爆内存（spec §6.3）。
    let body_bytes = match Limited::new(req.into_body(), max_body).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return anthropic_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorKind::InvalidRequest,
                format!("请求体超过上限 {} 字节", max_body),
            )
        }
    };

    let route_table = {
        let cfg = state.config.read().expect("config lock poisoned");
        RouteTable::build(&cfg)
    };

    let requested_model = extract_model(&body_bytes).unwrap_or_default();

    let resolved = if is_messages || is_count {
        match route_table.resolve(&requested_model) {
            Ok(r) => r,
            Err(e) => return resolve_error_response(&e),
        }
    } else {
        // 非 messages 路径：转发到 defaultTarget；未绑定则 404（spec §6.1）
        let default_target = {
            let cfg = state.config.read().expect("config lock poisoned");
            cfg.default_target.clone()
        };
        match default_target.as_ref().and_then(|t| route_table.resolve_target(t)) {
            Some(r) => r,
            None => {
                return anthropic_error(
                    StatusCode::NOT_FOUND,
                    ErrorKind::InvalidRequest,
                    format!("未配置 defaultTarget，无法转发 {path}"),
                )
            }
        }
    };

    // messages 类请求必须能改写 model；catch-all 路径的 body 可能为空或非 JSON，
    // 那种情况按原样转发（不能让一个 GET 因为"body 不是 JSON"变成 400）。
    let upstream_body = if is_messages || is_count {
        match rewrite_body(&body_bytes, &resolved.upstream_model) {
            Ok(b) => b,
            Err(e) => {
                return anthropic_error(
                    StatusCode::BAD_REQUEST,
                    ErrorKind::InvalidRequest,
                    rewrite_error_message(&e, &requested_model),
                )
            }
        }
    } else if body_bytes.is_empty() {
        Vec::new()
    } else {
        rewrite_body(&body_bytes, &resolved.upstream_model).unwrap_or_else(|_| body_bytes.to_vec())
    };

    let url = upstream_url(&resolved.base_url, &path, query.as_deref());
    let headers = build_headers(&resolved, &incoming_headers).headers;

    let started = Instant::now();
    // 保留客户端原始方法：catch-all 路径（如 `GET /v1/organizations`）不能被压平成 POST。
    let upstream_method = reqwest::Method::from_bytes(method.as_bytes())
        .unwrap_or(reqwest::Method::POST);
    let mut builder = state.client.request(upstream_method, &url);
    if !upstream_body.is_empty() {
        builder = builder.body(upstream_body);
    }
    for (name, value) in headers.iter() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let upstream = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            state.requests_served.fetch_add(1, Ordering::Relaxed);
            // 日志的 error 与响应体必须同源：两者都要点名 provider 与别名（Task 8 用例断言）。
            let message = format!(
                "上游请求失败（provider={} 别名={} url={}）: {}",
                resolved.provider_id, resolved.alias, url, e
            );
            state.log.push(crate::logging::LogEntry::error(
                &method,
                &path,
                &requested_model,
                &resolved,
                started.elapsed().as_millis() as u64,
                &message,
            ));
            let (status, kind) = if e.is_timeout() {
                (StatusCode::GATEWAY_TIMEOUT, ErrorKind::Api)
            } else {
                (StatusCode::BAD_GATEWAY, ErrorKind::Api)
            };
            return anthropic_error(status, kind, message);
        }
    };

    let status = upstream.status();
    let is_sse = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false);

    let mut resp = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers().iter() {
        if name == "content-length" || name == "transfer-encoding" {
            continue;
        }
        resp = resp.header(name.as_str(), value.as_bytes());
    }

    state.requests_served.fetch_add(1, Ordering::Relaxed);
    state.log.push(crate::logging::LogEntry::ok(
        &method,
        &path,
        &requested_model,
        &resolved,
        status.as_u16(),
        is_sse,
        started.elapsed().as_millis() as u64,
    ));

    let body: BoxedBody = if is_sse {
        let idle = std::time::Duration::from_millis(idle_timeout_ms);
        body_stream(idle_guarded(upstream.bytes_stream(), idle))
    } else {
        match upstream.bytes().await {
            Ok(b) => body_full(b),
            Err(e) => {
                return anthropic_error(
                    StatusCode::BAD_GATEWAY,
                    ErrorKind::Api,
                    format!("读取上游响应失败（provider={}）: {e}", resolved.provider_id),
                )
            }
        }
    };

    resp.body(body).expect("upstream headers are valid")
}

/// 空闲超时保护：两个块之间超过 `idle` 未到达则结束流（不 panic，已转发的块保持完整）。
fn idle_guarded<S, E>(
    stream: S,
    idle: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    futures_util::stream::unfold(
        (Box::pin(stream), idle),
        |(mut s, idle)| async move {
            match tokio::time::timeout(idle, s.next()).await {
                Ok(Some(item)) => Some((item, (s, idle))),
                // 超时或流结束：结束下游流。客户端断开时 hyper 会 drop 本 future，
                // 从而 drop 掉上游 reqwest 流，上游请求随之取消。
                Ok(None) => None,
                Err(_elapsed) => None,
            }
        },
    )
}

/// 合成 Anthropic 列表格式：id = 全部模型别名 ∪ extraRoutes 别名。
pub fn models_payload(cfg: &Config) -> serde_json::Value {
    use serde_json::json;
    let mut data = Vec::new();
    for p in &cfg.providers {
        for m in &p.models {
            data.push(json!({
                "type": "model",
                "id": m.alias,
                "display_name": format!("{} / {}", p.name, m.name),
            }));
        }
    }
    for r in &cfg.extra_routes {
        data.push(json!({
            "type": "model",
            "id": r.alias,
            "display_name": format!("{} / {} (extra route)", r.provider_id, r.model_id),
        }));
    }
    let first_id = cfg
        .providers
        .iter()
        .find_map(|p| p.models.first())
        .map(|m| m.alias.clone());
    let last_id = cfg
        .providers
        .iter()
        .rev()
        .find_map(|p| p.models.last())
        .map(|m| m.alias.clone());
    json!({
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id,
    })
}

fn health(state: &GatewayState) -> Response<BoxedBody> {
    let uptime = state.started_at.elapsed().as_secs();
    let payload = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "port": state.bound_port,
        "uptimeSec": uptime,
        "requestsServed": state.requests_served.load(Ordering::Relaxed),
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(body_full(serde_json::to_vec(&payload).unwrap()))
        .expect("static response is valid")
}

fn token_ok(req: &Request<Incoming>, state: &GatewayState) -> bool {
    let expected = {
        let cfg = state.config.read().expect("config lock poisoned");
        cfg.gateway.local_token.clone()
    };
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string());
    let api_key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string());
    bearer.as_deref() == Some(expected.as_str()) || api_key.as_deref() == Some(expected.as_str())
}

fn extract_model(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(|s| s.to_string())
}

fn resolve_error_response(e: &ResolveError) -> Response<BoxedBody> {
    anthropic_error(StatusCode::BAD_REQUEST, ErrorKind::InvalidRequest, e.to_string())
}

fn rewrite_error_message(e: &RewriteError, requested: &str) -> String {
    match e {
        RewriteError::MissingModel => format!(
            "请求缺少字符串类型的 model 字段（收到 '{requested}'）：无法确定路由目标"
        ),
        RewriteError::NotJsonObject(detail) => {
            format!("请求体不是合法的 JSON 对象（{detail}）：无法确定路由目标")
        }
    }
}

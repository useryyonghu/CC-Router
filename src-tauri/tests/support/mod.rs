//! 共享集成测试脚手架（Task 6 建立，Task 7/8 复用）。
//!
//! 这里的公开面是本任务**有意**超出当前用例需要的：`MockUpstream::start_sse`、
//! `RecordedRequest::query`、`MockUpstream::port` 等都是给流式（Task 7）与
//! 状态/日志（Task 8）测试预留的接口。它们在本任务里没有调用点，
//! 因此整个模块统一放行 dead_code，避免引入新告警。
#![allow(dead_code)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub type Recorded = Arc<Mutex<Vec<RecordedRequest>>>;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: serde_json::Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct MockUpstream {
    pub base_url: String,
    pub port: u16,
    pub recorded: Recorded,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl MockUpstream {
    /// 非流式：固定返回 `status` + JSON `body`。
    pub async fn start_json(status: u16, body: serde_json::Value) -> Self {
        let bytes = serde_json::to_vec(&body).unwrap();
        MockUpstream::start(move || {
            let bytes = bytes.clone();
            async move {
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(full(bytes))
                    .unwrap()
            }
        })
        .await
    }

    /// 非流式：固定返回原始字节与 content-type（用于 HTML 错误页等）。
    pub async fn start_raw(status: u16, content_type: &'static str, body: &'static [u8]) -> Self {
        MockUpstream::start(move || async move {
            Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(full(body.to_vec()))
                .unwrap()
        })
        .await
    }

    /// SSE：按 `chunks` 逐块发送，块间 sleep `gap`，首块前额外 sleep `first_delay`。
    pub async fn start_sse(
        chunks: Vec<String>,
        gap: std::time::Duration,
        first_delay: std::time::Duration,
    ) -> Self {
        MockUpstream::start(move || {
            let stream = async_stream_like(chunks.clone(), gap, first_delay);
            async move {
                Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(StreamBody::new(stream).boxed())
                    .unwrap()
            }
        })
        .await
    }

    /// handler 不接收请求：本脚手架只提供固定响应，请求内容一律通过 `recorded` 断言。
    /// 这样就不必构造 `hyper::body::Incoming`（它没有公开的 empty 构造器）。
    async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Response<BoxedMockBody>> + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded: Recorded = Arc::new(Mutex::new(Vec::new()));
        let rec = recorded.clone();
        let (tx, mut rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        let io = TokioIo::new(stream);
                        let handler = handler.clone();
                        let rec = rec.clone();
                        tokio::spawn(async move {
                            let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                                let handler = handler.clone();
                                let rec = rec.clone();
                                async move {
                                    let path = req.uri().path().to_string();
                                    let query = req.uri().query().map(|q| q.to_string());
                                    let headers = req
                                        .headers()
                                        .iter()
                                        .map(|(k, v)| {
                                            (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())
                                        })
                                        .collect::<Vec<_>>();
                                    let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                                    let body = serde_json::from_slice(&body_bytes)
                                        .unwrap_or(serde_json::Value::Null);
                                    rec.lock().unwrap().push(RecordedRequest { path, query, headers, body });
                                    let resp = handler().await;
                                    Ok::<_, Infallible>(resp)
                                }
                            });
                            let _ = ConnBuilder::new(TokioExecutor::new()).serve_connection(io, svc).await;
                        });
                    }
                }
            }
        });
        MockUpstream {
            base_url: format!("http://127.0.0.1:{port}"),
            port,
            recorded,
            shutdown: Some(tx),
            join,
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.recorded.lock().unwrap().clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

pub type BoxedMockBody =
    http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

fn full(bytes: Vec<u8>) -> BoxedMockBody {
    Full::new(Bytes::from(bytes))
        .map_err(|e: Infallible| -> Box<dyn std::error::Error + Send + Sync> { match e {} })
        .boxed()
}

fn async_stream_like(
    chunks: Vec<String>,
    gap: std::time::Duration,
    first_delay: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>> {
    futures_util::stream::unfold(
        (chunks.into_iter(), 0usize, gap, first_delay),
        |(mut it, idx, gap, first_delay)| async move {
            let delay = if idx == 0 { first_delay } else { gap };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match it.next() {
                Some(chunk) => Some((
                    Ok(Frame::data(Bytes::from(chunk))),
                    (it, idx + 1, gap, first_delay),
                )),
                None => None,
            }
        },
    )
}

pub fn test_config(upstream_base_url: &str, port: u16) -> cc_router::config::Config {
    use cc_router::config::*;
    Config {
        version: 1,
        gateway: GatewayConfig {
            bind: "127.0.0.1".into(),
            port,
            local_token: "sk-ccr-test-token".into(),
            max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 300_000,
        },
        providers: vec![
            Provider {
                id: "kimi".into(),
                name: "Kimi".into(),
                base_url: upstream_base_url.into(),
                api_key: "kimi-real-key".into(),
                auth_style: AuthStyle::Both,
                preset_id: None,
                models_url: None,
                models_fetch: None,
                models: vec![ModelSpec {
                    id: "k3".into(),
                    name: "k3".into(),
                    alias: "ccr-kimi-k3".into(),
                    context_1m: false,
                    context_window: None,
                    max_tokens: None,
                }],
            },
            Provider {
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                base_url: upstream_base_url.into(),
                api_key: "deepseek-real-key".into(),
                auth_style: AuthStyle::Bearer,
                preset_id: None,
                models_url: None,
                models_fetch: None,
                models: vec![ModelSpec {
                    id: "ds-pro".into(),
                    name: "ds-pro".into(),
                    alias: "ccr-deepseek-ds-pro".into(),
                    context_1m: true,
                    context_window: None,
                    max_tokens: None,
                }],
            },
        ],
        roles: Roles {
            main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
            fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
        },
        extra_routes: vec![],
        on_unknown_model: UnknownModelPolicy::Error,
        default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
        takeover: TakeoverState::default(),
        ui: UiConfig::default(),
    }
}

pub fn ok_message_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{ "type": "text", "text": "pong" }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 3, "output_tokens": 1 }
    })
}

pub async fn start_gateway(cfg: cc_router::config::Config) -> (cc_router::gateway::server::Gateway, cc_router::logging::RequestLog) {
    let log = cc_router::logging::RequestLog::new(500);
    let config = Arc::new(std::sync::RwLock::new(cfg));
    let gw = cc_router::gateway::server::start(config, log.clone()).await.unwrap();
    (gw, log)
}

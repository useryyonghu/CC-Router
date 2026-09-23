//! A2/A3 单元测试用的最小 HTTP mock（仅 `#[cfg(test)]` 编译）。
//!
//! 为什么自建而不是复用 `tests/support/mod.rs`：那是集成测试（独立 crate）的脚手架，
//! 单元测试看不到它；而 Plan 3 明确要求**不要改动**那个共享脚手架的成员与 allow。
//!
//! 与共享脚手架一样只提供固定响应：请求内容一律通过 [`MockUpstream::requests`] 断言。

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// 上游收到的一次请求（只记录本计划要断言的字段）。
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub api_key: Option<String>,
    pub authorization: Option<String>,
    pub body: serde_json::Value,
}

pub struct MockUpstream {
    pub base_url: String,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl MockUpstream {
    /// `handler` 按请求路径返回 `(status, json_body)`；绑定 `127.0.0.1:0`。
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&str) -> (u16, String) + Send + Sync + Clone + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = Arc::clone(&recorded);
        let (tx, mut rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        let io = TokioIo::new(stream);
                        let handler = handler.clone();
                        let rec = Arc::clone(&rec);
                        tokio::spawn(async move {
                            let svc = service_fn(move |req: Request<Incoming>| {
                                let handler = handler.clone();
                                let rec = Arc::clone(&rec);
                                async move {
                                    let method = req.method().as_str().to_string();
                                    let path = req.uri().path().to_string();
                                    let header = |name: &str| {
                                        req.headers()
                                            .get(name)
                                            .and_then(|v| v.to_str().ok())
                                            .map(str::to_string)
                                    };
                                    let (api_key, authorization) =
                                        (header("x-api-key"), header("authorization"));
                                    let bytes = req.into_body().collect().await.unwrap().to_bytes();
                                    let body = serde_json::from_slice(&bytes)
                                        .unwrap_or(serde_json::Value::Null);
                                    rec.lock().unwrap().push(Recorded {
                                        method,
                                        path: path.clone(),
                                        api_key,
                                        authorization,
                                        body,
                                    });
                                    let (status, json) = handler(&path);
                                    Ok::<_, Infallible>(
                                        Response::builder()
                                            .status(status)
                                            .header("content-type", "application/json")
                                            .body(Full::new(Bytes::from(json)))
                                            .unwrap(),
                                    )
                                }
                            });
                            let _ = ConnBuilder::new(TokioExecutor::new())
                                .serve_connection(io, svc)
                                .await;
                        });
                    }
                }
            }
        });
        MockUpstream {
            base_url: format!("http://127.0.0.1:{port}"),
            recorded,
            shutdown: Some(tx),
            join,
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use std::convert::Infallible;

pub type BoxedError = Box<dyn std::error::Error + Send + Sync>;
pub type BoxedBody = BoxBody<Bytes, BoxedError>;

pub fn body_full(bytes: impl Into<Bytes>) -> BoxedBody {
    Full::new(bytes.into())
        .map_err(|e: Infallible| -> BoxedError { match e {} })
        .boxed()
}

/// 把任意 `Result<Bytes, E>` 流包成响应体。**不做任何缓冲**：每一块到达即作为一帧下发。
pub fn body_stream<S, E>(stream: S) -> BoxedBody
where
    S: Stream<Item = Result<Bytes, E>> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let mapped = stream.map(|item| item.map(Frame::data).map_err(|e| -> BoxedError { Box::new(e) }));
    // `StreamBody` 同时实现了 `Body` 与 `Stream`，而 `BodyExt`/`StreamExt` 都提供了 `boxed`，
    // 所以必须显式指定用 `BodyExt::boxed`（否则 E0034：multiple applicable items in scope）。
    BodyExt::boxed(StreamBody::new(mapped))
}

use crate::gateway::body::{body_full, BoxedBody};
use hyper::{Response, StatusCode};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Authentication,
    InvalidRequest,
    Api,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Authentication => "authentication_error",
            ErrorKind::InvalidRequest => "invalid_request_error",
            ErrorKind::Api => "api_error",
        }
    }
}

pub fn anthropic_error(status: StatusCode, kind: ErrorKind, message: impl Into<String>) -> Response<BoxedBody> {
    let body = json!({
        "type": "error",
        "error": { "type": kind.as_str(), "message": message.into() }
    });
    let bytes = serde_json::to_vec(&body).expect("static json is serializable");
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body_full(bytes))
        .expect("static response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn produces_anthropic_shaped_error_body() {
        let resp = anthropic_error(
            StatusCode::UNAUTHORIZED,
            ErrorKind::Authentication,
            "本地令牌无效",
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "authentication_error");
        assert_eq!(v["error"]["message"], "本地令牌无效");
    }
}

use crate::routing::resolve::ResolvedTarget;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub ts: String,
    pub method: String,
    pub path: String,
    pub requested_model: String,
    #[serde(rename = "matchedBy")]
    pub matched_by: String,
    pub role: Option<String>,
    pub alias: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub status: Option<u16>,
    pub stream: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

impl LogEntry {
    pub fn ok(
        method: &str,
        path: &str,
        requested: &str,
        t: &ResolvedTarget,
        status: u16,
        stream: bool,
        latency_ms: u64,
    ) -> Self {
        let mut e = Self::base(method, path, requested, t, latency_ms);
        e.status = Some(status);
        e.stream = stream;
        e
    }

    pub fn error(
        method: &str,
        path: &str,
        requested: &str,
        t: &ResolvedTarget,
        latency_ms: u64,
        message: &str,
    ) -> Self {
        let mut e = Self::base(method, path, requested, t, latency_ms);
        e.error = Some(message.to_string());
        e
    }

    /// 路由阶段就失败的日志（当前唯一场景：未知模型 + policy=error → 400）。
    /// 这条路径没有 `ResolvedTarget`，但 spec §6.7 的 `matchedBy="error"` 必须留痕：
    /// 否则"别名打错"与"请求根本没到网关"在日志这个唯一凭据里无法区分。
    pub fn routing_error(
        method: &str,
        path: &str,
        requested: &str,
        status: u16,
        latency_ms: u64,
        message: &str,
    ) -> Self {
        LogEntry {
            ts: chrono::Local::now().to_rfc3339(),
            method: method.to_string(),
            path: path.to_string(),
            requested_model: requested.to_string(),
            matched_by: crate::routing::resolve::MatchedBy::Error.as_str().to_string(),
            role: None,
            alias: String::new(),
            provider_id: String::new(),
            upstream_model: String::new(),
            status: Some(status),
            stream: false,
            latency_ms,
            error: Some(message.to_string()),
        }
    }

    fn base(method: &str, path: &str, requested: &str, t: &ResolvedTarget, latency_ms: u64) -> Self {
        LogEntry {
            ts: chrono::Local::now().to_rfc3339(),
            method: method.to_string(),
            path: path.to_string(),
            requested_model: requested.to_string(),
            matched_by: t.matched_by.as_str().to_string(),
            role: t.role_label(),
            alias: t.alias.clone(),
            provider_id: t.provider_id.clone(),
            upstream_model: t.upstream_model.clone(),
            status: None,
            stream: false,
            latency_ms,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct RequestLog {
    capacity: usize,
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    tx: broadcast::Sender<LogEntry>,
    file: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl RequestLog {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(256);
        RequestLog {
            capacity,
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            tx,
            file: Arc::new(Mutex::new(None)),
        }
    }

    pub fn push(&self, entry: LogEntry) {
        {
            let mut buf = self.entries.lock().expect("log lock poisoned");
            if buf.len() == self.capacity {
                buf.pop_front();
            }
            buf.push_back(entry.clone());
        }
        let _ = self.tx.send(entry.clone());
        let path = self.file.lock().expect("log file lock poisoned").clone();
        if let Some(path) = path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(line) = serde_json::to_string(&entry) {
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }

    pub fn set_file_logging(&self, enabled: bool, path: std::path::PathBuf) {
        *self.file.lock().expect("log file lock poisoned") = if enabled { Some(path) } else { None };
    }

    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let buf = self.entries.lock().expect("log lock poisoned");
        buf.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("log lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M3：淘汰顺序必须被真正钉住——旧的先丢、新的保留。
    /// （集成用例 `ring_buffer_drops_oldest_entries` 只用同一个 `requestedModel`，
    /// 单看它无法区分"丢最旧"与"丢最新"，所以顺序断言放在这里，用可区分的模型名。）
    #[test]
    fn ring_buffer_drops_the_oldest_and_keeps_the_newest() {
        let log = RequestLog::new(3);
        for i in 0..5 {
            log.push(LogEntry::routing_error(
                "POST",
                "/v1/messages",
                &format!("model-{i}"),
                400,
                0,
                "boom",
            ));
        }

        assert_eq!(log.len(), 3, "capacity must be enforced");
        let models: Vec<String> = log
            .recent(10)
            .into_iter()
            .map(|e| e.requested_model)
            .collect();
        assert_eq!(
            models,
            vec!["model-4", "model-3", "model-2"],
            "entries 0/1 are the oldest and must be evicted; recent() is newest-first"
        );
    }

    /// I3 的构造器形状：`matchedBy="error"`、请求模型、错误消息与返回给客户端的状态码。
    #[test]
    fn routing_error_entry_carries_error_label_status_and_message() {
        let e = LogEntry::routing_error("POST", "/v1/messages", "ccr-typo", 400, 7, "未知模型");
        assert_eq!(e.matched_by, "error");
        assert_eq!(e.requested_model, "ccr-typo");
        assert_eq!(e.status, Some(400));
        assert_eq!(e.error.as_deref(), Some("未知模型"));
        assert_eq!(e.latency_ms, 7);
        assert_eq!(e.role, None);
        assert!(e.alias.is_empty() && e.provider_id.is_empty() && e.upstream_model.is_empty());
        assert!(!e.stream);
    }
}

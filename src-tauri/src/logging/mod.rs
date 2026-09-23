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

    /// spec §6.7 的 JSONL sink（`ui.requestLogToFile` 落地用）。此前它**零覆盖**：
    /// `set_file_logging` 连一个调用方都没有，落盘分支从未被任何用例走过。
    ///
    /// 这里钉住：未启用时一个文件都不建；启用后写入**恰好一行**完整 JSON（字段名与
    /// spec §6.7 的字段表一一对应）；关闭后不再追加；重新启用是**追加**而不是截断。
    #[test]
    fn set_file_logging_mirrors_entries_to_jsonl_and_stops_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        // 父目录故意不存在：sink 必须自己建（真实场景里 logs/ 可能还没被创建过）
        let path = dir.path().join("nested/requests-2026-09-22.jsonl");
        let log = RequestLog::new(10);

        log.push(LogEntry::routing_error("POST", "/v1/messages", "ccr-before", 500, 1, "未启用"));
        assert!(!path.exists(), "未启用时不得创建任何文件");

        log.set_file_logging(true, path.clone());
        log.push(LogEntry::routing_error("POST", "/v1/messages", "ccr-typo", 400, 7, "未知模型"));

        let text = std::fs::read_to_string(&path).expect("启用后必须落盘（含自动建父目录）");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "只应有启用之后的那一条: {text:?}");

        let entry: serde_json::Value = serde_json::from_str(lines[0]).expect("必须是合法 JSON 行");
        assert!(
            entry["ts"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
            "ts 必须是非空字符串: {entry}"
        );
        assert_eq!(entry["method"], "POST");
        assert_eq!(entry["path"], "/v1/messages");
        assert_eq!(entry["requestedModel"], "ccr-typo");
        assert_eq!(entry["matchedBy"], "error");
        assert_eq!(entry["role"], serde_json::Value::Null);
        assert_eq!(entry["alias"], "");
        assert_eq!(entry["providerId"], "");
        assert_eq!(entry["upstreamModel"], "");
        assert_eq!(entry["status"], 400);
        assert_eq!(entry["stream"], false);
        assert_eq!(entry["latencyMs"], 7);
        assert_eq!(entry["error"], "未知模型");
        // 键集合与 spec §6.7 的字段表完全一致（不是"至少包含"）
        let mut keys: Vec<String> = entry.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "alias",
                "error",
                "latencyMs",
                "matchedBy",
                "method",
                "path",
                "providerId",
                "requestedModel",
                "role",
                "status",
                "stream",
                "ts",
                "upstreamModel",
            ],
            "JSONL 行的字段必须与 spec §6.7 一致"
        );

        log.set_file_logging(false, path.clone());
        log.push(LogEntry::routing_error("POST", "/v1/messages", "ccr-after", 400, 1, "关闭后"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "关闭后不得再写入"
        );

        // 重新启用同一条路径：继续追加，不截断已有日志
        log.set_file_logging(true, path.clone());
        log.push(LogEntry::routing_error("POST", "/v1/messages", "ccr-again", 200, 1, "重新启用"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }
}

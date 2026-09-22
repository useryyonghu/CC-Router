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

    fn base(method: &str, path: &str, requested: &str, t: &ResolvedTarget, latency_ms: u64) -> Self {
        LogEntry {
            ts: chrono::Local::now().to_rfc3339(),
            method: method.to_string(),
            path: path.to_string(),
            requested_model: requested.to_string(),
            matched_by: format!("{:?}", t.matched_by).to_lowercase(),
            role: t.role.map(|r| format!("{r:?}").to_lowercase()),
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

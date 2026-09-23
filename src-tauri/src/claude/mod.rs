//! Claude Code 侧文件的读写：`~/.claude/settings.json` 的接管/还原，以及
//! `~/.claude/agents/*.md` 的 `model` 字段管理。
//!
//! 本模块**不依赖 Tauri**，因此可以用普通 `cargo test` 配合临时目录完整验证。

pub mod agents;
pub mod settings;

use crate::error::{Error, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// 与 `config::store` 相同的原子写策略，但写**原始字节**：
/// 还原时必须逐字节回放备份内容，重新序列化会重排 JSON 键序，做不到 AC8 的字节级相等。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.to_path_buf(), e))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".tmp.{}.{}", std::process::id(), n));
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(tmp.clone(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    Ok(())
}

/// 时间戳后缀，用于备份文件名：`20260922T153000`。
pub fn backup_stamp() -> String {
    chrono::Local::now().format("%Y%m%dT%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_bytes_round_trips_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_write_bytes(&path, b"{\n  \"x\": 1\n}\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\n  \"x\": 1\n}\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn atomic_write_bytes_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_write_bytes(&path, b"one").unwrap();
        atomic_write_bytes(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}

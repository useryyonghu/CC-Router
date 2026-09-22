use crate::error::{Error, Result};
use super::validate::validate;
use super::Config;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 原子写：同目录临时文件 → rename 覆盖。父目录不存在则创建。
pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.to_path_buf(), e))?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::ConfigInvalid(format!("序列化失败: {e}")))?;
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| Error::io(tmp.clone(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

pub fn load_from(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    let cfg: Config = serde_json::from_str(&text).map_err(|e| Error::Json {
        path: path.to_path_buf(),
        source: e,
    })?;
    validate(&cfg).map_err(|errs| {
        Error::ConfigInvalid(
            errs.iter()
                .map(|e| format!("{}: {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    Ok(cfg)
}

/// 加载或（文件不存在时）创建首跑配置并落盘。
pub fn load_or_init(path: &Path) -> Result<Config> {
    if path.exists() {
        return load_from(path);
    }
    let cfg = Config::first_run();
    atomic_write_json(path, &cfg)?;
    Ok(cfg)
}

#[derive(Clone)]
pub struct ConfigStore {
    path: PathBuf,
    inner: Arc<RwLock<Config>>,
}

impl ConfigStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let cfg = load_or_init(&path)?;
        Ok(ConfigStore { path, inner: Arc::new(RwLock::new(cfg)) })
    }

    /// 替换内存配置并原子落盘。校验失败则内存与磁盘都不变。
    pub fn save(&self, next: Config) -> Result<()> {
        validate(&next)
            .map_err(|errs| Error::ConfigInvalid(errs.iter().map(|e| format!("{}: {}", e.field, e.message)).collect::<Vec<_>>().join("; ")))?;
        atomic_write_json(&self.path, &next)?;
        *self.inner.write().expect("config lock poisoned") = next;
        Ok(())
    }

    pub fn snapshot(&self) -> Config {
        self.inner.read().expect("config lock poisoned").clone()
    }

    /// 直接改内存不动磁盘。仅用于接管状态等由其它模块自己落盘的场景。
    pub fn replace_in_memory(&self, next: Config) {
        *self.inner.write().expect("config lock poisoned") = next;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn shared(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthStyle, ModelSpec, Provider};

    fn demo_cfg() -> Config {
        let mut cfg = Config::first_run();
        cfg.providers.push(Provider {
            id: "kimi".into(),
            name: "Kimi".into(),
            base_url: "https://api.moonshot.cn/anthropic".into(),
            api_key: "secret".into(),
            auth_style: AuthStyle::Both,
            preset_id: Some("kimi".into()),
            models_url: None,
            models_fetch: None,
            models: vec![ModelSpec {
                id: "k3".into(),
                name: "Kimi K3".into(),
                alias: "ccr-kimi-k3".into(),
                context_1m: true,
                context_window: Some(256_000),
                max_tokens: None,
            }],
        });
        cfg
    }

    #[test]
    fn round_trips_config_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = demo_cfg();
        atomic_write_json(&path, &cfg).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn writes_camel_case_keys_and_two_space_indent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        atomic_write_json(&path, &demo_cfg()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"baseUrl\""), "must use camelCase: {text}");
        assert!(text.contains("\"localToken\""));
        assert!(text.contains("\n  \"gateway\""), "must be 2-space pretty printed");
        assert!(!text.starts_with('\u{feff}'), "must not write a BOM");
    }

    #[test]
    fn does_not_leave_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        atomic_write_json(&path, &demo_cfg()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn load_or_init_creates_first_run_config_with_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = load_or_init(&path).unwrap();
        assert!(path.exists());
        assert!(cfg.gateway.local_token.starts_with("sk-ccr-"));
        assert_eq!(cfg.gateway.local_token.len(), "sk-ccr-".len() + 32);
        assert_eq!(cfg.gateway.port, crate::config::DEFAULT_PORT);
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn rejects_invalid_config_on_load_and_keeps_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = demo_cfg();
        cfg.gateway.bind = "0.0.0.0".into();
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn save_rejects_invalid_and_leaves_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let store = ConfigStore::load(&path).unwrap();
        let good = demo_cfg();
        store.save(good.clone()).unwrap();

        let mut bad = good.clone();
        bad.gateway.port = 1;
        assert!(store.save(bad).is_err());
        assert_eq!(store.snapshot(), good, "in-memory config must not change");
    }

    #[test]
    fn generated_tokens_are_unique_and_lowercase_hex() {
        let a = crate::config::generate_local_token();
        let b = crate::config::generate_local_token();
        assert_ne!(a, b);
        let hex = a.trim_start_matches("sk-ccr-");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}

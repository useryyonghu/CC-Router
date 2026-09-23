//! 按磁盘上的 config.json 启动网关。
//!
//! Plan 1 交付的是网关内核，UI（含「启动网关」按钮）属于 Plan 3；在 UI 存在之前，
//! 这是手工联调真实配置的入口。与 `smoke.rs` 的区别：`smoke.rs` 用 `Config::first_run()`
//! （没有 provider，只能验健康检查），本文件走 `ConfigStore::load`，也就是应用启动时走的同一条路径
//! ——包括校验。
//!
//! 用法：
//! ```text
//! cargo run --manifest-path src-tauri/Cargo.toml --example serve
//! ```
//! 需要 `%APPDATA%\cc-router\config.json` 通过校验；文件不存在时会生成一份首跑配置。

#[tokio::main]
async fn main() {
    let path = cc_router::app_paths::config_path();
    let store = cc_router::config::store::ConfigStore::load(&path)
        .unwrap_or_else(|e| panic!("无法加载 {}: {e}", path.display()));

    let summary = {
        let cfg = store.snapshot();
        let models: Vec<String> = cfg
            .providers
            .iter()
            .flat_map(|p| p.models.iter().map(move |m| format!("{} -> {}/{}", m.alias, p.id, m.id)))
            .collect();
        models.join("; ")
    };

    let log = cc_router::logging::RequestLog::new(500);
    let gateway = cc_router::gateway::server::start(store.shared(), log)
        .await
        .expect("网关启动失败（端口被占用？）");

    println!("config:  {}", path.display());
    println!("aliases: {}", if summary.is_empty() { "(none configured)".to_string() } else { summary });
    println!("gateway listening on 127.0.0.1:{}", gateway.bound_port);
    println!("health:  curl -s http://127.0.0.1:{}/ccr/health", gateway.bound_port);

    tokio::signal::ctrl_c().await.expect("ctrl_c handler");
    println!("\nshutting down");
    gateway.shutdown().await;
}

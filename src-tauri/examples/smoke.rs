use std::sync::{Arc, RwLock};

#[tokio::main]
async fn main() {
    let mut cfg = cc_router::config::Config::first_run();
    cfg.gateway.port = 8787;
    let log = cc_router::logging::RequestLog::new(500);
    let gw = cc_router::gateway::server::start(Arc::new(RwLock::new(cfg)), log)
        .await
        .expect("start gateway");
    println!("gateway listening on 127.0.0.1:{}", gw.bound_port);
    tokio::signal::ctrl_c().await.unwrap();
    gw.shutdown().await;
}

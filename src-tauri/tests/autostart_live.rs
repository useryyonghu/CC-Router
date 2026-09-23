//! **会真的改本机注册表**的开机自启往返测试。
//!
//! 所以它默认被 `#[ignore]`：`cargo test` **不会**碰用户的 `HKCU`。
//! 只在显式要求时运行（T3 验收步骤）：
//!
//! ```text
//! cargo test --manifest-path src-tauri/Cargo.toml --test autostart_live live_enable -- --ignored --nocapture
//! reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "CC Router"
//! cargo test --manifest-path src-tauri/Cargo.toml --test autostart_live live_disable -- --ignored --nocapture
//! reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "CC Router"
//! ```
//!
//! 拆成两个用例是为了能在两次调用之间用 `reg query` 观察注册表的真实内容。
//! 测的是 `autostart_set` 命令所调用的**同一个函数**（`cc_router::autostart::set_enabled`）；
//! `#[tauri::command]` 外壳只是把它包了一层 `State` 注入，普通测试里构造不出 `State`。

#![cfg(windows)]

/// 直接跑 `reg.exe query` 并把原始输出打出来（证据用）。
fn raw_query() -> String {
    let out = std::process::Command::new("reg.exe")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "CC Router",
        ])
        .output()
        .expect("reg.exe 起不来");
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "会真的写本机 HKCU 注册表；只在显式 --ignored 时跑"]
fn live_enable() {
    assert!(
        !cc_router::autostart::is_enabled(),
        "前置条件：本用例开始前不应已是开启状态，先跑 live_disable"
    );
    let observed = cc_router::autostart::set_enabled(true);
    println!("raw query after enable:\n{}", raw_query());
    assert!(observed, "写入之后 re-read 必须观测到“已开启”");
    assert!(cc_router::autostart::is_enabled());
}

#[test]
#[ignore = "会真的删本机 HKCU 注册表值；只在显式 --ignored 时跑"]
fn live_disable() {
    let observed = cc_router::autostart::set_enabled(false);
    println!("raw query after disable:\n{}", raw_query());
    assert!(!observed, "删除之后 re-read 必须观测到“未开启”");
    assert!(!cc_router::autostart::is_enabled());
}

//! 系统托盘：图标、右键菜单、左键显示主窗口（spec §12 / Plan 4 T2）。
//!
//! 菜单里的每一项都**转调 `commands.rs` 里同一份实现**，不在这里抄第二份逻辑
//! （ledger R33：同一段顺序/逻辑写两遍，就会在两处各错一次；这个项目已经因为
//! `refresh_takeover_token` 的写序被抄两遍而让 Claude Code 每个请求 401 过一次）。
//!
//! 「启动网关」**不做任何事件推送**：前端每 1.5s 轮询 `gateway_status`，后端状态对了
//! 界面自己就会跟上。多推一条事件只会多一条会腐烂的第二事实来源。

use crate::commands::{self, AppState};
use std::sync::Arc;
use tauri::menu::{IsMenuItem, Menu, MenuItem};
// 注意：tauri 2.11.6 里 `MouseButton` / `MouseButtonState` **没有**在 crate 根重导出，
// 它们定义在 `tauri::tray` 下（crate 根的 `tauri::MouseButton` 不存在）。
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

/// 主窗口 label。`tauri.conf.json` 没显式写 `label`，Tauri 的默认值就是 `"main"`。
pub const MAIN_WINDOW_LABEL: &str = "main";

/// 托盘图标 id（同一进程内唯一即可）。
const TRAY_ID: &str = "cc-router-tray";

/// 菜单项 id 与文案。顺序即菜单顺序（spec §12：显示主窗口 / 启停网关 / 接管 / 还原 / 退出）。
pub const MENU_ITEMS: [(&str, &str); 6] = [
    ("tray-show-window", "显示主窗口"),
    ("tray-gateway-start", "启动网关"),
    ("tray-gateway-stop", "停止网关"),
    ("tray-takeover-apply", "接管 Claude Code"),
    ("tray-takeover-restore", "还原"),
    ("tray-quit", "退出"),
];

/// 显示并聚焦主窗口。
///
/// 调用方：左键点击托盘图标、「显示主窗口」菜单项、"退出时还原失败"把窗口拉回前台
/// （见 `lib.rs::exit_app`），以及**第二次启动时由单实例守卫转达**（见 `single_instance.rs`）。
pub(crate) fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    match app.get_webview_window(MAIN_WINDOW_LABEL) {
        Some(window) => {
            // `unminimize` 要一起做：窗口处于最小化时只调 `show()`，它仍留在任务栏里，
            // 用户再点一次图标会觉得"没反应"。
            if let Err(e) = window.unminimize() {
                eprintln!("[cc-router] 还原主窗口失败：{e}");
            }
            if let Err(e) = window.show() {
                eprintln!("[cc-router] 显示主窗口失败：{e}");
            }
            if let Err(e) = window.set_focus() {
                eprintln!("[cc-router] 聚焦主窗口失败：{e}");
            }
        }
        None => eprintln!("[cc-router] 找不到主窗口 {MAIN_WINDOW_LABEL:?}，无法显示"),
    }
}

fn app_state<R: Runtime>(app: &AppHandle<R>) -> Arc<AppState> {
    app.state::<Arc<AppState>>().inner().clone()
}

/// 建托盘图标。
///
/// 图标用 `default_window_icon()`（tauri.conf.json 的 `bundle.icon` 已经就绪）——
/// 因此**不需要** `image-png` / `image-ico` 特性，只开 `tray-icon` 一个。
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let mut items = Vec::with_capacity(MENU_ITEMS.len());
    for (id, label) in MENU_ITEMS {
        items.push(MenuItem::with_id(app, id, label, true, None::<&str>)?);
    }
    let refs: Vec<&dyn IsMenuItem<R>> = items.iter().map(|item| item as &dyn IsMenuItem<R>).collect();
    let menu = Menu::with_items(app, &refs)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("CC Router")
        // 左键留给"显示主窗口"，右键才弹菜单（默认是左键也弹菜单，那样左键就没法用了）。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    match app.default_window_icon().cloned() {
        Some(icon) => builder = builder.icon(icon),
        // 没图标也要建出托盘（否则整个「退出」入口都没了），但要如实说明它会不可见。
        None => eprintln!("[cc-router] 没有可用的默认窗口图标，托盘图标会不可见"),
    }

    builder.build(app)?;
    Ok(())
}

/// 托盘动作失败时的**唯一**上报路径：写 stderr **并且**把主窗口拉到前台。
///
/// 为什么必须把窗口拉到前台，而不是只写一行 stderr：Tauri 2 核心没有对话框 API
/// （也不允许为它新增依赖），而 release 构建是 windows 子系统、根本没有控制台 ——
/// 只写 stderr 的失败在用户眼里与成功**无法区分**。他点了「接管 Claude Code」，
/// 什么都没发生，于是合理地认为它成功了，而 Claude Code 其实根本没被接管 ——
/// 这是会让人按错误前提行事的"沉默的成功"，比报错更糟。
/// 拉前台后用户能当场看到真实状态（状态页 / 日志页就在那里）。
/// 只在**失败**时调用：成功时弹窗只会打扰"随手开个网关"这类日常操作。
fn report_tray_failure<R: Runtime>(app: &AppHandle<R>, action: &str, e: &str) {
    eprintln!("[cc-router] 托盘「{action}」失败：{e}");
    show_main_window(app);
}

/// 菜单事件分发。每一项都转调 `commands.rs` 的实现。
fn on_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "tray-show-window" => show_main_window(app),
        "tray-gateway-start" => {
            // `gateway_start` 是 async：菜单回调是同步的，必须丢回 async 运行时。
            // `AppHandle` 是 `Clone`：失败上报要用它，所以克隆一份进闭包。
            let state = app_state(app);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match commands::gateway_start_impl(&state).await {
                    Ok(port) => eprintln!("[cc-router] 托盘：网关已启动，端口 {port}"),
                    Err(e) => report_tray_failure(&handle, "启动网关", &e),
                }
            });
        }
        "tray-gateway-stop" => {
            let state = app_state(app);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match commands::gateway_stop_impl(&state).await {
                    Ok(()) => eprintln!("[cc-router] 托盘：网关已停止"),
                    Err(e) => report_tray_failure(&handle, "停止网关", &e),
                }
            });
        }
        "tray-takeover-apply" => {
            let state = app_state(app);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match commands::takeover_apply_impl(&state) {
                    Ok(status) => eprintln!("[cc-router] 托盘：接管状态 = {}", status.state),
                    Err(e) => report_tray_failure(&handle, "接管 Claude Code", &e),
                }
            });
        }
        "tray-takeover-restore" => {
            let state = app_state(app);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match commands::takeover_restore_state(&state) {
                    Ok(dto) => eprintln!(
                        "[cc-router] 托盘：已还原 {}（{} 个键）",
                        dto.path,
                        dto.changed_keys.len()
                    ),
                    Err(e) => report_tray_failure(&handle, "还原", &e),
                }
            });
        }
        "tray-quit" => {
            // 走与"关窗即退出"同一条退出路径：`restoreOnExit` 打开时先还原。
            // `app.exit(0)` 不受 `prevent_close()` 影响 —— 那个只作用于 `CloseRequested`。
            crate::exit_app(app);
        }
        other => eprintln!("[cc-router] 未知的托盘菜单项：{other}"),
    }
}

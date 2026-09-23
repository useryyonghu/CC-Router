pub mod app_paths;
pub mod autostart;
pub mod claude;
pub mod commands;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod preset;
pub mod provider;
pub mod routing;
pub mod single_instance;
pub mod tray;

use crate::commands::AppState;
use crate::config::store::ConfigStore;
use std::sync::Arc;
use tauri::{AppHandle, Manager, Runtime};

/// 退出路径的**唯一**实现：关窗即退出、托盘「退出」都走这里。
///
/// 返回 `true` 表示退出已经发起；`false` 表示**拒绝退出**（见下）。
///
/// `ui.restoreOnExit` 为真时先跑一键还原：应用走了、`settings.json` 还指着已经关掉的
/// 端口，Claude Code 每个请求都会失败（spec §13 风险表）。
///
/// 还原失败时**不退出**，并把主窗口拉到前台。这里刻意不用 `let _ =` 把错误丢掉
/// （ledger R34：同一类"报错撒谎"在这个项目里刚修过一次）——静默退出会让用户以为
/// 一切正常，而 Claude Code 已被留在一个死端口上。代价是用户按「退出」时应用不会关：
/// 这是没有对话框插件时唯一能让失败可见的信号（同时写 stderr）；修好原因后再点一次
/// 就会正常退出（还原成功后 `takeover` 状态被清空，重试是幂等的）。
pub(crate) fn exit_app<R: Runtime>(app: &AppHandle<R>) -> bool {
    let state = app.state::<Arc<AppState>>();
    if state.store.snapshot().ui.restore_on_exit {
        if let Err(e) = commands::takeover_restore_state(&state) {
            eprintln!("[cc-router] 退出时一键还原失败，已取消退出以免 Claude Code 停在死端口：{e}");
            tray::show_main_window(app);
            return false;
        }
    }
    app.exit(0);
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 单实例守卫必须在**建窗口之前**做：第二次启动（用户又点了一次图标）应当把已有窗口
    // 拿到前台，而不是再开一个进程+窗口。是第二个实例就立刻返回，连配置都不碰。
    let startup = crate::single_instance::claim();
    if matches!(startup, crate::single_instance::Startup::Secondary) {
        eprintln!("[cc-router] 已有实例在运行，已通知它把窗口拿到前台；本次不再启动第二个窗口");
        return;
    }
    if let crate::single_instance::Startup::Unavailable(why) = &startup {
        // 端口被陌生程序占用：照常启动，只是这次没法保证单实例。
        eprintln!("[cc-router] 单实例检查不可用（{why}）：本次启动不做重复实例拦截");
    }

    let store = ConfigStore::load(crate::app_paths::config_path())
        .expect("无法加载 config.json：请检查 %APPDATA%\\cc-router\\config.json");
    let state = AppState::new(store).shared();

    // spec §6.7：`ui.requestLogToFile` 为真时把请求日志镜像到
    // `<logsDir>/requests-<YYYY-MM-DD>.jsonl`。此前这个开关没有任何调用方 ——
    // 配置存得下、界面点得动，但一个字节也不会落盘。
    if let Some(path) = commands::enable_file_logging_if_configured(&state) {
        eprintln!("[cc-router] 请求日志落盘：{}", path.display());
    }

    // 开机自启**故意不在这里做任何事**：`ui.autostart` 只是注册表的镜像，
    // 启动时按它去写注册表等于"用户只要打开一次应用就被静默加了自启项"。
    // 注册表只在 `autostart_set` 里改变（裁定理由见 `crate::autostart`）。

    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            // 托盘在这里装配：早于窗口显示，用户从托盘起的网关与界面看到的是同一个状态。
            crate::tray::build(app.handle())?;
            // 第一个实例：在后台线程等后续启动请求，收到就回到主线程把窗口拿到前台。
            if let crate::single_instance::Startup::Primary(listener) = startup {
                let handle = app.handle().clone();
                crate::single_instance::serve_show_requests(listener, move || {
                    let for_main = handle.clone();
                    // 窗口操作要回主线程（回调来自监听线程）。
                    if let Err(e) = handle.run_on_main_thread(move || {
                        crate::tray::show_main_window(&for_main);
                    }) {
                        eprintln!("[cc-router] 把「显示主窗口」派发到主线程失败：{e}");
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let state = window.app_handle().state::<Arc<AppState>>();
            if state.store.snapshot().ui.close_to_tray {
                // 关窗 = 最小化到托盘，进程（以及网关）继续活着 —— Claude Code 随时可能启动。
                api.prevent_close();
                if let Err(e) = window.hide() {
                    eprintln!("[cc-router] 关闭到托盘时隐藏主窗口失败：{e}");
                }
                return;
            }
            // 没开「关闭到托盘」⇒ 关窗即退出；`restoreOnExit` 的还原在 exit_app 里，
            // 失败会返回 false，此时必须 prevent_close 把进程和窗口都留下来。
            if !exit_app(window.app_handle()) {
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_create,
            commands::agent_delete,
            commands::agent_set_model,
            commands::agent_backups_list,
            commands::agent_backup_delete,
            commands::agents_list,
            commands::autostart_get,
            commands::autostart_set,
            commands::backups_list,
            commands::gateway_restart,
            commands::gateway_start,
            commands::gateway_status,
            commands::gateway_stop,
            commands::get_config,
            commands::model_add,
            commands::model_remove,
            commands::models_add_many,
            commands::models_fetch,
            commands::presets_list,
            commands::provider_add,
            commands::provider_remove,
            commands::provider_test,
            commands::provider_update,
            commands::recent_logs,
            commands::role_set,
            commands::save_config,
            commands::settings_paths,
            commands::takeover_apply,
            commands::takeover_restore,
            commands::takeover_status,
            commands::token_regenerate,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

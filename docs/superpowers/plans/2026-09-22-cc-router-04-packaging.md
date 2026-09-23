# CC Router 打包与交付 Implementation Plan（Plan 4）

> **执行后记**：无（尚未执行）

**Goal:** 把已完成的界面与网关做成**可交付的 Windows 成品**：托盘常驻、开机自启、真实图标、NSIS 安装包，并通过真机端到端验收（Claude Code 真的走网关、主 agent 与 subagent 命中不同厂商）。

**Architecture:** 在既有 Tauri 2 应用上补齐桌面集成（托盘、关窗行为、自启动、图标）与打包元数据；不改动网关与接管逻辑。E2E 验收用真实 Claude Code + 真实厂商，证据取自状态页的请求日志。

**Spec:** `docs/superpowers/specs/2026-09-22-cc-router-design.md` §4.3（生命周期）、§12（打包与交付）、§11.3（手工 E2E 清单）、§13（风险）。

## Global Constraints

- **不新增 Cargo 依赖**；只允许给 `tauri` crate 开特性（`tray-icon`），以及给 `tauri.conf.json` 补打包元数据。
- 网关只绑 `127.0.0.1`；密钥只在 `config.json`；`~/.claude/settings.json` 只有本地令牌。
- **不修改 `docs/`**；不跑仓库级 `cargo fmt`；不留新编译告警。
- 现状：Plan 1（内核）已完成并终审通过；Plan 2（接管）、Plan 3（界面）在本计划之前完成。分支 `feat/gateway-core`。

## 文件结构

```
src-tauri/icons/icon.ico / icon.png / 32x32.png / 128x128.png / 128x128@2x.png   (T1) 真实图标
src-tauri/src/tray.rs        (T2) 托盘图标与菜单
src-tauri/src/autostart.rs   (T3) 开机自启（HKCU Run 项，reg.exe，无新依赖）
src-tauri/src/lib.rs         (T2/T3) 装配托盘与窗口事件
src-tauri/tauri.conf.json    (T1/T4) 图标清单 + 打包元数据
README.md                    (T5) 交付文档
```

---

## T1: 真实图标

**Files:** `src-tauri/icons/*`（新增/替换）、`src-tauri/tauri.conf.json`

- [ ] **Step 1: 生成图标**（PowerShell + .NET `System.Drawing`，无需新工具）：

设计：1024×1024 圆角方块，背景深蓝 `#1F3A5F`，前景两条水平箭头（`→` 与 `←`）表意"分流/双向路由"，颜色 `#5EEAD4`。用 `System.Drawing` 画好另存 PNG，再用 .NET 缩放导出 Tauri 需要的尺寸。
产出：`icon.png`(512)、`32x32.png`、`128x128.png`、`128x128@2x.png`(256)、`icon.ico`（多尺寸 ICO，含 16/32/48/256）。**必须真的画出来，不能留占位**（Plan 1 现在是一个 64×64 纯色占位）。

- [ ] **Step 2: 更新 `tauri.conf.json` 的 `bundle.icon`** 为 `["icons/32x32.png","icons/128x128.png","icons/128x128@2x.png","icons/icon.ico"]`。
- [ ] **Step 3: 验证** `cargo build --manifest-path src-tauri/Cargo.toml` 成功、`pnpm tauri build` 成功产出安装包。
- [ ] **Step 4: Commit** `chore(icons): 真实应用图标`

## T2: 托盘与关窗行为

**Files:** `src-tauri/src/tray.rs`（新）、`src-tauri/src/lib.rs`、`src-tauri/Cargo.toml`（`tauri` 加 `tray-icon` 特性）

- [ ] **Step 1: 开特性**：`tauri = { version = "2", features = ["tray-icon"] }`。
- [ ] **Step 2: 实现 `tray.rs`**：
  - `pub fn build(app: &tauri::AppHandle, state: Arc<AppState>) -> tauri::Result<()>`，菜单项：`显示主窗口`、`启动网关`、`停止网关`、`接管 Claude Code`、`还原`、`退出`。
  - 每项 `on_menu_event` 调 `commands` 里已有的逻辑（把 IPC 命令的实现抽成 `pub(crate) async fn` 供托盘复用，**不要让托盘重复实现**）。
  - 左键点击托盘图标 → 显示并聚焦主窗口。
- [ ] **Step 3: 关窗行为**：在 `lib.rs` 的 `.on_window_event(...)` 里，`WindowEvent::CloseRequested` 时读配置：`ui.closeToTray == true` → `api.prevent_close()` + `window.hide()`；否则走正常退出。退出时若 `ui.restoreOnExit == true` → 先 `takeover_restore` 再退出。
- [ ] **Step 4: 托盘 "启动网关" 在启动后应把 `gateway_start` 的结果同步回 UI**（UI 每 1.5s 轮询 `gateway_status`，因此只要后端状态对，UI 会自己跟上——**不需要事件推送**，实现时不要为此改后端）。
- [ ] **Step 5: 验证** `cargo build` 成功；手工跑 `pnpm tauri dev`，确认托盘出现、菜单可用、关窗后进程仍在且托盘可用、托盘"退出"能真正退出。
- [ ] **Step 6: Commit** `feat(desktop): 托盘常驻与关窗行为`

## T3: 开机自启

**Files:** `src-tauri/src/autostart.rs`（新）、`commands.rs`（`autostart_set(enabled: bool)`、`autostart_get() -> bool`）、`lib.rs`

- [ ] **Step 1: 实现（无新依赖）**：写/删 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下名为 `CC Router` 的字符串值，数据为当前 exe 的绝对路径（加引号）。读写用 `reg.exe query/add/delete` 走 `std::process::Command`（`CREATE_NO_WINDOW`），解析 `reg query` 输出判断是否已启用。**不申请管理员权限**（HKCU 不需要）。
- [ ] **Step 2: 与配置同步**：设置页的开关调 `autostart_set`；`autostart_get` 以**注册表真实状态**为准（不是配置里的值），避免配置与系统不一致。
- [ ] **Step 3: 验证**：手工调用一次 `autostart_set(true)` → `reg query` 确认键存在 → `autostart_set(false)` → 确认键消失。把两次 `reg query` 输出贴进报告。
- [ ] **Step 4: Commit** `feat(desktop): 开机自启（HKCU Run）`

## T4: 打包与元数据

**Files:** `src-tauri/tauri.conf.json`

- [ ] **Step 1: 补打包元数据**：`productName: "CC Router"`、`version: "0.1.0"`、`identifier: "com.ccrouter.desktop"`、`bundle.publisher`、`bundle.shortDescription`/`longDescription`、`bundle.targets: ["nsis"]`、`bundle.windows.nsis` 的 `installMode: "currentUser"`（免管理员）、`languages: ["SimpChinese","English"]`、`displayLanguageSelector: false`。
- [ ] **Step 2: 确认安装包**：`pnpm tauri build` → `src-tauri/target/release/bundle/nsis/CC Router_0.1.0_x64-setup.exe`。记录体积。
- [ ] **Step 3: 确认打的不是空壳**：安装后打开，五页都在、能启动网关、能接管（这是本计划真正的出口条件——Plan 1 曾经打出一个只有"骨架就绪"的安装包）。
- [ ] **Step 4: Commit** `chore(bundle): NSIS 打包元数据与免管理员安装`

## T5: README 与交付说明

**Files:** `README.md`（仓库根，新建）

- [ ] 内容必须包含：
  1. 一句话说明它解决什么问题（Claude Code 只能配一个第三方端点 → 本网关按别名分流）。
  2. 安装与首次配置（双击安装 → 打开 → 服务商页选预设或自定义 → 角色路由页分配模型 → 一键接管）。
  3. **明文密钥警示**：`%APPDATA%\cc-router\config.json` 含明文密钥，不要提交/分享。
  4. 接管与还原：改了什么键、备份在哪、如何逐字节还原。
  5. **与 cc-switch 的关系**：两者都会写 `~/.claude/settings.json`；本应用检测到被外部改写会在状态页显示红色横幅，不做自动互抢。
  6. 故障排查：网关未运行（Claude Code 会连不上）、端口被占用、上游 401/404 的含义、日志页怎么看 `matchedBy`。
  7. **第三方许可**：预设数据来自 `farion1231/cc-switch`（MIT，Copyright © 2025 Jason Young），附许可证全文位置。
  8. 隐私：无遥测；只转发 Claude Code 的请求。
- [ ] Commit `docs: README 交付说明`

## T6: 真机端到端验收（spec §11.3）

- [ ] 按下列顺序逐条执行并**把每条的观察结果贴进报告**（找不回证据的条目视为未通过）：
  1. 配置 main=厂商A、subagent=厂商B（**不同厂商**，这是整个产品的核心卖点），fast=厂商A 或 B。
  2. 一键接管 → 打开状态页确认接管状态为 applied。
  3. 启动一个真实 `claude` 会话问一句话 → 状态页日志出现 `matchedBy=alias`、`provider=A`。
  4. 触发一次子 agent（让 Claude Code 用 Task 工具做一件小事）→ 日志出现 `provider=B`。
  5. 在某个子 Agent 上指定第三家厂商的模型 → 触发它 → 日志显示对应 provider。
  6. 在 Claude Code 里 `/model haiku` → 日志 `role=fast`。
  7. 一键还原 → `~/.claude/settings.json` 的 SHA256 与接管前一致 → `claude` 恢复直连原端点可用。
  8. 卸载：确认卸载后 `~/.claude/settings.json` 未被破坏（若之前是接管状态，安装包**不**负责还原；README 必须写明卸载前先还原）。
- [ ] 报告里给出每条的**日志行原文**（provider / upstreamModel / status）作为证据。
- [ ] Commit（若过程中需要修小问题）：按需。

---

## 出口条件（本计划的"成品"定义）

- `pnpm tauri build` 产出 `CC Router_0.1.0_x64-setup.exe`，**在该机器上双击安装后可用**（不是空壳）。
- 五项界面全部可用；托盘常驻；关窗最小化到托盘；开机自启可开可关且以注册表为准。
- T6 的 8 条验收全部通过并留有日志原文证据；其中第 3、4 条（主 agent 与 subagent 命中**不同厂商**）是本产品存在的理由。
- README 可交付（含明文密钥警示、接管/还原说明、cc-switch 共存说明、MIT 署名）。
- 全量 `cargo test` 仍全绿。

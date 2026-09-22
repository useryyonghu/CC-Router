# CC Router 网关内核 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付一个可独立测试的多模型路由网关内核 —— 它监听 `127.0.0.1`，把 Claude Code 的 `/v1/messages` 请求按请求体中的 `model` 别名分发到不同厂商的 Anthropic 兼容端点，零缓冲透传 SSE。

**Architecture:** 单个 Tauri 2 应用进程，Rust 侧分四层：`config`（配置与原子落盘）、`routing`（别名表与解析，纯函数）、`gateway`（hyper 服务 + reqwest 上游转发）、`logging`（请求日志环形缓冲）。**核心三层完全不依赖 Tauri**，因此可用普通 `cargo test` 做集成测试；Tauri 只负责窗口与 IPC（本计划只到 M0 骨架 + 一个最小 IPC 面）。

**Tech Stack:** Rust 1.97 / Tauri 2 / tokio / hyper 1 + hyper-util（服务端）/ reqwest 0.12（上游客户端，含 TLS）/ serde / thiserror；前端 Vue 3 + TypeScript + Vite + Naive UI（本计划只做骨架）。

**Spec:** `docs/superpowers/specs/2026-09-22-cc-router-design.md`（executor 必须同时读 spec）

## Global Constraints

- **核心逻辑不得依赖 Tauri**：`config` / `routing` / `gateway` / `logging` 四个模块及其依赖不得 `use tauri::*`。只有 `lib.rs`、`commands.rs`、托盘相关代码可以。目的：集成测试可脱离 Tauri 运行时。
- **只监听 `127.0.0.1`**，绝不绑定 `0.0.0.0`（spec §9.3）。绑定地址来自 `config.gateway.bind`，默认 `127.0.0.1`。
- **除 `/ccr/health` 外所有请求都必须校验本地令牌**，失败返回 `401` + Anthropic 错误体（spec §6.1）。
- **`app_data_dir` 固定为 `%APPDATA%\cc-router`**（spec §5.1）。不允许用 Tauri 的 `appDataDir`（会带上 bundle id）。
- **所有写盘必须原子**：写同目录临时文件 → `std::fs::rename` 覆盖（spec §5.1）。
- **config.json 用 2 空格缩进、UTF-8、无 BOM**（spec §5.1）。
- **SSE 不得缓冲**：上游 `text/event-stream` 收到一块立刻转发一块，不等待流结束（spec §6.6）。
- **不在流上做任何解析/统计**（spec §6.6），因此本期无 token 计数。
- **别名不带 `[1M]` 后缀**；上游收到的模型名永远不含该后缀（spec §6.4）。
- **请求体上限 128 MiB**（`134217728`），超限返回 `413`（spec §6.3）。
- 错误体统一形状：`{"type":"error","error":{"type":"<kind>","message":"<msg>"}}`（spec §6.5）。
- 错误消息中必须带上别名与 provider 名（spec §6.5）。
- 默认端口 `8787`；连接超时 `10000ms`；空闲超时 `300000ms`（spec §5.2）。
- **无遥测**：除转发 Claude Code 请求外不发起任何网络请求（spec §9.3）。

## Review Focus

以下 7 类输入/失败模式 spec 隐含要求但容易漏测。每条都已在对应任务里加了测试：

1. **请求体不是 JSON，或缺少 `model` 字段** → 必须返回 `400` Anthropic 错误体；不得 500、不得 panic、不得把原始解析错误字符串直接吐给用户。（Task 6）
2. **`model` 字段不是字符串**（数字 / `null` / 对象）→ 同 1，按无效请求处理。（Task 6）
3. **上游返回非 2xx 且正文不是 JSON**（HTML 错误页、Cloudflare 502 页）→ 必须原样透传状态码与正文，不得尝试解析、不得替换成我们自己的错误体。（Task 6）
4. **端口被占用导致绑定失败** → 返回明确错误，进程不 panic，且不产生任何副作用。（Task 6）
5. **客户端在 SSE 中途断开** → 必须取消上游请求，不得留下悬挂任务或继续缓冲。（Task 7）
6. **`model` 字段前后有空白、大小写混杂、或带 `[1M]`/`[1m]` 后缀** → 都要命中同一条别名规则。（Task 4）
7. **上游在中途断流（发送若干块后连接复位）** → 已转发的块保持完整，客户端收到流的结束，不得 panic。（Task 7）

---## 文件结构

```
cc-router/
├ .gitignore                                  (Task 1)
├ package.json / vite.config.ts / tsconfig*.json / index.html   (Task 1)
├ src/                                       # Vue 前端（本计划仅骨架）
│  ├ main.ts / App.vue / vite-env.d.ts        (Task 1)
├ src-tauri/
│  ├ Cargo.toml / build.rs / tauri.conf.json  (Task 1)
│  ├ .cargo/config.toml                       (Task 1, 仅在需要镜像时创建)
│  ├ src/
│  │  ├ main.rs                               (Task 1) 调用 lib 的 run()
│  │  ├ lib.rs                                (Task 1, Task 8 修改) 模块声明 + Tauri run() + IPC
│  │  ├ error.rs                              (Task 2) 统一 Error 类型
│  │  ├ app_paths.rs                          (Task 2) %APPDATA%\cc-router 路径解析
│  │  ├ config/
│  │  │  ├ mod.rs                             (Task 2) Config 及其子结构体
│  │  │  ├ validate.rs                        (Task 2) 校验规则
│  │  │  └ store.rs                           (Task 2) ConfigStore：加载/原子保存/热更新
│  │  ├ routing/
│  │  │  ├ mod.rs                             (Task 3, Task 4) 导出
│  │  │  ├ alias.rs                           (Task 3) slug / id 推导 / 别名生成
│  │  │  └ resolve.rs                         (Task 4) RouteTable + resolve
│  │  ├ gateway/
│  │  │  ├ mod.rs                             (Task 6) start/stop/status
│  │  │  ├ error.rs                           (Task 6) Anthropic 错误体构造
│  │  │  ├ rewrite.rs                         (Task 5) model 改写 + 头构造 + 鉴权注入
│  │  │  ├ server.rs                          (Task 6, Task 7 修改) 监听、分发、转发
│  │  │  └ body.rs                            (Task 6, Task 7 修改) BoxedBody 与流包装
│  │  ├ logging/
│  │  │  └ mod.rs                             (Task 8) RequestLog 环形缓冲 + 订阅
│  │  └ commands.rs                           (Task 8) Tauri IPC 命令
│  └ tests/
│     ├ support/mod.rs                        (Task 6) mock 上游 + 配置夹具
│     ├ gateway_nonstream.rs                  (Task 6)
│     ├ gateway_stream.rs                     (Task 7)
│     └ request_log.rs                        (Task 8)
```

**每个文件一个职责**：`rewrite.rs` 只做纯函数式改写（可单测、无 IO），`server.rs` 只做 HTTP 分发与生命周期，`resolve.rs` 只做模型名 → 目标的解析。这样即使 `server.rs` 出错，路由与改写逻辑的测试仍然全绿。

---

## Task 1: 项目骨架与工具链验证（M0）

这是整个项目的**唯一前置风险点**：Tauri 全量构建需要数百个 crate，本机 cargo 缓存只有部分。本任务必须先跑通，否则后续全部白做。

**Files:**
- Create: `.gitignore`
- Create: `package.json`, `vite.config.ts`, `tsconfig.json`, `tsconfig.node.json`, `index.html`
- Create: `src/main.ts`, `src/App.vue`, `src/vite-env.d.ts`
- Create: `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/tauri.conf.json`
- Create: `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: 无（起点）
- Produces: `cc_router` 库 crate（`src-tauri/src/lib.rs` 暴露 `pub fn run()`）；后续所有 Rust 模块都挂在它下面。前端产物 `dist/` 由 `tauri.conf.json` 的 `frontendDist` 指向。

- [ ] **Step 1: 写 `.gitignore`**

```
node_modules/
dist/
src-tauri/target/
*.log
```

- [ ] **Step 2: 写前端骨架 `package.json`**

```json
{
  "name": "cc-router",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vue-tsc --noEmit && vite build",
    "tauri": "tauri"
  },
  "dependencies": {
    "@tauri-apps/api": "^2",
    "naive-ui": "^2",
    "pinia": "^2",
    "vue": "^3"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2",
    "@vitejs/plugin-vue": "^5",
    "typescript": "^5",
    "vite": "^6",
    "vue-tsc": "^2"
  }
}
```

- [ ] **Step 3: 写 `vite.config.ts`、`tsconfig.json`、`tsconfig.node.json`、`index.html`**

`vite.config.ts`:

```ts
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true, target: "chrome110" },
});
```

`tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "jsx": "preserve",
    "lib": ["ES2022", "DOM"],
    "types": ["vite/client"],
    "skipLibCheck": true,
    "noEmit": true,
    "isolatedModules": true
  },
  "include": ["src/**/*.ts", "src/**/*.vue"]
}
```

`tsconfig.node.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["vite.config.ts"]
}
```

`index.html`:

```html
<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>CC Router</title>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
```

- [ ] **Step 4: 写 `src/main.ts`、`src/App.vue`、`src/vite-env.d.ts`**

`src/main.ts`:

```ts
import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";

createApp(App).use(createPinia()).mount("#app");
```

`src/App.vue`:

```vue
<script setup lang="ts">
import { NConfigProvider, NMessageProvider, NLayout, NLayoutHeader, NLayoutContent, NH1, NP } from "naive-ui";
</script>

<template>
  <n-config-provider>
    <n-message-provider>
      <n-layout style="height: 100vh">
        <n-layout-header bordered style="padding: 16px">
          <n-h1 style="margin: 0">CC Router</n-h1>
        </n-layout-header>
        <n-layout-content style="padding: 16px">
          <n-p>骨架就绪。网关内核见实施计划 Task 2 起。</n-p>
        </n-layout-content>
      </n-layout>
    </n-message-provider>
  </n-config-provider>
</template>
```

`src/vite-env.d.ts`:

```ts
/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
```

- [ ] **Step 5: 写 `src-tauri/Cargo.toml`**

```toml
[package]
name = "cc-router"
version = "0.1.0"
edition = "2021"
rust-version = "1.77"

[lib]
name = "cc_router"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "time", "sync", "io-util"] }
hyper = { version = "1", features = ["server", "http1", "client"] }
hyper-util = { version = "0.1", features = ["tokio", "server", "server-auto"] }
http-body-util = "0.1"
bytes = "1"
futures-util = "0.3"
reqwest = { version = "0.12", features = ["stream", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
rand = "0.8"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 6: 写 `src-tauri/build.rs`、`src-tauri/tauri.conf.json`、`src-tauri/src/main.rs`、`src-tauri/src/lib.rs`**

`src-tauri/build.rs`:

```rust
fn main() {
    tauri_build::build()
}
```

`src-tauri/tauri.conf.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "CC Router",
  "version": "0.1.0",
  "identifier": "com.ccrouter.desktop",
  "build": {
    "beforeDevCommand": "pnpm dev",
    "beforeBuildCommand": "pnpm build",
    "devUrl": "http://localhost:1420",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      {
        "title": "CC Router — Claude Code 多模型路由",
        "width": 1100,
        "height": 720,
        "resizable": true
      }
    ],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": true,
    "targets": ["nsis"],
    "icon": []
  }
}
```

`src-tauri/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cc_router::run()
}
```

`src-tauri/src/lib.rs`:

```rust
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 7: 装前端依赖并确认安装源可用**

Run: `pnpm install`
Expected: 成功，生成 `pnpm-lock.yaml`，无 `ERR_PNPM_...` 网络错误。

- [ ] **Step 8: 验证 cargo 能拉全依赖（关键风险点）**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: `Finished` 无 error。

若出现 `failed to get ... as a dependency` / `network failure` / `spurious network error`，执行镜像回退：创建 `src-tauri/.cargo/config.toml` 并重跑：

```toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

（本机 `~/.cargo/registry/cache` 已有 `mirrors.tuna.tsinghua.edu.cn-*` 的缓存痕迹，说明该镜像此前可用。）

- [ ] **Step 9: 构建前端**

Run: `pnpm build`
Expected: 生成 `dist/index.html`，`vue-tsc` 无类型错误。

- [ ] **Step 10: 构建 exe（M0 的出口条件）**

Run: `pnpm tauri build`
Expected: 在 `src-tauri/target/release/bundle/nsis/` 下生成 `CC Router_0.1.0_x64-setup.exe`。

若 `bundle.icon` 为空导致打包报 icon 缺失错误，改为 `"icon": []` 并在 `tauri.conf.json` 的 `bundle` 里保留 `"targets": ["nsis"]`；仍报错则临时设 `"active": false` 先跑通 `pnpm tauri build --no-bundle`，把 icon 补齐留给 Task 8 之后的打包计划。

- [ ] **Step 11: Commit**

```bash
git add .gitignore package.json pnpm-lock.yaml vite.config.ts tsconfig.json tsconfig.node.json index.html src/ src-tauri/
git commit -m "chore: Tauri 2 + Vue 3 + Naive UI 骨架，验证工具链可构建 exe"
```

---

## Task 2: 配置类型、校验与原子读写

**Files:**
- Create: `src-tauri/src/error.rs`
- Create: `src-tauri/src/app_paths.rs`
- Create: `src-tauri/src/config/mod.rs`
- Create: `src-tauri/src/config/validate.rs`
- Create: `src-tauri/src/config/store.rs`
- Modify: `src-tauri/src/lib.rs`（增加 `pub mod` 声明）
- Test: `src-tauri/src/config/validate.rs` 内 `#[cfg(test)] mod tests`
- Test: `src-tauri/src/config/store.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 无
- Produces:
  - `cc_router::config::{Config, GatewayConfig, Provider, ModelSpec, Roles, Target, ExtraRoute, AuthStyle, UnknownModelPolicy, UiConfig, TakeoverState, ModelsFetch}`
  - `cc_router::config::{DEFAULT_PORT, DEFAULT_MAX_BODY_BYTES, DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_IDLE_TIMEOUT_MS}`
  - `cc_router::config::validate::{validate, ConfigError}`
  - `cc_router::config::store::{ConfigStore, atomic_write_json}`
  - `cc_router::app_paths::{app_data_dir, config_path, backups_dir, logs_dir}`
  - `cc_router::error::{Error, Result}`

- [ ] **Step 1: 写 `src-tauri/src/error.rs`**

```rust
use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("配置校验失败: {0}")]
    ConfigInvalid(String),
    #[error("读取 {path} 失败: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON 解析失败 ({path}): {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("网关绑定 {addr} 失败: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("网关响应错误: {0}")]
    Gateway(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }
}
```

- [ ] **Step 2: 写 `src-tauri/src/app_paths.rs`**

```rust
use std::path::PathBuf;

/// `%APPDATA%\cc-router`。刻意不用 Tauri 的 appDataDir（那会带上 bundle id）。
pub fn app_data_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(base) => PathBuf::from(base).join("cc-router"),
        None => PathBuf::from(".cc-router"),
    }
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.json")
}

pub fn backups_dir() -> PathBuf {
    app_data_dir().join("backups")
}

pub fn logs_dir() -> PathBuf {
    app_data_dir().join("logs")
}
```

- [ ] **Step 3: 写 `src-tauri/src/config/mod.rs`**

```rust
pub mod store;
pub mod validate;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 8787;
pub const DEFAULT_BIND: &str = "127.0.0.1";
pub const DEFAULT_MAX_BODY_BYTES: usize = 134_217_728; // 128 MiB
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub extra_routes: Vec<ExtraRoute>,
    pub on_unknown_model: UnknownModelPolicy,
    #[serde(default)]
    pub default_target: Option<Target>,
    #[serde(default)]
    pub takeover: TakeoverState,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    pub bind: String,
    pub port: u16,
    pub local_token: String,
    pub max_request_body_bytes: usize,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub models_url: Option<String>,
    #[serde(default)]
    pub models_fetch: Option<ModelsFetch>,
    #[serde(default)]
    pub models: Vec<ModelSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    Both,
    XApiKey,
    Bearer,
}

impl Default for AuthStyle {
    fn default() -> Self {
        AuthStyle::Both
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsFetch {
    pub last_at: String,
    pub last_url: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub alias: String,
    #[serde(default)]
    pub context_1m: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Roles {
    #[serde(default)]
    pub main: Option<Target>,
    #[serde(default)]
    pub fast: Option<Target>,
    #[serde(default)]
    pub subagent: Option<Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtraRoute {
    pub alias: String,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UnknownModelPolicy {
    Default,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    pub close_to_tray: bool,
    pub autostart: bool,
    pub request_log_to_file: bool,
    pub restore_on_exit: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            close_to_tray: true,
            autostart: true,
            request_log_to_file: false,
            restore_on_exit: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverState {
    pub enabled: bool,
    #[serde(default)]
    pub applied_at: Option<String>,
    #[serde(default)]
    pub backup_file: Option<String>,
    #[serde(default)]
    pub settings_keys: serde_json::Value,
    #[serde(default)]
    pub agent_files: serde_json::Value,
}

impl Config {
    /// 首次运行用的默认配置：无 provider、无角色绑定、策略 default、令牌随机生成。
    pub fn first_run() -> Self {
        Config {
            version: 1,
            gateway: GatewayConfig {
                bind: DEFAULT_BIND.to_string(),
                port: DEFAULT_PORT,
                local_token: generate_local_token(),
                max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
                connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
                idle_timeout_ms: DEFAULT_IDLE_TIMEOUT_MS,
            },
            providers: Vec::new(),
            roles: Roles::default(),
            extra_routes: Vec::new(),
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: None,
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }
}

pub fn generate_local_token() -> String {
    use rand::Rng;
    const HEX: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    let mut s = String::from("sk-ccr-");
    for _ in 0..32 {
        s.push(HEX[rng.gen_range(0..16)] as char);
    }
    s
}
```

- [ ] **Step 4: 写 `src-tauri/src/config/validate.rs`（含测试）**

校验规则来自 spec §5.3。注意 `roles.*` 允许 `null`，`models_url` 允许 `null`。

```rust
use super::{Config, Target};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: String,
    pub message: String,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError { field: field.into(), message: message.into() }
    }
}

/// api_key 必须能作为 HTTP 请求头的值发送：非空、无首尾空白、且字节只允许 tab 与 0x20..=0x7E。
/// 这里只在**本地**判定字节范围，而不调用 `HeaderValue::from_str`，以免让 `config` 依赖 HTTP 层。
///
/// 注意方向性：本谓词接受的范围是 `HeaderValue::from_str` 的**严格子集**
/// （`from_str` 还接受 0x80..=0xFF 的 obs-text），所以"通过校验 ⇒ 一定能构造出 HeaderValue"
/// 这个保证无条件成立；代价是像 `sk-密钥` 这种含非 ASCII 的 key 也会被拒——这是刻意的卫生规则，
/// 不是一个漏洞。加这条校验的原因：`build_headers` 里对非法 key 会静默降级成**空凭据**，
/// 于是一个坏 key 会表现为一个无法与"厂商挂了"区分的 401。
fn valid_api_key(s: &str) -> bool {
    !s.trim().is_empty()
        && s == s.trim()
        && s.bytes().all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

fn valid_provider_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0] as char;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| matches!(*b as char, 'a'..='z' | '0'..='9' | '-'))
}

pub fn validate(cfg: &Config) -> Result<(), Vec<ConfigError>> {
    let mut errs = Vec::new();

    if !(1024..=65535).contains(&cfg.gateway.port) {
        errs.push(ConfigError::new("gateway.port", "端口必须在 1024..=65535"));
    }
    if cfg.gateway.bind != "127.0.0.1" && cfg.gateway.bind != "::1" && cfg.gateway.bind != "localhost" {
        errs.push(ConfigError::new(
            "gateway.bind",
            "只允许监听回环地址（127.0.0.1 / ::1 / localhost）",
        ));
    }
    if cfg.gateway.local_token.trim().is_empty() {
        errs.push(ConfigError::new("gateway.localToken", "本地令牌不能为空"));
    }
    if cfg.gateway.max_request_body_bytes == 0 {
        errs.push(ConfigError::new("gateway.maxRequestBodyBytes", "请求体上限必须大于 0"));
    }

    let mut provider_ids: HashSet<&str> = HashSet::new();
    let mut aliases: HashSet<&str> = HashSet::new();

    for (pi, p) in cfg.providers.iter().enumerate() {
        let pf = |f: &str| format!("providers[{pi}].{f}");
        if !valid_provider_id(&p.id) {
            errs.push(ConfigError::new(pf("id"), "必须是 ^[a-z0-9][a-z0-9-]{0,31}$"));
        }
        if !provider_ids.insert(p.id.as_str()) {
            errs.push(ConfigError::new(pf("id"), "provider id 重复"));
        }
        if !(p.base_url.starts_with("http://") || p.base_url.starts_with("https://")) {
            errs.push(ConfigError::new(pf("baseUrl"), "必须以 http:// 或 https:// 开头"));
        } else if is_self_loop(&p.base_url, &cfg.gateway) {
            errs.push(ConfigError::new(
                pf("baseUrl"),
                "不允许指向本网关自身（会自环）",
            ));
        }
        if let Some(mu) = &p.models_url {
            if !(mu.starts_with("http://") || mu.starts_with("https://")) {
                errs.push(ConfigError::new(pf("modelsUrl"), "只能是完整 http(s):// URL 或 null"));
            }
        }
        if !valid_api_key(&p.api_key) {
            errs.push(ConfigError::new(
                pf("apiKey"),
                "密钥不能为空，不能含首尾空白，且只能包含可打印 ASCII 字符（否则无法作为 HTTP 请求头发送）",
            ));
        }
        let mut model_ids: HashSet<&str> = HashSet::new();
        for (mi, m) in p.models.iter().enumerate() {
            let mf = |f: &str| format!("providers[{pi}].models[{mi}].{f}");
            if m.id.trim().is_empty() {
                errs.push(ConfigError::new(mf("id"), "模型 id 不能为空"));
            }
            if !model_ids.insert(m.id.as_str()) {
                errs.push(ConfigError::new(mf("id"), "同一 provider 内模型 id 重复"));
            }
            if m.alias.trim().is_empty() {
                errs.push(ConfigError::new(mf("alias"), "别名不能为空"));
            }
            if !aliases.insert(m.alias.as_str()) {
                errs.push(ConfigError::new(mf("alias"), "别名全局重复"));
            }
        }
    }

    for r in cfg.extra_routes.iter() {
        let ef = |f: &str| format!("extraRoutes[{}].{f}", r.alias);
        if r.alias.trim().is_empty() {
            errs.push(ConfigError::new("extraRoutes[].alias", "别名不能为空"));
        } else if !aliases.insert(r.alias.as_str()) {
            errs.push(ConfigError::new(ef("alias"), "别名与已有别名冲突"));
        }
        check_target(cfg, &ef(""), &Target { provider_id: r.provider_id.clone(), model_id: r.model_id.clone() }, &mut errs);
    }

    for (name, slot) in [
        ("roles.main", &cfg.roles.main),
        ("roles.fast", &cfg.roles.fast),
        ("roles.subagent", &cfg.roles.subagent),
    ] {
        if let Some(t) = slot {
            check_target(cfg, name, t, &mut errs);
        }
    }

    if let Some(t) = &cfg.default_target {
        check_target(cfg, "defaultTarget", t, &mut errs);
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

fn check_target(cfg: &Config, field: &str, t: &Target, errs: &mut Vec<ConfigError>) {
    match cfg.providers.iter().find(|p| p.id == t.provider_id) {
        None => errs.push(ConfigError::new(
            field.to_string(),
            format!("providerId '{}' 不存在", t.provider_id),
        )),
        Some(p) => {
            if !p.models.iter().any(|m| m.id == t.model_id) {
                errs.push(ConfigError::new(
                    field.to_string(),
                    format!("provider '{}' 下不存在模型 '{}'", t.provider_id, t.model_id),
                ));
            }
        }
    }
}

/// 防自环：baseUrl 的 host:port 命中本网关监听地址即视为自环。
fn is_self_loop(base_url: &str, gw: &super::GatewayConfig) -> bool {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let host_port = rest.split('/').next().unwrap_or("");
    let host_port = host_port.split('?').next().unwrap_or(host_port);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()),
        None => (host_port, None),
    };
    let host_is_loopback = matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if !host_is_loopback {
        return false;
    }
    match port {
        Some(p) => p == gw.port,
        // 未写端口时按 scheme 默认端口比较
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthStyle, ExtraRoute, GatewayConfig, ModelSpec, Provider, Roles, TakeoverState, UiConfig,
        UnknownModelPolicy, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };

    fn gw() -> GatewayConfig {
        GatewayConfig {
            bind: DEFAULT_BIND.to_string(),
            port: 8787,
            local_token: "sk-ccr-test".to_string(),
            max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 300_000,
        }
    }

    fn provider(id: &str, base: &str, models: &[(&str, &str)]) -> Provider {
        Provider {
            id: id.to_string(),
            name: id.to_string(),
            base_url: base.to_string(),
            api_key: "k".to_string(),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models: models
                .iter()
                .map(|(mid, alias)| ModelSpec {
                    id: mid.to_string(),
                    name: mid.to_string(),
                    alias: alias.to_string(),
                    context_1m: false,
                    context_window: None,
                    max_tokens: None,
                })
                .collect(),
        }
    }

    fn base_cfg() -> Config {
        Config {
            version: 1,
            gateway: gw(),
            providers: vec![
                provider("kimi", "https://api.moonshot.cn/anthropic", &[("k3", "ccr-kimi-k3")]),
                provider("deepseek", "https://api.deepseek.com/anthropic", &[("ds-flash", "ccr-deepseek-ds-flash")]),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
            },
            extra_routes: Vec::new(),
            on_unknown_model: UnknownModelPolicy::Default,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    #[test]
    fn accepts_a_well_formed_config() {
        assert_eq!(validate(&base_cfg()), Ok(()));
    }

    #[test]
    fn allows_unbound_roles_and_null_default_target() {
        let mut cfg = base_cfg();
        cfg.roles.main = None;
        cfg.roles.fast = None;
        cfg.roles.subagent = None;
        cfg.default_target = None;
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let mut cfg = base_cfg();
        cfg.gateway.bind = "0.0.0.0".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "gateway.bind"));
    }

    #[test]
    fn rejects_self_loop_base_url() {
        let mut cfg = base_cfg();
        cfg.providers[0].base_url = "http://127.0.0.1:8787/anthropic".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].baseUrl"));
    }

    #[test]
    fn allows_loopback_base_url_on_a_different_port() {
        let mut cfg = base_cfg();
        cfg.providers[0].base_url = "http://127.0.0.1:9999/anthropic".to_string();
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn rejects_duplicate_aliases_across_providers() {
        let mut cfg = base_cfg();
        cfg.providers[1].models[0].alias = "ccr-kimi-k3".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field.ends_with(".alias")));
    }

    #[test]
    fn rejects_extra_route_alias_colliding_with_model_alias() {
        let mut cfg = base_cfg();
        cfg.extra_routes.push(ExtraRoute {
            alias: "ccr-kimi-k3".to_string(),
            provider_id: "kimi".to_string(),
            model_id: "k3".to_string(),
        });
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field.contains("extraRoutes")));
    }

    #[test]
    fn rejects_role_pointing_at_missing_model() {
        let mut cfg = base_cfg();
        cfg.roles.main = Some(Target { provider_id: "kimi".into(), model_id: "nope".into() });
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "roles.main"));
    }

    #[test]
    fn rejects_bad_provider_id_and_relative_models_url() {
        let mut cfg = base_cfg();
        cfg.providers[0].id = "Kimi_1".to_string();
        cfg.providers[0].models_url = Some("/v1/models".to_string());
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].id"));
        assert!(errs.iter().any(|e| e.field == "providers[0].modelsUrl"));
    }

    #[test]
    fn rejects_port_out_of_range() {
        let mut cfg = base_cfg();
        cfg.gateway.port = 80;
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "gateway.port"));
    }

    /// 空 / 纯空白 key 会在 build_headers 里静默降级成空凭据，表现为一个无法定位的 401。
    #[test]
    fn rejects_empty_or_whitespace_only_api_key() {
        for bad in ["", "   ", "\t"] {
            let mut cfg = base_cfg();
            cfg.providers[0].api_key = bad.to_string();
            let errs = validate(&cfg).unwrap_err();
            assert!(
                errs.iter().any(|e| e.field == "providers[0].apiKey"),
                "api_key {bad:?} must be rejected"
            );
        }
    }

    /// 含非 ASCII 的 key 无法作为 HTTP 请求头值发送。
    #[test]
    fn rejects_api_key_with_non_ascii_characters() {
        let mut cfg = base_cfg();
        cfg.providers[0].api_key = "sk-密钥".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].apiKey"));
    }

    /// 首尾空白几乎总是粘贴错误，且会导致鉴权头与服务端预期不一致。
    #[test]
    fn rejects_api_key_with_surrounding_whitespace() {
        let mut cfg = base_cfg();
        cfg.providers[0].api_key = " sk-real ".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "providers[0].apiKey"));
    }
}
```

- [ ] **Step 5: 运行校验测试，确认全部通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::validate`
Expected: `test result: ok.` 全部 13 个测试通过。

- [ ] **Step 6: 写 `src-tauri/src/config/store.rs`（含测试）**

`atomic_write_json` 是全项目唯一的落盘入口，必须被后续所有写配置的代码复用。

```rust
use crate::error::{Error, Result};
use super::validate::validate;
use super::Config;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

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

/// 进程内单调计数器：保证同一进程内两次写的临时文件名不同。
/// 只用 PID 会让两个并发写共享同一个临时文件，从而互相覆盖彼此的内容。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".tmp.{}.{}", std::process::id(), n));
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
    /// 串行化 save。必须是 `Arc`：`ConfigStore` 派生了 `Clone`，所有克隆只有共享同一把锁
    /// 才能互斥；裸 `Mutex` 会让每个克隆各持一把锁，等于没锁。
    save_lock: Arc<Mutex<()>>,
}

impl ConfigStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let cfg = load_or_init(&path)?;
        Ok(ConfigStore {
            path,
            inner: Arc::new(RwLock::new(cfg)),
            save_lock: Arc::new(Mutex::new(())),
        })
    }

    /// 替换内存配置并原子落盘。校验失败则内存与磁盘都不变。
    ///
    /// 全程持 `save_lock`：磁盘顺序（rename）与内存顺序（RwLock 获取）如果各自独立，
    /// 两个并发 save 会以不同次序落地，导致磁盘与内存指向不同的配置。
    /// 锁必须在 `atomic_write_json` 之前获取、在内存赋值之后释放。
    pub fn save(&self, next: Config) -> Result<()> {
        let _guard = self.save_lock.lock().expect("save lock poisoned");
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

    /// 临时文件名必须每次不同：只用 PID 时两个并发写会共用同一个临时文件并互相覆盖。
    #[test]
    fn tmp_sibling_names_are_unique_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let a = tmp_sibling(&path);
        let b = tmp_sibling(&path);
        assert_ne!(a, b, "temp name must not be reused: {a:?} vs {b:?}");
    }

    /// 并发 save 结束后磁盘与内存必须一致。
    /// 该用例在"save 不串行 + 临时名只用 PID"的旧实现下会真实失败
    /// （曾经复现出磁盘=thread-7、内存=thread-4 的分叉），因此它不是一个恒真断言。
    #[test]
    fn concurrent_saves_keep_disk_and_memory_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let store = ConfigStore::load(&path).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut cfg = Config::first_run();
                    cfg.gateway.port = 9000 + i;
                    cfg.gateway.local_token = format!("sk-ccr-thread-{i}");
                    store.save(cfg).expect("save must succeed");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("save thread must not panic");
        }

        let on_disk = load_from(&path).unwrap();
        let in_memory = store.snapshot();
        assert_eq!(on_disk, in_memory, "disk and memory must agree");
    }
}
```

- [ ] **Step 7: 在 `src-tauri/src/lib.rs` 增加模块声明**

把 `src-tauri/src/lib.rs` 改成：

```rust
pub mod app_paths;
pub mod config;
pub mod error;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 8: 运行本任务全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: `test result: ok.`，共 22 个测试通过（13 校验 + 9 store），0 failed。

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/error.rs src-tauri/src/app_paths.rs src-tauri/src/config src-tauri/src/lib.rs
git commit -m "feat(config): 配置类型、校验规则与原子读写"
```

---

## Task 3: 别名生成与 id 推导

**Files:**
- Create: `src-tauri/src/routing/mod.rs`
- Create: `src-tauri/src/routing/alias.rs`
- Modify: `src-tauri/src/lib.rs`（增加 `pub mod routing;`）
- Test: `src-tauri/src/routing/alias.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `cc_router::config::{Provider, ModelSpec}`
- Produces:
  - `cc_router::routing::alias::strip_1m_marker(&str) -> (String, bool)`
  - `cc_router::routing::alias::generate_alias(provider_id: &str, model_id: &str, taken: &HashSet<String>) -> String`
  - `cc_router::routing::alias::derive_provider_id(base_url: &str, taken: &HashSet<String>) -> String`
  - `cc_router::routing::alias::slugify(&str) -> String`

- [ ] **Step 1: 写 `src-tauri/src/routing/alias.rs` 的测试与实现**

规则来自 spec §5.4 与 §6.4。`[1M]` 剥离必须大小写不敏感且先 `trim_end`。

```rust
use std::collections::HashSet;

pub const ALIAS_PREFIX: &str = "ccr-";
const MAX_ALIAS_LEN: usize = 64;
const ONE_M_MARKER: &str = "[1m]";

/// 剥离结尾的 `[1M]`（大小写不敏感，允许尾随空白）。返回 (清理后的名字, 是否带标记)。
/// 上游 cc-switch 的 `hasClaudeOneMMarker` / `stripClaudeOneMMarker` 语义与此一致。
pub fn strip_1m_marker(model: &str) -> (String, bool) {
    let trimmed_end = model.trim_end();
    if trimmed_end.to_lowercase().ends_with(ONE_M_MARKER) {
        let cut = trimmed_end.len() - ONE_M_MARKER.len();
        (trimmed_end[..cut].trim_end().to_string(), true)
    } else {
        (model.to_string(), false)
    }
}

/// 转小写 → 非 `[a-z0-9._-]` 替换为 `-` → 折叠连续 `-` → 去首尾 `-`。
pub fn slugify(input: &str) -> String {
    let lowered = input.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_dash = false;
    for ch in lowered.chars() {
        let keep = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_';
        if keep {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

/// 默认别名 `ccr-<providerId>-<modelId>`；超长则截断并追加 6 位 hash；冲突则追加 -2/-3…
pub fn generate_alias(provider_id: &str, model_id: &str, taken: &HashSet<String>) -> String {
    let (model_no_1m, _) = strip_1m_marker(model_id);
    let base = format!("{}{}-{}", ALIAS_PREFIX, slugify(provider_id), slugify(&model_no_1m));
    let base = if base.len() > MAX_ALIAS_LEN {
        let keep = MAX_ALIAS_LEN - 7;
        let truncated = base.chars().take(keep).collect::<String>();
        format!("{}-{}", truncated.trim_end_matches('-'), fnv1a_hex(&base))
    } else {
        base
    };
    if !taken.contains(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 由 baseUrl 的域名推导 provider id（`https://api.moonshot.cn/anthropic` → `moonshot`）。
pub fn derive_provider_id(base_url: &str, taken: &HashSet<String>) -> String {
    let raw = base_url
        .trim()
        .strip_prefix("https://")
        .or_else(|| base_url.trim().strip_prefix("http://"))
        .unwrap_or(base_url.trim());
    let host_port = raw.split('/').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or(host_port);
    let mut labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    // 去掉常见前缀与顶级域，取最有辨识度的一段
    while labels.len() > 1
        && matches!(labels[0], "api" | "www" | "open" | "gateway" | "router" | "cc" | "chat")
    {
        labels.remove(0);
    }
    if labels.len() > 1 {
        labels.pop(); // 去掉 TLD
    }
    let candidate = slugify(labels.first().copied().unwrap_or(host));
    let candidate = if candidate.is_empty() { "custom".to_string() } else { candidate };
    let mut candidate: String = candidate.chars().take(32).collect();
    candidate = candidate.trim_matches('-').to_string();
    if candidate.is_empty() {
        candidate = "custom".to_string();
    }
    if !taken.contains(&candidate) {
        return candidate;
    }
    let mut n = 2u32;
    loop {
        let c = format!("{candidate}-{n}");
        if !taken.contains(&c) {
            return c;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn strips_1m_marker_case_insensitively_with_trailing_space() {
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1M]"), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1m]"), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1M]  "), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("ccr-kimi-k3"), ("ccr-kimi-k3".into(), false));
        assert_eq!(strip_1m_marker("weird[1M]name"), ("weird[1M]name".into(), false));
    }

    #[test]
    fn slugify_replaces_and_collapses() {
        assert_eq!(slugify("Kimi K3"), "kimi-k3");
        assert_eq!(slugify("mimo-v2.6-pro"), "mimo-v2.6-pro");
        assert_eq!(slugify("a__b"), "a__b");
        assert_eq!(slugify("a//b::c"), "a-b-c");
        assert_eq!(slugify("---x---"), "x");
        assert_eq!(slugify("中文名"), "");
    }

    #[test]
    fn generates_default_alias_and_strips_marker_from_model_id() {
        assert_eq!(generate_alias("kimi", "k3", &none()), "ccr-kimi-k3");
        assert_eq!(generate_alias("xiaomi", "mimo-v2.6-pro[1M]", &none()), "ccr-xiaomi-mimo-v2.6-pro");
    }

    #[test]
    fn resolves_alias_collisions_by_appending_index() {
        let mut taken = HashSet::new();
        taken.insert("ccr-kimi-k3".to_string());
        assert_eq!(generate_alias("kimi", "k3", &taken), "ccr-kimi-k3-2");
        taken.insert("ccr-kimi-k3-2".to_string());
        assert_eq!(generate_alias("kimi", "k3", &taken), "ccr-kimi-k3-3");
    }

    #[test]
    fn truncates_overlong_alias_and_keeps_it_within_limit() {
        let long_model = "x".repeat(120);
        let alias = generate_alias("provider", &long_model, &none());
        assert!(alias.len() <= MAX_ALIAS_LEN, "alias too long: {}", alias.len());
        assert!(alias.starts_with(ALIAS_PREFIX));
        assert!(!alias.ends_with('-'));
    }

    #[test]
    fn derives_provider_id_from_hostname() {
        assert_eq!(derive_provider_id("https://api.moonshot.cn/anthropic", &none()), "moonshot");
        assert_eq!(derive_provider_id("https://api.deepseek.com/anthropic", &none()), "deepseek");
        assert_eq!(derive_provider_id("https://api.xiaomimimo.com/anthropic", &none()), "xiaomimimo");
        assert_eq!(derive_provider_id("https://open.bigmodel.cn/api/anthropic", &none()), "bigmodel");
        // IPv4 主机名：只保留首段（"127" 已满足 ^[a-z0-9][a-z0-9-]{0,31}$）。
        // 这是刻意的简化——本地 provider 的 id 通常由用户改写，不值得为它做多段拼接。
        assert_eq!(derive_provider_id("http://127.0.0.1:9000/v1", &none()), "127");
    }

    #[test]
    fn derive_provider_id_resolves_collisions_and_handles_junk() {
        let mut taken = HashSet::new();
        taken.insert("moonshot".to_string());
        assert_eq!(derive_provider_id("https://api.moonshot.cn/anthropic", &taken), "moonshot-2");
        assert_eq!(derive_provider_id("https://武/", &none()), "custom");
    }
}
```

- [ ] **Step 2: 写 `src-tauri/src/routing/mod.rs`**

```rust
pub mod alias;
```

- [ ] **Step 3: 在 `src-tauri/src/lib.rs` 增加 `pub mod routing;`**

```rust
pub mod app_paths;
pub mod config;
pub mod error;
pub mod routing;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml routing::`
Expected: `test result: ok.`，7 个测试通过。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/routing src-tauri/src/lib.rs
git commit -m "feat(routing): 别名生成、slug 化与 provider id 推导"
```

---

## Task 4: 路由解析器

**Files:**
- Create: `src-tauri/src/routing/resolve.rs`
- Modify: `src-tauri/src/routing/mod.rs`
- Modify: `src-tauri/src/lib.rs`（无需改，`routing` 已声明）
- Test: `src-tauri/src/routing/resolve.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `config::{Config, Provider, ModelSpec, AuthStyle, UnknownModelPolicy, Target}`；`routing::alias::strip_1m_marker`
- Produces:
  - `cc_router::routing::resolve::{RouteTable, ResolvedTarget, MatchedBy, Role, ResolveError}`
  - `RouteTable::build(&Config) -> RouteTable`
  - `RouteTable::resolve(&self, requested_model: &str) -> Result<ResolvedTarget, ResolveError>`
  - `ResolvedTarget { provider_id, provider_name, base_url, api_key, auth_style, upstream_model, alias, matched_by, role, context_1m }`

解析顺序（spec §6.2）：别名精确匹配 → Claude 家族兜底 → 未知模型策略。**别名表覆盖全部已配置模型的别名**（spec §15 决策 8）。

- [ ] **Step 1: 写测试与实现**

```rust
use super::alias::strip_1m_marker;
use crate::config::{AuthStyle, Config, UnknownModelPolicy};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    Alias,
    Family,
    Fallback,
}

/// 必须派生 `Hash`：`RouteTable::role_aliases` 是 `HashMap<Role, String>`，HashMap 的 key
/// 需要 `Hash + Eq`。少了 `Hash` 会直接编译失败（E0277）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Main,
    Fast,
    Subagent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTarget {
    pub provider_id: String,
    pub provider_name: String,
    pub base_url: String,
    pub api_key: String,
    pub auth_style: AuthStyle,
    pub upstream_model: String,
    pub alias: String,
    pub matched_by: MatchedBy,
    pub role: Option<Role>,
    pub context_1m: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("模型 '{requested}' 未被 CC Router 路由：请在角色路由中为它添加规则，或设置默认目标")]
    UnknownModel { requested: String },
    #[error("模型 '{requested}' 命中了角色 '{role}'，但该角色未绑定模型")]
    UnboundRole { requested: String, role: &'static str },
    #[error("配置指向不存在的目标 {provider_id}/{model_id}")]
    DanglingTarget { provider_id: String, model_id: String },
}

#[derive(Debug, Clone)]
struct Entry {
    provider_id: String,
    provider_name: String,
    base_url: String,
    api_key: String,
    auth_style: AuthStyle,
    upstream_model: String,
    alias: String,
    context_1m: bool,
}

#[derive(Debug, Clone)]
pub struct RouteTable {
    aliases: HashMap<String, Entry>,
    role_aliases: HashMap<Role, String>,
    default_entry: Option<Entry>,
    policy: UnknownModelPolicy,
}

/// 手写 `Default`，不能用 `#[derive(Default)]`：`UnknownModelPolicy`（定义在 `config/mod.rs`，
/// 不在本任务的文件范围内）没有实现 `Default`，派生会失败（E0277/E0119）。
/// 空表上两种策略的行为完全一致（都得到 `UnknownModel`），所以这里取哪个值不可观测。
impl Default for RouteTable {
    fn default() -> Self {
        RouteTable {
            aliases: HashMap::new(),
            role_aliases: HashMap::new(),
            default_entry: None,
            policy: UnknownModelPolicy::Error,
        }
    }
}

impl RouteTable {
    pub fn build(cfg: &Config) -> Self {
        let mut aliases: HashMap<String, Entry> = HashMap::new();
        for p in &cfg.providers {
            for m in &p.models {
                aliases.insert(
                    m.alias.trim().to_lowercase(),
                    Entry {
                        provider_id: p.id.clone(),
                        provider_name: p.name.clone(),
                        base_url: p.base_url.clone(),
                        api_key: p.api_key.clone(),
                        auth_style: p.auth_style,
                        upstream_model: m.id.clone(),
                        alias: m.alias.clone(),
                        context_1m: m.context_1m,
                    },
                );
            }
        }
        for r in &cfg.extra_routes {
            if let Some(p) = cfg.providers.iter().find(|p| p.id == r.provider_id) {
                if let Some(m) = p.models.iter().find(|m| m.id == r.model_id) {
                    aliases.insert(
                        r.alias.trim().to_lowercase(),
                        Entry {
                            provider_id: p.id.clone(),
                            provider_name: p.name.clone(),
                            base_url: p.base_url.clone(),
                            api_key: p.api_key.clone(),
                            auth_style: p.auth_style,
                            upstream_model: m.id.clone(),
                            alias: r.alias.clone(),
                            context_1m: m.context_1m,
                        },
                    );
                }
            }
        }

        let lookup = |t: &Option<crate::config::Target>| -> Option<String> {
            let t = t.as_ref()?;
            let p = cfg.providers.iter().find(|p| p.id == t.provider_id)?;
            let m = p.models.iter().find(|m| m.id == t.model_id)?;
            Some(m.alias.clone())
        };

        let mut role_aliases = HashMap::new();
        for (role, slot) in [
            (Role::Main, &cfg.roles.main),
            (Role::Fast, &cfg.roles.fast),
            (Role::Subagent, &cfg.roles.subagent),
        ] {
            if let Some(alias) = lookup(slot) {
                role_aliases.insert(role, alias.trim().to_lowercase());
            }
        }

        let default_entry = cfg.default_target.as_ref().and_then(|t| {
            let p = cfg.providers.iter().find(|p| p.id == t.provider_id)?;
            let m = p.models.iter().find(|m| m.id == t.model_id)?;
            Some(Entry {
                provider_id: p.id.clone(),
                provider_name: p.name.clone(),
                base_url: p.base_url.clone(),
                api_key: p.api_key.clone(),
                auth_style: p.auth_style,
                upstream_model: m.id.clone(),
                alias: m.alias.clone(),
                context_1m: m.context_1m,
            })
        });

        RouteTable { aliases, role_aliases, default_entry, policy: cfg.on_unknown_model }
    }

    /// 反向查：别名 → 引用它的角色集合（用于日志展示，spec §6.7）。
    pub fn roles_for_alias(&self, alias: &str) -> Vec<Role> {
        let key = alias.trim().to_lowercase();
        let mut roles: Vec<Role> = self
            .role_aliases
            .iter()
            .filter(|(_, a)| **a == key)
            .map(|(r, _)| *r)
            .collect();
        roles.sort_by_key(|r| match r {
            Role::Main => 0,
            Role::Fast => 1,
            Role::Subagent => 2,
        });
        roles
    }

    pub fn resolve(&self, requested_model: &str) -> Result<ResolvedTarget, ResolveError> {
        let (without_marker, forced_1m) = strip_1m_marker(requested_model.trim());
        let key = without_marker.trim().to_lowercase();

        if let Some(entry) = self.aliases.get(&key) {
            return Ok(self.finish(entry.clone(), forced_1m, MatchedBy::Alias));
        }

        let family_role = if key.contains("haiku") {
            Some(Role::Fast)
        } else if key.contains("opus") || key.contains("sonnet") || key.contains("fable") {
            Some(Role::Main)
        } else {
            None
        };
        if let Some(role) = family_role {
            if let Some(alias) = self.role_aliases.get(&role) {
                if let Some(entry) = self.aliases.get(alias) {
                    let mut resolved = self.finish(entry.clone(), forced_1m, MatchedBy::Family);
                    resolved.role = Some(role);
                    return Ok(resolved);
                }
            }
        }

        match self.policy {
            UnknownModelPolicy::Default => match &self.default_entry {
                Some(entry) => {
                    let mut resolved = self.finish(entry.clone(), forced_1m, MatchedBy::Fallback);
                    resolved.alias = String::new();
                    Ok(resolved)
                }
                None => Err(ResolveError::UnknownModel { requested: without_marker }),
            },
            UnknownModelPolicy::Error => Err(ResolveError::UnknownModel { requested: without_marker }),
        }
    }

    /// 直接解析一个显式 Target。用于 catch-all 路径转发到 defaultTarget，
    /// 以及"未绑定角色但需要取该目标别名"的场景。返回值的 `alias` 为空、`role` 为 None
    /// （因为它不是通过别名命中的）。
    pub fn resolve_target(&self, t: &crate::config::Target) -> Option<ResolvedTarget> {
        let entry = self
            .aliases
            .values()
            .find(|e| e.provider_id == t.provider_id && e.upstream_model == t.model_id)
            .cloned()?;
        let mut r = self.finish(entry, false, MatchedBy::Fallback);
        r.alias = String::new();
        r.role = None;
        Some(r)
    }

    fn finish(&self, entry: Entry, forced_1m: bool, matched_by: MatchedBy) -> ResolvedTarget {
        let role = self.roles_for_alias(&entry.alias).first().copied();
        ResolvedTarget {
            provider_id: entry.provider_id,
            provider_name: entry.provider_name,
            base_url: entry.base_url,
            api_key: entry.api_key,
            auth_style: entry.auth_style,
            upstream_model: entry.upstream_model,
            context_1m: forced_1m || entry.context_1m,
            alias: entry.alias,
            matched_by,
            role,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AuthStyle, ExtraRoute, GatewayConfig, ModelSpec, Provider, Roles, Target, TakeoverState,
        UiConfig, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
    };

    fn model(id: &str, alias: &str, context_1m: bool) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            name: id.into(),
            alias: alias.into(),
            context_1m,
            context_window: None,
            max_tokens: None,
        }
    }

    fn provider(id: &str, name: &str, base: &str, models: Vec<ModelSpec>) -> Provider {
        Provider {
            id: id.into(),
            name: name.into(),
            base_url: base.into(),
            api_key: format!("{id}-key"),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models,
        }
    }

    fn cfg_with(policy: UnknownModelPolicy, extra: Vec<ExtraRoute>) -> Config {
        Config {
            version: 1,
            gateway: GatewayConfig {
                bind: DEFAULT_BIND.into(),
                port: 8787,
                local_token: "sk-ccr-test".into(),
                max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
                connect_timeout_ms: 10_000,
                idle_timeout_ms: 300_000,
            },
            providers: vec![
                provider("kimi", "Kimi", "https://api.moonshot.cn/anthropic", vec![model("k3", "ccr-kimi-k3", false)]),
                provider(
                    "deepseek",
                    "DeepSeek",
                    "https://api.deepseek.com/anthropic",
                    vec![model("ds-pro", "ccr-deepseek-ds-pro", true)],
                ),
            ],
            roles: Roles {
                main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
                fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
                subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            },
            extra_routes: extra,
            on_unknown_model: policy,
            default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            takeover: TakeoverState::default(),
            ui: UiConfig::default(),
        }
    }

    #[test]
    fn resolves_alias_exactly() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let r = t.resolve("ccr-kimi-k3").unwrap();
        assert_eq!(r.provider_id, "kimi");
        assert_eq!(r.upstream_model, "k3");
        assert_eq!(r.matched_by, MatchedBy::Alias);
        assert_eq!(r.api_key, "kimi-key");
        assert!(!r.context_1m, "context1m=false on the model spec");
    }

    #[test]
    fn alias_matching_tolerates_whitespace_case_and_1m_suffix() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        for input in ["  CCR-Kimi-K3  ", "ccr-kimi-k3[1M]", "ccr-kimi-k3[1m]"] {
            let r = t.resolve(input).unwrap_or_else(|e| panic!("{input} should resolve: {e}"));
            assert_eq!(r.provider_id, "kimi", "input={input}");
        }
    }

    #[test]
    fn forced_1m_marker_enables_context_1m_even_when_model_says_false() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        assert!(t.resolve("ccr-kimi-k3[1M]").unwrap().context_1m);
        assert!(t.resolve("ccr-deepseek-ds-pro").unwrap().context_1m, "model spec sets context1m=true");
    }

    #[test]
    fn every_configured_model_is_routable_without_role_binding() {
        let mut cfg = cfg_with(UnknownModelPolicy::Error, vec![]);
        cfg.roles = Roles::default();
        let t = RouteTable::build(&cfg);
        assert_eq!(t.resolve("ccr-deepseek-ds-pro").unwrap().provider_id, "deepseek");
        assert_eq!(t.resolve("ccr-kimi-k3").unwrap().provider_id, "kimi");
    }

    #[test]
    fn family_fallback_maps_haiku_to_fast_and_opus_sonnet_fable_to_main() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        assert_eq!(t.resolve("claude-haiku-4-5-20251001").unwrap().provider_id, "deepseek");
        for m in ["claude-opus-4-5-20251101", "claude-3-5-sonnet-20241022", "claude-fable-1"] {
            let r = t.resolve(m).unwrap();
            assert_eq!(r.provider_id, "kimi", "{m} should map to main");
            assert_eq!(r.matched_by, MatchedBy::Family);
            assert_eq!(r.role, Some(Role::Main));
            assert_eq!(r.upstream_model, "k3");
        }
    }

    #[test]
    fn family_fallback_is_skipped_when_role_unbound_and_falls_through() {
        let mut cfg = cfg_with(UnknownModelPolicy::Default, vec![]);
        cfg.roles.main = None;
        let t = RouteTable::build(&cfg);
        let r = t.resolve("claude-opus-4-5-20251101").unwrap();
        assert_eq!(r.matched_by, MatchedBy::Fallback);
        assert_eq!(r.alias, "", "fallback responses carry no alias");
    }

    #[test]
    fn unknown_model_uses_default_target_when_policy_is_default() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Default, vec![]));
        let r = t.resolve("totally-unknown-model").unwrap();
        assert_eq!(r.matched_by, MatchedBy::Fallback);
        assert_eq!(r.provider_id, "deepseek");
    }

    #[test]
    fn unknown_model_is_an_error_when_policy_is_error() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let err = t.resolve("totally-unknown-model").unwrap_err();
        assert!(matches!(err, ResolveError::UnknownModel { .. }));
        assert!(err.to_string().contains("totally-unknown-model"));
    }

    #[test]
    fn unknown_model_errors_when_policy_default_but_no_default_target() {
        let mut cfg = cfg_with(UnknownModelPolicy::Default, vec![]);
        cfg.default_target = None;
        let t = RouteTable::build(&cfg);
        assert!(t.resolve("nope").is_err());
    }

    #[test]
    fn extra_route_alias_resolves_to_its_target() {
        let extra = vec![ExtraRoute {
            alias: "ccr-big".into(),
            provider_id: "kimi".into(),
            model_id: "k3".into(),
        }];
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, extra));
        assert_eq!(t.resolve("ccr-big").unwrap().provider_id, "kimi");
    }

    #[test]
    fn reports_all_roles_sharing_one_alias() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let roles = t.roles_for_alias("ccr-deepseek-ds-pro");
        assert_eq!(roles, vec![Role::Fast, Role::Subagent]);
        assert_eq!(t.roles_for_alias("ccr-kimi-k3"), vec![Role::Main]);
        assert!(t.roles_for_alias("ccr-unknown").is_empty());
    }

    #[test]
    fn resolve_target_returns_entry_without_alias_or_role() {
        let t = RouteTable::build(&cfg_with(UnknownModelPolicy::Error, vec![]));
        let target = crate::config::Target {
            provider_id: "deepseek".into(),
            model_id: "ds-pro".into(),
        };
        let r = t.resolve_target(&target).unwrap();
        assert_eq!(r.provider_id, "deepseek");
        assert_eq!(r.upstream_model, "ds-pro");
        assert_eq!(r.api_key, "deepseek-key");
        assert_eq!(r.alias, "", "explicit-target resolution carries no alias");
        assert_eq!(r.role, None);

        let missing = crate::config::Target { provider_id: "nope".into(), model_id: "x".into() };
        assert!(t.resolve_target(&missing).is_none());
    }
}
```

- [ ] **Step 2: 在 `src-tauri/src/routing/mod.rs` 增加 `pub mod resolve;`**

```rust
pub mod alias;
pub mod resolve;
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml routing::resolve`
Expected: `test result: ok.`，12 个测试通过（本任务新增 12 个；此前全库 26 个，合计 38 个）。

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/routing/resolve.rs src-tauri/src/routing/mod.rs
git commit -m "feat(routing): 别名表、家族兜底与未知模型策略的解析器"
```

---

## Task 5: 请求改写（模型名 / 鉴权注入 / 请求头 / 1M beta）

纯函数，无 IO，因此可以完全单测。**这一层是路由正确性的最后一道关**：即使路由算对了目标，写错上游模型名或漏发密钥都会导致 401/404。

**Files:**
- Create: `src-tauri/src/gateway/mod.rs`
- Create: `src-tauri/src/gateway/rewrite.rs`
- Modify: `src-tauri/src/lib.rs`（增加 `pub mod gateway;`）
- Test: `src-tauri/src/gateway/rewrite.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `routing::resolve::ResolvedTarget`；`config::AuthStyle`
- Produces:
  - `cc_router::gateway::rewrite::{rewrite_body, RewriteError}`
  - `cc_router::gateway::rewrite::{upstream_url, build_headers, UpstreamHeaders}`
  - `cc_router::gateway::rewrite::CONTEXT_1M_BETA`
  - `rewrite_body(body: &[u8], upstream_model: &str) -> Result<Vec<u8>, RewriteError>`
  - `build_headers(target: &ResolvedTarget, incoming: &HeaderMap) -> UpstreamHeaders`

- [ ] **Step 1: 写测试与实现**

```rust
use crate::config::AuthStyle;
use crate::routing::resolve::ResolvedTarget;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

pub const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// 透传到上游的请求头白名单（spec §6.3）。
const PASSTHROUGH_HEADERS: &[&str] = &[
    "anthropic-version",
    "anthropic-beta",
    "content-type",
    "accept",
    "user-agent",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RewriteError {
    #[error("请求体不是合法的 JSON 对象: {0}")]
    NotJsonObject(String),
    #[error("请求体缺少字符串类型的 model 字段")]
    MissingModel,
}

pub fn rewrite_body(body: &[u8], upstream_model: &str) -> Result<Vec<u8>, RewriteError> {
    let mut value: Value = serde_json::from_slice(body)
        .map_err(|e| RewriteError::NotJsonObject(e.to_string()))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| RewriteError::NotJsonObject("顶层不是对象".to_string()))?;
    match obj.get("model") {
        Some(Value::String(_)) => {}
        _ => return Err(RewriteError::MissingModel),
    }
    obj.insert("model".to_string(), Value::String(upstream_model.to_string()));
    serde_json::to_vec(&value).map_err(|e| RewriteError::NotJsonObject(e.to_string()))
}

/// 上游 URL = `{baseUrl}/v1/messages`，保留入站 query string。
pub fn upstream_url(base_url: &str, path: &str, query: Option<&str>) -> String {
    let base = base_url.trim_end_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamHeaders {
    pub headers: HeaderMap,
}

pub fn build_headers(target: &ResolvedTarget, incoming: &HeaderMap) -> UpstreamHeaders {
    let mut out = HeaderMap::new();

    for name in PASSTHROUGH_HEADERS {
        let hn = HeaderName::from_static(name);
        if let Some(v) = incoming.get(&hn) {
            out.insert(hn, v.clone());
        }
    }
    if !out.contains_key("anthropic-version") {
        out.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
        );
    }

    // 客户端鉴权头一律剥离，改为注入 provider 的真实密钥
    out.remove("authorization");
    out.remove("x-api-key");
    let key = HeaderValue::from_str(&target.api_key)
        .unwrap_or_else(|_| HeaderValue::from_static(""));
    match target.auth_style {
        AuthStyle::Both => {
            out.insert(HeaderName::from_static("x-api-key"), key.clone());
            out.insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {}", target.api_key))
                    .unwrap_or_else(|_| HeaderValue::from_static("Bearer ")),
            );
        }
        AuthStyle::XApiKey => {
            out.insert(HeaderName::from_static("x-api-key"), key);
        }
        AuthStyle::Bearer => {
            out.insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {}", target.api_key))
                    .unwrap_or_else(|_| HeaderValue::from_static("Bearer ")),
            );
        }
    }

    if target.context_1m {
        let existing = out
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mut parts: Vec<String> = existing
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.iter().any(|p| p == CONTEXT_1M_BETA) {
            parts.push(CONTEXT_1M_BETA.to_string());
        }
        if let Ok(v) = HeaderValue::from_str(&parts.join(",")) {
            out.insert(HeaderName::from_static("anthropic-beta"), v);
        }
    }

    if let Ok(v) = HeaderValue::from_str(&target.alias) {
        if !target.alias.is_empty() {
            out.insert(HeaderName::from_static("x-ccr-alias"), v);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&target.provider_id) {
        out.insert(HeaderName::from_static("x-ccr-provider"), v);
    }

    // host / content-length / accept-encoding / connection 不在此白名单内，天然被丢弃
    UpstreamHeaders { headers: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthStyle;
    use crate::routing::resolve::{MatchedBy, ResolvedTarget};

    fn target(auth_style: AuthStyle, context_1m: bool) -> ResolvedTarget {
        ResolvedTarget {
            provider_id: "kimi".into(),
            provider_name: "Kimi".into(),
            base_url: "https://api.moonshot.cn/anthropic".into(),
            api_key: "real-secret".into(),
            auth_style,
            upstream_model: "k3".into(),
            alias: "ccr-kimi-k3".into(),
            matched_by: MatchedBy::Alias,
            role: None,
            context_1m,
        }
    }

    fn incoming() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        h.insert("anthropic-beta", HeaderValue::from_static("prompt-caching-2024-07-31"));
        h.insert("content-type", HeaderValue::from_static("application/json"));
        h.insert("authorization", HeaderValue::from_static("Bearer sk-ccr-localtoken"));
        h.insert("x-api-key", HeaderValue::from_static("sk-ccr-localtoken"));
        h.insert("accept-encoding", HeaderValue::from_static("gzip, br"));
        h.insert("host", HeaderValue::from_static("127.0.0.1:8787"));
        h.insert("content-length", HeaderValue::from_static("123"));
        h
    }

    #[test]
    fn rewrites_only_the_model_field() {
        let body = br#"{"model":"ccr-kimi-k3","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"system":[{"type":"text","text":"s","cache_control":{"type":"ephemeral"}}]}"#;
        let out = rewrite_body(body, "k3").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "k3");
        assert_eq!(v["max_tokens"], 16);
        assert_eq!(v["messages"][0]["content"], "hi");
        assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn rejects_non_json_body_and_non_object_toplevel() {
        assert!(matches!(rewrite_body(b"<html>502</html>", "k3"), Err(RewriteError::NotJsonObject(_))));
        assert!(matches!(rewrite_body(b"[1,2,3]", "k3"), Err(RewriteError::NotJsonObject(_))));
        assert!(matches!(rewrite_body(b"\"just a string\"", "k3"), Err(RewriteError::NotJsonObject(_))));
    }

    #[test]
    fn rejects_missing_or_non_string_model() {
        assert!(matches!(rewrite_body(br#"{"max_tokens":1}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":123}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":null}"#, "k3"), Err(RewriteError::MissingModel)));
        assert!(matches!(rewrite_body(br#"{"model":{}}"#, "k3"), Err(RewriteError::MissingModel)));
    }

    #[test]
    fn builds_upstream_url_with_and_without_query() {
        assert_eq!(
            upstream_url("https://api.moonshot.cn/anthropic/", "/v1/messages", None),
            "https://api.moonshot.cn/anthropic/v1/messages"
        );
        assert_eq!(
            upstream_url("https://api.moonshot.cn/anthropic", "/v1/messages", Some("beta=true")),
            "https://api.moonshot.cn/anthropic/v1/messages?beta=true"
        );
    }

    #[test]
    fn strips_client_auth_and_injects_provider_key_for_both_style() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert_eq!(h.get("x-api-key").unwrap(), "real-secret");
        assert_eq!(h.get("authorization").unwrap(), "Bearer real-secret");
        assert!(!h.get("authorization").unwrap().to_str().unwrap().contains("localtoken"));
    }

    #[test]
    fn honours_each_auth_style() {
        let x = build_headers(&target(AuthStyle::XApiKey, false), &incoming()).headers;
        assert_eq!(x.get("x-api-key").unwrap(), "real-secret");
        assert!(x.get("authorization").is_none());

        let b = build_headers(&target(AuthStyle::Bearer, false), &incoming()).headers;
        assert!(b.get("x-api-key").is_none());
        assert_eq!(b.get("authorization").unwrap(), "Bearer real-secret");
    }

    #[test]
    fn drops_host_content_length_and_accept_encoding() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert!(h.get("host").is_none());
        assert!(h.get("content-length").is_none());
        assert!(h.get("accept-encoding").is_none());
    }

    #[test]
    fn defaults_anthropic_version_when_absent() {
        let h = build_headers(&target(AuthStyle::Both, false), &HeaderMap::new()).headers;
        assert_eq!(h.get("anthropic-version").unwrap(), DEFAULT_ANTHROPIC_VERSION);
    }

    #[test]
    fn appends_context_1m_beta_without_duplicating() {
        let h = build_headers(&target(AuthStyle::Both, true), &incoming()).headers;
        let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains("prompt-caching-2024-07-31"), "existing beta kept: {beta}");
        assert!(beta.contains(CONTEXT_1M_BETA), "1m beta appended: {beta}");
        assert_eq!(beta.matches(CONTEXT_1M_BETA).count(), 1);

        let mut twice = incoming();
        twice.insert("anthropic-beta", HeaderValue::from_str(CONTEXT_1M_BETA).unwrap());
        let h2 = build_headers(&target(AuthStyle::Both, true), &twice).headers;
        assert_eq!(
            h2.get("anthropic-beta").unwrap().to_str().unwrap().matches(CONTEXT_1M_BETA).count(),
            1,
            "must not duplicate an already-present 1m beta"
        );
    }

    #[test]
    fn does_not_add_context_1m_beta_when_disabled() {
        let h = build_headers(&target(AuthStyle::Both, false), &incoming()).headers;
        assert!(!h.get("anthropic-beta").unwrap().to_str().unwrap().contains(CONTEXT_1M_BETA));
    }
}
```

- [ ] **Step 2: 写 `src-tauri/src/gateway/mod.rs`**

```rust
pub mod rewrite;
```

- [ ] **Step 3: 在 `src-tauri/src/lib.rs` 增加 `pub mod gateway;`**

```rust
pub mod app_paths;
pub mod config;
pub mod error;
pub mod gateway;
pub mod routing;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml gateway::rewrite`
Expected: `test result: ok.`，10 个测试通过。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/gateway src-tauri/src/lib.rs
git commit -m "feat(gateway): 请求体模型改写、鉴权注入与 1M beta 头"
```

---

## Task 6: 网关服务（health / 令牌 / 非流式转发 / 错误体）

本任务首次把 HTTP 服务接起来，并建立集成测试基础设施（mock 上游）。

**Files:**
- Create: `src-tauri/src/gateway/error.rs`
- Create: `src-tauri/src/gateway/body.rs`
- Create: `src-tauri/src/gateway/server.rs`
- Modify: `src-tauri/src/gateway/mod.rs`
- Create: `src-tauri/tests/support/mod.rs`
- Create: `src-tauri/tests/gateway_nonstream.rs`

**Interfaces:**
- Consumes: `config::store::ConfigStore`；`routing::resolve::RouteTable`；`gateway::rewrite`
- Produces:
  - `cc_router::gateway::error::{anthropic_error, ErrorKind}`
  - `cc_router::gateway::body::{BoxedBody, body_full, body_stream}`
  - `cc_router::gateway::server::{Gateway, GatewayState, start, start_with_port}`
  - `Gateway { bound_port: u16, shutdown() }`
  - 测试支撑：`tests/support/mod.rs` 暴露 `MockUpstream`（`start_json` / `start_raw` / `start_sse` /
    `requests` / `shutdown`，公开字段 `base_url`、`port`）、`RecordedRequest`（含 `header(name)`）、
    `test_config`、`ok_message_body`、`start_gateway` 与类型别名 `BoxedMockBody`。
    （**没有** `MockResponse` 类型——早先这里写错过，代码里从来不存在它，Task 7/8 也不需要。）

- [ ] **Step 1: 写 `src-tauri/src/gateway/error.rs`**

```rust
use crate::gateway::body::{body_full, BoxedBody};
use hyper::{Response, StatusCode};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Authentication,
    InvalidRequest,
    Api,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Authentication => "authentication_error",
            ErrorKind::InvalidRequest => "invalid_request_error",
            ErrorKind::Api => "api_error",
        }
    }
}

pub fn anthropic_error(status: StatusCode, kind: ErrorKind, message: impl Into<String>) -> Response<BoxedBody> {
    let body = json!({
        "type": "error",
        "error": { "type": kind.as_str(), "message": message.into() }
    });
    let bytes = serde_json::to_vec(&body).expect("static json is serializable");
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body_full(bytes))
        .expect("static response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn produces_anthropic_shaped_error_body() {
        let resp = anthropic_error(
            StatusCode::UNAUTHORIZED,
            ErrorKind::Authentication,
            "本地令牌无效",
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "authentication_error");
        assert_eq!(v["error"]["message"], "本地令牌无效");
    }
}
```

- [ ] **Step 2: 写 `src-tauri/src/gateway/body.rs`**

```rust
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use std::convert::Infallible;

pub type BoxedError = Box<dyn std::error::Error + Send + Sync>;
pub type BoxedBody = BoxBody<Bytes, BoxedError>;

pub fn body_full(bytes: impl Into<Bytes>) -> BoxedBody {
    Full::new(bytes.into())
        .map_err(|e: Infallible| -> BoxedError { match e {} })
        .boxed()
}

/// 把任意 `Result<Bytes, E>` 流包成响应体。**不做任何缓冲**：每一块到达即作为一帧下发。
///
/// `S` 必须 `Sync`：`BoxBody` 的内部是 `Pin<Box<dyn Body<..> + Send + Sync + 'static>>`，
/// 所以 `BodyExt::boxed` 要求 `Self: Send + Sync`，而 `StreamBody<Map<S, F>>` 只有在 `S: Sync`
/// 时才是 `Sync`。唯一的调用点 `reqwest::Response::bytes_stream()` 满足该约束。
///
/// 另外必须写成 `BodyExt::boxed(...)` 而不是 `.boxed()`：`http-body-util` 0.1.5 同时为
/// `StreamBody` 实现了 `Body` 与 `Stream`，而本文件两个 trait 都在作用域内，方法调用会二义
/// （E0034）。
pub fn body_stream<S, E>(stream: S) -> BoxedBody
where
    S: Stream<Item = Result<Bytes, E>> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let mapped = stream.map(|item| item.map(Frame::data).map_err(|e| -> BoxedError { Box::new(e) }));
    BodyExt::boxed(StreamBody::new(mapped))
}
```

- [ ] **Step 3: 写 `src-tauri/src/gateway/server.rs`**

只实现 `GET /ccr/health`、令牌校验、`POST /v1/messages`（非流式与流式都走同一条转发路径，流式在 Task 7 补测试）、`POST /v1/messages/count_tokens`、`GET /v1/models` 占位（Task 8 补内容）、其它路径转发到 defaultTarget。

```rust
use crate::config::{Config, DEFAULT_BIND};
use crate::gateway::body::{body_full, body_stream, BoxedBody};
use crate::gateway::error::{anthropic_error, ErrorKind};
use crate::gateway::rewrite::{build_headers, rewrite_body, upstream_url, RewriteError};
use crate::routing::resolve::{ResolveError, RouteTable};
use crate::error::{Error, Result};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct GatewayState {
    pub config: Arc<RwLock<Config>>,
    pub client: reqwest::Client,
    pub started_at: Instant,
    pub requests_served: Arc<AtomicU64>,
    /// 实际绑定的端口（配置里写 0 时由 OS 分配，health 必须报告真实值）。
    pub bound_port: u16,
    pub log: crate::logging::RequestLog,
}

#[derive(Debug)]
pub struct Gateway {
    pub bound_port: u16,
    requests_served: Arc<AtomicU64>,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl Gateway {
    /// 已服务请求数（含未到达上游的失败请求）。
    pub fn requests_served(&self) -> u64 {
        self.requests_served.load(Ordering::Relaxed)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

pub async fn start(config: Arc<RwLock<Config>>, log: crate::logging::RequestLog) -> Result<Gateway> {
    let (bind, port, connect_timeout_ms) = {
        let cfg = config.read().expect("config lock poisoned");
        (cfg.gateway.bind.clone(), cfg.gateway.port, cfg.gateway.connect_timeout_ms)
    };
    start_on(config, log, &bind, port, connect_timeout_ms).await
}

/// `port = 0` 时由操作系统分配端口，便于测试。
pub async fn start_on(
    config: Arc<RwLock<Config>>,
    log: crate::logging::RequestLog,
    bind: &str,
    port: u16,
    connect_timeout_ms: u64,
) -> Result<Gateway> {
    let bind = if bind.is_empty() { DEFAULT_BIND } else { bind };
    let listener = TcpListener::bind((bind, port))
        .await
        .map_err(|e| Error::Bind { addr: format!("{bind}:{port}"), source: e })?;
    let bound_port = listener
        .local_addr()
        .map_err(|e| Error::Bind { addr: format!("{bind}:{port}"), source: e })?
        .port();

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(connect_timeout_ms))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| Error::Gateway(format!("构造 HTTP 客户端失败: {e}")))?;

    let state = GatewayState {
        config,
        client,
        started_at: Instant::now(),
        requests_served: Arc::new(AtomicU64::new(0)),
        bound_port,
        log,
    };

    let requests_served = state.requests_served.clone();
    let (tx, mut rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(v) => v,
                        Err(e) => {
                            tracing_less_warn(&format!("accept 失败: {e}"));
                            continue;
                        }
                    };
                    let io = TokioIo::new(stream);
                    let st = state.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| {
                            let st = st.clone();
                            async move { Ok::<_, std::convert::Infallible>(handle(req, st, peer).await) }
                        });
                        if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                            .serve_connection(io, svc)
                            .await
                        {
                            tracing_less_warn(&format!("连接结束: {e}"));
                        }
                    });
                }
            }
        }
    });

    Ok(Gateway { bound_port, requests_served, shutdown: Some(tx), join })
}

fn tracing_less_warn(msg: &str) {
    eprintln!("[cc-router] {msg}");
}

pub async fn handle(req: Request<Incoming>, state: GatewayState, _peer: SocketAddr) -> Response<BoxedBody> {
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    let method = req.method().as_str().to_string();

    // 免鉴权仅限 GET /ccr/health（spec §6.1 定义的就是这个端点）。
    // 不能只判 path：那会让 POST /ccr/health 等所有方法都免鉴权。
    if path == "/ccr/health" && req.method() == hyper::Method::GET {
        return health(&state);
    }

    if !token_ok(&req, &state) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            ErrorKind::Authentication,
            "本地令牌无效：请在请求中携带 Authorization: Bearer <localToken> 或 x-api-key: <localToken>",
        );
    }

    let is_messages = path == "/v1/messages";
    let is_count = path == "/v1/messages/count_tokens";

    // 必须在 into_body() 之前克隆入站头：改写阶段要用它们决定透传白名单。
    let incoming_headers = req.headers().clone();

    let (max_body, idle_timeout_ms) = {
        let cfg = state.config.read().expect("config lock poisoned");
        (cfg.gateway.max_request_body_bytes, cfg.gateway.idle_timeout_ms)
    };
    let _ = idle_timeout_ms; // Task 7 的流式分支使用它

    // 用 Limited 包住 body，防止超大请求体打爆内存（spec §6.3）。
    let body_bytes = match Limited::new(req.into_body(), max_body).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return anthropic_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorKind::InvalidRequest,
                format!("请求体超过上限 {} 字节", max_body),
            )
        }
    };

    let route_table = {
        let cfg = state.config.read().expect("config lock poisoned");
        RouteTable::build(&cfg)
    };

    let requested_model = extract_model(&body_bytes).unwrap_or_default();

    let resolved = if is_messages || is_count {
        match route_table.resolve(&requested_model) {
            Ok(r) => r,
            Err(e) => return resolve_error_response(&e),
        }
    } else {
        // 非 messages 路径：转发到 defaultTarget；未绑定则 404（spec §6.1）
        let default_target = {
            let cfg = state.config.read().expect("config lock poisoned");
            cfg.default_target.clone()
        };
        match default_target.as_ref().and_then(|t| route_table.resolve_target(t)) {
            Some(r) => r,
            None => {
                return anthropic_error(
                    StatusCode::NOT_FOUND,
                    ErrorKind::InvalidRequest,
                    format!("未配置 defaultTarget，无法转发 {path}"),
                )
            }
        }
    };

    // messages 类请求必须能改写 model；catch-all 路径的 body 可能为空或非 JSON，
    // 那种情况按原样转发（不能让一个 GET 因为"body 不是 JSON"变成 400）。
    let upstream_body = if is_messages || is_count {
        match rewrite_body(&body_bytes, &resolved.upstream_model) {
            Ok(b) => b,
            Err(e) => {
                return anthropic_error(
                    StatusCode::BAD_REQUEST,
                    ErrorKind::InvalidRequest,
                    rewrite_error_message(&e, &requested_model),
                )
            }
        }
    } else if body_bytes.is_empty() {
        Vec::new()
    } else {
        rewrite_body(&body_bytes, &resolved.upstream_model).unwrap_or_else(|_| body_bytes.to_vec())
    };

    let url = upstream_url(&resolved.base_url, &path, query.as_deref());
    let headers = build_headers(&resolved, &incoming_headers).headers;

    let started = Instant::now();
    // 必须保留入站方法：写死 post 会把所有 catch-all 请求（例如 GET /v1/organizations）
    // 变成无 body 的 POST 打到上游——语义被静默改掉，而测试很难发现。
    // 同时：body 为空时不要附加 body，避免给无 body 的 GET 加上 Content-Length: 0。
    let upstream_method = reqwest::Method::from_bytes(method.as_bytes())
        .unwrap_or(reqwest::Method::POST);
    let mut builder = state.client.request(upstream_method, &url);
    if !upstream_body.is_empty() {
        builder = builder.body(upstream_body);
    }
    for (name, value) in headers.iter() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let upstream = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            state.requests_served.fetch_add(1, Ordering::Relaxed);
            state.log.push(crate::logging::LogEntry::error(
                &method,
                &path,
                &requested_model,
                &resolved,
                started.elapsed().as_millis() as u64,
                &e.to_string(),
            ));
            let (status, kind) = if e.is_timeout() {
                (StatusCode::GATEWAY_TIMEOUT, ErrorKind::Api)
            } else {
                (StatusCode::BAD_GATEWAY, ErrorKind::Api)
            };
            return anthropic_error(
                status,
                kind,
                format!(
                    "上游请求失败（provider={} 别名={} url={}）: {}",
                    resolved.provider_id, resolved.alias, url, e
                ),
            );
        }
    };

    let status = upstream.status();
    let is_sse = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false);

    let mut resp = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers().iter() {
        if name == "content-length" || name == "transfer-encoding" {
            continue;
        }
        resp = resp.header(name.as_str(), value.as_bytes());
    }

    state.requests_served.fetch_add(1, Ordering::Relaxed);
    state.log.push(crate::logging::LogEntry::ok(
        &method,
        &path,
        &requested_model,
        &resolved,
        status.as_u16(),
        is_sse,
        started.elapsed().as_millis() as u64,
    ));

    let body: BoxedBody = if is_sse {
        body_stream(upstream.bytes_stream())
    } else {
        match upstream.bytes().await {
            Ok(b) => body_full(b),
            Err(e) => {
                return anthropic_error(
                    StatusCode::BAD_GATEWAY,
                    ErrorKind::Api,
                    format!("读取上游响应失败（provider={}）: {e}", resolved.provider_id),
                )
            }
        }
    };

    resp.body(body).expect("upstream headers are valid")
}

fn health(state: &GatewayState) -> Response<BoxedBody> {
    let uptime = state.started_at.elapsed().as_secs();
    let payload = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "port": state.bound_port,
        "uptimeSec": uptime,
        "requestsServed": state.requests_served.load(Ordering::Relaxed),
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(body_full(serde_json::to_vec(&payload).unwrap()))
        .expect("static response is valid")
}

fn token_ok(req: &Request<Incoming>, state: &GatewayState) -> bool {
    let expected = {
        let cfg = state.config.read().expect("config lock poisoned");
        cfg.gateway.local_token.clone()
    };
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string());
    let api_key = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string());
    bearer.as_deref() == Some(expected.as_str()) || api_key.as_deref() == Some(expected.as_str())
}

fn extract_model(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(|s| s.to_string())
}

fn resolve_error_response(e: &ResolveError) -> Response<BoxedBody> {
    anthropic_error(StatusCode::BAD_REQUEST, ErrorKind::InvalidRequest, e.to_string())
}

fn rewrite_error_message(e: &RewriteError, requested: &str) -> String {
    match e {
        RewriteError::MissingModel => format!(
            "请求缺少字符串类型的 model 字段（收到 '{requested}'）：无法确定路由目标"
        ),
        RewriteError::NotJsonObject(detail) => {
            format!("请求体不是合法的 JSON 对象（{detail}）：无法确定路由目标")
        }
    }
}
```

- [ ] **Step 4: 写 `src-tauri/tests/support/mod.rs`（mock 上游）**

```rust
//! 集成测试共用的脚手架：一个可编程的 mock 上游 + 配置夹具。
//!
//! 本模块同时服务 Task 6 / 7 / 8，因此**故意**暴露了在 Task 6 尚未被用到的成员
//! （`start_sse`、`async_stream_like`、公开字段 `port`、`RecordedRequest::query`）。
//! 若不加 `allow(dead_code)`，这些"为后续任务预留的接口"会产生 4 条 dead_code 警告，
//! 而删掉它们又会破坏后续任务依赖的接口——所以在模块级一次性豁免。
#![allow(dead_code)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub type Recorded = Arc<Mutex<Vec<RecordedRequest>>>;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// 入站方法。必须有：否则"转发时把方法压成 POST"这类缺陷无法被测出来。
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: serde_json::Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct MockUpstream {
    pub base_url: String,
    pub port: u16,
    pub recorded: Recorded,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl MockUpstream {
    /// 非流式：固定返回 `status` + JSON `body`。
    pub async fn start_json(status: u16, body: serde_json::Value) -> Self {
        let bytes = serde_json::to_vec(&body).unwrap();
        MockUpstream::start(move || {
            let bytes = bytes.clone();
            async move {
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(full(bytes))
                    .unwrap()
            }
        })
        .await
    }

    /// 非流式：固定返回原始字节与 content-type（用于 HTML 错误页等）。
    pub async fn start_raw(status: u16, content_type: &'static str, body: &'static [u8]) -> Self {
        MockUpstream::start(move || async move {
            Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(full(body.to_vec()))
                .unwrap()
        })
        .await
    }

    /// SSE：按 `chunks` 逐块发送，块间 sleep `gap`，首块前额外 sleep `first_delay`。
    pub async fn start_sse(
        chunks: Vec<String>,
        gap: std::time::Duration,
        first_delay: std::time::Duration,
    ) -> Self {
        MockUpstream::start(move || {
            let stream = async_stream_like(chunks.clone(), gap, first_delay);
            async move {
                Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(StreamBody::new(stream).boxed())
                    .unwrap()
            }
        })
        .await
    }

    /// handler 不接收请求：本脚手架只提供固定响应，请求内容一律通过 `recorded` 断言。
    /// 这样就不必构造 `hyper::body::Incoming`（它没有公开的 empty 构造器）。
    async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Response<BoxedMockBody>> + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded: Recorded = Arc::new(Mutex::new(Vec::new()));
        let rec = recorded.clone();
        let (tx, mut rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        let io = TokioIo::new(stream);
                        let handler = handler.clone();
                        let rec = rec.clone();
                        tokio::spawn(async move {
                            let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                                let handler = handler.clone();
                                let rec = rec.clone();
                                async move {
                                    let method = req.method().as_str().to_string();
                                    let path = req.uri().path().to_string();
                                    let query = req.uri().query().map(|q| q.to_string());
                                    let headers = req
                                        .headers()
                                        .iter()
                                        .map(|(k, v)| {
                                            (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())
                                        })
                                        .collect::<Vec<_>>();
                                    let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                                    let body = serde_json::from_slice(&body_bytes)
                                        .unwrap_or(serde_json::Value::Null);
                                    rec.lock().unwrap().push(RecordedRequest { method, path, query, headers, body });
                                    let resp = handler().await;
                                    Ok::<_, Infallible>(resp)
                                }
                            });
                            let _ = ConnBuilder::new(TokioExecutor::new()).serve_connection(io, svc).await;
                        });
                    }
                }
            }
        });
        MockUpstream {
            base_url: format!("http://127.0.0.1:{port}"),
            port,
            recorded,
            shutdown: Some(tx),
            join,
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.recorded.lock().unwrap().clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

pub type BoxedMockBody =
    http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

fn full(bytes: Vec<u8>) -> BoxedMockBody {
    Full::new(Bytes::from(bytes))
        .map_err(|e: Infallible| -> Box<dyn std::error::Error + Send + Sync> { match e {} })
        .boxed()
}

fn async_stream_like(
    chunks: Vec<String>,
    gap: std::time::Duration,
    first_delay: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>> {
    futures_util::stream::unfold(
        (chunks.into_iter(), 0usize, gap, first_delay),
        |(mut it, idx, gap, first_delay)| async move {
            let delay = if idx == 0 { first_delay } else { gap };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match it.next() {
                Some(chunk) => Some((
                    Ok(Frame::data(Bytes::from(chunk))),
                    (it, idx + 1, gap, first_delay),
                )),
                None => None,
            }
        },
    )
}

pub fn test_config(upstream_base_url: &str, port: u16) -> cc_router::config::Config {
    use cc_router::config::*;
    Config {
        version: 1,
        gateway: GatewayConfig {
            bind: "127.0.0.1".into(),
            port,
            local_token: "sk-ccr-test-token".into(),
            max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 300_000,
        },
        providers: vec![
            Provider {
                id: "kimi".into(),
                name: "Kimi".into(),
                base_url: upstream_base_url.into(),
                api_key: "kimi-real-key".into(),
                auth_style: AuthStyle::Both,
                preset_id: None,
                models_url: None,
                models_fetch: None,
                models: vec![ModelSpec {
                    id: "k3".into(),
                    name: "k3".into(),
                    alias: "ccr-kimi-k3".into(),
                    context_1m: false,
                    context_window: None,
                    max_tokens: None,
                }],
            },
            Provider {
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                base_url: upstream_base_url.into(),
                api_key: "deepseek-real-key".into(),
                auth_style: AuthStyle::Bearer,
                preset_id: None,
                models_url: None,
                models_fetch: None,
                models: vec![ModelSpec {
                    id: "ds-pro".into(),
                    name: "ds-pro".into(),
                    alias: "ccr-deepseek-ds-pro".into(),
                    context_1m: true,
                    context_window: None,
                    max_tokens: None,
                }],
            },
        ],
        roles: Roles {
            main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
            fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
            subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
        },
        extra_routes: vec![],
        on_unknown_model: UnknownModelPolicy::Error,
        default_target: Some(Target { provider_id: "deepseek".into(), model_id: "ds-pro".into() }),
        takeover: TakeoverState::default(),
        ui: UiConfig::default(),
    }
}

pub fn ok_message_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{ "type": "text", "text": "pong" }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 3, "output_tokens": 1 }
    })
}

pub async fn start_gateway(cfg: cc_router::config::Config) -> (cc_router::gateway::server::Gateway, cc_router::logging::RequestLog) {
    let log = cc_router::logging::RequestLog::new(500);
    let config = Arc::new(std::sync::RwLock::new(cfg));
    let gw = cc_router::gateway::server::start(config, log.clone()).await.unwrap();
    (gw, log)
}
```

> 测试脚手架的取舍：mock 上游把收到的请求体解析成 `serde_json::Value` 记录下来（够断言用），handler 只返回固定响应。这样避免了在测试里构造 `hyper::body::Incoming`（它没有公开的空构造器）。

- [ ] **Step 5: 写 `src-tauri/tests/gateway_nonstream.rs`**

```rust
mod support;

use support::{ok_message_body, start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

async fn raw_request(port: u16, request: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).to_string()
}

fn messages_request(token: Option<&str>, body: &str) -> String {
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn health_endpoint_requires_no_token() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(
        gw.bound_port,
        "GET /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("\"status\":\"ok\""));
    assert!(resp.contains(&format!("\"port\":{}", gw.bound_port)));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn messages_without_token_is_rejected_with_anthropic_error() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(None, r#"{"model":"ccr-kimi-k3"}"#)).await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(resp.contains("\"type\":\"authentication_error\""), "{resp}");
    assert!(upstream.requests().is_empty(), "upstream must not be contacted");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(Some("sk-ccr-wrong"), r#"{"model":"ccr-kimi-k3"}"#)).await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn routes_alias_to_upstream_with_rewritten_model_and_injected_key() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("pong"), "{resp}");

    let seen = upstream.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/messages");
    assert_eq!(seen[0].body["model"], "k3", "alias must be rewritten to upstream model");
    assert_eq!(seen[0].body["max_tokens"], 16, "other fields untouched");
    assert_eq!(seen[0].header("x-api-key"), Some("kimi-real-key"));
    assert_eq!(seen[0].header("authorization"), Some("Bearer kimi-real-key"));
    assert_eq!(seen[0].header("x-ccr-alias"), Some("ccr-kimi-k3"));
    assert_eq!(seen[0].header("x-ccr-provider"), Some("kimi"));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn two_aliases_reach_two_providers_with_their_own_keys() {
    let upstream = MockUpstream::start_json(200, ok_message_body("ds-pro")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let b1 = r#"{"model":"ccr-kimi-k3","max_tokens":8,"messages":[]}"#;
    let b2 = r#"{"model":"ccr-deepseek-ds-pro","max_tokens":8,"messages":[]}"#;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b1)).await;
    let _ = raw_request(gw.bound_port, &messages_request(Some(TOKEN), b2)).await;

    let seen = upstream.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].body["model"], "k3");
    assert_eq!(seen[0].header("x-api-key"), Some("kimi-real-key"));
    assert_eq!(seen[1].body["model"], "ds-pro");
    assert_eq!(seen[1].header("authorization"), Some("Bearer deepseek-real-key"));
    assert!(seen[1].header("x-api-key").is_none(), "bearer style must not send x-api-key");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn non_json_body_returns_400_not_500() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), "<html>oops</html>")).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "must be 400, got: {resp}");
    assert!(resp.contains("\"type\":\"invalid_request_error\""));
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn body_without_model_field_returns_400() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    for body in [r#"{"max_tokens":5}"#, r#"{"model":123}"#, r#"{"model":null}"#] {
        let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
        assert!(resp.starts_with("HTTP/1.1 400"), "body={body} got: {resp}");
        assert!(resp.contains("\"type\":\"invalid_request_error\""), "body={body}");
    }
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unknown_model_is_400_when_policy_is_error() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"totally-unknown","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
    assert!(resp.contains("totally-unknown"), "error must name the model: {resp}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn upstream_non_json_error_body_is_passed_through_verbatim() {
    let upstream = MockUpstream::start_raw(502, "text/html", b"<html><body>Bad Gateway</body></html>").await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    assert!(resp.contains("<html><body>Bad Gateway</body></html>"), "body must pass through: {resp}");
    assert!(!resp.contains("\"type\":\"api_error\""), "must not replace upstream body");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unreachable_provider_yields_502_naming_provider_and_alias() {
    // 关掉上游，模拟连接被拒
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let dead_url = upstream.base_url.clone();
    upstream.shutdown().await;

    let (gw, _log) = start_gateway(test_config(&dead_url, 0)).await;
    let body = r#"{"model":"ccr-kimi-k3","max_tokens":5,"messages":[]}"#;
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), body)).await;
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    assert!(resp.contains("\"type\":\"api_error\""));
    assert!(resp.contains("provider=kimi"), "must name provider: {resp}");
    assert!(resp.contains("ccr-kimi-k3"), "must name alias: {resp}");

    gw.shutdown().await;
}

#[tokio::test]
async fn binding_an_occupied_port_returns_error_instead_of_panicking() {
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = occupied.local_addr().unwrap().port();

    let log = cc_router::logging::RequestLog::new(10);
    let config = std::sync::Arc::new(std::sync::RwLock::new(test_config("http://127.0.0.1:1", port)));
    let err = cc_router::gateway::server::start(config, log).await.unwrap_err();
    assert!(matches!(err, cc_router::error::Error::Bind { .. }), "{err:?}");
    assert!(err.to_string().contains(&port.to_string()));
}

#[tokio::test]
async fn oversized_body_returns_413() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.gateway.max_request_body_bytes = 64;
    let (gw, _log) = start_gateway(cfg).await;

    let big = format!(
        r#"{{"model":"ccr-kimi-k3","pad":"{}"}}"#,
        "x".repeat(500)
    );
    let resp = raw_request(gw.bound_port, &messages_request(Some(TOKEN), &big)).await;
    assert!(resp.starts_with("HTTP/1.1 413"), "{resp}");
    assert!(resp.contains("\"type\":\"invalid_request_error\""));
    assert!(upstream.requests().is_empty());

    gw.shutdown().await;
    upstream.shutdown().await;
}
```

- [ ] **Step 6: 在 `src-tauri/src/gateway/mod.rs` 与 `lib.rs` 挂上模块**

`src-tauri/src/gateway/mod.rs`:

```rust
pub mod body;
pub mod error;
pub mod rewrite;
pub mod server;
```

`src-tauri/src/lib.rs`:

```rust
pub mod app_paths;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod routing;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

`logging` 模块在 Task 8 才实现；为了让本任务可编译，**先创建 `src-tauri/src/logging/mod.rs` 的最小实现**（Task 8 再扩展）：

```rust
use crate::routing::resolve::ResolvedTarget;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
}

impl RequestLog {
    pub fn new(capacity: usize) -> Self {
        RequestLog { capacity, entries: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))) }
    }

    pub fn push(&self, entry: LogEntry) {
        let mut buf = self.entries.lock().expect("log lock poisoned");
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(entry);
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
```

- [ ] **Step 7: 运行全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: `test result: ok.` —— **lib 单元测试 52 个通过**（Task 2–5 的 51 个 + 本任务 `gateway::error` 的 1 个）
+ **集成测试 `gateway_nonstream` 15 个通过**，合计 **67**，0 failed。

> **评审修复轮 1 新增的 3 个用例**（首轮实现 12 个，评审发现两处 Important 缺陷后补）：
> - `post_to_health_requires_token` —— `POST /ccr/health` 无令牌必须 401（修掉"健康检查对所有方法免鉴权"）。
> - `catch_all_preserves_method_and_forwards_bodyless_get` —— `GET /v1/organizations` 到上游必须仍是 GET 且无 body
>   （修掉"catch-all 一律压成 POST"；这条缺陷此前因完全没有 catch-all 测试而被掩盖）。
> - `token_accepts_x_api_key_header` —— 仅用 `x-api-key` 也须通过令牌校验，并断言入站本地令牌被替换为厂商密钥。
> 前两条在这个修复轮里被证明是"修复前必然失败"的（分别打印出真实的 `200 OK` 健康检查响应、以及 `left: "POST" / right: "GET"`）。

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/gateway src-tauri/src/logging src-tauri/src/lib.rs src-tauri/tests
git commit -m "feat(gateway): HTTP 服务、令牌校验、非流式转发与 Anthropic 错误体"
```

---

## Task 7: SSE 零缓冲透传与并发/断连

spec §6.6 是硬约束：**不得缓冲**。本任务的测试用"块间 sleep + 首块延迟"来证明它。

**Files:**
- Modify: `src-tauri/src/gateway/server.rs`（流式分支已存在，本任务补 `idle_timeout` 与断连取消）
- Create: `src-tauri/tests/gateway_stream.rs`

**Interfaces:**
- Consumes: Task 6 的 `support::{MockUpstream, start_gateway, test_config}`
- Produces: 无新公共 API（行为约束）

- [ ] **Step 1: 写 `src-tauri/tests/gateway_stream.rs`**

```rust
mod support;

// 注意：这里**不要** `use futures_util::StreamExt;`——本文件里 `join_all` 用的是全限定路径
// `futures_util::future::join_all`，`read_to_string` 来自 `tokio::io::AsyncReadExt`，
// 所以 StreamExt 完全没被用到，加了会产生 `unused_imports` 警告（而警告算评审发现）。
use std::time::{Duration, Instant};
use support::{start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

fn sse_chunks(n: usize) -> Vec<String> {
    let mut v = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"k3\"}}\n\n".to_string(),
    ];
    for i in 0..n {
        v.push(format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"chunk{i}\"}}}}\n\n"
        ));
    }
    v.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());
    v
}

fn sse_request(body: &str) -> String {
    format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn streams_chunks_without_buffering() {
    // 上游首块延迟 300ms，之后每块间隔 250ms，共 5 块 → 总时长 ≥ 1.3s
    let upstream = MockUpstream::start_sse(
        sse_chunks(5),
        Duration::from_millis(250),
        Duration::from_millis(300),
    )
    .await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"max_tokens":64,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    // 读到第一个数据块的时间必须远早于整体结束时间
    let started = Instant::now();
    let mut buf = vec![0u8; 4096];
    let mut first_chunk_at: Option<Duration> = None;
    let mut total = String::new();
    loop {
        let read = stream.read(&mut buf).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        total.push_str(&String::from_utf8_lossy(&buf[..read]));
        if first_chunk_at.is_none() && total.contains("message_start") {
            first_chunk_at = Some(started.elapsed());
        }
    }
    let finished_at = started.elapsed();

    assert!(total.contains("HTTP/1.1 200"), "{total}");
    assert!(total.contains("text/event-stream"), "content-type must be preserved: {total}");
    let first = first_chunk_at.expect("must observe message_start");
    assert!(
        first < finished_at - Duration::from_millis(800),
        "first chunk at {first:?} vs finished at {finished_at:?}: looks buffered"
    );
    // 对齐 spec AC4：首块到达应约等于上游首块延迟（300ms）+ 很小的转发开销。
    // 留 700ms 余量以容忍 CI 抖动；若真发生缓冲，first 会接近 finished（≥1.3s）。
    assert!(
        first < Duration::from_millis(300 + 700),
        "first chunk arrival {first:?} exceeds upstream first-byte delay budget (AC4)"
    );
    assert!(total.contains("chunk4"), "all chunks must arrive: {total}");
    assert!(total.contains("message_stop"));

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn preserves_chunk_boundaries_in_order() {
    let chunks = sse_chunks(3);
    let upstream = MockUpstream::start_sse(chunks.clone(), Duration::from_millis(20), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut total = String::new();
    let _ = stream.read_to_string(&mut total).await;

    let mut last = 0;
    for (i, _) in chunks.iter().enumerate() {
        let needle = if i == 0 {
            "message_start".to_string()
        } else if i == chunks.len() - 1 {
            "message_stop".to_string()
        } else {
            format!("chunk{}", i - 1)
        };
        let pos = total.find(&needle).unwrap_or_else(|| panic!("missing {needle} in {total}"));
        assert!(pos >= last, "{needle} out of order in {total}");
        last = pos;
    }

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn handles_sixteen_concurrent_streams_without_crosstalk() {
    let upstream = MockUpstream::start_sse(sse_chunks(2), Duration::from_millis(30), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut handles = Vec::new();
    for _ in 0..16 {
        let port = gw.bound_port;
        handles.push(tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
            stream.write_all(sse_request(body).as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out).await;
            out
        }));
    }
    let results = futures_util::future::join_all(handles).await;
    for r in results {
        let text = r.unwrap();
        // 必须数帧头（`event: message_start`），不能数裸子串：本文件 `sse_chunks` 的首块同时含
        // `event: message_start` 与 `"type":"message_start"`，裸子串计数恒为 2，断言 `== 1` 不可能成立。
        assert_eq!(text.matches("event: message_start").count(), 1, "crosstalk detected: {text}");
        assert_eq!(text.matches("event: message_stop").count(), 1, "crosstalk detected: {text}");
    }
    assert_eq!(upstream.requests().len(), 16);

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn client_disconnect_does_not_panic_the_gateway() {
    let upstream = MockUpstream::start_sse(sse_chunks(50), Duration::from_millis(50), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
        let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
        stream.write_all(sse_request(body).as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = vec![0u8; 128];
        let _ = stream.read(&mut buf).await;
        // 直接 drop 连接，模拟客户端中断
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 网关仍然可用
    let mut healthy = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    healthy
        .write_all(b"GET /ccr/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    let _ = healthy.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 200"), "gateway died after client disconnect: {out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn upstream_stream_that_ends_early_still_completes_the_response() {
    // 只发 3 块就结束（不是正常的 message_stop 结尾），客户端仍应收到完整已转发内容并正常收流
    let chunks = vec![
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"delta\":{\"text\":\"partial\"}}\n\n".to_string(),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n".to_string(),
    ];
    let upstream = MockUpstream::start_sse(chunks, Duration::from_millis(10), Duration::from_millis(0)).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let body = r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#;
    stream.write_all(sse_request(body).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("partial"), "already-forwarded chunks must not be lost: {out}");
    assert!(out.contains("overloaded_error"), "mid-stream error frame must pass through: {out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn count_tokens_is_routed_but_not_streamed() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"input_tokens": 42})).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-deepseek-ds-pro","messages":[]}"#;
    let req = format!(
        "POST /v1/messages/count_tokens HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("\"input_tokens\":42"), "{out}");
    let seen = upstream.requests();
    assert_eq!(seen[0].path, "/v1/messages/count_tokens");
    assert_eq!(seen[0].body["model"], "ds-pro");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn query_string_is_preserved_upstream() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"ok": true})).await;
    let (gw, _log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let body = r#"{"model":"ccr-kimi-k3","messages":[]}"#;
    let req = format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;

    assert!(upstream.requests()[0].query.as_deref() == Some("beta=true"), "query must be forwarded");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn unmatched_path_without_default_target_returns_404() {
    let upstream = MockUpstream::start_json(200, serde_json::json!({"ok": true})).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.default_target = None;
    let (gw, _log) = start_gateway(cfg).await;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    let req = format!(
        "GET /v1/organizations HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 404"), "{out}");

    gw.shutdown().await;
    upstream.shutdown().await;
}
```

- [ ] **Step 2: 运行流式测试，观察是否失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test gateway_stream`
Expected: 若 Task 6 的实现真的零缓冲，则 **PASS**；若 `streams_chunks_without_buffering` 报 "looks buffered"，说明 `body_stream` 或 hyper 层引入了缓冲 —— 检查是否误用了 `upstream.bytes()`（整体读取）而不是 `bytes_stream()`。

- [ ] **Step 3: 补齐断连取消与空闲超时**

在 `src-tauri/src/gateway/server.rs` 的流式分支中，把 `body_stream(upstream.bytes_stream())` 换成带空闲超时与取消的版本，并**删掉 Task 6 里那行 `let _ = idle_timeout_ms; // Task 7 的流式分支使用它`**（它已不再被丢弃）。同时在 `server.rs` 顶部加上 `use futures_util::StreamExt;` —— `idle_guarded` 里调用 `s.next()` 需要该 trait 在作用域内，否则编译报错 `no method named next found`：

```rust
let idle = std::time::Duration::from_millis(idle_timeout_ms);
body_stream(idle_guarded(upstream.bytes_stream(), idle))
```

并在同文件加入：

```rust
/// 空闲超时保护：两个块之间超过 `idle` 未到达则结束流（不 panic，已转发的块保持完整）。
fn idle_guarded<S, E>(
    stream: S,
    idle: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    futures_util::stream::unfold(
        (Box::pin(stream), idle),
        |(mut s, idle)| async move {
            match tokio::time::timeout(idle, s.next()).await {
                Ok(Some(item)) => Some((item, (s, idle))),
                // 超时或流结束：结束下游流。客户端断开时 hyper 会 drop 本 future，
                // 从而 drop 掉上游 reqwest 流，上游请求随之取消。
                Ok(None) => None,
                Err(_elapsed) => None,
            }
        },
    )
}
```

`idle_timeout_ms` 需要从配置读出并传入（在 `handle` 开头与 `max_body` 一起读）。

- [ ] **Step 4: 重跑流式测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test gateway_stream`
Expected: `test result: ok.`，8 个测试全部通过（全库合计 75 = 52 lib + 15 `gateway_nonstream` + 8 `gateway_stream`）。

> **已知覆盖缺口（评审已记录，交给 Task 8 或后续补齐）**：这 8 个用例里没有一个会触发空闲超时
> （`test_config` 的 `idle_timeout_ms = 300_000`），所以 `idle_guarded` 里 `Err(_elapsed)` 那条分支
> **没有已提交的测试pin**。实现时用临时用例验证过（`idle = 300ms` + 上游块间隔 2s → 下游在 306.78ms
> 干净收流且未送达任何块；把 `server.rs` 回退到 Task 6 版本后同一条流会跑满 10.04s），但临时用例已删除。
> 另外：空闲超时导致的下游截断与"正常结束"在下游不可区分（不发错误帧），这是 brief 指定的行为。

- [ ] **Step 5: 重跑全部测试确保无回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全绿。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/gateway/server.rs src-tauri/tests/gateway_stream.rs
git commit -m "feat(gateway): SSE 零缓冲透传、空闲超时与并发/断连处理"
```

---

## Task 8: 请求日志、`/v1/models` 合成与最小 IPC 面

**Files:**
- Modify: `src-tauri/src/logging/mod.rs`（补订阅与 jsonl 落盘开关）
- Modify: `src-tauri/src/gateway/server.rs`（补 `GET /v1/models`）
- Create: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/request_log.rs`

**Interfaces:**
- Consumes: Task 6/7 的 gateway；Task 2 的 `ConfigStore`
- Produces:
  - `cc_router::logging::{RequestLog, LogEntry}`；`RequestLog::subscribe() -> tokio::sync::broadcast::Receiver<LogEntry>`；`RequestLog::set_file_logging(bool, PathBuf)`
  - `cc_router::gateway::server::models_payload(&Config) -> serde_json::Value`
  - Tauri 命令：`gateway_status`、`gateway_start`、`gateway_stop`、`get_config`、`save_config`、`recent_logs`

- [ ] **Step 1: 写 `src-tauri/tests/request_log.rs`**

```rust
mod support;

use support::{ok_message_body, start_gateway, test_config, MockUpstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "sk-ccr-test-token";

async fn post(port: u16, path: &str, body: &str) -> String {
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    out
}

#[tokio::test]
async fn logs_alias_match_with_provider_and_role() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;

    let entries = log.recent(10);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let e = &entries[0];
    assert_eq!(e.requested_model, "ccr-kimi-k3");
    assert_eq!(e.matched_by, "alias");
    assert_eq!(e.provider_id, "kimi");
    assert_eq!(e.upstream_model, "k3");
    assert_eq!(e.status, Some(200));
    assert_eq!(e.role.as_deref(), Some("main"));
    assert!(e.error.is_none());

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn logs_family_fallback_and_reports_all_sharing_roles() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"claude-opus-4-5-20251101","messages":[]}"#).await;
    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-deepseek-ds-pro","messages":[]}"#).await;

    let entries = log.recent(10);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].matched_by, "family");
    assert_eq!(entries[1].provider_id, "kimi");
    assert_eq!(entries[1].role.as_deref(), Some("main"));
    assert_eq!(entries[0].role.as_deref(), Some("fast"), "first role wins in display order");
    assert_eq!(entries[0].alias, "ccr-deepseek-ds-pro");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn logs_streaming_flag_and_upstream_failure_with_alias_and_provider() {
    let upstream = MockUpstream::start_sse(
        vec!["event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()],
        std::time::Duration::from_millis(0),
        std::time::Duration::from_millis(0),
    )
    .await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;
    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","stream":true,"messages":[]}"#).await;
    assert!(log.recent(1)[0].stream, "streaming request must be flagged");
    gw.shutdown().await;
    upstream.shutdown().await;

    let dead = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let dead_url = dead.base_url.clone();
    dead.shutdown().await;
    let (gw2, log2) = start_gateway(test_config(&dead_url, 0)).await;
    let body = post(gw2.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;
    assert!(body.contains("provider=kimi"));
    let e = &log2.recent(1)[0];
    assert!(e.error.as_deref().unwrap_or("").contains("kimi"), "error must name provider: {e:?}");
    assert_eq!(e.provider_id, "kimi");
    assert!(e.status.is_none(), "failed requests have no upstream status");
    gw2.shutdown().await;
}

#[tokio::test]
async fn ring_buffer_drops_oldest_entries() {
    let log = cc_router::logging::RequestLog::new(3);
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let cfg = test_config(&upstream.base_url, 0);
    let state_log = log.clone();
    let config = std::sync::Arc::new(std::sync::RwLock::new(cfg));
    let gw = cc_router::gateway::server::start(config, state_log).await.unwrap();

    for i in 0..5 {
        let body = format!(r#"{{"model":"ccr-kimi-k3","pad":{i},"messages":[]}}"#);
        let _ = post(gw.bound_port, "/v1/messages", &body).await;
    }
    assert_eq!(log.len(), 3, "capacity must be enforced");
    let entries = log.recent(10);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].requested_model, "ccr-kimi-k3", "newest first");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn subscribers_receive_entries() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let (gw, log) = start_gateway(test_config(&upstream.base_url, 0)).await;
    let mut rx = log.subscribe();

    let _ = post(gw.bound_port, "/v1/messages", r#"{"model":"ccr-kimi-k3","messages":[]}"#).await;

    let entry = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("broadcast must deliver within 2s")
        .expect("channel must not be closed");
    assert_eq!(entry.provider_id, "kimi");

    gw.shutdown().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn models_endpoint_lists_all_aliases_and_extra_routes() {
    let upstream = MockUpstream::start_json(200, ok_message_body("k3")).await;
    let mut cfg = test_config(&upstream.base_url, 0);
    cfg.extra_routes.push(cc_router::config::ExtraRoute {
        alias: "ccr-big".into(),
        provider_id: "kimi".into(),
        model_id: "k3".into(),
    });
    let (gw, _log) = start_gateway(cfg).await;

    let req = format!(
        "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", gw.bound_port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;

    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let json_start = out.find("\r\n\r\n").unwrap() + 4;
    let v: serde_json::Value = serde_json::from_str(out[json_start..].trim()).unwrap();
    let ids: Vec<String> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"ccr-kimi-k3".to_string()), "{ids:?}");
    assert!(ids.contains(&"ccr-deepseek-ds-pro".to_string()), "{ids:?}");
    assert!(ids.contains(&"ccr-big".to_string()), "extra route alias must be listed: {ids:?}");
    assert_eq!(v["has_more"], false);
    let first = &v["data"][0];
    assert!(first["display_name"].as_str().unwrap().contains('/'), "display name is 'provider / model'");

    gw.shutdown().await;
    upstream.shutdown().await;
}
```

- [ ] **Step 2: 给 `RequestLog` 补订阅与文件落盘**

在 `src-tauri/src/logging/mod.rs` 的 `RequestLog` 中加入字段与方法（保留 Task 6 已有的 `push`/`recent`/`len`）：

```rust
use std::io::Write;
use tokio::sync::broadcast;

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
}
```

`LogEntry` 需要 `Clone`（Task 6 已派生）。

- [ ] **Step 3: 在 `server.rs` 实现 `GET /v1/models`**

在 `handle` 的令牌校验之后、`is_messages` 判断之前插入：

```rust
if path == "/v1/models" && req.method() == hyper::Method::GET {
    let cfg = state.config.read().expect("config lock poisoned");
    let payload = models_payload(&cfg);
    return Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(body_full(serde_json::to_vec(&payload).unwrap()))
        .expect("static response is valid");
}
```

并在同文件加入：

```rust
/// 合成 Anthropic 列表格式：id = 全部模型别名 ∪ extraRoutes 别名。
pub fn models_payload(cfg: &Config) -> serde_json::Value {
    use serde_json::json;
    let mut data = Vec::new();
    for p in &cfg.providers {
        for m in &p.models {
            data.push(json!({
                "type": "model",
                "id": m.alias,
                "display_name": format!("{} / {}", p.name, m.name),
            }));
        }
    }
    for r in &cfg.extra_routes {
        data.push(json!({
            "type": "model",
            "id": r.alias,
            "display_name": format!("{} / {} (extra route)", r.provider_id, r.model_id),
        }));
    }
    let first_id = cfg
        .providers
        .iter()
        .find_map(|p| p.models.first())
        .map(|m| m.alias.clone());
    let last_id = cfg
        .providers
        .iter()
        .rev()
        .find_map(|p| p.models.last())
        .map(|m| m.alias.clone());
    json!({
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id,
    })
}
```

- [ ] **Step 4: 写 `src-tauri/src/commands.rs` 与运行期状态**

```rust
use crate::config::{store::ConfigStore, Config};
use crate::gateway::server::Gateway;
use crate::logging::{LogEntry, RequestLog};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

pub struct AppState {
    pub store: ConfigStore,
    pub log: RequestLog,
    pub gateway: Mutex<Option<Gateway>>,
}

impl AppState {
    pub fn new(store: ConfigStore) -> Self {
        let log = RequestLog::new(500);
        AppState { store, log, gateway: Mutex::new(None) }
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub requests_served: u64,
}

#[tauri::command]
pub async fn gateway_status(state: State<'_, Arc<AppState>>) -> Result<GatewayStatus, String> {
    let guard = state.gateway.lock().await;
    Ok(match guard.as_ref() {
        Some(g) => GatewayStatus {
            running: true,
            port: Some(g.bound_port),
            requests_served: g.requests_served(),
        },
        None => GatewayStatus { running: false, port: None, requests_served: 0 },
    })
}

#[tauri::command]
pub async fn gateway_start(state: State<'_, Arc<AppState>>) -> Result<u16, String> {
    let mut guard = state.gateway.lock().await;
    if let Some(g) = guard.as_ref() {
        return Ok(g.bound_port);
    }
    let config = state.store.shared();
    let gw = crate::gateway::server::start(config, state.log.clone())
        .await
        .map_err(|e| e.to_string())?;
    let port = gw.bound_port;
    *guard = Some(gw);
    Ok(port)
}

#[tauri::command]
pub async fn gateway_stop(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut guard = state.gateway.lock().await;
    if let Some(gw) = guard.take() {
        gw.shutdown().await;
    }
    Ok(())
}

#[tauri::command]
pub fn get_config(state: State<'_, Arc<AppState>>) -> Config {
    state.store.snapshot()
}

#[tauri::command]
pub fn save_config(state: State<'_, Arc<AppState>>, config: Config) -> Result<(), String> {
    state.store.save(config).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recent_logs(state: State<'_, Arc<AppState>>, limit: usize) -> Vec<LogEntry> {
    state.log.recent(limit.clamp(1, 500))
}
```

在 `src-tauri/src/commands.rs` 的 `AppState` 里，`Gateway::requests_served()` 已在 Task 6 定义，这里直接调用即可。

- [ ] **Step 5: 更新 `src-tauri/src/lib.rs` 注册 IPC 与状态**

```rust
pub mod app_paths;
pub mod commands;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod routing;

use crate::config::store::ConfigStore;
use crate::commands::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = ConfigStore::load(crate::app_paths::config_path())
        .expect("无法加载 config.json：请检查 %APPDATA%\\cc-router\\config.json");
    let state = AppState::new(store).shared();

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::gateway_status,
            commands::gateway_start,
            commands::gateway_stop,
            commands::get_config,
            commands::save_config,
            commands::recent_logs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 6: 运行全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: `test result: ok.`，包含 `request_log` 的 6 个测试。

- [ ] **Step 7: 全量构建验证（含前端与 exe）**

Run: `pnpm build && cargo build --manifest-path src-tauri/Cargo.toml`
Expected: 两者都成功。

- [ ] **Step 8: 手工冒烟：真实启动一次网关**

在 `src-tauri` 目录下运行一个临时 bin（或直接用 `cargo test -- --nocapture` 之外的路径）。最简方式：加一次性 `examples/smoke.rs` 并运行：

```rust
// src-tauri/examples/smoke.rs
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
```

Run: `cargo run --manifest-path src-tauri/Cargo.toml --example smoke`
Expected: 打印 `gateway listening on 127.0.0.1:8787`。另开一个终端执行
`curl -s http://127.0.0.1:8787/ccr/health`
Expected: 返回含 `"status":"ok"` 的 JSON。Ctrl-C 结束。

（`tokio` 需要 `signal` feature：把 `tokio` 的 features 加上 `"signal"`。）

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/logging src-tauri/src/gateway/server.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/examples src-tauri/tests/request_log.rs src-tauri/Cargo.toml
git commit -m "feat: 请求日志订阅、/v1/models 合成与最小 Tauri IPC 面"
```

---

## 计划完成后的状态

跑完本计划后应当具备：

- `pnpm tauri build` 能产出 exe（Task 1）。
- `cargo test` 全绿，覆盖：配置校验与原子读写、别名生成与冲突、路由三档解析、请求改写与鉴权注入、令牌校验与错误体、非流式转发、SSE 零缓冲、并发无串扰、客户端断连、上游断流、`/v1/models` 合成、日志环形缓冲与订阅。
- **可手工端到端验证**：把 `ANTHROPIC_BASE_URL` 指向 `http://127.0.0.1:8787`、`ANTHROPIC_AUTH_TOKEN` 设为 config 里的 `localToken`，即可让 Claude Code 走本网关（此时角色别名还需手工写进 env，因为自动接管属于 Plan 2）。

**不在本计划范围**（后续计划）：

- Plan 2：`~/.claude/settings.json` 接管/还原与还原清单、子 Agent frontmatter 管理（M3 + M5）。
- Plan 3：Vue 五个页面、93 条预设迁移、自定义服务商、一键获取模型、测试连接（M4）。
- Plan 4：托盘、开机自启、NSIS 图标与 README、真机 E2E（M6）。

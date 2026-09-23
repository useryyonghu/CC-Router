# CC Router 界面与预设 Implementation Plan（Plan 3）

> **执行后记**：无（尚未执行）

**Goal:** 把已完成的网关内核与接管能力做成**有界面的成品**：能增删改服务商与模型、给 main/fast/subagent 三个角色挑模型、一键接管/还原 Claude Code、看实时请求日志、管理子 Agent；并内置 93 条厂家预设、支持自定义服务商与一键获取模型列表。

**Architecture:** 分两部分，文件互不重叠，可先后独立执行：
- **Part A（后端）**：预设目录（cc-switch 数据迁移 + 用户覆盖）、服务商/模型的增删改与鉴权测试、模型列表拉取（多候选探测）、以及一整套 IPC 命令。
- **Part B（前端）**：Vue 3 + Naive UI 五页 + Pinia store，只调用 Part A 定义的 IPC 命令。

**Tech Stack:** Rust 1.97 / serde_json / reqwest（已有）；Vue 3 + TypeScript + Vite + Naive UI + Pinia（已在骨架里）。

**Spec:** `docs/superpowers/specs/2026-09-22-cc-router-design.md` §5.6（预设）、§5.7（一键获取模型）、§5.8（自定义服务商）、§10（界面）、§6.1（`/v1/models` 与 `GET /ccr/health`）、§9（安全）。

## Global Constraints

- `config`/`routing`/`gateway`/`logging`/`claude` 及本计划新增的 `preset`/`provider`/`fetch` 模块**不得 `use tauri::*`**；只有 `lib.rs`、`commands.rs` 可以。
- **不新增 Cargo 依赖**；前端不新增依赖（Naive UI / Pinia 已在 `package.json`）。
- 所有写盘原子；配置只经 `ConfigStore::save`（走校验）；`presets.user.json` 与 `presets.json` 只读加载。
- **密钥只存在于 `%APPDATA%\cc-router\config.json`**（明文，spec §9.1 已确认）；**绝不写进 `~/.claude/settings.json`**（那里只有本地令牌）。
- 网关**只绑 `127.0.0.1`**；`/ccr/health` 之外的端点都要本地令牌（UI 用 `get_config` 拿令牌后自用，不经 HTTP）。
- **不修改 `docs/`**；不许有新编译告警；不跑仓库级 `cargo fmt`。
- 现状：`cargo test` 全绿（Plan 2 完成后约 111）；分支 `feat/gateway-core`。
- 预设原始数据已存档：`docs/reference/cc-switch-presets.raw.json`（93 条，来自 `farion1231/cc-switch`，**MIT**）。许可证全文在 `docs/reference/cc-switch-LICENSE.txt`。

---

# Part A：后端（预设目录 + 服务商/模型管理 + 模型拉取 + IPC）

## 文件结构

```
tools/build-presets.mjs                    (A1) 一次性生成器（Node，无依赖）
src-tauri/presets.json                     (A1) 生成的 93 条目录（入库）
src-tauri/src/preset/mod.rs                (A1) 加载 + 用户覆盖 + 查询
src-tauri/src/provider/mod.rs              (A2) 服务商/模型增删改 + 校验测试连接
src-tauri/src/provider/fetch.rs            (A3) 多候选探测拉取模型列表
src-tauri/src/commands.rs                  (A4) 新增 IPC 命令
src-tauri/src/lib.rs                       (A4) 模块声明 + 命令注册
```

## A1: 预设目录

**产出**
- `pub struct Preset { pub id: String, pub name: String, pub category: String, pub base_url: String, pub auth_style: AuthStyle, pub models_url: Option<String>, pub api_key_url: Option<String>, pub website_url: Option<String>, pub icon: Option<String>, pub icon_color: Option<String>, pub api_key_field: Option<String>, pub api_format: Option<String>, pub template_values: Option<BTreeMap<String, TemplateValue>>, pub default_env: BTreeMap<String, String>, pub supported: bool, pub unsupported_reason: Option<String>, pub verified_at: Option<String>, pub default_models: Vec<DefaultModel> }`
- `pub fn load_presets(builtin_json: &str, user_json: Option<&str>) -> Result<Vec<Preset>>`（同 `id` 用户覆盖内置；按 `category` 排序：`official` → 合作伙伴 → 其它，组内按名称）
- `pub fn presets_builtin() -> &'static str`（`include_str!("../../presets.json")`）
- `pub fn presets_user_path() -> PathBuf`（`%APPDATA%\cc-router\presets.user.json`）

**`tools/build-presets.mjs` 的转换规则（必须可复现，脚本入库）**

读 `docs/reference/cc-switch-presets.raw.json`，对每条：
1. `id` = `name` 的 slug（小写、非 `[a-z0-9-]` → `-`、折叠、去首尾）；重名冲突时加 `-2`、`-3`。
2. `baseUrl` = `settingsConfig.env.ANTHROPIC_BASE_URL`；**特例**：`Claude Official` 的 env 是 `{}` → `baseUrl = "https://api.anthropic.com"`、`authStyle = "x-api-key"`。
3. `supported` / `unsupportedReason`：
   - `apiFormat === "anthropic"`（或缺省）且 `!requiresOAuth` 且 `!providerType` ⇒ `supported: true`；
   - 否则 `supported: false`，原因按优先级：`requiresOAuth` ⇒ `"需要 OAuth，本期仅支持 API Key"`；`gemini_native` ⇒ `"需要 Gemini 原生格式转换"`；`openai_chat` ⇒ `"需要 OpenAI Chat 格式转换"`；`openai_responses` ⇒ `"需要 OpenAI Responses 格式转换"`。
4. **剥离追踪参数**：对 `websiteUrl`、`apiKeyUrl` 删掉 `aff`、`ref`、`invitecode`、`ch`、`utm_*` 等查询参数（保留其它参数与 fragment）。剥离后若路径形如 `/go/...` 或剩余查询为空且原为纯跳转（无正常路径），则把 `apiKeyUrl` 置 `null`。
5. `defaultEnv` = 原 `settingsConfig.env` 原样（`Claude Official` 的 `{}` 除外，给它 `{"ANTHROPIC_BASE_URL": "https://api.anthropic.com"}`）。
6. `templateValues` / `modelsUrl` / `endpointCandidates` / `isOfficial` / `hidden` / `websiteUrl` / `icon` / `iconColor` / `apiKeyField` 原样带上。
7. `verifiedAt`：**只有** `deepseek`、`xiaomi-mimo`（name 含 `Xiaomi MiMo` 且 baseUrl 为 `https://api.xiaomimimo.com/anthropic` 的那条）、`claude-official` 三条填 `"2026-09-22"`（本机实际验证过）；**其余一律 `null`**。
8. `defaultModels`：从 `defaultEnv` 里的 `ANTHROPIC_MODEL`/`ANTHROPIC_DEFAULT_OPUS_MODEL` 等取值去重得到（取不到就 `[]`）。
9. 生成文件顶层：`{ "version": 1, "source": "farion1231/cc-switch", "license": "MIT", "sourceRef": "main", "presets": [...] }`；JSON 2 空格缩进。

**测试**（`src-tauri/src/preset/mod.rs` 的 `#[cfg(test)]`）
- `loads_all_93_presets`
- `strips_affiliate_and_tracking_params`（断言**没有任何** `baseUrl`/`websiteUrl`/`apiKeyUrl` 含 `aff=`、`invitecode=`、`utm_`；对原始数据里 26 条带参的逐条检查）
- `marks_exactly_5_presets_unsupported_with_reasons`（并断言 5 条分别是 Gemini Native / Nvidia / GitHub Copilot / Codex / xAI）
- `marks_exactly_3_presets_with_template_values`
- `only_three_presets_are_verified`
- `claude_official_has_explicit_anthropic_base_url`
- `user_override_replaces_builtin_by_id`（给一段只含 1 条同 `id` 的 user JSON，断言该条被替换、其余 92 条仍在）
- `ids_are_unique_and_slug_shaped`

## A2: 服务商与模型管理

**产出**（`provider/mod.rs`，全部纯函数式作用于 `Config`，便于单测）
- `pub fn add_provider(cfg: &mut Config, p: NewProvider) -> Result<String>`（生成 `id`/`name` 从 baseUrl 推导（复用 `routing::alias::derive_provider_id`）、`authStyle` 默认 `Both`、返回新 id）
- `pub fn update_provider(cfg: &mut Config, p: Provider) -> Result<()>`
- `pub fn remove_provider(cfg: &mut Config, id: &str) -> Result<()>`（同时清掉指向它的 `roles`/`defaultTarget`/`extraRoutes`，并把悬空处置 `None`）
- `pub fn add_model(cfg: &mut Config, provider_id: &str, model_id: &str, name: Option<&str>, context1m: bool, context_window: Option<u64>, max_tokens: Option<u64>) -> Result<String>`（用 `routing::alias::generate_alias` 生成别名并返回；`[1m]` 后缀剥离且置 `context1m = true`）
- `pub fn remove_model(cfg: &mut Config, provider_id: &str, model_id: &str) -> Result<()>`（同样清掉指向它的角色/默认目标/额外规则）
- `pub fn set_role(cfg: &mut Config, role: RoleSlot, target: Option<Target>) -> Result<()>`
- `pub fn test_connection(cfg: &Config, provider_id: &str, model_id: Option<&str>) -> impl Future<Output = Result<TestResult, Error>>` —— 实际发一条最小 `POST /v1/messages`（`max_tokens: 16`，单轮 user），15s 超时，返回 `{ ok, status, latency_ms, message }`；不写日志、不计入 `requestsServed`。

**测试**：新增服务商后 `id` 合法且不与现有冲突；`remove_provider` 清空悬空引用（断言 `roles.main == None`）；`add_model` 生成的别名唯一、`[1M]` 被剥离；`remove_model` 清空引用；`set_role(None)` 表示未绑定。

## A3: 一键获取模型列表

**产出**（`provider/fetch.rs`）
- `pub struct FetchAttempt { pub url: String, pub outcome: AttemptOutcome }`、`pub enum AttemptOutcome { Http(u16), Error(String) }`
- `pub struct FetchOutcome { pub models: Vec<FetchedModel>, pub used_url: String, pub attempts: Vec<FetchAttempt> }`
- `pub struct FetchedModel { pub id: String, pub display_name: Option<String>, pub context_window: Option<u64>, pub max_tokens: Option<u64>, pub looks_non_chat: bool }`
- `pub fn candidate_urls(provider: &Provider) -> Vec<String>`：若 `provider.models_url` 有值则**只**返回它；否则依次 ① `{baseUrl}/v1/models` ② `{baseUrl}/models` ③ 剥离 `/anthropic`、`/api`、`/api/anthropic`、`/api/claudecode`、`/coding`、`/step_plan`、`/api/coding`、`/api/compatible`、`/api/plan` 后缀后的站点根 + 各自 `/v1/models` 与 `/models`（去重、保序）。
- `pub fn parse_models(bytes: &[u8]) -> Result<Vec<FetchedModel>, Error>`：**同时接受** Anthropic（`data[].id` / `display_name`）与 OpenAI（`data[].id` / `object`）形状，以及裸数组；可提取 `context_length`/`context_window`/`max_input_tokens`、`max_output_tokens`/`max_tokens`；`looks_non_chat` 由 `id` 命中 `embed|embedding|rerank|tts|whisper|audio|image|moderation|dall-e` 判定。
- `pub async fn fetch_models(client: &reqwest::Client, provider: &Provider) -> Result<FetchOutcome, Error>`：按候选顺序"命中即停"（2xx 且能解析出非空列表）；全失败则返回 `Err`，但错误信息里**必须带上每一次尝试的 URL 与状态码/错误摘要**（spec §5.7 的"失败三出路"依赖它）。
- 鉴权复用 `gateway::rewrite` 的注入方式（`authStyle`，`Both` 同发两种头）。

**测试**（用 mock 上游，参照 `tests/support/mod.rs` 的做法，但**不要**改那个共享脚手架——本模块的测试用 `tempfile` 之外的独立 mock 或直接测纯函数）：
- `candidate_urls_prefers_explicit_models_url`（有 `models_url` 时只返回它）
- `candidate_urls_strips_known_compat_subpaths`（`https://api.deepseek.com/anthropic` → 含 `https://api.deepseek.com/models`）
- `parses_anthropic_openai_and_bare_array_shapes`（三种形状 + 缺字段 + 空列表）
- `extracts_context_window_from_either_naming`、`flags_non_chat_models`
- `fetch_reports_every_attempt_on_total_failure`（用一个返回 404 的 mock/本地监听，断言 `attempts` 覆盖所有候选且错误信息含每个 URL）

## A4: IPC 命令

在 `commands.rs` 追加（名称固定，Part B 直接依赖）：

```
presets_list() -> Vec<PresetDto>
provider_add(new: NewProviderDto) -> Result<String, String>
provider_update(provider: ProviderDto) -> Result<(), String>
provider_remove(id: String) -> Result<(), String>
model_add(providerId, modelId, name?, context1m, contextWindow?, maxTokens?) -> Result<String, String>   // 返回别名
model_remove(providerId, modelId) -> Result<(), String>
role_set(role: String, target: Option<TargetDto>) -> Result<(), String>
provider_test(providerId: String, modelId: Option<String>) -> Result<TestResultDto, String>
models_fetch(providerId: String, modelsUrl: Option<String>) -> Result<FetchOutcomeDto, String>
models_add_many(providerId: String, models: Vec<FetchPickDto>) -> Result<Vec<String>, String>   // 返回新别名
gateway_restart() -> Result<u16, String>
token_regenerate() -> Result<String, String>          // 重新生成后若已接管则同步重写 settings.json
backups_list() -> Vec<BackupDto>                       // %APPDATA%\cc-router\backups 下的文件
settings_paths() -> SettingsPathsDto                   // 供 UI 显示"打开配置文件/备份目录"
```

**约定**：`provider_add` 成功后**不自动**绑定角色（spec §5.8 说"UI 询问"——由前端在返回后调用 `role_set`）；`models_fetch` 用 `ConfigStore` 里的 provider，`modelsUrl` 参数用于失败后的**手动重试**（临时覆盖候选）；`models_add_many` 逐条 `provider::add_model`，最后一次性 `ConfigStore::save`。

**测试**：IPC 只做参数校验与转调，核心逻辑已在 A1–A3 覆盖。为 `provider_add` 的空/非法入参、`role_set` 的非法 role 字符串、`token_regenerate` 后令牌确实变化各加一个测试（可以直接测底层函数，不必起 Tauri）。
`lib.rs` 注册全部新命令；`cargo build` 必须成功。

## Part A 出口条件

- `cargo test` 全绿（预计新增 ~30 个测试），无新告警；`cargo build` 成功。
- `src-tauri/presets.json` 已入库，且 `tools/build-presets.mjs` 重跑结果与入库文件一致（脚本幂等）。
- 93 条预设可被 `presets_list` 读出；5 条不支持带原因；3 条带 `templateValues`；只有 3 条有 `verifiedAt`。

---

# Part B：前端（五页界面）

## 文件结构

```
src/main.ts                          (B1) 挂载 Pinia + Naive UI 主题
src/App.vue                          (B1) 布局 + 侧边导航（五页）
src/api/ipc.ts                       (B1) 所有 invoke 的类型化封装（唯一 IPC 出口）
src/stores/config.ts                 (B1) 配置 / 状态 / 日志
src/views/StatusView.vue             (B2) 首页：网关开关、端口、接管状态、请求日志表
src/views/ProvidersView.vue          (B3) 服务商与模型：预设选择、自定义、测试连接、一键获取模型
src/views/RolesView.vue              (B4) 角色路由：main/fast/subagent 三个下拉 + 额外规则 + 未知模型策略
src/views/AgentsView.vue             (B5) 子 Agent：列表 + 三选一模型下拉 + 新建/删除
src/views/SettingsView.vue           (B6) 设置：端口、开机自启、关窗行为、令牌、备份、关于（含 MIT 署名）
src/components/ProviderForm.vue      (B3) 服务商编辑表单（含模型列表 CRUD）
src/components/ModelPicker.vue       (B4/B5) "provider / 模型"两级下拉（复用）
```

## B1: 骨架、IPC 封装、导航

- `src/api/ipc.ts`：对 Part A 的**每个**命令写一个类型化函数（`invoke('provider_add', {...})`），并统一把后端 `String` 错误转成抛出。**前端不允许在别处直接 `invoke`**。
- `App.vue`：`NLayout` + `NLayoutSider` 五项导航 + `NLayoutContent` 路由切换（用本地 `ref` 切页即可，不引 vue-router）。
- `stores/config.ts`：`config`、`gatewayStatus`、`logs` 三个 state + `refreshAll()`；日志用 `setInterval` 每 1.5s 拉 `recent_logs`（后端已有环形缓冲；**不新增事件推送**，避免改后端）。

## B2: 状态页（首页）

- 网关卡片：运行状态、地址 `http://127.0.0.1:<port>`、`启动/停止/重启`。
- 接管卡片：`takeover_status` 的 applied/stale/not_applied 三态 + `一键接管` / `一键还原`；`stale` 时显示红色横幅说明"被其它程序改写"。
- 顶部一行摘要：`主 Agent → <provider> / <model>`、`subagent → …`（由 `roles` 渲染）——用户一眼能确认分流配置。
- 请求日志表：时间、requestedModel、matchedBy、role、provider、upstreamModel、状态码、流式、延迟；按 provider 过滤；显示最近 200 条。

## B3: 服务商页

- 列表：名称、baseUrl、模型数、已验证角标（`verifiedAt` 有无）。
- **新建 = 先选预设或自定义**：预设选择器（搜索框 + 按 `category` 分组，`official` 在前），`supported: false` 的**禁用并显示原因**；含 `templateValues` 的选中后弹输入框，填完替换 `${VAR}`。**预设绝不预填密钥。**
- 「+ 自定义」：只需 **Base URL + API Key** 两栏 → 「创建并获取模型」（失败也要创建成功，落到 A3 的三条出路：手动 URL 重试 / 手动输入模型名 / 用预设 `defaultModels`）。
- 每行 `测试连接`（显示状态码/延迟/错误）。
- 「一键获取模型」：多选列表 + 搜索 + 「疑似非对话模型」默认不勾 + 已添加的显示"已添加"；顶部显示"上次拉取：时间 / 来源 URL / 数量"。
- 模型列表 CRUD：`id`、显示名、别名（自动生成、可改）、`context1m` 开关。

## B4: 角色路由页

- 三个 `ModelPicker`（`main` / `fast` / `subagent`），选项来自**全部已配置模型**；每项旁边显示将写入的别名；未选 = 未绑定（允许，`takeover_apply` 会拦）。
- 只读的 env 预览（把将写入 `~/.claude/settings.json` 的键值列出来）+ 「应用到 Claude Code」按钮（调 `takeover_apply`）。
- 额外规则表（`extraRoutes`）：别名 → 目标。
- 未知模型策略：`default` / `error` + `defaultTarget` 选择。

## B5: 子 Agent 页

- 列表：文件名、`name`、`description`、当前 `model`、生效说明（跟随主模型 / subagent 默认 / 指定）。
- 每行三选一下拉（`inherit` / `subagent_default` / `alias` + `ModelPicker`）。
- `新建子 Agent`（名称、描述、模型、system prompt 正文）、`删除`。
- 说明文字：`~/.claude/agents/` 为空时提示"还没有子 Agent，新建一个即可给它单独指定模型"。

## B6: 设置页

- 端口（改后提示需重启网关，并提供「立即重启」）、关窗行为、开机自启（Plan 4 接真实实现前先只存配置）、退出时自动还原。
- 令牌：显示（默认掩码）/复制/重新生成（提示"会同步重写 Claude Code 配置"）。
- 备份列表 + 「打开备份目录」；导入/导出配置（导出可选移除密钥）。
- **关于**：版本、`source: farion1231/cc-switch` + **MIT 许可证全文**（从 `docs/reference/cc-switch-LICENSE.txt` 复制进前端常量或由 `include_str!` 提供）。

## Part B 出口条件

- `pnpm build` 通过（`vue-tsc --noEmit` 无类型错误）、`cargo build` 通过。
- 五页都能打开，且每个按钮都有真实后端调用（无 TODO 占位）。
- **手工验收**：`pnpm tauri dev` 启动 → 状态页显示网关未运行 → 点启动 → 状态变绿 → 在服务商页加一个真实 provider → 角色路由页把 main/subagent 指到不同模型 → 一键接管 → 用 `claude --settings <临时文件> -p "..."` 验证主/子模型分流 → 一键还原。
- 前端不直接 `invoke`，全部经 `src/api/ipc.ts`。

---

## 不在本计划

- 托盘、开机自启的真实注册、NSIS 打包与真机 E2E 收尾 → Plan 4。
- 用量/费用统计看板、故障转移、协议转换 → spec 的非目标。

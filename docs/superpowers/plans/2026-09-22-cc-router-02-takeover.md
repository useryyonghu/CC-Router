# CC Router 接管与子 Agent Implementation Plan（Plan 2）

> **执行后记**：无（尚未执行）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development 或直接执行。步骤用 `- [ ]` 勾选跟踪。

**Goal:** 让 CC Router 能一键把 Claude Code 接管到本网关（改 `~/.claude/settings.json`），并支持**逐字节精确还原**；同时管理 `~/.claude/agents/*.md` 的 `model:` 字段，使每一个子 Agent 可以选择已配置的模型。

**Architecture:** 新增 `src-tauri/src/claude/` 模块，纯文件操作、不依赖 Tauri（便于用普通 `cargo test` 验证）。两个子模块：`settings.rs`（接管/还原）与 `agents.rs`（frontmatter 的 `model` 行手术）。通过 7 个新 IPC 命令暴露给前端（Plan 3 的 UI 调用）。

**Tech Stack:** Rust 1.97、serde_json、std::fs、chrono（均已存在，**不新增依赖**）。

**Spec:** `docs/superpowers/specs/2026-09-22-cc-router-design.md` §7（接管）、§8（子 Agent）、§9（安全）。

## Global Constraints

- **`claude` 模块及 `config`/`routing`/`gateway`/`logging` 一律不得 `use tauri::*`**；只有 `lib.rs`、`commands.rs` 可用。这是 `cargo test` 能脱离 Tauri 运行的原因。
- **所有写盘必须原子**：同目录临时文件 → `std::fs::rename`。复用 `config::store::atomic_write_json` 的写法（新增一个写**字节**的孪生函数，见 Task 1）。
- **接管前必须备份**，备份落在 `%APPDATA%\cc-router\backups\`，且**同一次接管只备份一次**。
- **只动自己拥有的键**（见 Task 2 的清单）；`settings.json` 顶层其它键（`attribution`、`permissions`…）与 `env` 中其它键（`API_TIMEOUT_MS`…）**一律不改**。
- **AC8 是硬指标**：接管 → 还原后，`settings.json` 必须与接管前**逐字节相同**。
- **子 Agent 的 `model` 是唯一改动点**：frontmatter 其它字段与正文逐字节保留，且必须保持原文件的 CRLF/LF 与"文件末尾是否有换行"。
- **不修改 `docs/`**。**不新增依赖**。**不许出现新的编译警告**。
- 现状：分支 `feat/gateway-core`，`cargo test` → **93 passed, 0 failed**（58 lib + 16 + 10 + 9）。
- 真实环境事实：`~/.claude/settings.json` 现在被 cc-switch 写成指向小米 MiMo（`ANTHROPIC_BASE_URL=https://api.xiaomimimo.com/anthropic`），含 `ANTHROPIC_DEFAULT_FABLE_MODEL`、`*_MODEL_NAME`、`CLAUDE_CODE_SUBAGENT_MODEL`。`~/.claude/agents/` 目前**为空**。`~/.claude/settings.local.json` **没有 `env` 块**（只有 permissions），所以不存在覆盖冲突。

## 文件结构

```
src-tauri/src/
├ claude/
│  ├ mod.rs          (Task 1) 导出 + 共用的原子字节写入
│  ├ settings.rs     (Task 2) 接管/还原 + 还原清单
│  └ agents.rs       (Task 3) frontmatter model 行手术
├ commands.rs        (Task 4) +7 个 IPC 命令
└ lib.rs             (Task 4) + pub mod claude; + 注册命令
src-tauri/tests/
└ claude_takeover.rs (Task 2, Task 3) 端到端夹具测试
```

---

## Task 1: `claude` 模块骨架与字节级原子写入

**Files:**
- Create: `src-tauri/src/claude/mod.rs`
- Modify: `src-tauri/src/lib.rs`（加 `pub mod claude;`，保持字母序：`app_paths, claude, commands, config, error, gateway, logging, routing`）

**Interfaces:**
- Consumes: `crate::error::{Error, Result}`
- Produces: `cc_router::claude::{settings, agents}`；`cc_router::claude::atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()>`

- [ ] **Step 1: 写 `claude/mod.rs`**

```rust
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
```

- [ ] **Step 2: 在 `lib.rs` 挂上模块**

把模块声明改成（字母序）：

```rust
pub mod app_paths;
pub mod claude;
pub mod commands;
pub mod config;
pub mod error;
pub mod gateway;
pub mod logging;
pub mod routing;
```

- [ ] **Step 3: 先建空的子模块让编译通过**：`settings.rs` 与 `agents.rs` 各写一行模块文档注释，内容在 Task 2/3 填。

- [ ] **Step 4: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml claude::`
Expected: `test result: ok.` 2 个测试通过（全库 95）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claude src-tauri/src/lib.rs
git commit -m "feat(claude): claude 模块骨架与字节级原子写入"
```

---

## Task 2: 接管与逐字节精确还原

**Files:**
- Create: `src-tauri/src/claude/settings.rs`
- Create: `src-tauri/tests/claude_takeover.rs`（与 Task 3 共用该文件）
- Modify: 无

**Interfaces:**
- Consumes: `crate::config::Config`、`crate::claude::atomic_write_bytes`
- Produces:
  - `pub const OWNED_KEYS: [&str; N]`（写入的键）
  - `pub const REMOVED_KEYS: [&str; 3]`
  - `pub fn claude_settings_path() -> PathBuf`
  - `pub fn backups_dir() -> PathBuf`
  - `pub struct TakeoverManifest { pub applied_at: String, pub pre_bytes_file: String, pub post_bytes_file: String, pub env_existed: bool, pub keys: BTreeMap<String, KeySnapshot> }`
  - `pub struct KeySnapshot { pub existed: bool, pub value: Option<String> }`
  - `pub fn build_env_updates(cfg: &Config) -> BTreeMap<String, String>`（只算要写的键）
  - `pub fn apply_takeover(cfg: &Config, settings_path: &Path, backups_dir: &Path) -> Result<TakeoverManifest>`
  - `pub fn restore(manifest: &TakeoverManifest, settings_path: &Path, backups_dir: &Path) -> Result<RestoreOutcome>`
  - `pub fn takeover_state(cfg: &Config, settings_path: &Path) -> TakeoverState`（`Applied` / `Stale` / `NotApplied`）

**关键设计（必须照做，理由见 spec §7 与 AC8）**

1. **写入的键**（存在则覆盖）：
   `ANTHROPIC_BASE_URL` = `http://127.0.0.1:<cfg.gateway.port>`；
   `ANTHROPIC_AUTH_TOKEN` = `cfg.gateway.local_token`；
   `ANTHROPIC_DEFAULT_OPUS_MODEL` / `_SONNET_MODEL` / `_FABLE_MODEL` = main 角色模型的别名；
   `ANTHROPIC_DEFAULT_HAIKU_MODEL` = fast 角色模型的别名；
   `CLAUDE_CODE_SUBAGENT_MODEL` = subagent 角色模型的别名；
   以及每个已写入槽位对应的 `..._MODEL_NAME` = `"<provider名> / <模型显示名>"`（人类可读展示名）。
   **未绑定的槽位（`roles.* == None`）对应的键不写入。**
2. **移除的键**（存在则删）：`ANTHROPIC_MODEL`、`ANTHROPIC_API_KEY`、`ANTHROPIC_SMALL_FAST_MODEL`。
3. **`*_MODEL_NAME` 之外的键一律不动。** 顶层非 `env` 的键也不动。
4. **两份原始快照**：接管前把文件**原始字节**存为 `backups/settings.json.<stamp>.pre.bak`；接管后把写出的字节存为 `...post.bak`。这是 AC8 能成立的原因——还原时如果当前文件字节 == post 快照，就**直接写回 pre 快照的原始字节**，天然逐字节相等。
5. **清单（manifest）** 记录 `env_existed` 与每个被写入/移除键的原值，用于：`post` 不匹配时（说明接管后被别的程序改过，例如 Claude Code 自己加键）做**合并式回放**，并在返回里标明用的是哪条路径。
6. 写入格式：`serde_json::to_string_pretty`（2 空格）+ 末尾单换行 + UTF-8 无 BOM。文件不存在按 `{}` 处理且 `env_existed = false`。
7. JSON 非法时**报错并且不写任何东西**（`Error::ConfigInvalid` 或 `Error::Json`），文件保持原样。

**Interfaces（续）**

```rust
pub enum TakeoverState { Applied, Stale { found: String }, NotApplied }
pub enum RestorePath { Verbatim, MergedManifest }
pub struct RestoreOutcome { pub path: RestorePath, pub changed_keys: Vec<String> }
```

- [ ] **Step 1: 写测试（RED）** — 在 `tests/claude_takeover.rs` 里覆盖下列夹具，全部用 `tempfile::tempdir()`，**绝不触碰真实 `~/.claude/`**：
  1. `takeover_then_restore_is_byte_exact`（AC8 主用例）：夹具用**真实形状**——顶层有 `attribution`、`env` 里有 MiMo 的 `ANTHROPIC_BASE_URL`、`ANTHROPIC_DEFAULT_FABLE_MODEL`、`*_MODEL_NAME`、`CLAUDE_CODE_SUBAGENT_MODEL`。接管后断言：`env.ANTHROPIC_BASE_URL` 指向本网关、`ANTHROPIC_MODEL`/`API_KEY`/`SMALL_FAST_MODEL` 不存在、`attribution` 原样、未绑定槽位的键没被写入、`ANTHROPIC_DEFAULT_OPUS_MODEL_NAME` 已是 `"<provider> / <model>"`；还原后断言 `fs::read(path) == before`（**字节相等**）。
  2. `takeover_then_restore_when_file_absent`：文件不存在 → 接管后文件被创建；还原后**文件不存在**（`env_existed=false` ⇒ 删掉我们创建的内容；若原来整个文件都不存在，则还原后应删除该文件）。**这条要按 spec §7.3 实现**：`file_existed=false` 时 `RestoreOutcome` 应删除文件。
  3. `takeover_preserves_other_env_keys`：`env` 里塞 `API_TIMEOUT_MS`、`CLAUDE_CODE_EFFORT_LEVEL`，断言接管后仍在且值不变。
  4. `unbound_roles_write_no_keys`：`roles` 全 `None` 时，只写 `ANTHROPIC_BASE_URL`/`AUTH_TOKEN`，其余都不写。
  5. `malformed_json_is_rejected_and_file_untouched`：写入 `{ not json`，接管返回 `Err` 且文件字节不变、备份也未产生崩溃。
  6. `restore_after_external_edit_uses_manifest_and_keeps_foreign_keys`：接管 → 模拟 Claude Code 自己加了 `env.NEW_KEY` → 还原 → 断言 `NEW_KEY` 仍在（合并式回放）、我们的键已恢复、`RestoreOutcome.path == MergedManifest`。
  7. `takeover_state_detects_applied_stale_and_not_applied`。
  8. `backup_files_are_written_once_per_takeover`：同一 `stamp` 下第二次接管不覆盖已有 pre 备份（spec §7.2 步骤 3）。
- [ ] **Step 2: 跑测试确认全部失败**（模块还是空的）：`cargo test --manifest-path src-tauri/Cargo.toml --test claude_takeover` → 编译失败（函数不存在），记录输出。
- [ ] **Step 3: 实现 `settings.rs`**（按上面 7 条关键设计）。
- [ ] **Step 4: 跑测试到全绿**，并在报告里贴 GREEN 输出。
- [ ] **Step 5: 也跑一次全量** `cargo test --manifest-path src-tauri/Cargo.toml`，确认 95 + 8 = **103** 通过、0 失败、无新警告。
- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/claude/settings.rs src-tauri/tests/claude_takeover.rs
git commit -m "feat(claude): settings.json 接管、备份与逐字节精确还原"
```

---

## Task 3: 子 Agent 的 `model` 字段管理

**Files:**
- Create: `src-tauri/src/claude/agents.rs`
- Modify: `src-tauri/tests/claude_takeover.rs`（追加测试）

**Interfaces:**
- Consumes: `crate::claude::atomic_write_bytes`
- Produces:
  - `pub struct AgentFile { pub path: PathBuf, pub name: Option<String>, pub description: Option<String>, pub model: Option<String>, pub model_line: Option<usize> }`
  - `pub fn list_agents(agents_dir: &Path) -> Result<Vec<AgentFile>>`（递归 `**/*.md`，按路径排序）
  - `pub enum ModelChoice<'a> { Inherit, SubagentDefault, Alias(&'a str) }`
  - `pub fn set_model(path: &Path, choice: ModelChoice<'_>, backups_dir: &Path) -> Result<()>`
  - `pub fn create_agent(agents_dir: &Path, name: &str, description: &str, choice: ModelChoice<'_>, body: &str, backups_dir: &Path) -> Result<PathBuf>`
  - `pub fn delete_agent(path: &Path, backups_dir: &Path) -> Result<()>`
  - `pub struct AgentManifestEntry { pub path: PathBuf, pub existed: bool, pub model_line: Option<String>, pub crlf: bool, pub trailing_newline: bool }`
  - `pub fn set_model_recorded(...) -> Result<AgentManifestEntry>`（`set_model` 的记录版，供还原用）
  - `pub fn restore_agent(entry: &AgentManifestEntry, backups_dir: &Path) -> Result<()>`

**关键设计**

1. **只改 frontmatter 里的 `model:` 行**。frontmatter = 文件开头若为 `---\n`（或 `---\r\n`）则到下一个 `---` 行为止。其余字节**逐字节保留**。
2. 三选一：`Inherit` → 写 `model: inherit`；`SubagentDefault` → **删除** `model:` 行；`Alias(a)` → 写 `model: a`。
3. **保持换行风格与末尾换行**：原文件用 CRLF 就用 CRLF 插入，且保持"末尾是否有换行"不变。这是还原能逐字节相等的前提。
4. 没有 frontmatter 的文件：`Alias/Inherit` 时**在文件开头插入**一段 frontmatter（`---\nname: <stem>\nmodel: …\n---\n`，用文件原换行风格）；`SubagentDefault` 时若没有 `model` 行则**不改动**。
5. 修改前先备份到 `backups/agents/<文件名>.<stamp>.bak`。
6. `delete_agent`：先备份再删除。
7. **不校验 `model` 的值**是否在别名表里（那是 UI 的职责，Plan 3）。

- [ ] **Step 1: 追加测试（RED）**：
  1. `set_model_replace_insert_remove`：三种选择各改一次，断言最终只有 `model:` 行不同（用"去掉 model 行后两串相等"来证明其它内容没动）。
  2. `set_model_preserves_crlf_and_trailing_newline`：CRLF 夹具 + 无末尾换行夹具，改写后换行风格与末尾换行都不变。
  3. `set_model_preserves_other_frontmatter_and_body`：夹具含 `tools:`、`description:` 与正文（含 `---` 分隔线之外的连字符行），断言这些字节未变。
  4. `agent_round_trip_is_byte_exact`：`set_model` → `restore_agent` → 逐字节等于原文件。
  5. `create_then_delete_round_trips`：`create_agent` 生成的文件含 `name`/`description`/`model` 与正文；`delete_agent` 后文件不存在，且 `backups/agents/` 里有备份。
  6. `list_agents_is_recursive_and_sorted`：建两个子目录各一个 `.md`，断言都能列出且顺序稳定。
  7. `no_frontmatter_file_gains_one_for_alias_but_not_for_default`。
- [ ] **Step 2: 确认失败**（函数不存在，编译失败），记录输出。
- [ ] **Step 3: 实现 `agents.rs`**。
- [ ] **Step 4: 全绿**；全量测试应为 **103 + 7 = 110** 通过。
- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/claude/agents.rs src-tauri/tests/claude_takeover.rs
git commit -m "feat(claude): 子 Agent frontmatter model 字段管理（含精确还原）"
```

---

## Task 4: IPC 命令与还原清单落盘

**Files:**
- Modify: `src-tauri/src/commands.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/config/mod.rs`（`TakeoverState` 增加字段，见下）

**Interfaces:**
- 新增 7 个命令（名称固定，Plan 3 的 UI 依赖它们）：
  - `takeover_status() -> TakeoverStatusDto { state: "applied"|"stale"|"not_applied", gatewayUrl: String, found: Option<String>, appliedAt: Option<String> }`
  - `takeover_apply() -> TakeoverStatusDto`（前置检查：网关配置校验通过、三个角色槽位都已绑定，否则返回 `Err` 说明缺哪个）
  - `takeover_restore() -> RestoreDto { path: "verbatim"|"merged_manifest", changedKeys: Vec<String> }`
  - `agents_list() -> Vec<AgentDto { path, name, description, model }>`
  - `agent_set_model(path: String, choice: String, alias: Option<String>) -> Result<(), String>`（`choice` ∈ `inherit|subagent_default|alias`）
  - `agent_create(name, description, choice, alias, body) -> Result<String, String>`（返回新文件路径）
  - `agent_delete(path) -> Result<(), String>`
- `TakeoverState`（在 `config/mod.rs`）改为可承载清单：新增 `#[serde(default)] pub manifest: Option<serde_json::Value>`，其余字段保持。**这是为了让 Plan 3 的 UI 能显示"接管时间/备份文件"，并且让还原在应用重启后仍然可用**——`takeover_apply` 把 `claude::settings::TakeoverManifest` 序列化进 `takeover.manifest` 并 `ConfigStore::save`；`takeover_restore` 从配置里读回它。若 `manifest` 为 `None` 而 `enabled == true`，返回 `Err("接管清单缺失，请用备份文件手动还原")`。
- `AppState` 增加一个便捷方法 `claude_paths()` 返回 `(settings_path, backups_dir)`，从 `app_paths` 推导：`claude_settings_path() = $HOME/.claude/settings.json`（用 `USERPROFILE`，与 `app_paths.rs` 取 `APPDATA` 同一风格），`backups_dir() = %APPDATA%\cc-router\backups`。

- [ ] **Step 1: 先写集成测试**（`src-tauri/tests/claude_takeover.rs` 追加一节，**用 tempdir + 显式传入路径**测试 `apply_takeover`/`restore` 与配置往返：把 manifest 序列化进 `TakeoverState.manifest` 再反序列化回来，断言还原依然逐字节相等）。**不要**在测试里调用真实 IPC（需要 Tauri 运行时）；IPC 只做参数校验与转调，逻辑已在 Task 2/3 覆盖。
- [ ] **Step 2: 实现命令**（薄封装：取路径 → 调 `claude::*` → 映射错误为 `String`；`takeover_apply` 成功后写回配置）。
- [ ] **Step 3: `lib.rs` 注册 7 个命令**到现有 `generate_handler!`，保持字母序。
- [ ] **Step 4: 全量测试** `cargo test --manifest-path src-tauri/Cargo.toml` → 期望 **110 + 1 = 111** 通过、0 失败、无新警告；再跑 `cargo build --manifest-path src-tauri/Cargo.toml` 确认 IPC 装配无误。
- [ ] **Step 5: 手工验证一次真实接管与还原（关键，必须做）**：
  ```powershell
  # 1) 记录当前真实文件的哈希与内容
  $s = "$env:USERPROFILE\.claude\settings.json"
  $before = [System.IO.File]::ReadAllBytes($s); (Get-FileHash $s -Algorithm SHA256).Hash
  # 2) 用一个临时 example（或直接跑应用）触发 takeover_apply —— 见下方说明
  # 3) 断言 BASE_URL 已指向 127.0.0.1:<port>、*_MODEL_NAME 已更新、attribution 未变
  # 4) 触发 takeover_restore，断言 SHA256 与 $before 完全一致
  ```
  由于 IPC 需要 Tauri 运行时，**实现时在 `src-tauri/examples/takeover_check.rs` 里写一个一次性示例**（读真实配置 → `apply_takeover` → 打印结果 → `restore` → 打印 SHA 对比），跑完把输出贴进报告。**该示例可在验证后删除**，删掉也要在报告里说明。
- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/src/config/mod.rs src-tauri/tests/claude_takeover.rs
git commit -m "feat(ipc): 接管/还原与子 Agent 管理的 7 个 IPC 命令"
```

---

## 完成后的状态与出口条件

- `cargo test` → **111 passed, 0 failed**；无新编译警告；`claude` 模块零 Tauri 引用。
- **AC8 成立**：接管 → 还原后 `settings.json` 与接管前**逐字节相同**（用真实形状夹具 + 真实文件各验证一次）。
- 三个角色槽位、未绑定槽位、`*_MODEL_NAME`、需移除的三个旧键、外部改写后的合并式回放——都有测试。
- 子 Agent：三选一、CRLF/末尾换行保持、其它字段与正文不动、创建/删除/还原都有测试。
- 7 个 IPC 命令已注册，Plan 3 的 UI 可以直接调用。
- **不在本计划**：UI（Plan 3）、预设/自定义服务商/一键获取模型（Plan 3）、托盘与安装包（Plan 4）。

# CC Router

**让 Claude Code 同时用多家模型：主 agent 用一家厂商，subagent 用另一家。**

Claude Code 只能配置**一个**第三方模型端点（一个 `ANTHROPIC_BASE_URL` + 一把密钥）。所以「主 agent 用 Kimi、subagent 用 DeepSeek」这种需求，靠配置本身做不到——它们域名不同、密钥不同。

CC Router 在本机起一个路由网关。Claude Code 指向这个网关，网关**按请求体里的 `model` 字段**把请求分发到不同厂商的 Anthropic 兼容端点：

```
                        ┌──────────────────────────────────────┐
   claude.exe ─────────▶│  CC Router 网关  http://127.0.0.1:8787 │
   （只认一个端点）      │  别名 → (厂商, 真实模型) 的路由表       │
                        └───────┬──────────────────────┬───────┘
                                │                      │
                    主 agent ───┘                      └─── subagent
                    ccr-kimi-k3                            ccr-ds-flash
                                │                      │
                                ▼                      ▼
                     Kimi /anthropic            DeepSeek /anthropic
                        （A 厂商的密钥）           （B 厂商的密钥）
```

---

## 快速开始

1. **安装**：双击 `CC Router_0.1.0_x64-setup.exe`。装完从开始菜单打开（也可以让它开机自启）。
2. **加服务商**：在「服务商」页选一个**厂家预设**（内置 90+ 家），或者点「+ 自定义」——只需要粘贴 **Base URL** 和 **API Key** 两栏，然后点「创建并获取模型」，模型列表会自动拉取。
3. **分配模型**：在「角色路由」页给三个角色各挑一个模型：
   - `主 Agent`（main）
   - `快速任务`（fast，Claude Code 的后台小任务）
   - `子 Agent 默认`（subagent）
   - 想做「主 agent 用 A 厂商、subagent 用 B 厂商」，就在这里把 main 和 subagent 指到不同厂商的模型。
4. **（可选）给单个子 Agent 单独指定模型**：在「子 Agent」页为每个子 agent 选模型（跟随主模型 / 用 subagent 默认 / 指定某个模型）。
5. **一键接管**：回「状态」页点「一键接管」。它会改 `~/.claude/settings.json`，让 Claude Code 走本网关。
6. 打开一个新的 `claude` 会话即可。状态页的**请求日志**会实时显示每次请求命中了哪个厂商、哪个模型。

> 页面顶部那行 `主 Agent → <厂商> / <模型>`、`subagent → <厂商> / <模型>` 就是用来一眼确认分流是否生效的。

---

## 它到底改了什么

接管只写这些环境变量（在 `~/.claude/settings.json` 的 `env` 里）：

| 键 | 值 |
|---|---|
| `ANTHROPIC_BASE_URL` | `http://127.0.0.1:<端口>` |
| `ANTHROPIC_AUTH_TOKEN` | 本应用生成的**本地令牌**（真密钥不进这个文件） |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` / `_SONNET_MODEL` / `_FABLE_MODEL` | main 角色模型的**别名** |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL` | fast 角色模型的别名 |
| `CLAUDE_CODE_SUBAGENT_MODEL` | subagent 角色模型的别名 |
| `ANTHROPIC_DEFAULT_OPUS_MODEL_NAME` / `_SONNET_MODEL_NAME` / `_FABLE_MODEL_NAME` / `_HAIKU_MODEL_NAME` | 上面对应四个键的人类可读展示名，形如 `DeepSeek 官方 / DeepSeek V4 Pro`。注意 `CLAUDE_CODE_SUBAGENT_MODEL` **没有**配套的 `_MODEL_NAME`（Claude Code 未提供该变量） |

同时**移除**三个会绕过别名路由的旧键：`ANTHROPIC_MODEL`、`ANTHROPIC_API_KEY`、`ANTHROPIC_SMALL_FAST_MODEL`。

**其它键一律不动**——`env` 里的 `API_TIMEOUT_MS`、`CLAUDE_CODE_EFFORT_LEVEL` 之类，以及顶层的 `attribution`、`permissions`，都不碰。

关键概念是**别名**：每个已配置模型会得到一个稳定的虚拟 ID（如 `ccr-deepseek-deepseek-v4-pro`）。Claude Code 只会看到别名，网关负责把别名换回厂商的真实模型名并用该厂商的密钥发出去。所以改厂商、换模型都不用动 Claude Code 的配置——回状态页点一次「应用到 Claude Code」即可。

## 接管是可逆的，而且是**逐字节**可逆

- 接管前会把 `settings.json` 的**原始字节**备份到 `%APPDATA%\cc-router\backups\settings.json.<时间戳>.pre.bak`；接管后的内容也留一份 `.post.bak`。
- 点「一键还原」时：如果文件自接管以来没被别的程序改过，就直接写回 `.pre.bak` 的原始字节——**保证逐字节等于接管前**。如果文件被外部改过（例如 Claude Code 自己加了键），则按还原清单做**合并式回放**：恢复我们改过的键，保留别人加的键，并明确告诉你走的是哪条路径。
- 状态页会在 `settings.json` 被外部程序改写时显示红色横幅（"接管已失效"），并提供〔重新接管〕〔还原〕。
- 应用退出/卸载**不会**自动还原。**卸载前请先点一次「一键还原」**，否则 Claude Code 会继续指向一个已经不存在的端口。

## 明文密钥警示

`%APPDATA%\cc-router\config.json` **以明文保存各厂商的 API Key**（这是刻意的设计选择，与 Claude Code 自身把密钥明文放在 `settings.json` 里同一信任边界）。因此：

- **不要**把这个文件提交到 git、贴到聊天里、或放进任何同步盘/分享包。
- 「设置 → 导出配置」提供**移除密钥**的选项，分享配置时请用它。
- 该文件位于你的用户目录下，默认只有你自己的账户能读。

## 与 cc-switch 共存

cc-switch 和本应用**都会写** `~/.claude/settings.json`。两者同时启用时必然互相覆盖。建议二选一：

- 用 CC Router 做路由 → 在 cc-switch 里**关闭它的本地代理/路由模式**；
- 或者干脆只用其中一个。

本应用**不会**为了抢回配置而反复重写，只会在状态页告警。

---

## 界面

| 页面 | 作用 |
|---|---|
| 状态 / 日志 | 网关启停、端口、接管状态、**实时请求日志**（时间 / 请求的模型 / 命中方式 / 角色 / 厂商 / 上游模型 / 状态码 / 是否流式 / 延迟） |
| 服务商 | 增删改服务商与模型；厂家预设；自定义（粘贴即用）；测试连接；一键获取模型列表 |
| 角色路由 | main / fast / subagent 三个角色挑模型；额外别名规则；未知模型策略；只读 env 预览 |
| 子 Agent | 管理 `~/.claude/agents/*.md` 的 `model` 字段（只改这一行，其余字节不动） |
| 设置 | 端口、开机自启、关窗行为、令牌、备份列表、配置导入导出、关于 |

日志里的「命中方式」（`matchedBy`）含义：

| 值 | 含义 |
|---|---|
| `alias` | 精确命中了模型别名（正常情况） |
| `family` | 请求里是 Claude 家族名（`claude-opus-*`/`sonnet`/`fable`/`haiku-*`），按角色兜底 |
| `fallback` | 未知模型，按「未知模型策略」落到了默认目标 |
| `passthrough` | 非 `/v1/messages` 的路径，整体转发到默认目标 |
| `error` | 路由失败（没命中且策略为 `error`），本次返回 400 |

---

## 故障排查

| 现象 | 原因 / 处理 |
|---|---|
| Claude Code 报连不上 / 连接被拒绝 | 网关没在跑。回状态页点「启动网关」 |
| 一打开 Claude Code 就报错，且状态页显示"接管已失效" | `settings.json` 被别的程序（多半是 cc-switch）改写了。点「重新接管」或「还原」 |
| 日志里某条请求 `status` 是 401 | 上游厂商拒绝了密钥。去「服务商」页点「测试连接」看具体错误 |
| 日志里某条 `status` 是 404 | 上游不认识这个模型名。确认「服务商」页里该模型的**上游模型 ID** 拼写与厂商文档一致 |
| 日志里出现 `matchedBy=fallback` | 请求的模型名没有任何别名命中，被兜底转发了。要么在角色路由里绑定正确的模型，要么把未知模型策略改成 `error` 以便在配置阶段就暴露问题 |
| 日志里出现 `matchedBy=error` | 同上，但策略是 `error`，请求被拒（400）。错误消息里会写明是哪个模型名 |
| 502 | 网关连不上上游（DNS/网络/厂商挂了）。错误消息里带 `provider=` 与 `别名=` |
| 504 | 上游连上了但超时（非流式响应体在 `idleTimeoutMs` 内没读完），或流式空闲超时 |
| 端口 8787 被占用 | 设置页换一个端口，然后点「立即重启网关」 |
| 改完角色/模型没生效 | 回状态页点「应用到 Claude Code」，然后**新开**一个 `claude` 会话（已运行的会话读的是启动时的环境变量） |

**隐私**：本应用不采集任何遥测。除了转发 Claude Code 的请求、以及你在「测试连接」「一键获取模型」时主动发起的请求之外，不发起任何网络连接。网关只监听 `127.0.0.1`（不对外暴露），且除 `GET /ccr/health` 之外所有请求都要求本地令牌。

---

## 开发

```bash
pnpm install
cargo test --manifest-path src-tauri/Cargo.toml     # 全部后端测试
pnpm tauri dev                                      # 开发模式启动桌面应用
pnpm tauri build                                    # 产出 NSIS 安装包
```

按磁盘配置**手工启动网关**（没有 UI 时用来联调，会打印别名路由表）：

```bash
cargo run --manifest-path src-tauri/Cargo.toml --example serve
curl -s http://127.0.0.1:8787/ccr/health
```

配置文件与备份的位置：

| 路径 | 内容 |
|---|---|
| `%APPDATA%\cc-router\config.json` | 全部配置（**含明文密钥**） |
| `%APPDATA%\cc-router\presets.user.json` | 可选：覆盖/追加厂家预设（同 `id` 覆盖内置），改厂商地址不必等应用发版 |
| `%APPDATA%\cc-router\backups\` | 接管前的 `settings.json` 原始字节备份 |
| `%APPDATA%\cc-router\logs\` | 应用日志 |

## 第三方许可

厂家预设数据（内置 `presets.json`，90+ 条厂商连接信息）来自开源项目 **[farion1231/cc-switch](https://github.com/farion1231/cc-switch)**，遵循 **MIT 许可证**（Copyright © 2025 Jason Young）。许可证全文见应用「设置 → 关于」页，仓库内副本见 `docs/reference/cc-switch-LICENSE.txt`。

上游项目的预设里带有它自己的推广/联盟链接，本项目在迁移时**不搬运任何第三方推广关系**。规则是**白名单**（而不是"拉黑一批参数名"）：链接（`websiteUrl` / `apiKeyUrl`）里只保留确认有功能作用的参数（`apikey`、`redirect`、`tab`），**其余参数一律剥离**，并在生成预设时逐条打印出来供人工复核；如果**路径**里带着上游项目的推广标记（或该参数被保留、无法剥离），整条链接置空；无法判断落点的**不透明短链**（如 `s.qiniu.com/nMvAvy`）也置空，改由界面显示厂商主页。
之所以用白名单：参数名是开放集合，黑名单先后漏过 7 个参数和 2 条完全不带参数的推广路径。反过来，仅仅因为某个**会被删掉的**追踪参数里出现了推广标记就整条丢弃也是错的 —— 那会连 `火山引擎` 的密钥页（它依赖 `apikey=%7B%7D` 才能打开）一起丢掉。

预设中标注"未验证"的条目表示该地址尚未在本机实测（内置 93 条里只有 3 条在本机验证过），请用「测试连接」自行确认。

## 非目标

不做：故障转移与多目标权重、用量/费用统计看板、OpenAI/Gemini 协议转换、Codex/Gemini CLI 等其他客户端。

因此内置 93 条预设里有 **7 条标记为"不支持"**并在界面上禁用、附原因：5 条是要走 OpenAI/Gemini 原生协议的厂商，另 2 条是 **AWS Bedrock**（Claude Code 需带厂商凭证直连 Bedrock，会绕过本网关，且本版本网关只做 Anthropic 格式透传）。它们仍保留在列表里，将来加协议转换时可直接启用。

## 已知限制（这一版没做的）

- **接管是"全量"的**：一次接管让 Claude Code 的所有模型都走本网关；没有"只接管某几个模型"的开关。
- **备份只用于还原，不能在界面上"还原到某个历史备份"**：设置页能列出备份文件，但一键还原走的是"接管前那一份 + 还原清单"。
- **子 Agent 页只改 `~/.claude/agents/*.md` frontmatter 里的 `model:` 行**，不支持编辑 frontmatter 的其它字段或正文。
- **请求日志**：后端在内存里保留最近 **500** 条（重启即清空），但界面只请求并展示其中最近 **200** 条（每 1.5 秒刷新一次）。
- **服务商的 `id` 由名称推导且创建后不可改**（改 id 会让已有别名与角色绑定失效）；显示名称可以随便改。
- **「测试连接」的结果不会写回预设目录** —— 预设的"已验证"标记是本应用构建时就固定的，不是你自己点出来的。
- 密钥以**明文**存在 `%APPDATA%\cc-router\config.json`（见上文警示）。

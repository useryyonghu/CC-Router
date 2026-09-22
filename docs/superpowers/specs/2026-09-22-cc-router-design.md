# CC Router 设计规格（spec）

- 日期：2026-09-22
- 状态：待评审（评审通过后进入 writing-plans）
- 项目路径：仓库根目录
- 产品名：CC Router（中文名：Claude Code 多模型路由）

---

## 1. 背景与问题

Claude Code 只允许配置**一个**第三方模型端点（`ANTHROPIC_BASE_URL` + 一个令牌）。因此无法做到「主 agent 用 Kimi、subagent 用 DeepSeek」——因为二者是不同厂商、不同域名、不同密钥。

本机实测现状（作为需求证据）：

| 项 | 实测值 |
|---|---|
| Claude Code | `claude.exe` 2.1.278，配置在 `~/.claude/settings.json` |
| 当前第三方端点 | 小米 MiMo `https://api.xiaomimimo.com/anthropic`（单一厂商） |
| 主 agent 模型 | `mimo-v2.6-pro[1M]`（`ANTHROPIC_DEFAULT_OPUS/SONNET_MODEL`） |
| subagent 模型 | `mimo-v2.6-flash[1M]`（`CLAUDE_CODE_SUBAGENT_MODEL`） |
| `~/.claude/settings.local.json` | **无 `env` 块**，只有 `permissions` → 不存在覆盖冲突 |
| `~/.claude/agents/` | **空目录** → 子 agent 文件由本应用创建 |
| cc-switch | 已装（`D:\CC Switch\cc-switch.exe`），`~/.cc-switch/settings.json` 中 `enableLocalProxy: true`，同样会写 `~/.claude/settings.json` |
| 工具链 | Node 24.15 / pnpm 10.34 / Rust 1.97 / WebView2 153 就绪；cargo 缓存已有 `tauri 2.11`、`tokio`、`hyper`、`reqwest` |

## 2. 目标与非目标

### 2.1 目标
1. 提供一个带图形界面的 Windows 桌面应用，在其中集中管理**多个**模型服务商与模型（比照 DSH 那样的多模型配置体验，但本应用独立存储、不读写 DSH 配置）。
2. 应用在本机运行一个**路由网关**（HTTP，仅监听 `127.0.0.1`）。Claude Code 指向该网关；网关**按请求体中的 `model` 字段**把请求分发到不同厂商的不同模型。
3. 实现「主 agent 用 A 厂商、subagent 用 B 厂商」以及「某个具体子 agent 用 C 厂商」。
4. 由应用自动接管 `~/.claude/settings.json`（仅改动自己拥有的键），并提供**精确到字节的一键还原**。
5. 真密钥不写入 `~/.claude/settings.json`——该文件只出现本地令牌。
6. 提供**厂家预设**（选中厂商即自动填好 baseUrl、鉴权风格、模型列表接口、取密钥的控制台链接）与**一键获取模型列表**（直接向厂商拉取可用模型，多选后批量加入配置），把"手工抄 URL、手工敲模型名"降到最低。

### 2.2 非目标（本期明确不做）
- 故障转移、多目标权重、健康探测式熔断。
- 用量/费用统计看板；**不解析 SSE 流以统计 token**（理由见 6.6）。
- OpenAI 兼容协议翻译（本期只支持 Anthropic 兼容上游）。
- 支持 Codex / Gemini CLI / 其它客户端。
- 与 DSH 的任何集成或配置导入。
- macOS / Linux 打包（代码尽量跨平台，但只交付 Windows 安装包）。

## 3. 术语

| 术语 | 含义 |
|---|---|
| **provider（服务商）** | 一个 Anthropic 兼容端点：`baseUrl` + 密钥 + 鉴权风格 |
| **模型（model）** | 某 provider 下的一个上游模型名，例如 `k3`、`deepseek-v4-pro` |
| **别名（alias）** | 本网关对外暴露的虚拟模型 ID，形如 `ccr-kimi-k3`。Claude Code 只会看到别名 |
| **角色（role）** | 三类槽位：`main`（主 agent）、`fast`（后台/快速任务）、`subagent`（子 agent 默认） |
| **目标（target）** | 一个 `(providerId, modelId)` 二元组 |
| **预设（preset）** | 一个厂商的连接信息模板（baseUrl / 鉴权风格 / 模型列表接口 / 取密钥链接），来自内置数据文件或用户覆盖文件 |
| **接管（takeover）** | 把本网关地址与别名写入 `~/.claude/settings.json` 的 `env` 块 |

## 4. 架构

### 4.1 进程与组件

单进程：Tauri 2 应用。Rust 后端内含五个组件，前端通过 Tauri IPC 调用。

```
┌──────────────────────── Tauri 2 应用（单进程）─────────────────────────┐
│  Vue 3 + Naive UI 前端 (WebView2)                                       │
│        ⇅ Tauri IPC (invoke / event)                                     │
│  Rust 后端                                                              │
│   ├── ConfigStore   Arc<RwLock<Config>>；原子落盘；热生效               │
│   ├── Gateway       tokio + hyper；127.0.0.1:port；路由 + 转发 + SSE    │
│   ├── Takeover      ~/.claude/settings.json 读写 + 备份 + 还原清单      │
│   ├── Agents        ~/.claude/agents/*.md frontmatter 中 model 字段     │
│   └── RequestLog    内存环形缓冲（最近 500 条）+ 可选 jsonl 文件         │
└────────────────────────────────────────────────────────────────────────┘
      ▲                                                    │
      │ ANTHROPIC_BASE_URL=http://127.0.0.1:8787            │ 按 model 别名分流
      │ ANTHROPIC_AUTH_TOKEN=<本地令牌>                     ▼
 claude.exe ────────────────────────────────────────►  Kimi / DeepSeek / MiMo / …
                                                        （各自携带真实密钥）
```

### 4.2 核心机制与其证据

Claude Code 会把「当前角色所选定的模型名」原样放进 `/v1/messages` 请求体的 `model` 字段。`CLAUDE_CODE_SUBAGENT_MODEL` 决定未显式指定模型的子 agent 用哪个模型（见 [Claude Code 环境变量文档](https://code.claude.com/docs/zh-CN/env-vars)）；`ANTHROPIC_DEFAULT_OPUS/SONNET/HAIKU_MODEL` 决定 `/model` 家族别名映射到哪个模型（见 [Model configuration](https://code.claude.com/docs/en/model-config)）；子 agent 的 `.claude/agents/*.md` frontmatter 中 `model:` 可单独覆盖。

**因此网关只依据 `model` 字段路由，不解析 prompt、不依赖私有 header。** 本机现有配置（`mimo-v2.6-pro[1M]` 作为 `ANTHROPIC_DEFAULT_OPUS_MODEL`）能正常工作，即该机制在本机已被验证。

### 4.3 进程生命周期
- 窗口关闭默认**最小化到托盘，网关继续运行**（Claude Code 随时可能启动）。
- 提供「退出应用」显式动作；退出时**不自动还原**（还原是独立动作），但设置里提供「退出时自动还原」可选项，默认关闭。
- 提供开机自启开关（默认开启）。
- 网关只绑定 `127.0.0.1`，避免 Windows 防火墙弹窗。

## 5. 配置规格

### 5.1 位置与文件

| 路径 | 内容 |
|---|---|
| `%APPDATA%\cc-router\config.json` | 全部配置（**含明文密钥**，见 9.1） |
| `%APPDATA%\cc-router\backups\settings.json.<yyyyMMddTHHmmss>.bak` | 接管前的 settings.json 备份 |
| `%APPDATA%\cc-router\backups\agents\<file>.<yyyyMMddTHHmmss>.bak` | 修改子 agent 文件前的备份 |
| `%APPDATA%\cc-router\logs\app.log` | 应用日志 |
| `%APPDATA%\cc-router\logs\requests-YYYY-MM-DD.jsonl` | 可选（默认关闭）请求日志 |

所有写盘操作必须**原子**：写临时文件 → `fs::rename` 覆盖。config.json 使用 2 空格缩进、UTF-8、无 BOM，便于手工编辑。

### 5.2 config.json schema

```jsonc
{
  "version": 1,
  "gateway": {
    "bind": "127.0.0.1",
    "port": 8787,
    "localToken": "sk-ccr-<32位小写hex>",
    "maxRequestBodyBytes": 134217728,     // 128 MiB
    "connectTimeoutMs": 10000,
    "idleTimeoutMs": 300000
  },
  "providers": [
    {
      "id": "kimi",                        // 唯一，[a-z0-9-]，用户可改
      "name": "Kimi Coding",
      "baseUrl": "https://api.moonshot.cn/anthropic",
      "apiKey": "sk-...",                  // 明文
      "authStyle": "both",                 // both | x-api-key | bearer
      "presetId": "kimi",                  // 来自哪个预设；手动创建为 null
      "modelsPath": null,                  // 模型列表接口：null=自动探测（见 5.7）；可为相对路径或完整 URL
      "modelsFetch": {                     // 最近一次"一键获取"的元信息，null 表示从未拉取
        "lastAt": "2026-09-22T10:00:00+08:00",
        "lastUrl": "https://api.moonshot.cn/v1/models",   // 实际试通的那个 URL
        "count": 12
      },
      "models": [
        {
          "id": "k3",                      // 上游真实模型名（不含 [1M] 后缀）
          "name": "Kimi K3",               // 显示名
          "alias": "ccr-kimi-k3",          // 持久化，生成后不再变化
          "context1m": false,              // true 时附加 1M beta 头
          "contextWindow": 256000,
          "maxTokens": 32000
        }
      ]
    }
  ],
  "roles": {
    // 每个槽位可为 null（未绑定）。未绑定时不写入对应的 env 键。
    "main":     { "providerId": "kimi",     "modelId": "k3" },
    "fast":     { "providerId": "deepseek", "modelId": "deepseek-v4-flash" },
    "subagent": { "providerId": "deepseek", "modelId": "deepseek-v4-pro" }
  },
  // 自定义附加别名：在每个模型自带的默认别名之外，额外暴露的快捷别名
  "extraRoutes": [
    { "alias": "ccr-big", "providerId": "kimi", "modelId": "k3" }
  ],
  "onUnknownModel": "default",             // default | error
  "defaultTarget": { "providerId": "deepseek", "modelId": "deepseek-v4-pro" },
  "takeover": {
    "enabled": false,
    "appliedAt": null,
    "backupFile": null,
    "settingsKeys": {},                    // 见 7.2 还原清单
    "agentFiles": {}                       // 见 8.3 还原清单
  },
  "ui": {
    "closeToTray": true,
    "autostart": true,
    "requestLogToFile": false,
    "restoreOnExit": false
  }
}
```

### 5.3 校验规则
- `providers[].id` 唯一且匹配 `^[a-z0-9][a-z0-9-]{0,31}$`。
- `baseUrl` 必须 `http(s)://` 开头；**禁止**指向 `127.0.0.1`/`localhost` 的本应用端口（防自环）。
- 同一 provider 内 `models[].id` 唯一；全局 `alias` 唯一。
- `roles.*`（非 `null` 时）与 `defaultTarget` 必须指向存在的 `(providerId, modelId)`。
- `extraRoutes[].alias` 不得与任何模型的默认别名冲突。
- `modelsPath` 只能是 `null`、以 `/` 开头的相对路径、或完整 `http(s)://` URL。
- `gateway.port` ∈ [1024, 65535]。
- 子 agent 的模型指派**不进 config.json**：唯一来源是 `~/.claude/agents/*.md` 的 frontmatter，避免双份事实来源。
- 校验失败时：UI 拒绝保存并指明字段；若启动时读到的 config.json 校验失败，网关**不启动**，UI 显示明确错误（不静默用默认值覆盖用户文件）。

### 5.4 别名生成规则
- 默认 `ccr-<providerId>-<modelId>`：转小写 → 非 `[a-z0-9._-]` 字符替换为 `-` → 折叠连续 `-` → 去首尾 `-`；若超过 64 字符则截断并追加 `-<6位hash>`。
- 若与已有别名冲突，追加 `-2`、`-3`…
- 生成结果**持久化**到 `models[].alias`，此后不再自动变化（保证 Claude Code 侧引用稳定）。
- 用户在 UI 粘贴带 `[1M]`/`[1m]` 后缀的模型名时，自动剥离后缀并把 `context1m` 置为 `true`。

### 5.5 热生效
UI 保存 → 后端校验 → 原子写盘 → 更新 `Arc<RwLock<Config>>` → 网关下一请求即生效，**无需重启**。唯一例外：修改 `gateway.port` 需要重启网关（UI 明确提示并提供「立即重启网关」按钮）。

### 5.6 厂家预设（presets）
预设是**数据，不是代码**：内置 `presets.json` 随应用打包；用户可在 `%APPDATA%\cc-router\presets.user.json` 追加或覆盖（同 `id` 覆盖内置），**无需重新打包应用**。

```jsonc
{
  "version": 1,
  "presets": [{
    "id": "deepseek",
    "name": "DeepSeek 官方",
    "baseUrl": "https://api.deepseek.com/anthropic",
    "authStyle": "both",
    "modelsPath": null,                     // null = 按 5.7 自动探测
    "docsUrl": "https://api-docs.deepseek.com/",
    "apiKeyUrl": "https://platform.deepseek.com/api_keys",   // UI 一键打开取密钥页
    "defaultModels": [ { "id": "deepseek-v4-pro", "name": "deepseek-v4-pro" } ],
    "notes": "baseUrl 已在本机现有可用配置中验证",
    "verifiedAt": "2026-09-22"
  }]
}
```

- 选中预设 → 预填 `baseUrl` / `authStyle` / `modelsPath` / `presetId`，并展示 `docsUrl` 与 `apiKeyUrl` 链接。**预设不含、也绝不自动填写密钥。**
- `defaultModels` 是"一键获取失败"时的**离线兜底候选**（见 5.7），由用户勾选后加入。
- **诚实标注**：`verifiedAt` 为 `null` 的预设，UI 显示"未验证"角标。
- **首版内置范围**
  - 已在本机实际配置中验证：**DeepSeek 官方**（`https://api.deepseek.com/anthropic`，见 `~/.claude/envs.json` 的可用预设）、**小米 MiMo**（`https://api.xiaomimimo.com/anthropic`，见当前生效的 `settings.json`）、**Anthropic 官方**。
  - 需在 M4 阶段用真实端点逐个验证后才标 `verifiedAt`：**Kimi / Moonshot**（`~/.dsh/settings.yaml` 里只有 `kimi-coding` 的 provider 名与密钥引用，未暴露 baseUrl）、智谱 GLM、通义千问、硅基流动、OpenRouter 等常见厂商。
  - 验证不通过的条目：保留但标 `verifiedAt: null`，或从内置列表移除。**绝不把未经验证的 URL 当作可用预设提供。**

> 反例警示：cc-switch 出现过"预设生成的 config 与官方文档不一致，且自动拉取模型列表不通"（[issue #6566](https://github.com/farion1231/cc-switch/issues/6566)）。因此本设计把"预设 URL 必须实测"与"拉取失败必须有出路"作为硬要求，并把预设做成可被用户覆盖的数据文件——厂商改 URL 时不必等应用发版。

### 5.7 一键获取模型列表

**请求**：**自动探测**依次尝试以下 3 个候选 URL，命中即停；若 `modelsPath` 已显式设置，则只试它（不再探测）：

1. `provider.modelsPath`
2. `{baseUrl}/v1/models`（Anthropic 兼容约定）
3. 去掉已知路径后缀（`/anthropic`、`/api/anthropic`）后的**站点根** + `/v1/models`（OpenAI 兼容约定）

失败后用户还可以在 UI 中**手动填入完整 URL** 重试（这条不是自动探测的一部分，见下方"失败处理"）。

- 鉴权：复用 provider 的 `authStyle` 注入真实密钥（与网关同一套逻辑）。
- 超时 15s。结果写入 `modelsFetch`，并把试通的 URL 记入 `modelsFetch.lastUrl`，下次优先试它。

**响应解析**：Anthropic 与 OpenAI 两种形状都接受（二者共用 `data[].id`），并防御性接受裸数组 `[…]`：

```
Anthropic: { "data": [{ "type": "model", "id": "…", "display_name": "…" }], "has_more": false }
OpenAI:    { "object": "list", "data": [{ "id": "…", "object": "model", "created": … }] }
```

可提取的附加字段：`display_name` / `name`、`created`、`context_length` / `context_window` / `max_input_tokens`、`max_output_tokens` / `max_tokens`。

> 这里只是**解析列表响应**，不属于 §2.2 所排除的"OpenAI 协议翻译"——消息转发（`/v1/messages`）仍然只支持 Anthropic 兼容上游。

**选择与写入**
- 结果以多选列表呈现，带**搜索框**（部分厂商有 300+ 模型）与全选/反选。
- 疑似非对话模型（`id` 命中 `embed`/`embedding`/`rerank`/`tts`/`whisper`/`audio`/`image`/`moderation`/`dall-e` 等）默认**不勾选**但可见，并标"疑似非对话模型"。
- 已存在于该 provider 的模型显示「已添加」，不可重复加入。
- 加入时逐条生成：`id`（自动剥离结尾 `[1m]` 并把 `context1m` 置 `true`）、`name`（`display_name` 优先）、`alias`（按 5.4）、`contextWindow` / `maxTokens`（响应里有则填，否则 `null`）。
- `contextWindow` / `maxTokens` **仅用于界面展示**；网关不依赖它们（`max_tokens` 由 Claude Code 自己发送）。

**失败处理（一等公民，不允许静默失败）**
- 全部候选失败时，列出**每个候选 URL 的尝试结果**（状态码 / 错误摘要），并给出三条出路：
  1. 手动填入完整模型列表 URL 重试；
  2. 手动输入模型名（走已有的模型 CRUD）；
  3. 使用该预设的 `defaultModels` 作为离线兜底候选。
- 常见原因在 UI 直接解释：401（密钥错或无权限）、404（厂商不提供模型列表接口）、403（地区或账号限制）。

## 6. 网关规格

### 6.1 端点契约

| 方法 路径 | 行为 |
|---|---|
| `GET /ccr/health` | 无需令牌。返回 `{"status":"ok","version":"…","port":8787,"uptimeSec":…}` |
| `POST /v1/messages` | 路由 → 改写 `model` → 转发 → 原样回传（含 SSE 流式） |
| `POST /v1/messages/count_tokens` | 同路由逻辑，非流式直通 |
| `GET /v1/models` | 合成 Anthropic 列表格式，`id` 为**全部已配置模型的默认别名 + `extraRoutes` 的别名**，`display_name` 为 `"<provider名> / <模型显示名>"` |
| 其它任意路径 | 若 `defaultTarget` 已绑定 → 转发到该 provider（日志 `matchedBy="passthrough"` 并标黄）；未绑定 → `404` |

- 除 `/ccr/health` 外，所有请求都必须携带本地令牌，否则 `401` + Anthropic 错误体。
- 令牌校验：`Authorization: Bearer <token>` 或 `x-api-key: <token>` 任一匹配即通过。
- query string 原样保留（例如 `?beta=true`）。

### 6.2 路由解析顺序（固定，先命中先返回）

1. **别名精确匹配**：对请求 `model` 去首尾空白、转小写、剥离结尾 `[1m]` 后，在别名表中查找。
   - 别名表 = **全部已配置模型的默认别名** ∪ **`extraRoutes[].alias`**（全局唯一，故每条恰好对应一个目标）。
   - 因此"已配置的模型"天然可以直接用其别名调用（例如手打 `/model ccr-kimi-k3`）；角色绑定只决定我们往 Claude Code 里写哪些别名，不影响可路由范围。
2. **Claude 家族兜底**：`model` 含 `opus`、`sonnet` 或 `fable`（不区分大小写）→ `roles.main`；含 `haiku` → `roles.fast`。
   - 若对应的兜底槽位为 `null`（未绑定），则本条视为未命中，转入第 3 步。
   - `fable` 家族的确切语义待实施时用 `claude --help` 与官方文档核实；核实前按 main 家族保守处理（见 7.1）。
3. **未知模型**：按 `onUnknownModel`
   - `default` → 用 `defaultTarget` 转发，日志 `matchedBy="fallback"` 并标黄；
   - `error` → 返回 `400`，错误体见 6.5。

> 说明：subagent 的识别**完全依赖 Claude Code 送来的别名**（来自 `CLAUDE_CODE_SUBAGENT_MODEL` 或 agent frontmatter）。不做任何基于 prompt 内容的启发式判断。

### 6.3 请求改写
- **只改**请求体 JSON 的顶层 `model` 字段（别名 → `target.modelId`）。
- `system`、`messages`、`tools`、`tool_choice`、`metadata`、`cache_control`、`thinking` 等一律原样透传（语义零改动）。
- 请求体必须完整读入内存才能改写 `model`；超过 `maxRequestBodyBytes` 返回 `413`。改写方式为 `serde_json` 反序列化 → 改 `model` → 重新序列化，**语义等价但不保证字节等价**（JSON 空白与键序可能变化，对 Anthropic 协议无影响）。转发时不使用 chunked，重算 `Content-Length`。
- **鉴权注入**：剥离客户端传来的 `authorization` 与 `x-api-key`，再按 provider 的 `authStyle` 注入真实密钥：
  - `both`（默认）：同时发送 `x-api-key: <key>` 与 `Authorization: Bearer <key>`（对中转站兼容性最好）；
  - `x-api-key` / `bearer`：只发对应一个。
- **请求头**：
  - 透传：`anthropic-version`（缺失时补 `2023-06-01`）、`anthropic-beta`、`content-type`、`accept`、`user-agent`。
  - 丢弃：`host`、`content-length`（重算）、`authorization`、`x-api-key`、`connection`、`accept-encoding`（避免上游压缩导致流式解析复杂化；统一以未压缩传输）。
  - 新增：`x-ccr-alias`、`x-ccr-provider`、`x-ccr-role`（便于用上游日志排查，上游忽略未知头）。

### 6.4 `[1M]` 上下文与 beta 头
- 别名本身**不带**后缀；判定 `force1m = 请求 model 以 [1m] 结尾`。
- 生效条件：`force1m || target.model.context1m`。命中时向 `anthropic-beta` 追加 `context-1m-2025-08-07`（逗号连接、去重）。
- 上游收到的模型名**永远不含** `[1M]` 后缀。

> 设计依据：cc-switch 在本地路由模式下明确要求**不要**在模型名里写 `[1M]` 后缀（[issue #2337](https://github.com/farion1231/cc-switch/issues/2337)）。本网关通过在别名匹配阶段剥离后缀、改由显式布尔开关控制 beta 头，一次性消除该类问题。
>
> 同时注意与此相反的另一条：cc-switch 认为本地路由模式**不该**写 `ANTHROPIC_DEFAULT_*_MODEL`（[issue #4238](https://github.com/farion1231/cc-switch/issues/4238)），那是因为 cc-switch 用真实家族名匹配。**本设计中别名正是网关与 Claude Code 之间的契约，必须写**。此差异为有意决策，不得当作缺陷"修复"。

### 6.5 响应与错误
- 上游响应**原样透传**（状态码、响应头中的 `content-type`、`request-id` 等；剔除 `content-length`/`transfer-encoding` 由本地重新协商）。
- 网关自身产生的错误统一为 Anthropic 错误体：
  ```json
  { "type": "error", "error": { "type": "invalid_request_error", "message": "…" } }
  ```
  | 场景 | 状态码 | `error.type` |
  |---|---|---|
  | 令牌缺失/不匹配 | 401 | `authentication_error` |
  | 未知模型且策略为 `error` | 400 | `invalid_request_error` |
  | 请求体超过上限 | 413 | `invalid_request_error` |
  | provider 连接失败 / DNS 失败 | 502 | `api_error` |
  | provider 连接或空闲超时 | 504 | `api_error` |
  | config.json 运行时无效 | 500 | `api_error` |
- 错误消息中带上别名与 provider 名，便于定位是哪条规则出问题。

### 6.6 流式转发（关键约束）
- 上游 `content-type` 为 `text/event-stream` 时，用 `Body::wrap_stream` 逐块转发，**不得缓冲、不得等待流结束**；每收到一块立即 flush。
- 客户端断开 → 立即取消对应的上游请求（drop 掉上游响应流）。
- 并发：解析出目标后**立即释放配置读锁**（把 `(baseUrl, apiKey, authStyle, upstreamModel, beta追加)` 克隆成值再开始转发），因为子 agent 会并发发起多个流式请求。
- **不在流上做任何解析/统计**（因此本期没有 token 计数，见 2.2）。这是为了把"零缓冲"这一硬要求的出错面压到最小。
- 非流式响应同样透传，不解析。

### 6.7 请求日志
内存环形缓冲，最近 500 条；可选落 jsonl。每条字段：

```jsonc
{ "ts": "2026-09-22T10:11:12.345+08:00", "method": "POST", "path": "/v1/messages",
  "requestedModel": "ccr-main", "matchedBy": "alias|family|fallback|passthrough|error",
  "role": "main|fast|subagent|null|逗号连接的多值", "alias": "ccr-kimi-k3",
  "providerId": "kimi", "upstreamModel": "k3",
  "status": 200, "stream": true, "latencyMs": 1834, "error": null }
```

UI 的「状态/日志」页展示这些字段——**这是验证"主 agent 走了 Kimi、subagent 走了 DeepSeek"的凭据**。

`role` 是**反向查得**的：别名 → 引用该别名的角色集合。当 main 与 subagent 恰好指向同一个模型时，二者别名相同、无法区分，此时该字段给出多值（如 `main,subagent`）。这只影响日志展示，不影响可用性与分流正确性。

## 7. 接管 Claude Code

### 7.1 写入的键（只动这些）

```
ANTHROPIC_BASE_URL             = http://127.0.0.1:<port>
ANTHROPIC_AUTH_TOKEN           = <localToken>
ANTHROPIC_DEFAULT_OPUS_MODEL   = <roles.main 所指模型的别名>
ANTHROPIC_DEFAULT_SONNET_MODEL = <roles.main 所指模型的别名>
ANTHROPIC_DEFAULT_FABLE_MODEL  = <roles.main 所指模型的别名>   // 语义待核实，保守按 main 处理
ANTHROPIC_DEFAULT_HAIKU_MODEL  = <roles.fast 所指模型的别名>
CLAUDE_CODE_SUBAGENT_MODEL     = <roles.subagent 所指模型的别名>
```

未绑定的槽位（`roles.* = null`）对应的键**不写入**。

**另需移除**（若存在，它们会把请求钉死在单一模型、绕过别名路由）：`ANTHROPIC_MODEL`、`ANTHROPIC_API_KEY`。

**明确不动**：`ANTHROPIC_DEFAULT_*_MODEL_NAME`（本机现有配置中存在，疑似展示名用途），以及 `env` 中其它键（`API_TIMEOUT_MS`、`CLAUDE_CODE_EFFORT_LEVEL`、`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`…）和 `settings.json` 顶层其它键（`attribution`、`permissions`…）**一律不改**。

> `FABLE` 键出现在本机现有 `settings.json` 中（由 cc-switch 写入），说明它很可能是 Claude Code 的一个真实家族槽位。**保守策略是"写入 + 家族兜底"而不是删除**：既不破坏该家族，又让它走 main 目标。实施时用 `claude --help` 与官方文档核实其确切语义后再决定是否调整。

### 7.2 接管动作
1. 前置检查：网关已启动、配置校验通过、`roles.*` 均已绑定。
2. 读 `~/.claude/settings.json`；文件不存在按 `{}` 处理。
3. **备份**：复制到 `backups\settings.json.<时间戳>.bak`（同一次接管只备份一次；若备份已存在则复用）。
4. **记录还原清单**：写入 `takeover.settingsKeys = { "envExisted": bool, "keys": { "<键名>": {"existed": bool, "value": <原值>} } }`。
   - `keys` 必须覆盖**所有被写入或被移除的键**（含 `ANTHROPIC_MODEL`、`ANTHROPIC_API_KEY` 这类"移除项"，否则无法还原）。
   - `envExisted` 记录顶层原本是否就有 `env` 键：原文件若没有 `env`，还原时必须把 `env` 整体删除，才能做到字节级一致。
   - 这是"精确还原"的唯一依据。
5. 合并写入拥有的键、移除需移除的键；保持 JSON 2 空格缩进、UTF-8 无 BOM；原子写盘。
6. 更新 `takeover.enabled=true`、`appliedAt`、`backupFile`。

### 7.3 还原动作
对 `takeover.settingsKeys.keys` 中每个键：`existed=true` → 写回原值；`existed=false` → 删除该键。若 `envExisted=false`，再把顶层 `env` 整体删除。然后原子写盘。**验收标准：还原后文件内容与接管前字节级一致。** 最后清除 `takeover` 状态。同样处理 8.3 的子 agent 还原清单（被新建的文件需删除）。

### 7.4 防呆与外部冲突告警
- 应用启动时与每次接管后校验：`settings.json` 的 `ANTHROPIC_BASE_URL` 是否仍等于本网关地址。
  - 不等于 → 顶部横幅：「接管已失效，settings.json 被其它程序改写（可能是 cc-switch）」，并提供〔重新接管〕〔还原〕按钮。
- **与 cc-switch 的关系（已与用户确认）**：用户不使用 cc-switch 的路由模式（`enableLocalProxy` 会保持关闭），两者不构成实际冲突。因此只保留通用的"外部改写检测 + 横幅告警"（成本低，且对任何写入者都有效），**不针对 cc-switch 做专门检测，也不做自动重写循环**。
- 网关未运行但已接管 → UI 红色状态 + 明确提示「此时 Claude Code 无法使用，请启动网关或点还原」。

## 8. 子 Agent 管理

### 8.1 作用域
管理 `~/.claude/agents/**/*.md`。当前该目录为空，因此首版必须支持**新建**子 agent，否则"给 subagent 指定模型"无从配置。

### 8.2 每个子 agent 的三选一
| UI 选项 | 写入 frontmatter | 效果 |
|---|---|---|
| 跟随主模型 | `model: inherit` | 与主 agent 同模型 |
| 使用 subagent 默认 | 删除 `model` 行 | 走 `CLAUDE_CODE_SUBAGENT_MODEL` |
| 指定模型 | `model: <别名>` | 该子 agent 单独使用所选模型 |

- 编辑只针对 frontmatter 内的 `model` 行：替换 / 插入 / 删除；frontmatter 其它字段与正文**逐字节保留**。
- **换行风格必须保持**：插入/删除行时沿用原文件的 CRLF/LF 风格，并保持"文件末尾是否有换行"不变，否则无法做到字节级还原。
- 提供「原始 frontmatter 文本框」作为逃生通道（高级用户直接编辑），保存前做 YAML 语法校验。
- 新建 agent 的最少字段：`name`、`description`、`model`（可选）、正文（system prompt）。`tools` 等其它字段通过原始文本框填写。
- `model: inherit` 是本设计的首选写法；**若实施时核实到当前 Claude Code 版本不接受该取值，则该项退化为"删除 `model` 行"并在 UI 明确提示**（不静默失败）。
- 删除 agent 文件前先备份到 `backups\agents\`。

### 8.3 还原清单
对每个被修改/新建的文件记录 `{ "path": …, "existed": bool, "modelLine": <原 model 行文本或 null> }`；新建的文件在还原时删除。写入 `takeover.agentFiles`。

> 子 agent 改动与网关路由**解耦**：即使未接管 settings.json，也可以管理 agent 文件的 `model` 字段。还原动作两者一起回退（因为它们同属"接管"这一状态）。

## 9. 安全

### 9.1 密钥存储（已决策：明文 JSON）
- `config.json` 含明文密钥。文件位于 `%APPDATA%`（默认仅当前用户可访问）。
- README 明确警示：**不要**把 `config.json` 提交到 git、不要分享；导出配置时提供"移除密钥"选项。
- 真密钥**不会**写入 `~/.claude/settings.json`；该文件只出现本地令牌。这比现状（密钥明文写在 settings.json）是净提升。

### 9.2 本地令牌
- 首次运行生成 `sk-ccr-<32位hex>`，持久化在 config.json。
- 任何本地进程都能读到它（与 settings.json 同一信任边界），可接受。
- 提供「重新生成令牌」按钮：重新生成后自动重写 settings.json（属于接管更新，不触发新备份）。

### 9.3 网络面
- 只监听 `127.0.0.1`，不监听 `0.0.0.0`。
- 除转发 Claude Code 的请求、以及「测试连接」按钮外，不发起任何网络请求。**无遥测**。

## 10. 界面规格

### 10.1 状态 / 日志（首页）
- 网关：运行状态、地址、端口、`启动/停止`、`重启`。
- 接管状态：已接管/未接管/已失效（横幅）；`一键接管`、`一键还原`。
- 请求日志表格：时间、requestedModel、matchedBy、role、provider、upstreamModel、状态码、是否流式、延迟；可按 provider 过滤；最近 500 条。
- 顶部状态区实时显示「主 agent → X 厂商 / subagent → Y 厂商」，直接用 5.2 的 `roles` 渲染。

### 10.2 服务商
- 表格：名称、baseUrl、模型数、启用状态、上次拉取时间。
- **新建服务商的第一步是选厂家预设**（下拉 + 搜索，带"已验证/未验证"角标）：选中即自动填好 `baseUrl` / `authStyle` / `modelsPath`，并给出 `docsUrl`、`apiKeyUrl`（一键打开厂商取密钥页）；同时支持"从空白创建"。
- 编辑表单：`id`、`name`、`baseUrl`、`apiKey`（密文显示 + 切换明文）、`authStyle`、`modelsPath`、模型列表 CRUD（`id`、`name`、`alias`(自动生成，可改)、`context1m`、`contextWindow`、`maxTokens`）。
- `测试连接`：向该 provider 发一条最小 `POST /v1/messages`（`max_tokens: 16`，单轮 user），15s 超时，显示状态码、延迟、错误消息。
- `一键获取模型`：见 5.7。拉取成功后多选加入；失败时展示**每个候选 URL 的尝试结果**与三条出路。列表顶部显示"上次拉取：时间 / 来源 URL / 数量"，从未拉取时提示"可一键获取"。

### 10.3 角色路由
- 三个下拉：`main` / `fast` / `subagent`，选项为"provider / 模型"两级展开的全部已配置模型；每个下拉旁显示将写入的别名。
- 显示生成的 env 预览（只读）与「应用到 Claude Code」按钮。
- 额外规则表（`extraRoutes`）：别名 → 目标。
- 未知模型策略：`default` / `error` + `defaultTarget` 选择。

### 10.4 子 Agent
- 列出 `~/.claude/agents/**/*.md`：文件名、`name`、`description`、当前 `model`、生效说明。
- 每行一个模型下拉（跟随主模型 / 使用 subagent 默认 / 指定模型）＋原始 frontmatter 编辑入口。
- `新建子 Agent`、`删除`。
- 保存前显示 diff 预览（只影响 `model` 行时高亮该行）。

### 10.5 设置
- 端口、开机自启、关窗行为（最小化到托盘 / 退出）、退出时自动还原、请求日志落盘开关。
- 令牌：显示/复制/重新生成。
- 备份列表（含时间与来源）与 `还原到某个备份`。
- 配置导入 / 导出（导出可选移除密钥）。

### 10.6 前端技术
- Vue 3 + TypeScript + Vite + Naive UI。状态管理用 Pinia。
- Rust ↔ 前端通过 Tauri `invoke` 命令 + 事件（`gateway://log`、`gateway://status`）推送日志与状态。

## 11. 测试策略与验收标准

### 11.1 单元测试（Rust）
- 别名生成与碰撞处理（含超长、含非法字符、含 `[1M]` 后缀输入）。
- 路由解析：别名命中 / 大小写与空白容错 / 家族兜底（opus、sonnet、fable、haiku、`claude-3-5-sonnet-*`）/ 兜底槽位为 `null` 时落入未知模型路径 / 未知模型两种策略。
- `[1M]` 判定与 beta 头追加（去重、与既有 `anthropic-beta` 合并、上游模型名不含后缀）。
- 鉴权注入三种 `authStyle`；确认客户端原始鉴权头被剥离。
- 请求头过滤/透传白名单；`content-length` 重算。
- 配置校验各条规则（含 baseUrl 自环检测、`modelsPath` 形式校验）。
- 预设加载与合并：内置 `presets.json` + 用户 `presets.user.json` 同 `id` 覆盖；`verifiedAt: null` 的角标标注逻辑。
- 模型列表响应解析：Anthropic 形状 / OpenAI 形状 / 裸数组 / 缺字段 / 空列表。
- 疑似非对话模型的识别启发式。
- 模型 id 结尾 `[1m]` 被剥离且 `context1m` 置 `true`。
- 获取模型的候选 URL 顺序与"命中即停"；全部失败时各候选结果的聚合。
- 错误体构造：401 / 400 / 413 / 502 / 504 的 JSON 形状与 `error.type`。

### 11.2 集成测试（不访问外网）
- 用 hyper 起 mock 上游，断言收到的 `model`、鉴权头、`anthropic-beta`、路径与 query 正确。
- **多模型分流**：同一网关上连续请求两个别名，断言分别到达两个 mock 上游。
- **SSE 零缓冲**：mock 上游分块发送、块间 sleep 300ms；断言客户端**首块到达时间**显著早于整体结束时间（证明未缓冲）。
- 并发：16 个并发流式请求，断言各自响应体不串流、绑定正确。
- 客户端中途断开：断言上游请求被取消。
- 大请求体：超过上限返回 413。
- 接管夹具测试：对 `settings.json` 的多种夹具（不存在 / `{}` / 缺 env / 有多余键 / 已有我们全部键 / 非法 JSON）断言「接管 → 还原」字节级等于原文件，并断言备份内容等于原文件。
- 子 agent 夹具测试：断言只改 `model` 行，frontmatter 其它字段与正文未变；还原后字节级等于原文件。
- **一键获取模型**：mock 上游让候选 1 返回 404、候选 2 返回 200，断言最终采用候选 2、`modelsFetch.lastUrl` 记录正确、模型解析正确、拉取请求带上了正确鉴权头；再断言下一次拉取优先试 `lastUrl`。
- 拉取全部失败：断言返回信息足以渲染三条出路（含每个候选的 URL 与状态码）。

### 11.3 手工 E2E 验收清单（真 Claude Code + 真厂商）
1. `roles`: main=Kimi、subagent=DeepSeek、fast=DeepSeek → 一键接管 → 重启 `claude`。
2. 主 agent 提问，日志出现 `matchedBy=alias`、`provider=kimi`。
3. 触发一次 Task 子 agent，日志出现 `provider=deepseek`。
4. 在某子 agent 上指定第三个模型，触发该子 agent，日志显示对应 provider。
5. `/model haiku` → 日志 `role=fast`。
6. `一键还原` → `settings.json` 回到接管前；Claude Code 恢复直连原端点可正常使用。

### 11.4 验收标准（可判定）
| 编号 | 标准 |
|---|---|
| AC1 | `GET /ccr/health` 返回 200 且含端口与运行时长 |
| AC2 | 别名请求到达正确 provider，且上游收到 `target.modelId` 与正确密钥 |
| AC3 | 两个别名在同一网关下到达两个不同 provider（多模型分流成立） |
| AC4 | 流式首块到达时间 < 上游首块 + 200ms |
| AC5 | 无令牌请求返回 401 Anthropic 错误体 |
| AC6 | `context1m=false` 时不出现 `context-1m-2025-08-07`；为 `true` 时出现，且上游模型名无 `[1M]` |
| AC7 | `claude-haiku-*` 无别名命中时走 `roles.fast` |
| AC8 | 接管后 settings.json 仅新增我们拥有的键、其它键不变、备份文件存在；还原后字节级等于接管前 |
| AC9 | 手工 E2E 第 2、3 步通过（主 agent 与 subagent 命中不同 provider） |
| AC10 | 选择任一预设后 `baseUrl` / `authStyle` / `modelsPath` 被正确预填，且 `apiKey` 保持为空 |
| AC11 | `一键获取模型` 能从 mock 上游解析出模型列表并把所选模型写入 provider；全部候选失败时 UI 展示每个候选的 URL 与状态，并给出三条出路 |
| AC12 | 每个内置预设的 `baseUrl` 都经过实测：通过者标 `verifiedAt`，未通过者标 `null` 并有"未验证"角标 |

## 12. 打包与交付
- `pnpm tauri build` 产出 NSIS 安装包（`.exe`，预期 5–10 MB）+ 可选 MSI。
- 托盘图标 + 右键菜单（显示主窗口 / 启停网关 / 接管 / 还原 / 退出）。
- 开机自启（Tauri autostart 插件）。
- 内置 `presets.json`（厂家预设数据；可被 `%APPDATA%\cc-router\presets.user.json` 覆盖，厂商改 URL 时无需等应用发版）。
- 交付物：安装包、README（安装、首次配置、厂家预设与一键获取模型、接管/还原、外部改写告警、故障排查、明文密钥警示）。

## 13. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| **cargo/crates.io 不可达**，Tauri 全量构建需数百 crate（缓存中仅部分存在） | 阻塞全部工作 | **实施第 0 步**：先构建"空 Tauri + Vue3 + Naive UI"骨架并产出 exe，验证工具链并预热缓存；不通则切 rsproxy/TUNA 镜像（`~/.cargo` 已有 TUNA 缓存痕迹） |
| 与 cc-switch 争抢 `settings.json` | 低（用户不使用 cc-switch 路由模式） | 7.4：只保留通用"外部改写检测 + 横幅告警"，不做专门检测、不做自动重写循环 |
| **预设 baseUrl 与厂商实际不符**，或厂商不提供模型列表接口 | 用户按预设建好却连不通 / 拉不到模型 | 5.6 要求逐个实测并标 `verifiedAt`，未验证的显示角标；预设为可覆盖的数据文件；5.7 多候选探测 + 失败三出路（见 cc-switch [#6566](https://github.com/farion1231/cc-switch/issues/6566) 的反例） |
| 上游 provider 拒绝未知 `anthropic-beta` 值 | 请求失败 | 只透传客户端已有 beta（本机已证明对 MiMo 可行）；我们**新增**的 1M beta 仅由 `context1m=true` 显式开启，默认关闭 |
| 某些 provider 对 `[1M]` 后缀或 `x-api-key`/`Bearer` 支持不一致 | 请求被拒 | 网关统一剥离后缀；`authStyle=both` 同时发两种头，可按 provider 覆盖 |
| Claude Code 版本变化导致 `ANTHROPIC_DEFAULT_*_MODEL` 语义或 env 键变动 | 别名路由失效 | 家族兜底（6.2 第 2 步）作为不依赖 env 的第二道保险；E2E 清单可快速回归 |
| 大请求体全量读入内存 | 内存峰值 | 128 MiB 上限 + 超限 413；并在 UI 提示 |
| 应用退出后 settings.json 仍指向死端口 | Claude Code 完全不可用 | 默认托盘常驻 + 开机自启；未运行且已接管时红色告警；还原清单保证一键恢复 |

## 14. 里程碑（实施计划将细化）
| 里程碑 | 内容 | 出口条件 |
|---|---|---|
| M0 | 工具链验证：空 Tauri 2 + Vue 3 + Naive UI 骨架 | 产出可运行 exe；缓存/镜像问题解决 |
| M1 | 配置中心 + 网关骨架（health、令牌、非流式路由、别名生成） | AC1/AC2/AC3/AC5/AC6 通过 |
| M2 | SSE 流式 + 家族兜底 + 未知模型策略 + 请求日志 | AC4/AC7 通过 |
| M3 | 接管 / 还原（含备份与还原清单、夹具测试） | AC8 通过 |
| M4 | UI 五页 + 厂家预设 + 测试连接 + 一键获取模型 | 无阻塞缺陷；AC10/AC11/AC12 通过（预设 baseUrl 逐个实测并标注 `verifiedAt`） |
| M5 | 子 Agent 管理（新建 / 模型三选一 / 还原） | 夹具测试通过 |
| M6 | 打包、托盘、开机自启、README、E2E | AC9 通过并交付安装包 |

## 15. 设计决策记录（含被否决的替代方案）
1. **不用独立 sidecar 进程**：网关跑在 Tauri 进程内（与 cc-switch 一致）。否决 sidecar——IPC 与打包复杂度换不来收益，托盘常驻已能保证可用性。
2. **不按真实 Claude 家族名做主路由**：主路由用我们自己的别名；家族名仅作兜底。否决"只按家族名映射"——依赖 Claude Code 内部模型 ID 的稳定性，脆弱且无法表达"任意两个已配置模型分角色"。
3. **不做流上统计**：为保证零缓冲，流不解析，因此不做 token 计数。否决 tee/旁路解析——它是流式路径上最容易引入延迟缺陷的地方。
4. **不做自动重写循环对抗 cc-switch**：只告警不互抢，避免无限拉锯。
5. **密钥明文 JSON（用户决策）**：保留 DPAPI 加密作为后续可选升级，本期实现明文并明确警示。
6. **子 agent 用 frontmatter 而非网关侧分组**：因为 `model:` 是 Claude Code 官方支持的 per-agent 覆盖手段（[sub-agents 文档](https://code.claude.com/docs/en/sub-agents)），无需网关猜测"这次请求来自哪个 agent"。
7. **子 agent 模型指派不进 config.json**：唯一来源是 agent 文件 frontmatter。否决在 config 中再存一份 `agentOverrides`——双份事实来源必然产生不一致。
8. **别名表覆盖全部已配置模型**，而不是只覆盖被角色引用的模型。理由：否则 `GET /v1/models` 列出的别名与实际可路由集合不一致；副作用（可手打任意已配置模型的别名）是正面能力。
9. **`FABLE` 家族保守处理**：写入 `ANTHROPIC_DEFAULT_FABLE_MODEL` 并纳入 main 家族兜底，而不是像 `*_MODEL_NAME` 那样不管或直接删除。理由：它出现在用户当前可用的配置里，很可能是真实家族槽位；语义核实前"写入 + 兜底"风险最低。
10. **未绑定槽位的键不写入**：`roles.*` 为 `null` 时不写对应 env 键。这样"半配置"状态不会把 Claude Code 指向一个无法路由的别名。
11. **预设是数据不是代码**：内置 `presets.json` + 用户 `presets.user.json` 覆盖。否决把厂商 URL 硬编码进代码——厂商改 URL 时不该以"重新发版"为修复前提（cc-switch [#6566](https://github.com/farion1231/cc-switch/issues/6566) 正是预设与实际不符的代价）。
12. **模型列表拉取用多候选探测 + 失败三出路**，而不是单一固定路径。理由：Anthropic 兼容（`{base}/v1/models`）与 OpenAI 兼容（站点根 `/v1/models`）两种约定并存，且部分厂商根本不提供该接口；静默失败会让用户完全无从下手。
13. **`contextWindow` / `maxTokens` 只作展示**，网关不依赖。避免厂商未在列表接口返回这些字段时，产生"必须手填才能用"的假门槛。

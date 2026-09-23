<script setup lang="ts">
/**
 * B5：子 Agent。
 *
 * 数据源是 `~/.claude/agents/` 目录下的 .md 文件（`agents_list`），
 * 每行三选一（spec §8.2）：
 *  - 跟随主模型 → `agent_set_model(path, "inherit")`
 *  - 使用 subagent 默认 → `agent_set_model(path, "subagent_default")`（删掉 `model` 行）
 *  - 指定模型 → `agent_set_model(path, "alias", <别名>)`
 * 新建 / 删除分别走 `agent_create` / `agent_delete`。
 */
import { computed, h, onMounted, reactive, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NEmpty,
  NFormItem,
  NInput,
  NModal,
  NRadio,
  NRadioGroup,
  NSelect,
  NSpace,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { AgentBackupDto, AgentInfo, AgentModelChoice, TargetDto } from "../api/ipc";
import { errorText } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import { useCopy } from "../composables/useCopy";
import ModelPicker from "../components/ModelPicker.vue";
import { useConfigStore } from "../stores/config";

const store = useConfigStore();
const { isBusy, run, message } = useAction();
const { confirm } = useConfirm();
const { copyText } = useCopy();

const agents = ref<AgentInfo[]>([]);
const loading = ref(false);

interface RowState {
  choice: AgentModelChoice;
  target: TargetDto | null;
}
const rowState = reactive<Record<string, RowState>>({});

/**
 * 每行**磁盘上的 `model` 值**（trim 后；空串 = 没有 `model` 行）。
 * 它是「这一行的模型选择是否已经在磁盘上落定」的身份指纹 —— M10 同类修复用。
 */
const diskModel = new Map<string, string>();

const CHOICE_OPTIONS = [
  { label: "跟随主模型（inherit）", value: "inherit" },
  { label: "使用 subagent 默认", value: "subagent_default" },
  { label: "指定模型（别名）", value: "alias" },
];

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** 从一行磁盘数据推出它对应的选择 / 目标。 */
function rowStateFromDisk(agent: AgentInfo): RowState {
  const model = agent.model?.trim() ? agent.model.trim() : null;
  let choice: AgentModelChoice = "subagent_default";
  if (model) choice = model.toLowerCase() === "inherit" ? "inherit" : "alias";
  const matched = choice === "alias" ? store.modelByAlias(model) : null;
  return {
    choice,
    target: matched ? { providerId: matched.providerId, modelId: matched.modelId } : null,
  };
}

/** 磁盘指纹（`agent.model` 的归一化值）。 */
function modelSignature(agent: AgentInfo): string {
  return agent.model?.trim() ?? "";
}

/**
 * **按身份合并**，而不是整体覆盖（M10 同类修复，与 RolesView / SettingsView 同一套做法）。
 *
 * 行身份 = `agent.path`（`rowState` 的键，也是表格的 `row-key`）；
 * 是否需要重灌 = 该行**磁盘上的 `model` 值**是否真的变了。于是：
 *  - 用户选了「指定模型」但还没挑具体模型（`changeChoice` 有意不落盘）：磁盘没变
 *    ⇒ 保留草稿对象，点「刷新」或去改别的行都不会再把它打回磁盘值；
 *  - 这一行的 `model` 真的在磁盘上变了（本页写盘、或外部改文件）：指纹不同
 *    ⇒ 重灌为磁盘真值，界面跟着上游走，磁盘的新值不会被旧草稿悄悄盖掉；
 *  - 列表里不再存在的行：连同指纹一起清掉，不留残留。
 */
function syncRows(): void {
  const seen = new Set<string>();
  for (const agent of agents.value) {
    seen.add(agent.path);
    const signature = modelSignature(agent);
    if (rowState[agent.path] && diskModel.get(agent.path) === signature) continue;
    diskModel.set(agent.path, signature);
    rowState[agent.path] = rowStateFromDisk(agent);
  }
  for (const path of Object.keys(rowState)) {
    if (seen.has(path)) continue;
    delete rowState[path];
    diskModel.delete(path);
  }
}

/** 把某一行强制拉回磁盘真值（写盘失败时用：那次乐观选择并没有落到 frontmatter 上）。 */
function resetRowFromDisk(path: string): void {
  const agent = agents.value.find((item) => item.path === path);
  if (!agent) return;
  diskModel.set(path, modelSignature(agent));
  rowState[path] = rowStateFromDisk(agent);
}

async function load(): Promise<void> {
  loading.value = true;
  try {
    agents.value = await ipc.agentsList();
    syncRows();
  } catch (err) {
    message.error(errorText(err), { duration: 8000, closable: true });
  } finally {
    loading.value = false;
  }
  // 备份清单与 agent 列表总是成对变化（删除/改动都会产生备份），顺手一起刷新。
  await loadAgentBackups();
}

// ---------------------------------------------------------------- 子 Agent 备份清单

/**
 * 删除/改动子 Agent 时会把原文件备份到 `backups/agents/`，但**设置页的备份列表看不到它**
 * （那个命令只列 `backups/` 顶层文件）。用户反馈"提示会备份，事后不好找"，所以在这里列出来：
 * 名称、原路径、备份文件路径，两条路径都点击即复制。
 */
const agentBackups = ref<AgentBackupDto[]>([]);
const backupsLoading = ref(false);

async function loadAgentBackups(): Promise<void> {
  backupsLoading.value = true;
  try {
    agentBackups.value = await ipc.agentBackupsList();
  } catch (err) {
    message.error(`读取子 Agent 备份失败：${errorText(err)}`, { duration: 8000, closable: true });
  } finally {
    backupsLoading.value = false;
  }
}

const KIND_META: Record<string, { text: string; type: "error" | "warning" | "default" }> = {
  deleted: { text: "已删除", type: "error" },
  modified: { text: "改动前备份", type: "warning" },
  unknown: { text: "来源未知", type: "default" },
};

/** 删除一条备份（不可撤销，所以先确认；并如实说明代价）。 */
async function removeBackup(row: AgentBackupDto): Promise<void> {
  const go = await confirm({
    title: "删除这个备份？",
    content:
      `将永久删除备份文件：${row.backupPath}\n\n` +
      "删除备份不影响你当前的配置或子 Agent 文件；代价是这个文件将来需要「一键还原」时，" +
      "少了一份可以逐字节回放的原文。此操作不可撤销。",
    positiveText: "删除",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      await ipc.agentBackupDelete(row.backupPath);
      await loadAgentBackups();
    },
    "备份已删除",
    `rm-backup-${row.backupPath}`,
  );
}

function formatBackupTime(iso: string | null): string {
  if (!iso) return "—";
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime()) ? iso : parsed.toLocaleString();
}

/** 可点击复制的路径单元格：点击即复制，光标与下划线提示"这里能点"。 */
function pathCell(text: string | null, label: string, guess = false) {
  if (!text) {
    return h(NText, { depth: 3 }, { default: () => "（无法从备份名确定原文件，用右侧备份文件即可取回）" });
  }
  return h(
    "span",
    {
      class: "mono",
      title: "点击复制这个路径",
      style: "cursor: pointer; text-decoration: underline dotted; text-underline-offset: 2px;",
      onClick: () => copyText(text, label),
    },
    // 推测出来的路径要标出来：备份名里的 `_` 无法区分"路径分隔符"和"文件名里本来就有的下划线"
    guess ? `${text}（推测）` : text,
  );
}

const backupColumns = computed<DataTableColumns<AgentBackupDto>>(() => [
  {
    title: "名称",
    key: "name",
    width: 180,
    ellipsis: { tooltip: true },
    render: (row) => h(NText, { strong: true }, { default: () => row.name }),
  },
  {
    title: "状态",
    key: "kind",
    width: 110,
    render: (row) => {
      const meta = KIND_META[row.kind] ?? KIND_META.unknown;
      return h(NTag, { size: "small", type: meta.type }, { default: () => meta.text });
    },
  },
  {
    title: "原文件路径（点击复制）",
    key: "agentPath",
    minWidth: 300,
    ellipsis: { tooltip: true },
    render: (row) => pathCell(row.agentPath, "原文件路径", row.agentPathIsGuess),
  },
  {
    title: "备份文件（点击复制）",
    key: "backupPath",
    minWidth: 300,
    ellipsis: { tooltip: true },
    render: (row) => pathCell(row.backupPath, "备份文件路径"),
  },
  {
    title: "备份时间",
    key: "modifiedAt",
    width: 170,
    render: (row) => formatBackupTime(row.modifiedAt),
  },
  {
    title: "大小",
    key: "sizeBytes",
    width: 90,
    render: (row) => `${row.sizeBytes} B`,
  },
  {
    title: "操作",
    key: "actions",
    width: 100,
    // 固定在最右：这张表也宽于默认窗口，不固定则按钮够不到
    fixed: "right",
    render: (row) =>
      h(
        NButton,
        {
          size: "small",
          type: "error",
          quaternary: true,
          loading: isBusy(`rm-backup-${row.backupPath}`),
          onClick: () => void removeBackup(row),
        },
        { default: () => "删除" },
      ),
  },
]);

onMounted(() => {
  void load();
});

/** 生效说明：把 frontmatter 里的 model 值翻译成「会走哪条路」。 */
function effectText(agent: AgentInfo): string {
  const model = agent.model?.trim() ? agent.model.trim() : null;
  if (!model) return "使用 subagent 默认：走 CLAUDE_CODE_SUBAGENT_MODEL（角色路由页的 subagent 槽位）";
  if (model.toLowerCase() === "inherit") return "跟随主模型：与主 agent 同模型";
  const matched = store.modelByAlias(model);
  if (matched) return `指定模型：${matched.providerName} / ${matched.modelName}`;
  return `指定别名 ${model}（不在本应用当前配置里，网关可能不识别）`;
}

async function setModel(
  agent: AgentInfo,
  choice: AgentModelChoice,
  alias: string | null,
): Promise<boolean> {
  const ok = await run(
    async () => {
      await ipc.agentSetModel(agent.path, choice, alias);
      await load();
    },
    `已更新 ${fileName(agent.path)} 的模型`,
    `agent-${agent.path}`,
  );
  if (ok) return true;
  // 写盘失败：`run` 已经弹错（spec §5.7 不允许静默失败），而这次选择**并没有**落到 frontmatter 上。
  // 按身份合并的 syncRows 会把这份乐观选择当成"用户草稿"保留下来 —— 那会让行内选择
  // 显示一个磁盘上并不存在的值，所以这里显式拉回磁盘真值。
  await load();
  resetRowFromDisk(agent.path);
  return false;
}

function changeChoice(agent: AgentInfo, choice: AgentModelChoice): void {
  const state = rowState[agent.path];
  if (!state) return;
  state.choice = choice;
  if (choice === "alias") {
    // 选了「指定模型」但还没挑具体模型：等 ModelPicker 的回调再落盘，
    // 避免中间态把 frontmatter 写成空。
    if (state.target) void applyAlias(agent, state.target);
    return;
  }
  state.target = null;
  void setModel(agent, choice, null);
}

function applyAlias(agent: AgentInfo, target: TargetDto | null): void {
  const state = rowState[agent.path];
  if (state) state.target = target;
  if (!target) return;
  const alias = store.aliasOf(target);
  if (!alias) {
    message.error("该模型不在当前配置中，无法作为别名写入");
    return;
  }
  void setModel(agent, "alias", alias);
}

async function removeAgent(agent: AgentInfo) {
  const go = await confirm({
    title: "删除子 Agent",
    content: `删除 ${fileName(agent.path)}？删除前会先备份到 %APPDATA%\\cc-router\\backups\\agents\\。`,
    positiveText: "删除",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      await ipc.agentDelete(agent.path);
      await load();
    },
    "子 Agent 已删除",
    `rm-${agent.path}`,
  );
}

// ---------------------------------------------------------------- 新建

const createOpen = ref(false);
const draft = reactive({
  name: "",
  description: "",
  choice: "subagent_default" as AgentModelChoice,
  target: null as TargetDto | null,
  body: "",
});

function openCreate(): void {
  draft.name = "";
  draft.description = "";
  draft.choice = "subagent_default";
  draft.target = null;
  draft.body = "";
  createOpen.value = true;
}

function createAgent() {
  const name = draft.name.trim();
  const description = draft.description.trim();
  if (!name) {
    message.warning("请填写名称（Claude Code 里用 @名称 调用它）");
    return Promise.resolve(false);
  }
  if (!description) {
    message.warning("请填写描述（Claude Code 靠它决定什么时候用这个子 Agent）");
    return Promise.resolve(false);
  }
  let alias: string | null = null;
  if (draft.choice === "alias") {
    alias = store.aliasOf(draft.target);
    if (!alias) {
      message.warning("请为「指定模型」选择一个已配置的模型");
      return Promise.resolve(false);
    }
  }
  return run(
    async () => {
      const path = await ipc.agentCreate({
        name,
        description,
        choice: draft.choice,
        alias,
        body: draft.body,
      });
      createOpen.value = false;
      await load();
      message.success(`已创建 ${fileName(path)}`);
    },
    undefined,
    "create-agent",
  );
}

// ---------------------------------------------------------------- 表格

const columns = computed<DataTableColumns<AgentInfo>>(() => [
  {
    title: "文件 / 名称",
    key: "name",
    width: 220,
    render: (row) =>
      h(NSpace, { vertical: true, size: 0 }, {
        default: () => [
          h(NText, { strong: true }, { default: () => row.name || "（无 name）" }),
          h("div", { class: "mono" }, fileName(row.path)),
        ],
      }),
  },
  {
    // 「模型指派」是这一页唯一真正要操作的东西，放在最前面：
    // 默认窗口（1100×720）下可用宽度只有 800 出头，排在两个长文本列后面就会被挤出可视区。
    title: "模型指派",
    key: "assign",
    width: 470,
    render: (row) => {
      const state = rowState[row.path] ?? { choice: "subagent_default" as AgentModelChoice, target: null };
      const children = [
        h(NSelect, {
          value: state.choice,
          options: CHOICE_OPTIONS,
          size: "small",
          style: "width: 220px",
          "onUpdate:value": (value: unknown) => changeChoice(row, value as AgentModelChoice),
        }),
      ];
      if (state.choice === "alias") {
        children.push(
          h(ModelPicker, {
            modelValue: state.target,
            size: "small",
            placeholder: "选择要指定的模型",
            "onUpdate:modelValue": (target: TargetDto | null) => applyAlias(row, target),
          }),
        );
      }
      return h(NSpace, { size: 6, align: "center" }, { default: () => children });
    },
  },
  {
    title: "frontmatter 里的 model",
    key: "model",
    width: 150,
    ellipsis: { tooltip: true },
    render: (row) =>
      row.model
        ? h("span", { class: "mono" }, row.model)
        : h(NText, { depth: 3 }, { default: () => "（无 model 行）" }),
  },
  {
    title: "生效说明",
    key: "effect",
    width: 200,
    ellipsis: { tooltip: true },
    render: (row) => effectText(row),
  },
  {
    title: "描述",
    key: "description",
    width: 180,
    ellipsis: { tooltip: true },
    render: (row) => row.description ?? h(NText, { depth: 3 }, { default: () => "—" }),
  },
  {
    title: "操作",
    key: "actions",
    width: 100,
    // 固定在最右：表格比窗口宽（默认窗口就会），不固定则按钮够不到
    fixed: "right",
    render: (row) =>
      h(
        NButton,
        {
          size: "small",
          type: "error",
          quaternary: true,
          loading: isBusy(`rm-${row.path}`),
          onClick: () => void removeAgent(row),
        },
        { default: () => "删除" },
      ),
  },
]);
</script>

<template>
  <div>
    <n-card size="small" title="子 Agent（~/.claude/agents/*.md）">
      <template #header-extra>
        <n-space>
          <n-button size="small" :loading="loading" @click="load">刷新</n-button>
          <n-button size="small" type="primary" @click="openCreate">+ 新建子 Agent</n-button>
        </n-space>
      </template>

      <n-alert type="info" style="margin-bottom: 12px">
        <b>这一页只管「单个具名子 Agent 的模型覆盖」</b>（<span class="mono">~/.claude/agents/*.md</span> 的 frontmatter）。
        只想给<b>所有</b>子 Agent 设一个默认模型的话，<b>这里不用建任何东西</b> —— 去「角色路由」页配
        <span class="mono">subagent</span> 槽位一次就够了；本页留空完全不影响它。
        <div style="margin-top: 6px">
          三种写法的落盘差异：<b>跟随主模型</b> 写入 <span class="mono">model: inherit</span>；
          <b>使用 subagent 默认</b> 删除 <span class="mono">model</span> 行（于是回落到角色路由页的 subagent 槽位）；
          <b>指定模型</b> 写入 <span class="mono">model: &lt;别名&gt;</span>。
          改动只针对 <span class="mono">model</span> 行，frontmatter 其它字段与正文逐字节保留。
        </div>
        <div style="margin-top: 6px">
          例：只想让 <span class="mono">reviewer</span> 这个子 Agent 用更贵的模型、其余保持默认，就在这里新建
          <span class="mono">reviewer</span> 并选「指定模型」。
        </div>
      </n-alert>

      <n-empty
        v-if="!loading && !agents.length"
        description="这一页是可选的覆盖层，现在没有任何具名子 Agent"
        style="padding: 32px 0"
      >
        <template #extra>
          <n-space vertical align="center">
            <n-text depth="3">
              Claude Code 的 ~/.claude/agents/ 目录当前是空的 ⇒ 所有子 Agent 都走「角色路由」页的 subagent 槽位，
              不需要在这一页做任何事。
            </n-text>
            <n-text depth="3">
              只有当你想要某个具体子 Agent（例如 reviewer）用<b>不同</b>的模型时，才点下面的按钮新建它：
              填名称、描述与 system prompt 即可。
            </n-text>
            <n-button type="primary" @click="openCreate">+ 新建子 Agent</n-button>
          </n-space>
        </template>
      </n-empty>

      <n-data-table
        v-else
        :columns="columns"
        :data="agents"
        :row-key="(row) => row.path"
        :loading="loading"
        :bordered="false"
        size="small"
        :scroll-x="1320"
      />
    </n-card>

    <!--
      用户反馈：删除子 Agent 时提示"会备份到某处"，事后却不好找。
      原因：设置页的 `backups_list` 只列 `backups/` 顶层文件，**子目录 `backups/agents/` 从来不出现**。
      所以在这里把子 Agent 备份单独列出来，并把两条路径都做成"点击即复制"。
    -->
    <n-card size="small" title="已删除 / 已备份的子 Agent" style="margin-top: 12px">
      <template #header-extra>
        <n-button size="small" :loading="backupsLoading" @click="loadAgentBackups">刷新</n-button>
      </template>
      <n-text depth="3">
        删除或改动子 Agent 前，原文件会备份到
        <span class="mono">%APPDATA%\cc-router\backups\agents\</span>（按时间倒序）。
        <b>点路径即可复制</b> —— 需要还原时把备份文件复制回「原文件路径」覆盖即可。
      </n-text>
      <n-data-table
        :columns="backupColumns"
        :data="agentBackups"
        :row-key="(row) => row.backupPath"
        :loading="backupsLoading"
        :bordered="false"
        size="small"
        style="margin-top: 10px"
        :max-height="260"
        :scroll-x="1250"
        :locale="{ empty: '还没有子 Agent 备份（删除或改动子 Agent 时会自动生成）' }"
      />
    </n-card>

    <n-modal v-model:show="createOpen" preset="card" style="width: 760px" title="新建子 Agent">
      <n-space vertical :size="12">
        <n-form-item label="名称（Claude Code 里用 @名称 调用）" :show-feedback="false">
          <n-input v-model:value="draft.name" placeholder="例如 reviewer" />
        </n-form-item>
        <n-form-item label="描述（Claude Code 靠它决定何时调用）" :show-feedback="false">
          <n-input v-model:value="draft.description" placeholder="例如：代码评审，逐条给出可执行的修改建议" />
        </n-form-item>
        <n-form-item label="模型指派" :show-feedback="false">
          <n-space vertical :size="8" style="width: 100%">
            <n-radio-group v-model:value="draft.choice">
              <n-space vertical :size="4">
                <n-radio value="inherit">跟随主模型（model: inherit）</n-radio>
                <n-radio value="subagent_default">使用 subagent 默认（不写 model 行）</n-radio>
                <n-radio value="alias">指定模型（model: 别名）</n-radio>
              </n-space>
            </n-radio-group>
            <div v-if="draft.choice === 'alias'" style="width: 420px">
              <ModelPicker v-model="draft.target" placeholder="选择该子 Agent 使用的模型" />
            </div>
          </n-space>
        </n-form-item>
        <n-form-item label="system prompt 正文" :show-feedback="false">
          <n-input
            v-model:value="draft.body"
            type="textarea"
            :autosize="{ minRows: 6, maxRows: 14 }"
            placeholder="你是一名严格的代码评审员，只关注可验证的缺陷……"
          />
        </n-form-item>
        <n-space>
          <n-button type="primary" :loading="isBusy('create-agent')" @click="createAgent">
            创建
          </n-button>
          <n-button quaternary @click="createOpen = false">取消</n-button>
        </n-space>
        <n-tag v-if="!store.models.length" type="warning" size="small">
          当前还没有配置任何模型：可以先用「使用 subagent 默认」，之后再到角色路由页绑定 subagent 槽位。
        </n-tag>
      </n-space>
    </n-modal>
  </div>
</template>

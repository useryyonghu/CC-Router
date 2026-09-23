<script setup lang="ts">
/**
 * B3：服务商与模型。
 *
 * 三个入口：
 *  1. 「+ 新建服务商」→ 选预设（93 条，可搜索、按 category 分组、official 在前、
 *     `supported: false` 禁用并显示原因）或「+ 自定义」（只要 Base URL + API Key）。
 *     含 `templateValues` 的预设先弹输入框替换 `${VAR}`。**预设绝不预填密钥。**
 *  2. 「一键获取模型」→ `models_fetch` 多候选探测 → 多选加入（`models_add_many`）；
 *     失败时列出每次尝试并列 spec §5.7 的三条出路。
 *  3. 行内「编辑」→ `ProviderForm`（模型 CRUD）+「测试连接」（`provider_test`）。
 */
import { computed, h, onMounted, reactive, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NCollapse,
  NCollapseItem,
  NDataTable,
  NDivider,
  NFormItem,
  NInput,
  NModal,
  NRadio,
  NRadioGroup,
  NScrollbar,
  NSpace,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { FetchedModel, FetchAttempt, NewProviderDto, Preset, Provider, RoleName, TestResult } from "../api/ipc";
import { attemptOutcomeText, errorText, pickFromFetched } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import {
  defaultModelIds,
  filterPresets,
  groupPresets,
  presetVerificationText,
  resolvePresetFields,
  templateInitialValue,
  templateLabel,
  templatePlaceholder,
  templateVariables,
  unstoredTemplateVariables,
} from "../presets";
import { useConfigStore } from "../stores/config";
import ProviderForm from "../components/ProviderForm.vue";

const store = useConfigStore();
const { isBusy, run, message } = useAction();
const { confirm } = useConfirm();

onMounted(() => {
  void loadPresets();
});

async function loadPresets(): Promise<void> {
  if (store.presets.length) return;
  await store.refreshPresets();
}

function presetOf(provider: Provider): Preset | null {
  if (!provider.presetId) return null;
  return store.presets.find((preset) => preset.id === provider.presetId) ?? null;
}

function formatDateTime(ts: string | null | undefined): string {
  if (!ts) return "—";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return ts;
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ---------------------------------------------------------------- 测试连接

const testResults = reactive<Record<string, TestResult>>({});

function testProvider(provider: Provider) {
  return run(
    async () => {
      testResults[provider.id] = await ipc.providerTest(provider.id, null);
    },
    undefined,
    `test-${provider.id}`,
  );
}

// ---------------------------------------------------------------- 删除

async function removeProvider(provider: Provider) {
  const go = await confirm({
    title: "删除服务商",
    content: `删除「${provider.name}」（${provider.id}）？指向它的角色绑定、默认目标与额外规则会被一并清空，已接管的 Claude Code 配置需要重新接管。`,
    positiveText: "删除",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      await ipc.providerRemove(provider.id);
      await store.refreshConfig();
      await store.refreshTakeover();
    },
    "服务商已删除",
    `rm-${provider.id}`,
  );
}

// ---------------------------------------------------------------- 新建

const createOpen = ref(false);
const stage = ref<"pick" | "template" | "form">("pick");
const presetQuery = ref("");
const selectedPresetId = ref("__custom__");
const templateValues = reactive<Record<string, string>>({});
const resolvedInitial = ref<NewProviderDto | null>(null);
const unresolvedVars = ref<string[]>([]);

const selectedPreset = computed<Preset | null>(() =>
  selectedPresetId.value === "__custom__"
    ? null
    : store.presets.find((preset) => preset.id === selectedPresetId.value) ?? null,
);

const filteredGroups = computed(() => groupPresets(filterPresets(store.presets, presetQuery.value)));

const selectedPresetVars = computed(() => {
  const preset = selectedPreset.value;
  return preset ? templateVariables(preset) : [];
});

/** 预设声明了、但本版本不保存的变量（只提示，不向用户索要；见 I3）。 */
const unstoredPresetVars = computed(() => {
  const preset = selectedPreset.value;
  return preset ? unstoredTemplateVariables(preset) : [];
});

function openCreate(): void {
  createOpen.value = true;
  stage.value = "pick";
  presetQuery.value = "";
  selectedPresetId.value = "__custom__";
  resolvedInitial.value = null;
  unresolvedVars.value = [];
  for (const key of Object.keys(templateValues)) delete templateValues[key];
  void loadPresets();
}

function nextFromPick(): void {
  const preset = selectedPreset.value;
  if (!preset) {
    resolvedInitial.value = { baseUrl: "", apiKey: "", authStyle: "both" };
    stage.value = "form";
    return;
  }
  const vars = templateVariables(preset);
  if (vars.length) {
    for (const key of Object.keys(templateValues)) delete templateValues[key];
    // 只填**真默认值**，不填 placeholder（示例）—— 否则用户直接点「替换并继续」
    // 就把示例当值写进地址，而"还有变量没填"的警告因值为非空而永不触发。
    for (const name of vars) templateValues[name] = templateInitialValue(preset, name);
    stage.value = "template";
    return;
  }
  applyPreset({});
}

function applyPreset(values: Record<string, string>): void {
  const preset = selectedPreset.value;
  if (!preset) return;
  const resolved = resolvePresetFields(preset, values);
  unresolvedVars.value = resolved.unresolved;
  resolvedInitial.value = {
    baseUrl: resolved.baseUrl,
    apiKey: "",
    authStyle: preset.authStyle,
    presetId: preset.id,
    ...(resolved.modelsUrl ? { modelsUrl: resolved.modelsUrl } : {}),
  };
  stage.value = "form";
}

function onCreated(id: string, fetchModels: boolean): void {
  createOpen.value = false;
  if (fetchModels && store.providerById(id)) {
    openFetch(id);
  } else {
    message.success("服务商已创建。可点行内「一键获取模型」拉取模型列表");
  }
}

// ---------------------------------------------------------------- 一键获取模型

const fetchOpen = ref(false);
const fetchProviderId = ref<string | null>(null);
const fetchState = ref<"loading" | "ok" | "error">("loading");
const fetchError = ref("");
const fetchedModels = ref<FetchedModel[]>([]);
const fetchUsedUrl = ref("");
const fetchAttempts = ref<FetchAttempt[]>([]);
const fetchQuery = ref("");
/** NDataTable 的勾选键是 `Array<string | number>`。 */
const checkedIds = ref<Array<string | number>>([]);
const manualUrl = ref("");
const manualModelId = ref("");
const manualModelName = ref("");
const fallbackBusy = ref<string | null>(null);

const fetchProvider = computed<Provider | null>(() =>
  fetchProviderId.value ? store.providerById(fetchProviderId.value) : null,
);

const presetForFetch = computed<Preset | null>(() => {
  const provider = fetchProvider.value;
  if (!provider) return null;
  return presetOf(provider);
});

const fallbackIds = computed(() =>
  defaultModelIds(presetForFetch.value).filter((id) => !isAdded(id)),
);

function isAdded(modelId: string): boolean {
  return fetchProvider.value?.models.some((model) => model.id === modelId) ?? false;
}

const visibleModels = computed(() => {
  const needle = fetchQuery.value.trim().toLowerCase();
  if (!needle) return fetchedModels.value;
  return fetchedModels.value.filter(
    (model) =>
      model.id.toLowerCase().includes(needle) ||
      (model.displayName ?? "").toLowerCase().includes(needle),
  );
});

function openFetch(providerId: string): void {
  fetchProviderId.value = providerId;
  fetchOpen.value = true;
  fetchedModels.value = [];
  fetchAttempts.value = [];
  fetchUsedUrl.value = "";
  checkedIds.value = [];
  fetchQuery.value = "";
  fetchError.value = "";
  manualUrl.value = "";
  manualModelId.value = "";
  manualModelName.value = "";
  void runFetch();
}

async function runFetch(url?: string): Promise<void> {
  const provider = fetchProvider.value;
  if (!provider) return;
  fetchState.value = "loading";
  fetchError.value = "";
  try {
    const outcome = await ipc.modelsFetch(provider.id, url ?? null);
    fetchedModels.value = outcome.models;
    fetchUsedUrl.value = outcome.usedUrl;
    fetchAttempts.value = outcome.attempts;
    checkedIds.value = outcome.models
      .filter((model) => !model.looksNonChat && !isAdded(model.id))
      .map((model) => model.id);
    fetchState.value = "ok";
  } catch (err) {
    fetchError.value = errorText(err);
    fetchState.value = "error";
  } finally {
    await store.refreshConfig();
  }
}

async function addSelected(): Promise<boolean> {
  const provider = fetchProvider.value;
  if (!provider) return false;
  const picks = fetchedModels.value
    .filter((model) => checkedIds.value.includes(model.id) && !isAdded(model.id))
    .map(pickFromFetched);
  if (!picks.length) {
    message.warning("没有可加入的模型（已添加的不会重复加入）");
    return false;
  }
  // 拉取成功后默认会把所有模型都勾上（见 `runFetch`），一点就是几十个 —— 每个模型都占用一个
  // 全局唯一别名，加错了要一个个删。所以一次加很多之前先确认。
  if (picks.length > 5) {
    const go = await confirm({
      title: `一次加入 ${picks.length} 个模型？`,
      content:
        `每个模型都会生成一个全局唯一的别名并写入配置。若厂商返回了几十个模型而你只想用其中几个，` +
        `建议先在列表里取消勾选，或关闭本弹窗后用「手动输入模型名」只加需要的那个。`,
      positiveText: `加入 ${picks.length} 个`,
    });
    if (!go) return false;
  }
  return run(
    async () => {
      const aliases = await ipc.modelsAddMany(provider.id, picks);
      await store.refreshConfig();
      // 已加入的模型在下一次拉取前不能再选，顺手把勾选清掉，避免"勾着却加不进去"。
      const added = new Set(picks.map((pick) => pick.id));
      checkedIds.value = checkedIds.value.filter((id) => !added.has(String(id)));
      message.success(`已加入 ${aliases.length} 个模型：${aliases.join("、")}`);
      await offerRoleBinding(provider.id);
    },
    undefined,
    "add-many",
  );
}

/** 出路 3：用预设的 defaultModels 离线兜底（spec §5.7）。 */
async function addFallbackModel(modelId: string): Promise<void> {
  const provider = fetchProvider.value;
  if (!provider) return;
  fallbackBusy.value = modelId;
  try {
    const aliases = await ipc.modelsAddMany(provider.id, [{ id: modelId }]);
    await store.refreshConfig();
    message.success(`已加入兜底模型，别名：${aliases.join("、")}`);
    await offerRoleBinding(provider.id);
  } catch (err) {
    message.error(errorText(err), { duration: 8000, closable: true });
  } finally {
    fallbackBusy.value = null;
  }
}

/** 出路 2：手动输入模型名（spec §5.7）。 */
function addManualModel() {
  const provider = fetchProvider.value;
  if (!provider) return Promise.resolve(false);
  const modelId = manualModelId.value.trim();
  if (!modelId) {
    message.warning("请填写模型名");
    return Promise.resolve(false);
  }
  return run(
    async () => {
      const alias = await ipc.modelAdd(provider.id, modelId, {
        name: manualModelName.value.trim() || null,
        context1m: /\[1m\]$/i.test(modelId),
      });
      await store.refreshConfig();
      manualModelId.value = "";
      manualModelName.value = "";
      message.success(`已手动加入模型，别名：${alias}`);
      await offerRoleBinding(provider.id);
    },
    undefined,
    "add-manual",
  );
}

/** 创建 / 加入模型后，若角色槽位还空着，顺手问一次（spec §5.8「创建后一键收尾」）。 */
async function offerRoleBinding(providerId: string): Promise<void> {
  const provider = store.providerById(providerId);
  const first = provider?.models[0];
  if (!provider || !first) return;
  // 三个槽位都要问：`fast` 没绑会同样拦住「一键接管」（后端要求 main/fast/subagent 都有值），
  // 只问 main/subagent 会让用户以为已经收尾完成。
  const roleLabels: Record<RoleName, string> = {
    main: "主模型（main）",
    fast: "快速任务（fast）",
    subagent: "子 Agent（subagent）",
  };
  for (const role of ["main", "fast", "subagent"] as RoleName[]) {
    if (store.roleTargets[role] !== null) continue;
    const label = roleLabels[role];
    const go = await confirm({
      title: "顺手绑定角色？",
      content: `把${label}设为「${provider.name} / ${first.name || first.id}」（别名 ${first.alias}）？未绑定的槽位会拦住「一键接管」。`,
      positiveText: "绑定",
      negativeText: "暂不",
    });
    if (go) {
      await run(
        async () => {
          await ipc.roleSet(role, { providerId: provider.id, modelId: first.id });
          await store.refreshConfig();
        },
        `已把${label}绑定到 ${provider.name} / ${first.name || first.id}`,
        "role-bind",
      );
    }
    return; // 只问一次，避免连环弹窗
  }
}

// ---------------------------------------------------------------- 编辑

const editOpen = ref(false);
const editProviderId = ref<string | null>(null);

function openEdit(providerId: string): void {
  editProviderId.value = providerId;
  editOpen.value = true;
}

// ---------------------------------------------------------------- 表格

const providerColumns: DataTableColumns<Provider> = [
  {
    title: "名称 / id",
    key: "name",
    width: 180,
    render: (row) =>
      h(NSpace, { vertical: true, size: 0 }, {
        default: () => [
          h(NText, { strong: true }, { default: () => row.name }),
          h("div", { class: "mono" }, row.id),
        ],
      }),
  },
  {
    title: "Base URL",
    key: "baseUrl",
    minWidth: 200,
    // 地址很长：不省略就会把表格撑开、把「操作」列挤出可视区
    ellipsis: { tooltip: true },
    render: (row) => h("span", { class: "mono" }, row.baseUrl),
  },
  {
    title: "模型数",
    key: "models",
    width: 80,
    render: (row) => String(row.models.length),
  },
  {
    title: "预设验证状态",
    key: "verified",
    width: 160,
    ellipsis: { tooltip: true },
    render: (row) => {
      const preset = presetOf(row);
      if (!preset) return h(NTag, { size: "small" }, { default: () => "自定义" });
      return h(
        NTag,
        { size: "small", type: preset.verifiedAt ? "success" : "default" },
        { default: () => presetVerificationText(preset) },
      );
    },
  },
  {
    title: "上次拉取",
    key: "modelsFetch",
    width: 180,
    ellipsis: { tooltip: true },
    render: (row) => {
      const info = row.modelsFetch;
      if (!info) return h(NText, { depth: 3 }, { default: () => "从未拉取" });
      return h(NSpace, { vertical: true, size: 0 }, {
        default: () => [
          h("span", {}, formatDateTime(info.lastAt)),
          h("div", { class: "mono" }, `${info.count} 个 · ${info.lastUrl}`),
        ],
      });
    },
  },
  {
    title: "测试连接",
    key: "test",
    width: 180,
    // 失败时的 message 往往很长，180px 会截断 ⇒ 给 tooltip 让用户能看到完整原因
    ellipsis: { tooltip: true },
    render: (row) => {
      const result = testResults[row.id];
      if (!result) return h(NText, { depth: 3 }, { default: () => "—" });
      return h(
        NTag,
        { size: "small", type: result.ok ? "success" : "error" },
        {
          default: () =>
            result.ok
              ? `${result.status ?? 200} · ${result.latencyMs} ms`
              : `${result.status ?? "无状态码"} · ${result.message ?? "失败"}`,
        },
      );
    },
  },
  {
    title: "操作",
    key: "actions",
    width: 320,
    // 固定在最右侧：表格比窗口宽时（默认窗口 1100×720 就会）这一列仍然可见、可点 ——
    // 否则「编辑 / 测试连接 / 一键获取模型 / 删除」会被挤出可视区，用户根本够不到。
    fixed: "right",
    render: (row) =>
      h(NSpace, { size: 4 }, {
        default: () => [
          h(
            NButton,
            { size: "small", onClick: () => openEdit(row.id) },
            { default: () => "编辑" },
          ),
          h(
            NButton,
            {
              size: "small",
              loading: isBusy(`test-${row.id}`),
              onClick: () => void testProvider(row),
            },
            { default: () => "测试连接" },
          ),
          h(
            NButton,
            { size: "small", type: "primary", onClick: () => openFetch(row.id) },
            { default: () => "一键获取模型" },
          ),
          h(
            NButton,
            {
              size: "small",
              type: "error",
              quaternary: true,
              loading: isBusy(`rm-${row.id}`),
              onClick: () => void removeProvider(row),
            },
            { default: () => "删除" },
          ),
        ],
      }),
  },
];

const fetchColumns = computed<DataTableColumns<FetchedModel>>(() => [
  {
    type: "selection",
    disabled: (row) => isAdded(row.id),
  },
  {
    title: "模型 id",
    key: "id",
    minWidth: 240,
    render: (row) => h("span", { class: "mono" }, row.id),
  },
  {
    title: "显示名",
    key: "displayName",
    width: 180,
    render: (row) => row.displayName ?? "",
  },
  {
    title: "上下文窗口",
    key: "contextWindow",
    width: 130,
    render: (row) => (row.contextWindow === null ? "—" : String(row.contextWindow)),
  },
  {
    title: "max_tokens",
    key: "maxTokens",
    width: 120,
    render: (row) => (row.maxTokens === null ? "—" : String(row.maxTokens)),
  },
  {
    title: "标记",
    key: "flags",
    width: 150,
    render: (row) => {
      if (isAdded(row.id)) {
        return h(NTag, { size: "small", type: "success" }, { default: () => "已添加" });
      }
      if (row.looksNonChat) {
        return h(NTag, { size: "small", type: "warning" }, { default: () => "疑似非对话模型" });
      }
      return "";
    },
  },
]);
</script>

<template>
  <div>
    <n-card size="small" title="服务商与模型">
      <template #header-extra>
        <n-space>
          <n-button size="small" :loading="isBusy('refresh')" @click="run(() => store.refreshConfig(), undefined, 'refresh')">
            刷新
          </n-button>
          <n-button size="small" type="primary" @click="openCreate">+ 新建服务商</n-button>
        </n-space>
      </template>

      <n-alert v-if="!store.providers.length" type="info" title="还没有服务商" style="margin-bottom: 12px">
        点「+ 新建服务商」：可以从 <b>93 条厂家预设</b> 里挑一个（会自动填好地址），也可以选「＋ 自定义」，
        粘贴 Base URL 与 API Key 后点「创建并获取模型」。密钥只保存在本机
        <span class="mono">%APPDATA%\cc-router\config.json</span>。
      </n-alert>

      <n-data-table
        :columns="providerColumns"
        :data="store.providers"
        :row-key="(row) => row.id"
        :bordered="false"
        size="small"
        :scroll-x="1150"
        :locale="{ empty: '还没有服务商' }"
      />
    </n-card>

    <!-- ------------------------------------------------------ 新建服务商 -->
    <n-modal
      v-model:show="createOpen"
      preset="card"
      style="width: 780px"
      :title="stage === 'pick' ? '新建服务商：选预设或自定义' : stage === 'template' ? '填写预设变量' : '填写服务商信息'"
    >
      <template v-if="stage === 'pick'">
        <n-space vertical :size="10">
          <n-input
            v-model:value="presetQuery"
            clearable
            placeholder="搜索厂家预设（名称 / id / 分类 / 地址），共 93 条"
          />
          <n-text depth="3">
            7 条预设会列出但禁用：5 条需要协议转换或 OAuth，2 条（AWS Bedrock）需要 Claude Code 带厂商凭证直连、
            会绕过本网关。预设数据来自 farion1231/cc-switch（MIT），不含任何密钥。
          </n-text>
          <n-scrollbar style="max-height: 430px">
            <n-radio-group v-model:value="selectedPresetId">
              <template v-for="group in filteredGroups" :key="group.category">
                <n-divider style="margin: 8px 0">{{ group.label }}（{{ group.presets.length }}）</n-divider>
                <div v-for="preset in group.presets" :key="preset.id" class="preset-row">
                  <n-radio :value="preset.id" :disabled="!preset.supported">
                    <span>{{ preset.name }}</span>
                    <span class="mono" style="margin-left: 6px">{{ preset.id }}</span>
                  </n-radio>
                  <n-tag v-if="preset.verifiedAt" size="tiny" type="success">已验证 {{ preset.verifiedAt }}</n-tag>
                  <n-tag v-else size="tiny">未在本机验证</n-tag>
                  <n-tag v-if="!preset.supported" size="tiny" type="error">{{ preset.unsupportedReason }}</n-tag>
                  <span class="mono" style="color: #999">{{ preset.baseUrl }}</span>
                </div>
              </template>
              <n-divider style="margin: 8px 0">自定义</n-divider>
              <div class="preset-row">
                <n-radio value="__custom__">＋ 自定义服务商（只需 Base URL + API Key）</n-radio>
              </div>
            </n-radio-group>
          </n-scrollbar>
          <!-- 这一步必须能走出去：`nextFromPick()` 负责按所选预设决定进「填变量」还是「填表单」。
               此前这个按钮根本不存在，导致选了预设之后没有任何反应、无法新建服务商。 -->
          <n-space justify="end">
            <n-button quaternary @click="createOpen = false">取消</n-button>
            <n-button type="primary" :disabled="!selectedPresetId" @click="nextFromPick">下一步</n-button>
          </n-space>
        </n-space>
      </template>

      <template v-else-if="stage === 'template'">
        <n-space vertical :size="12">
          <n-alert v-if="selectedPreset" type="info">
            「{{ selectedPreset.name }}」的地址里有需要你填的变量，替换后才会落地。
            <n-text v-if="unstoredPresetVars.length" depth="3" style="display: block; margin-top: 4px">
              该预设还声明了 {{ unstoredPresetVars.join("、") }}，但本版本的服务商只存一个 API Key
              （没有 env 字段），所以这些变量不会被索要、也不会写进配置。
            </n-text>
          </n-alert>
          <n-form-item
            v-for="name in selectedPresetVars"
            :key="name"
            :label="selectedPreset ? templateLabel(selectedPreset, name) : name"
          >
            <n-input
              v-model:value="templateValues[name]"
              :placeholder="selectedPreset ? templatePlaceholder(selectedPreset, name) : ''"
            />
          </n-form-item>
          <n-space>
            <n-button type="primary" @click="applyPreset(templateValues)">替换并继续</n-button>
            <n-button quaternary @click="stage = 'pick'">返回上一步</n-button>
          </n-space>
        </n-space>
      </template>

      <template v-else>
        <n-alert v-if="unresolvedVars.length" type="warning" style="margin-bottom: 12px">
          还有变量没填：{{ unresolvedVars.join("、") }} —— 请把地址里的 <span class="mono">${...}</span> 补全。
        </n-alert>
        <ProviderForm
          mode="create"
          :initial="resolvedInitial"
          :preset="selectedPreset"
          @created="onCreated"
          @cancel="createOpen = false"
        />
      </template>
    </n-modal>

    <!-- ------------------------------------------------------ 一键获取模型 -->
    <n-modal
      v-model:show="fetchOpen"
      preset="card"
      style="width: 880px"
      :title="fetchProvider ? `一键获取模型 — ${fetchProvider.name}` : '一键获取模型'"
    >
      <n-space vertical :size="12">
        <n-alert v-if="fetchProvider" type="default">
          <n-space vertical :size="2">
            <n-text>
              上次拉取：
              <template v-if="fetchProvider.modelsFetch">
                {{ formatDateTime(fetchProvider.modelsFetch.lastAt) }} ·
                <span class="mono">{{ fetchProvider.modelsFetch.lastUrl }}</span> ·
                {{ fetchProvider.modelsFetch.count }} 个
              </template>
              <template v-else>从未拉取（可一键获取）</template>
            </n-text>
            <n-text depth="3" class="mono">Base URL {{ fetchProvider.baseUrl }}</n-text>
          </n-space>
        </n-alert>

        <n-alert v-if="fetchState === 'loading'" type="info" title="正在拉取模型列表…">
          按候选顺序依次尝试（显式 modelsUrl → <span class="mono">{baseUrl}/v1/models</span> →
          <span class="mono">{baseUrl}/models</span> → 剥离兼容子路径后的站点根），命中即停。
        </n-alert>

        <n-space v-if="fetchState === 'ok'" align="center">
          <n-input
            v-model:value="fetchQuery"
            clearable
            placeholder="在拉取结果里搜索模型"
            style="width: 320px"
          />
          <n-text depth="3">共 {{ fetchedModels.length }} 个；勾选 {{ checkedIds.length }} 个</n-text>
          <n-button size="small" @click="runFetch()">重新拉取</n-button>
        </n-space>

        <n-data-table
          v-if="fetchState === 'ok'"
          v-model:checked-row-keys="checkedIds"
          :columns="fetchColumns"
          :data="visibleModels"
          :row-key="(row) => row.id"
          :max-height="330"
          :bordered="false"
          size="small"
          :scroll-x="860"
        />

        <n-collapse v-if="fetchState === 'ok' && fetchAttempts.length > 1">
          <n-collapse-item :title="`探测过程（${fetchAttempts.length} 次尝试，命中 ${fetchUsedUrl}）`">
            <div v-for="attempt in fetchAttempts" :key="attempt.url" class="attempt-row">
              <span class="mono">{{ attempt.url }}</span>
              <n-text depth="3">{{ attemptOutcomeText(attempt.outcome) }}</n-text>
            </div>
          </n-collapse-item>
        </n-collapse>

        <n-alert v-if="fetchState === 'error'" type="error" title="拉取失败（provider 已保留，可继续用下面的三条出路）">
          <pre class="raw">{{ fetchError }}</pre>
          <n-space vertical :size="4" style="margin-top: 8px">
            <n-text depth="3">常见原因：401 = 密钥错或无权限；404 = 厂商不提供模型列表接口；403 = 地区或账号限制。</n-text>
          </n-space>
        </n-alert>

        <template v-if="fetchState === 'error'">
          <n-divider style="margin: 4px 0">出路 1：手动填入完整模型列表 URL 重试</n-divider>
          <n-space>
            <n-input
              v-model:value="manualUrl"
              placeholder="例如 https://api.example.com/v1/models"
              style="width: 420px"
            />
            <n-button @click="runFetch(manualUrl.trim() || undefined)">
              用这个 URL 重试
            </n-button>
          </n-space>

          <n-divider style="margin: 4px 0">出路 2：用预设的离线兜底模型</n-divider>
          <n-space v-if="fallbackIds.length" align="center" :size="6">
            <n-button
              v-for="id in fallbackIds"
              :key="id"
              size="small"
              :loading="fallbackBusy === id"
              @click="addFallbackModel(id)"
            >
              + {{ id }}
            </n-button>
          </n-space>
          <n-text v-else depth="3">
            该服务商没有可用的预设兜底模型（预设未提供 defaultModels，或已全部添加）。
          </n-text>
        </template>

        <!--
          手动输入模型名不该只在「拉取失败」时才有：接口通了、但返回的列表里没有你要的模型
          （或返回空数组）时同样需要它，否则只能关掉弹窗→编辑→模型列表→添加。
        -->
        <template v-if="fetchState !== 'loading'">
          <n-divider style="margin: 4px 0">也可以直接手动输入模型名</n-divider>
          <n-space>
            <n-input v-model:value="manualModelId" placeholder="模型名，例如 k3 或 k3[1M]" style="width: 300px" />
            <n-input v-model:value="manualModelName" placeholder="显示名（可空）" style="width: 220px" />
            <n-button :loading="isBusy('add-manual')" @click="addManualModel">加入模型</n-button>
          </n-space>
        </template>

        <n-space justify="end">
          <n-button quaternary @click="fetchOpen = false">关闭</n-button>
          <n-button
            v-if="fetchState === 'ok'"
            type="primary"
            :loading="isBusy('add-many')"
            @click="addSelected"
          >
            加入所选（{{ checkedIds.length }}）
          </n-button>
        </n-space>
      </n-space>
    </n-modal>

    <!-- ------------------------------------------------------ 编辑服务商 -->
    <n-modal
      v-model:show="editOpen"
      preset="card"
      style="width: 900px"
      :title="editProviderId ? `编辑服务商 — ${editProviderId}` : '编辑服务商'"
    >
      <n-scrollbar style="max-height: 68vh">
        <ProviderForm mode="edit" :provider-id="editProviderId" @cancel="editOpen = false" />
      </n-scrollbar>
    </n-modal>
  </div>
</template>

<style scoped>
.preset-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 0;
  flex-wrap: wrap;
}
.attempt-row {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  font-size: 12px;
}
</style>

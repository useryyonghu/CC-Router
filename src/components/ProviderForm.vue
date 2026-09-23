<script setup lang="ts">
/**
 * B3：服务商编辑表单（含模型列表 CRUD）。
 *
 * 两种模式：
 *  - `create`：从预设或「+ 自定义」进入。**预设绝不预填密钥**（spec §5.6/§10.2）；
 *    自定义模式下只有 Base URL + API Key 两栏（高级项折叠），主按钮「创建并获取模型」。
 *  - `edit`：编辑既有服务商（`provider_update`），含模型 CRUD（`model_add` /
 *    `model_remove`）与「测试连接」（`provider_test`）。
 *
 * 模型字段（name / alias / context1m / contextWindow / maxTokens）的改动先落在本地缓冲，
 * 点「保存服务商」时随 `provider_update` 一起提交；新增 / 删除模型立即生效。
 */
import { computed, h, reactive, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NDataTable,
  NDivider,
  NForm,
  NFormItem,
  NInput,
  NInputNumber,
  NSelect,
  NSpace,
  NSwitch,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { AuthStyle, NewProviderDto, Preset, Provider, TestResult } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import { authStyleLabel } from "../presets";
import { useConfigStore } from "../stores/config";

const props = withDefaults(
  defineProps<{
    mode: "create" | "edit";
    providerId?: string | null;
    /** create 模式的起始字段（已由调用方做过 `${VAR}` 替换）。 */
    initial?: NewProviderDto | null;
    /** create 模式的来源预设；为 null 表示「+ 自定义」。 */
    preset?: Preset | null;
  }>(),
  { providerId: null, initial: null, preset: null },
);

const emit = defineEmits<{
  created: [id: string, fetchModels: boolean];
  cancel: [];
}>();

const store = useConfigStore();
const { isBusy, run, message } = useAction();
const { confirm } = useConfirm();

const isCustom = computed(() => props.mode === "create" && !props.preset);
const showAdvanced = ref(!isCustom.value);

const authStyleOptions = (["both", "x-api-key", "bearer"] as AuthStyle[]).map((style) => ({
  label: authStyleLabel(style),
  value: style,
}));

// ---------------------------------------------------------------- create

const createDraft = reactive({
  name: props.initial?.name ?? "",
  baseUrl: props.initial?.baseUrl ?? "",
  apiKey: "",
  authStyle: (props.initial?.authStyle ?? "both") as AuthStyle,
  modelsUrl: props.initial?.modelsUrl ?? "",
});

function validateCreate(): string | null {
  if (!createDraft.baseUrl.trim()) return "Base URL 不能为空，例如 https://api.example.com/anthropic";
  if (!/^https?:\/\//i.test(createDraft.baseUrl.trim())) return "Base URL 必须以 http:// 或 https:// 开头";
  if (!createDraft.apiKey.trim()) return "API Key 不能为空（预设不带密钥，需要粘贴你自己的）";
  return null;
}

function create(fetchModels: boolean) {
  const problem = validateCreate();
  if (problem) {
    message.warning(problem);
    return Promise.resolve(false);
  }
  return run(
    async () => {
      const dto: NewProviderDto = {
        baseUrl: createDraft.baseUrl.trim(),
        apiKey: createDraft.apiKey.trim(),
        authStyle: createDraft.authStyle,
      };
      if (createDraft.name.trim()) dto.name = createDraft.name.trim();
      if (createDraft.modelsUrl.trim()) dto.modelsUrl = createDraft.modelsUrl.trim();
      if (props.preset) dto.presetId = props.preset.id;
      const id = await ipc.providerAdd(dto);
      await store.refreshConfig();
      emit("created", id, fetchModels);
    },
    // 不拉模型时**不在这里**弹提示：调用方（ProvidersView 的 onCreated）会给出更完整的一句
    // （含「可点行内一键获取模型」），两边都弹就会出现同一动作两条 toast。
    fetchModels ? "服务商已创建，正在获取模型列表…" : undefined,
    "create",
  );
}

// ---------------------------------------------------------------- edit

interface ModelRow {
  id: string;
  name: string;
  alias: string;
  context1m: boolean;
  contextWindow: number | null;
  maxTokens: number | null;
}

const provider = computed<Provider | null>(() =>
  props.providerId ? store.providerById(props.providerId) : null,
);

const editDraft = reactive({
  id: "",
  name: "",
  baseUrl: "",
  apiKey: "",
  authStyle: "both" as AuthStyle,
  modelsUrl: "",
});

const modelRows = ref<ModelRow[]>([]);
const showKey = ref(false);
const newModelId = ref("");
const newModelName = ref("");
const testModelId = ref<string | null>(null);
const testResult = ref<TestResult | null>(null);

/**
 * 从配置快照重建模型行，但**保留已存在行的未保存改动**（按模型 id 对齐）。
 * 加 / 删模型会 `refreshConfig()`，这一步只让表格跟上磁盘上的模型集合，
 * 不会碰到用户正在编辑的显示名 / 别名 / 1M / 上下文窗口 / max_tokens。
 */
function seedModelRows(current: Provider): void {
  const drafts = new Map(modelRows.value.map((row) => [row.id, row]));
  modelRows.value = current.models.map(
    (model) =>
      drafts.get(model.id) ?? {
        id: model.id,
        name: model.name,
        alias: model.alias,
        context1m: model.context1m,
        contextWindow: model.contextWindow,
        maxTokens: model.maxTokens,
      },
  );
}

/**
 * **只在服务商身份（id）变化时**重置草稿：切换服务商、或首次拿到配置快照。
 *
 * 原来 `deep: true` 监听整个 provider 对象，而「加 / 删模型」成功后会
 * `refreshConfig()` ⇒ 用户改过但还没点保存的 API Key / 名称 / Base URL / 别名
 * 会在加一个模型的瞬间被静默丢弃（I2）。身份没变就不重置，这些编辑就能活到保存。
 */
watch(
  () => provider.value?.id ?? null,
  (id) => {
    const current = provider.value;
    if (!id || !current) return;
    editDraft.id = current.id;
    editDraft.name = current.name;
    editDraft.baseUrl = current.baseUrl;
    editDraft.apiKey = current.apiKey;
    editDraft.authStyle = current.authStyle;
    editDraft.modelsUrl = current.modelsUrl ?? "";
    modelRows.value = []; // 换服务商：整表按新身份重建
    seedModelRows(current);
  },
  { immediate: true },
);

function saveProvider() {
  const current = provider.value;
  if (!current) {
    message.error("该服务商已不存在（可能被删除），请关闭后重试");
    return Promise.resolve(false);
  }
  if (!editDraft.baseUrl.trim()) {
    message.warning("Base URL 不能为空");
    return Promise.resolve(false);
  }
  return run(
    async () => {
      const next: Provider = {
        ...current,
        // id 只读：`provider_update` 按 id 定位（provider/mod.rs），改名必然报"不存在"。
        id: current.id,
        name: editDraft.name.trim() || current.id,
        baseUrl: editDraft.baseUrl.trim(),
        apiKey: editDraft.apiKey.trim(),
        authStyle: editDraft.authStyle,
        modelsUrl: editDraft.modelsUrl.trim() ? editDraft.modelsUrl.trim() : null,
        models: modelRows.value.map((row) => ({
          id: row.id,
          name: row.name.trim() || row.id,
          alias: row.alias.trim(),
          context1m: row.context1m,
          contextWindow: row.contextWindow ?? null,
          maxTokens: row.maxTokens ?? null,
        })),
      };
      await ipc.providerUpdate(next);
      await store.refreshConfig();
    },
    "服务商已保存",
    "save-provider",
  );
}

function addModel() {
  const current = provider.value;
  if (!current) return Promise.resolve(false);
  const raw = newModelId.value.trim();
  if (!raw) {
    message.warning("请填写模型名（上游真实模型 id，可带结尾 [1M]）");
    return Promise.resolve(false);
  }
  return run(
    async () => {
      // 后端按 spec §5.4 生成别名：这里只把 [1M] 意图透传过去（后端也会自己剥离后缀）。
      const context1m = /\[1m\]$/i.test(raw);
      const alias = await ipc.modelAdd(current.id, raw, {
        name: newModelName.value.trim() || null,
        context1m,
      });
      await store.refreshConfig();
      // 只把模型行对齐到新快照：API Key / 名称 / Base URL / 其它模型的编辑都留在草稿里。
      const fresh = store.providerById(current.id);
      if (fresh) seedModelRows(fresh);
      newModelId.value = "";
      newModelName.value = "";
      message.success(`已添加模型，别名：${alias}`);
    },
    undefined,
    "add-model",
  );
}

async function removeModel(modelId: string) {
  const current = provider.value;
  if (!current) return;
  const go = await confirm({
    title: "删除模型",
    content: `删除「${current.name}」的模型 ${modelId}？引用它的角色绑定与额外规则会被一并清空。`,
    positiveText: "删除",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      await ipc.modelRemove(current.id, modelId);
      await store.refreshConfig();
      // 同上：只让被删的行消失，未保存的其它编辑不丢。
      const fresh = store.providerById(current.id);
      if (fresh) seedModelRows(fresh);
    },
    "模型已删除",
    `rm-${modelId}`,
  );
}

function testConnection() {
  const current = provider.value;
  if (!current) return Promise.resolve(false);
  return run(
    async () => {
      testResult.value = await ipc.providerTest(current.id, testModelId.value);
    },
    undefined,
    "test",
  );
}

const modelColumns = computed<DataTableColumns<ModelRow>>(() => [
  {
    title: "模型 id（上游）",
    key: "id",
    width: 190,
    render: (row) => h("span", { class: "mono" }, row.id),
  },
  {
    title: "显示名",
    key: "name",
    width: 180,
    render: (row) =>
      h(NInput, {
        value: row.name,
        size: "small",
        placeholder: row.id,
        "onUpdate:value": (value: string) => (row.name = value),
      }),
  },
  {
    title: "别名（写入 Claude Code）",
    key: "alias",
    width: 200,
    render: (row) =>
      h(NInput, {
        value: row.alias,
        size: "small",
        "onUpdate:value": (value: string) => (row.alias = value),
      }),
  },
  {
    title: "1M",
    key: "context1m",
    width: 70,
    render: (row) =>
      h(NSwitch, {
        value: row.context1m,
        size: "small",
        "onUpdate:value": (value: unknown) => (row.context1m = value === true),
      }),
  },
  {
    title: "上下文窗口",
    key: "contextWindow",
    width: 140,
    render: (row) =>
      h(NInputNumber, {
        value: row.contextWindow,
        size: "small",
        placeholder: "仅展示",
        "onUpdate:value": (value: number | null) => (row.contextWindow = value),
      }),
  },
  {
    title: "max_tokens",
    key: "maxTokens",
    width: 130,
    render: (row) =>
      h(NInputNumber, {
        value: row.maxTokens,
        size: "small",
        placeholder: "仅展示",
        "onUpdate:value": (value: number | null) => (row.maxTokens = value),
      }),
  },
  {
    title: "操作",
    key: "actions",
    width: 110,
    // 固定在最右：这张表比弹窗宽，不固定的话「删除」会被挤出可视区
    fixed: "right",
    render: (row) =>
      h(
        NButton,
        {
          size: "small",
          quaternary: true,
          type: "error",
          loading: isBusy(`rm-${row.id}`),
          onClick: () => void removeModel(row.id),
        },
        { default: () => "删除" },
      ),
  },
]);
</script>

<template>
  <!-- ---------------------------------------------------------- 新建 -->
  <n-form v-if="props.mode === 'create'" label-placement="top" size="small">
    <n-alert v-if="props.preset" type="info" style="margin-bottom: 12px">
      <n-space vertical :size="4">
        <n-text strong>预设：{{ props.preset.name }}（{{ props.preset.id }}）</n-text>
        <n-text depth="3">
          分类 {{ props.preset.category }} · 鉴权 {{ authStyleLabel(props.preset.authStyle) }}
          <template v-if="props.preset.apiKeyField"> · 该厂商习惯用 {{ props.preset.apiKeyField }}</template>
        </n-text>
        <n-text depth="3">
          预设只填地址，<b>不含任何密钥</b> —— 请在下面粘贴你自己的 API Key。
        </n-text>
        <n-text v-if="props.preset.websiteUrl" depth="3" class="mono">
          官网 {{ props.preset.websiteUrl }}
        </n-text>
        <n-text v-if="props.preset.apiKeyUrl" depth="3" class="mono">
          取密钥页 {{ props.preset.apiKeyUrl }}
        </n-text>
      </n-space>
    </n-alert>

    <n-form-item label="Base URL（必填）">
      <n-input v-model:value="createDraft.baseUrl" placeholder="https://api.example.com/anthropic" />
    </n-form-item>

    <n-form-item label="API Key（必填，只保存在本机 %APPDATA%\cc-router\config.json）">
      <n-input
        v-model:value="createDraft.apiKey"
        type="password"
        show-password-on="click"
        placeholder="粘贴你的 API Key"
      />
    </n-form-item>

    <n-space v-if="isCustom" style="margin-bottom: 12px">
      <n-button text type="primary" @click="showAdvanced = !showAdvanced">
        {{ showAdvanced ? "收起高级选项" : "高级选项（名称 / 鉴权方式 / 模型列表 URL）" }}
      </n-button>
    </n-space>

    <template v-if="showAdvanced">
      <n-alert type="default" style="margin-bottom: 12px">
        <span class="mono">id</span> 由 Base URL 的域名自动推导（例如
        <span class="mono">api.moonshot.cn/anthropic → moonshot</span>，重名时自动加
        <span class="mono">-2</span>）。后端不支持改名，所以这里不提供 id 输入框 ——
        需要改名时请删除该服务商后重建（角色绑定会一并失效）。
      </n-alert>
      <n-form-item label="名称（留空则用 Base URL 的主机名）">
        <n-input v-model:value="createDraft.name" placeholder="留空自动推导" />
      </n-form-item>
      <n-form-item label="鉴权方式">
        <n-select v-model:value="createDraft.authStyle" :options="authStyleOptions" />
      </n-form-item>
      <n-form-item label="模型列表 URL（留空则按 spec §5.7 自动探测多个候选）">
        <n-input v-model:value="createDraft.modelsUrl" placeholder="https://api.example.com/v1/models" />
      </n-form-item>
    </template>

    <n-space>
      <n-button type="primary" :loading="isBusy('create')" @click="create(true)">
        创建并获取模型
      </n-button>
      <n-button :loading="isBusy('create')" @click="create(false)">仅创建（稍后获取模型）</n-button>
      <n-button quaternary @click="emit('cancel')">取消</n-button>
    </n-space>
  </n-form>

  <!-- ---------------------------------------------------------- 编辑 -->
  <div v-else>
    <n-alert v-if="!provider" type="warning">该服务商已不存在，请关闭本窗口后刷新列表。</n-alert>
    <template v-else>
      <n-form label-placement="top" size="small">
        <n-form-item label="id（由 Base URL 推导，后端按 id 定位服务商 ⇒ 不支持改名）">
          <n-input :value="editDraft.id" readonly />
        </n-form-item>
        <n-form-item label="名称">
          <n-input v-model:value="editDraft.name" />
        </n-form-item>
        <n-form-item label="Base URL">
          <n-input v-model:value="editDraft.baseUrl" />
        </n-form-item>
        <n-form-item label="API Key">
          <n-space align="center" style="width: 100%">
            <n-input
              v-model:value="editDraft.apiKey"
              :type="showKey ? 'text' : 'password'"
              style="flex: 1"
            />
            <n-button size="small" quaternary @click="showKey = !showKey">
              {{ showKey ? "隐藏" : "显示明文" }}
            </n-button>
          </n-space>
        </n-form-item>
        <n-form-item label="鉴权方式">
          <n-select v-model:value="editDraft.authStyle" :options="authStyleOptions" />
        </n-form-item>
        <n-form-item label="模型列表 URL（留空则自动探测；失败时可在「一键获取模型」里手动指定）">
          <n-input v-model:value="editDraft.modelsUrl" placeholder="留空 = 自动探测" />
        </n-form-item>
        <n-form-item label="来源预设">
          <n-tag size="small">{{ provider.presetId || "custom" }}</n-tag>
        </n-form-item>
      </n-form>

      <n-space style="margin-bottom: 12px">
        <n-button type="primary" :loading="isBusy('save-provider')" @click="saveProvider">
          保存服务商
        </n-button>
        <n-select
          v-model:value="testModelId"
          :options="modelRows.map((row) => ({ label: row.id, value: row.id }))"
          placeholder="测试连接（可指定模型）"
          clearable
          size="small"
          style="width: 240px"
        />
        <n-button :loading="isBusy('test')" @click="testConnection">测试连接</n-button>
      </n-space>

      <n-alert
        v-if="testResult"
        :type="testResult.ok ? 'success' : 'error'"
        :title="testResult.ok ? '连接成功' : '连接失败'"
        closable
        style="margin-bottom: 12px"
        @close="testResult = null"
      >
        状态码 {{ testResult.status ?? "—" }} · 延迟 {{ testResult.latencyMs }} ms
        <template v-if="testResult.message"> · {{ testResult.message }}</template>
        <template v-else-if="!testResult.ok">（没有拿到 HTTP 状态码：通常是网络不通或地址写法问题）</template>
      </n-alert>

      <n-divider style="margin: 8px 0">模型列表（{{ modelRows.length }} 个）</n-divider>
      <n-data-table
        :columns="modelColumns"
        :data="modelRows"
        :row-key="(row) => row.id"
        :bordered="false"
        size="small"
        :max-height="260"
        :scroll-x="1020"
      />
      <n-text depth="3" style="display: block; margin: 8px 0">
        显示名 / 别名 / 1M / 上下文窗口 / max_tokens 的改动点「保存服务商」后生效；新增与删除立即生效
        （只重读模型列表，不碰上面还没保存的改动）。上下文窗口与 max_tokens 仅用于界面展示（网关不依赖它们）。
      </n-text>

      <n-space align="center">
        <n-input v-model:value="newModelId" placeholder="模型名，例如 deepseek-v4-pro 或 k3[1M]" size="small" style="width: 260px" />
        <n-input v-model:value="newModelName" placeholder="显示名（可空）" size="small" style="width: 200px" />
        <n-button size="small" type="primary" :loading="isBusy('add-model')" @click="addModel">
          添加模型
        </n-button>
      </n-space>

      <n-space style="margin-top: 16px">
        <n-button type="primary" :loading="isBusy('save-provider')" @click="saveProvider">
          保存服务商
        </n-button>
        <n-button quaternary @click="emit('cancel')">关闭</n-button>
      </n-space>
    </template>
  </div>
</template>

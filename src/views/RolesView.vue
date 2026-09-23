<script setup lang="ts">
/**
 * B4：角色路由。
 *
 * - 三个槽位 `main` / `fast` / `subagent` 各一个 `ModelPicker`（选项来自全部已配置模型），
 *   旁边显示**将写入 Claude Code 的别名**；改动立即 `role_set`（未选 = 未绑定，允许）。
 * - 只读 env 预览：把 spec §7.1 将写入 `~/.claude/settings.json` 的键值列出来，
 *   令牌默认掩码；「应用到 Claude Code」调 `takeover_apply`。
 * - 额外规则表（`extraRoutes`）与未知模型策略（`onUnknownModel` / `defaultTarget`）：
 *   A4 没有为它们单独开命令，走 `save_config`（后端 `ConfigStore::save` 会跑完整校验）。
 */
import { computed, h, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NDivider,
  NFormItem,
  NInput,
  NRadio,
  NRadioGroup,
  NSpace,
  NSwitch,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { Config, ExtraRoute, RoleName, TargetDto, UnknownModelPolicy } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import ModelPicker from "../components/ModelPicker.vue";
import { useConfigStore } from "../stores/config";

const store = useConfigStore();
const { isBusy, run, message } = useAction();
const { confirm } = useConfirm();

const ROLE_LABELS: Record<RoleName, string> = {
  main: "主 Agent（main）",
  fast: "快速（fast / haiku）",
  subagent: "子 Agent（subagent）",
};
const ROLE_HINTS: Record<RoleName, string> = {
  main: "写入 ANTHROPIC_DEFAULT_OPUS / SONNET / FABLE_MODEL",
  fast: "写入 ANTHROPIC_DEFAULT_HAIKU_MODEL",
  subagent: "写入 CLAUDE_CODE_SUBAGENT_MODEL",
};

const roleSlots = computed(() =>
  (["main", "fast", "subagent"] as RoleName[]).map((role) => ({
    role,
    label: ROLE_LABELS[role],
    hint: ROLE_HINTS[role],
    target: store.roleTargets[role],
    model: store.modelOf(store.roleTargets[role]),
  })),
);

function setRole(role: RoleName, target: TargetDto | null) {
  return run(
    async () => {
      await ipc.roleSet(role, target);
      await store.refreshConfig();
      await store.refreshTakeover();
    },
    target ? `已更新「${ROLE_LABELS[role]}」绑定` : `已解除「${ROLE_LABELS[role]}」绑定`,
    `role-${role}`,
  );
}

// ---------------------------------------------------------------- env 预览（spec §7.1）

const showToken = ref(false);

const REMOVED_KEYS = ["ANTHROPIC_MODEL", "ANTHROPIC_API_KEY", "ANTHROPIC_SMALL_FAST_MODEL"];

function maskToken(token: string): string {
  if (token.length <= 12) return "•".repeat(token.length);
  return `${token.slice(0, 7)}…${token.slice(-4)}`;
}

interface EnvRow {
  key: string;
  value: string;
}

const envPreview = computed(() => {
  const config = store.config;
  const rows: EnvRow[] = [];
  const missing: string[] = [];
  if (!config) return { rows, missing };

  rows.push({ key: "ANTHROPIC_BASE_URL", value: `http://127.0.0.1:${config.gateway.port}` });
  rows.push({
    key: "ANTHROPIC_AUTH_TOKEN",
    value: showToken.value ? config.gateway.localToken : maskToken(config.gateway.localToken),
  });

  const main = store.roleTargets.main;
  const mainAlias = store.aliasOf(main);
  const mainName = store.displayNameOf(main);
  for (const family of ["OPUS", "SONNET", "FABLE"]) {
    if (mainAlias && mainName) {
      rows.push({ key: `ANTHROPIC_DEFAULT_${family}_MODEL`, value: mainAlias });
      rows.push({ key: `ANTHROPIC_DEFAULT_${family}_MODEL_NAME`, value: mainName });
    } else {
      missing.push(`main → ANTHROPIC_DEFAULT_${family}_MODEL`);
    }
  }

  const fast = store.roleTargets.fast;
  const fastAlias = store.aliasOf(fast);
  const fastName = store.displayNameOf(fast);
  if (fastAlias && fastName) {
    rows.push({ key: "ANTHROPIC_DEFAULT_HAIKU_MODEL", value: fastAlias });
    rows.push({ key: "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME", value: fastName });
  } else {
    missing.push("fast → ANTHROPIC_DEFAULT_HAIKU_MODEL");
  }

  const subagent = store.roleTargets.subagent;
  const subagentAlias = store.aliasOf(subagent);
  if (subagentAlias) {
    rows.push({ key: "CLAUDE_CODE_SUBAGENT_MODEL", value: subagentAlias });
  } else {
    missing.push("subagent → CLAUDE_CODE_SUBAGENT_MODEL");
  }

  return { rows, missing };
});

async function applyTakeover() {
  if (!store.gatewayRunning) {
    const go = await confirm({
      title: "网关未运行",
      content: "接管后 Claude Code 会指向本机网关，但网关没跑时 Claude Code 将不可用。仍然接管？",
      positiveText: "仍然接管",
      danger: true,
    });
    if (!go) return;
  }
  await run(
    async () => {
      await ipc.takeoverApply();
      await store.refreshTakeover();
      await store.refreshConfig();
    },
    "已应用到 Claude Code",
    "takeover-apply",
  );
}

// ---------------------------------------------------------------- 额外规则

const newRouteAlias = ref("");
const newRouteTarget = ref<TargetDto | null>(null);

function addRoute() {
  const config = store.config;
  if (!config) return Promise.resolve(false);
  const alias = newRouteAlias.value.trim();
  const target = newRouteTarget.value;
  if (!alias) {
    message.warning("请填写别名，例如 ccr-big");
    return Promise.resolve(false);
  }
  if (!target) {
    message.warning("请选择该别名指向的模型");
    return Promise.resolve(false);
  }
  if (store.aliases.includes(alias)) {
    message.warning(`别名 ${alias} 与某个模型的默认别名冲突，请换一个`);
    return Promise.resolve(false);
  }
  if (config.extraRoutes.some((route) => route.alias === alias)) {
    message.warning(`别名 ${alias} 已存在于额外规则里`);
    return Promise.resolve(false);
  }
  return run(
    async () => {
      const next: Config = {
        ...config,
        extraRoutes: [...config.extraRoutes, { alias, providerId: target.providerId, modelId: target.modelId }],
      };
      await ipc.saveConfig(next);
      await store.refreshConfig();
      newRouteAlias.value = "";
      newRouteTarget.value = null;
    },
    `额外规则已添加：${alias}`,
    "add-route",
  );
}

async function removeRoute(alias: string) {
  const config = store.config;
  if (!config) return;
  const go = await confirm({
    title: "删除额外规则",
    content: `删除别名 ${alias}？该别名将不再被网关识别为路由入口。`,
    positiveText: "删除",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      const next: Config = {
        ...config,
        extraRoutes: config.extraRoutes.filter((route) => route.alias !== alias),
      };
      await ipc.saveConfig(next);
      await store.refreshConfig();
    },
    "额外规则已删除",
    `rm-route-${alias}`,
  );
}

const routeColumns: DataTableColumns<ExtraRoute> = [
  { title: "别名", key: "alias", width: 220, render: (row) => h("span", { class: "mono" }, row.alias) },
  {
    title: "目标",
    key: "target",
    minWidth: 280,
    render: (row) =>
      store.displayNameOf({ providerId: row.providerId, modelId: row.modelId }) ??
      `${row.providerId} / ${row.modelId}`,
  },
  {
    title: "操作",
    key: "actions",
    width: 100,
    render: (row) =>
      h(
        NButton,
        {
          size: "small",
          type: "error",
          quaternary: true,
          loading: isBusy(`rm-route-${row.alias}`),
          onClick: () => void removeRoute(row.alias),
        },
        { default: () => "删除" },
      ),
  },
];

// ---------------------------------------------------------------- 未知模型策略

const policy = ref<UnknownModelPolicy>("default");
const defaultTarget = ref<TargetDto | null>(null);

watch(
  () => store.config,
  (config) => {
    if (!config) return;
    policy.value = config.onUnknownModel;
    defaultTarget.value = config.defaultTarget;
  },
  { immediate: true },
);

function savePolicy() {
  const config = store.config;
  if (!config) return Promise.resolve(false);
  return run(
    async () => {
      const next: Config = {
        ...config,
        onUnknownModel: policy.value,
        defaultTarget: defaultTarget.value,
      };
      await ipc.saveConfig(next);
      await store.refreshConfig();
    },
    "未知模型策略已保存",
    "save-policy",
  );
}
</script>

<template>
  <div>
    <n-card size="small" title="角色路由：决定谁用哪个模型">
      <n-alert type="info" style="margin-bottom: 12px">
        这里的三个槽位就是「主 agent 用 A 厂商、subagent 用 B 厂商」的开关。改动立即生效
        （`role_set`），无需保存；未绑定的槽位会让「一键接管」被拦下。
      </n-alert>

      <div v-for="slot in roleSlots" :key="slot.role" class="role-row">
        <div class="role-head">
          <n-text strong>{{ slot.label }}</n-text>
          <n-text depth="3">{{ slot.hint }}</n-text>
        </div>
        <n-space align="center" :size="10">
          <div style="width: 380px">
            <ModelPicker
              :model-value="slot.target"
              :placeholder="`为 ${slot.label} 选择模型`"
              @update:model-value="(target) => setRole(slot.role, target)"
            />
          </div>
          <template v-if="slot.model">
            <n-tag size="small" type="success">别名 {{ slot.model.alias }}</n-tag>
            <n-text depth="3">{{ slot.model.providerName }} / {{ slot.model.modelName }}</n-text>
            <n-button
              size="tiny"
              quaternary
              :loading="isBusy(`role-${slot.role}`)"
              @click="setRole(slot.role, null)"
            >
              解除绑定
            </n-button>
          </template>
          <n-tag v-else size="small" type="warning">未绑定</n-tag>
        </n-space>
      </div>
    </n-card>

    <n-card size="small" title="将写入 ~/.claude/settings.json 的 env（只读预览）" style="margin-top: 12px">
      <template #header-extra>
        <n-space align="center" :size="8">
          <n-text depth="3">显示令牌明文</n-text>
          <n-switch v-model:value="showToken" size="small" />
        </n-space>
      </template>

      <n-alert v-if="envPreview.missing.length" type="warning" style="margin-bottom: 12px">
        以下槽位未绑定，对应的键不会被写入：{{ envPreview.missing.join("、") }}
      </n-alert>

      <div v-for="row in envPreview.rows" :key="row.key" class="env-row">
        <span class="mono env-key">{{ row.key }}</span>
        <span class="mono env-value">{{ row.value }}</span>
      </div>

      <n-divider style="margin: 10px 0">接管时会移除这些键（避免绕过别名路由）</n-divider>
      <n-space :size="6">
        <n-tag v-for="key in REMOVED_KEYS" :key="key" size="small">{{ key }}</n-tag>
      </n-space>

      <n-space style="margin-top: 16px">
        <n-button
          type="primary"
          :loading="isBusy('takeover-apply')"
          :disabled="!store.config"
          @click="applyTakeover"
        >
          应用到 Claude Code（一键接管）
        </n-button>
        <n-text depth="3">
          {{ store.takeover?.state === "applied" ? "当前已接管；再点会用最新配置重写" : "当前未接管" }}
        </n-text>
      </n-space>
    </n-card>

    <n-card size="small" title="额外规则（别名 → 目标）" style="margin-top: 12px">
      <n-text depth="3">
        在模型自带别名之外，额外暴露的快捷别名。别名全局唯一，不能与任何模型的默认别名相同。
      </n-text>
      <n-data-table
        :columns="routeColumns"
        :data="store.config?.extraRoutes ?? []"
        :row-key="(row) => row.alias"
        :bordered="false"
        size="small"
        style="margin-top: 10px"
        :locale="{ empty: '还没有额外规则' }"
      />
      <n-space align="center" style="margin-top: 12px">
        <n-input v-model:value="newRouteAlias" placeholder="别名，例如 ccr-big" size="small" style="width: 220px" />
        <div style="width: 320px">
          <ModelPicker v-model="newRouteTarget" placeholder="选择目标模型" />
        </div>
        <n-button size="small" type="primary" :loading="isBusy('add-route')" @click="addRoute">
          添加规则
        </n-button>
      </n-space>
    </n-card>

    <n-card size="small" title="未知模型策略" style="margin-top: 12px">
      <n-text depth="3">
        请求的 model 既不是已配置别名、也不含 Claude 家族名（opus/sonnet/haiku/fable）时怎么处理。
      </n-text>
      <n-space vertical :size="12" style="margin-top: 12px">
        <n-radio-group v-model:value="policy">
          <n-space vertical :size="6">
            <n-radio value="default">转发到「默认目标」并标黄记入日志（matchedBy = fallback）</n-radio>
            <n-radio value="error">直接返回 400，不转发（更安全，能立刻发现模型名打错）</n-radio>
          </n-space>
        </n-radio-group>
        <n-form-item label="默认目标（策略为 default 时使用）" :show-feedback="false">
          <div style="width: 420px">
            <ModelPicker v-model="defaultTarget" placeholder="选择默认目标模型" :disabled="policy === 'error'" />
          </div>
        </n-form-item>
        <n-space>
          <n-button
            type="primary"
            size="small"
            :loading="isBusy('save-policy')"
            :disabled="!store.config"
            @click="savePolicy"
          >
            保存策略
          </n-button>
        </n-space>
      </n-space>
    </n-card>
  </div>
</template>

<style scoped>
.role-row {
  padding: 10px 0;
  border-bottom: 1px solid #f0f0f5;
}
.role-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 6px;
}
.env-row {
  display: flex;
  gap: 12px;
  padding: 2px 0;
}
.env-key {
  width: 340px;
  color: #666;
}
.env-value {
  word-break: break-all;
}
</style>

<script setup lang="ts">
/**
 * B2：状态页（首页）。
 *
 * - 顶部摘要行：`主 Agent → <provider> / <model>`、`subagent → …`，直接由 `roles` 渲染
 *   ——这是用户「一眼确认分流」的核心凭据（计划 B2 / spec §10.1）。
 * - 网关卡片：运行状态、地址、启动 / 停止 / 重启（`gateway_start` / `gateway_stop` / `gateway_restart`）。
 * - 接管卡片：三态 + 一键接管 / 一键还原 + `stale` 红色横幅（spec §7.4）。
 * - 请求日志表：`recent_logs` 最近 200 条，store 每 1.5s 轮询；可按服务商过滤。
 */
import { computed, h, ref } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NGrid,
  NGi,
  NSelect,
  NSpace,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { LogEntry, RoleName } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import type { PageKey } from "../constants";
import { useConfigStore } from "../stores/config";

const emit = defineEmits<{ navigate: [PageKey] }>();

const store = useConfigStore();
const { busy, isBusy, run } = useAction();
const { confirm } = useConfirm();

const ROLE_LABELS: Record<RoleName, string> = {
  main: "主 Agent",
  fast: "快速（haiku）",
  subagent: "子 Agent",
};

const roleSlots = computed(() =>
  (["main", "fast", "subagent"] as RoleName[]).map((role) => ({
    role,
    label: ROLE_LABELS[role],
    target: store.roleTargets[role],
    model: store.modelOf(store.roleTargets[role]),
  })),
);

// ---------------------------------------------------------------- 网关

function startGateway() {
  return run(async () => {
    await ipc.gatewayStart();
    await store.refreshGateway();
  }, "网关已启动");
}

function stopGateway() {
  return run(async () => {
    await ipc.gatewayStop();
    await store.refreshGateway();
  }, "网关已停止");
}

function restartGateway() {
  return run(async () => {
    await ipc.gatewayRestart();
    await store.refreshGateway();
    await store.refreshConfig();
  }, "网关已重启");
}

// ---------------------------------------------------------------- 接管

const takeoverState = computed(() => store.takeover?.state ?? "not_applied");

const takeoverTag = computed(() => {
  switch (takeoverState.value) {
    case "applied":
      return { type: "success" as const, text: "已接管" };
    case "stale":
      return { type: "error" as const, text: "已失效（被外部改写）" };
    default:
      return { type: "default" as const, text: "未接管" };
  }
});

async function applyTakeover() {
  if (!store.gatewayRunning) {
    const go = await confirm({
      title: "网关未运行",
      content:
        "接管后 Claude Code 会指向本机网关，但网关此刻没有运行 —— 接管期间 Claude Code 将不可用。仍然接管？",
      positiveText: "仍然接管",
      danger: true,
    });
    if (!go) return;
  }
  await run(async () => {
    await ipc.takeoverApply();
    await store.refreshTakeover();
    await store.refreshConfig();
  }, "已接管 Claude Code（settings.json 已指向本机网关）");
}

async function restoreTakeover() {
  const go = await confirm({
    title: "还原 Claude Code 配置",
    content: "将按接管清单逐字节回放 ~/.claude/settings.json，并回退子 Agent 的 model 改动。继续？",
  });
  if (!go) return;
  await run(async () => {
    await ipc.takeoverRestore();
    await store.refreshTakeover();
    await store.refreshConfig();
  }, "已还原 Claude Code 配置");
}

// ---------------------------------------------------------------- 日志

const providerFilter = ref<string>("");

const providerFilterOptions = computed(() => [
  { label: "全部服务商", value: "" },
  ...store.providers.map((provider) => ({ label: `${provider.name} (${provider.id})`, value: provider.id })),
]);

const filteredLogs = computed(() =>
  providerFilter.value
    ? store.logs.filter((entry) => entry.providerId === providerFilter.value)
    : store.logs,
);

const MATCHED_BY_LABELS: Record<string, string> = {
  alias: "别名精确匹配",
  family: "Claude 家族兜底",
  fallback: "默认目标（fallback）",
  passthrough: "透传（passthrough）",
  error: "路由错误",
};

function matchedByType(matchedBy: string): "success" | "warning" | "error" | "info" | "default" {
  switch (matchedBy) {
    case "alias":
      return "success";
    case "family":
      return "info";
    case "fallback":
    case "passthrough":
      return "warning";
    case "error":
      return "error";
    default:
      return "default";
  }
}

function formatTime(ts: string): string {
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return ts;
  const pad = (value: number, width = 2) => String(value).padStart(width, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(
    date.getMilliseconds(),
    3,
  )}`;
}

function providerName(id: string): string {
  if (!id) return "—";
  return store.providerById(id)?.name ?? id;
}

const logColumns: DataTableColumns<LogEntry> = [
  {
    title: "时间",
    key: "ts",
    width: 130,
    render: (row) => h("span", { class: "mono" }, formatTime(row.ts)),
  },
  {
    title: "请求模型",
    key: "requestedModel",
    width: 170,
    render: (row) => h("span", { class: "mono" }, row.requestedModel || "—"),
  },
  {
    title: "匹配方式",
    key: "matchedBy",
    width: 150,
    render: (row) =>
      h(
        NTag,
        { size: "small", type: matchedByType(row.matchedBy) },
        { default: () => MATCHED_BY_LABELS[row.matchedBy] ?? row.matchedBy },
      ),
  },
  {
    title: "角色",
    key: "role",
    width: 110,
    render: (row) => (row.role ? row.role : h(NText, { depth: 3 }, { default: () => "—" })),
  },
  {
    title: "服务商",
    key: "providerId",
    width: 140,
    render: (row) => providerName(row.providerId),
  },
  {
    title: "上游模型",
    key: "upstreamModel",
    width: 190,
    render: (row) => h("span", { class: "mono" }, row.upstreamModel || "—"),
  },
  {
    title: "状态码",
    key: "status",
    width: 90,
    render: (row) => {
      if (row.status === null) return h(NText, { depth: 3 }, { default: () => "—" });
      const type = row.status >= 200 && row.status < 300 ? "success" : row.status >= 500 ? "error" : "warning";
      return h(NTag, { size: "small", type }, { default: () => String(row.status) });
    },
  },
  {
    title: "流式",
    key: "stream",
    width: 70,
    render: (row) => (row.stream ? "是" : "否"),
  },
  {
    title: "延迟",
    key: "latencyMs",
    width: 90,
    render: (row) => `${row.latencyMs} ms`,
  },
  {
    title: "错误",
    key: "error",
    minWidth: 160,
    render: (row) => (row.error ? h("span", { style: "color:#d03050" }, row.error) : ""),
  },
];

function logRowProps(row: LogEntry) {
  if (row.matchedBy === "error" || (row.error && !row.status)) return { class: "log-row-error" };
  if (row.matchedBy === "fallback" || row.matchedBy === "passthrough") return { class: "log-row-warn" };
  return {};
}
</script>

<template>
  <div>
    <n-grid :cols="1" :y-gap="12">
      <n-gi>
        <n-card size="small" title="当前分流（直接由 roles 渲染）">
          <div v-for="slot in roleSlots" :key="slot.role" class="role-line">
            <n-tag size="small" :type="slot.model ? 'info' : 'warning'" style="width: 108px; justify-content: center">
              {{ slot.label }}
            </n-tag>
            <template v-if="slot.model">
              <n-text strong style="font-size: 15px">
                {{ slot.model.providerName }} / {{ slot.model.modelName }}
              </n-text>
              <n-text depth="3" class="mono">
                别名 {{ slot.model.alias }}
                <template v-if="slot.model.context1m">（1M 上下文）</template>
              </n-text>
            </template>
            <template v-else>
              <n-text type="warning">未绑定</n-text>
              <n-text depth="3">接管前必须绑定该槽位</n-text>
            </template>
          </div>
          <n-space v-if="!roleSlots.every((slot) => slot.model)" style="margin-top: 8px">
            <n-button size="small" @click="emit('navigate', 'roles')">去「角色路由」绑定</n-button>
          </n-space>
        </n-card>
      </n-gi>

      <n-gi>
        <n-grid :cols="2" :x-gap="12">
          <n-gi>
            <n-card size="small" title="网关">
              <n-space vertical :size="10">
                <n-space align="center" :size="8">
                  <n-tag :type="store.gatewayRunning ? 'success' : 'warning'" size="small">
                    {{ store.gatewayRunning ? "运行中" : "未运行" }}
                  </n-tag>
                  <n-text class="mono">{{ store.gatewayUrl || "—" }}</n-text>
                </n-space>
                <n-text depth="3">
                  已服务请求：{{ store.gatewayStatus?.requestsServed ?? 0 }} · 端口配置：{{
                    store.config?.gateway.port ?? "—"
                  }}
                </n-text>
                <n-space>
                  <n-button
                    type="primary"
                    :disabled="store.gatewayRunning"
                    :loading="isBusy('gateway-start')"
                    @click="startGateway"
                  >
                    启动网关
                  </n-button>
                  <n-button
                    :disabled="!store.gatewayRunning"
                    :loading="isBusy('gateway-stop')"
                    @click="stopGateway"
                  >
                    停止网关
                  </n-button>
                  <n-button
                    :disabled="!store.gatewayRunning"
                    :loading="isBusy('gateway-restart')"
                    @click="restartGateway"
                  >
                    重启网关
                  </n-button>
                </n-space>
              </n-space>
            </n-card>
          </n-gi>

          <n-gi>
            <n-card size="small" title="Claude Code 接管">
              <n-space vertical :size="10">
                <n-space align="center" :size="8">
                  <n-tag :type="takeoverTag.type" size="small">{{ takeoverTag.text }}</n-tag>
                  <n-text depth="3">{{ store.takeover?.gatewayUrl || "—" }}</n-text>
                </n-space>
                <n-text depth="3">
                  接管时间：{{ store.takeover?.appliedAt || "—" }}
                </n-text>
                <n-space>
                  <n-button
                    type="primary"
                    :disabled="takeoverState === 'applied' || !store.providers.length"
                    :loading="isBusy('takeover-apply')"
                    @click="applyTakeover"
                  >
                    一键接管
                  </n-button>
                  <n-button
                    :disabled="takeoverState === 'not_applied'"
                    :loading="isBusy('takeover-restore')"
                    @click="restoreTakeover"
                  >
                    一键还原
                  </n-button>
                </n-space>
                <n-text v-if="!store.providers.length" depth="3">
                  还没有服务商：先到「服务商与模型」加一个。
                </n-text>
              </n-space>
            </n-card>
          </n-gi>
        </n-grid>
      </n-gi>

      <n-gi v-if="takeoverState === 'stale'">
        <n-alert type="error" title="接管已失效：settings.json 被其它程序改写">
          <div>
            ~/.claude/settings.json 里的 ANTHROPIC_BASE_URL 现在是
            <span class="mono">{{ store.takeover?.found || "（空）" }}</span>，
            而不是本机网关 <span class="mono">{{ store.takeover?.gatewayUrl }}</span>。
          </div>
          <n-space style="margin-top: 8px">
            <n-button size="small" type="primary" :loading="busy" @click="applyTakeover">重新接管</n-button>
            <n-button size="small" @click="restoreTakeover">还原</n-button>
          </n-space>
        </n-alert>
      </n-gi>

      <n-gi v-if="store.takeoverWithoutGateway">
        <n-alert type="error" title="已接管但网关未运行">
          Claude Code 现在无法使用 —— 请点上面的「启动网关」，或点「一键还原」回到接管前。
        </n-alert>
      </n-gi>

      <n-gi>
        <n-card size="small" title="请求日志">
          <template #header-extra>
            <n-space align="center" :size="8">
              <n-select
                v-model:value="providerFilter"
                :options="providerFilterOptions"
                size="small"
                style="width: 220px"
              />
              <n-button size="small" :loading="isBusy('logs-refresh')" @click="run(() => store.pollFastState(), undefined, 'logs-refresh')">
                立即刷新
              </n-button>
            </n-space>
          </template>
          <n-data-table
            :columns="logColumns"
            :data="filteredLogs"
            :row-props="logRowProps"
            :max-height="420"
            :bordered="false"
            size="small"
            :locale="{
              empty: '还没有请求。把 Claude Code 接管后发一条消息，这里就会出现分流记录。',
            }"
          />
          <n-text depth="3" style="display: block; margin-top: 8px">
            每 1.5 秒自动刷新；黄底 = fallback / passthrough，红底 = 路由错误。
          </n-text>
        </n-card>
      </n-gi>
    </n-grid>
  </div>
</template>

<style scoped>
.role-line {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 4px 0;
}
</style>

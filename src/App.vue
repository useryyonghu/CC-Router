<script setup lang="ts">
/**
 * Plan 3 B1：布局 + 侧边导航（五页）。
 *
 * 切页用本地 `ref`（计划明确不引 vue-router）；五页组件按需切换，
 * 全局只有状态页在轮询日志（store 里 1.5s 一次）。
 */
import { onBeforeUnmount, onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NConfigProvider,
  NDialogProvider,
  NLayout,
  NLayoutContent,
  NLayoutSider,
  NMenu,
  NMessageProvider,
  NSpace,
  NTag,
  NText,
  dateZhCN,
  zhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import type { PageKey } from "./constants";
import { useConfigStore } from "./stores/config";
import AgentsView from "./views/AgentsView.vue";
import ProvidersView from "./views/ProvidersView.vue";
import RolesView from "./views/RolesView.vue";
import SettingsView from "./views/SettingsView.vue";
import StatusView from "./views/StatusView.vue";

const menuOptions: MenuOption[] = [
  { label: "状态与日志", key: "status" },
  { label: "服务商与模型", key: "providers" },
  { label: "角色路由", key: "roles" },
  { label: "子 Agent", key: "agents" },
  { label: "设置", key: "settings" },
];

const page = ref<PageKey>("status");
const collapsed = ref(false);
const store = useConfigStore();

function navigate(key: PageKey): void {
  page.value = key;
}

onMounted(async () => {
  await store.refreshAll();
  store.refreshVersion();
  store.startLogPolling();
});

onBeforeUnmount(() => store.stopLogPolling());
</script>

<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
    <n-message-provider>
      <n-dialog-provider>
        <n-layout has-sider style="height: 100vh">
          <n-layout-sider
            bordered
            collapse-mode="width"
            :collapsed-width="64"
            :width="208"
            :collapsed="collapsed"
            show-trigger
            @collapse="collapsed = true"
            @expand="collapsed = false"
          >
            <div class="brand">
              <n-text strong style="font-size: 17px">CC Router</n-text>
            </div>
            <n-menu
              :value="page"
              :options="menuOptions"
              :collapsed="collapsed"
              :collapsed-width="64"
              :collapsed-icon-size="18"
              @update:value="(key: string) => (page = key as PageKey)"
            />
          </n-layout-sider>

          <n-layout-content content-style="height: 100%; overflow: auto;">
            <div class="page">
              <n-alert
                v-if="store.loadError"
                type="error"
                title="后端调用失败"
                closable
                style="margin-bottom: 16px"
                @close="store.dismissLoadError()"
              >
                <pre class="raw">{{ store.loadError }}</pre>
                <n-space style="margin-top: 8px">
                  <n-button size="small" @click="store.refreshAll()">重新读取</n-button>
                </n-space>
              </n-alert>

              <div class="topbar">
                <n-space align="center" :size="12">
                  <template v-if="store.ready">
                    <n-text depth="3">网关</n-text>
                    <n-tag :type="store.gatewayRunning ? 'success' : 'warning'" size="small" round>
                      {{ store.gatewayRunning ? "运行中" : "未运行" }}
                    </n-tag>
                    <n-text depth="3">{{ store.gatewayUrl || "—" }}</n-text>
                    <n-text depth="3">
                      服务商 {{ store.providers.length }} · 模型 {{ store.models.length }}
                    </n-text>
                  </template>
                  <n-text v-else depth="3">正在读取配置…</n-text>
                </n-space>
              </div>

              <StatusView v-if="page === 'status'" @navigate="navigate" />
              <ProvidersView v-else-if="page === 'providers'" />
              <RolesView v-else-if="page === 'roles'" />
              <AgentsView v-else-if="page === 'agents'" />
              <SettingsView v-else />
            </div>
          </n-layout-content>
        </n-layout>
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>

<style>
body {
  margin: 0;
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
}
.brand {
  padding: 16px 20px 12px;
}
.page {
  padding: 16px 20px 32px;
}
.topbar {
  margin-bottom: 12px;
}
/* 后端错误原文常常是多行（每个候选 URL 一行），必须保留换行。 */
pre.raw {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-all;
  font-size: 12px;
}
/* spec §6.7：fallback / passthrough / error 的行要标黄提醒。 */
.log-row-warn td {
  background-color: #fffbe6 !important;
}
.log-row-error td {
  background-color: #fff1f0 !important;
}
.mono {
  font-family: Consolas, "Courier New", monospace;
  font-size: 12px;
}
.license-box {
  white-space: pre-wrap;
  font-family: Consolas, "Courier New", monospace;
  font-size: 12px;
  background: #fafafc;
  border: 1px solid #efeff5;
  border-radius: 4px;
  padding: 12px;
  max-height: 260px;
  overflow: auto;
}
</style>

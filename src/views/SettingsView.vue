<script setup lang="ts">
/**
 * B6：设置。
 *
 * - 端口（改后提示需重启网关 + 「立即重启网关」→ `gateway_restart`）、关窗行为、
 *   开机自启（`autostart_get` / `autostart_set`，**注册表为准**，见下方「开机自启」一节）、
 *   退出时自动还原、请求日志落盘。
 * - 令牌：掩码显示 / 复制 / 重新生成（`token_regenerate`，会同步重写已接管的 Claude Code 配置）。
 * - 备份列表（`backups_list`）+ 备份目录路径复制；配置导入 / 导出（导出可选移除密钥）。
 * - 关于：版本、来源 farion1231/cc-switch 与 **MIT 许可证全文**（`src/constants.ts`）。
 *
 * 说明：A4 没有提供「打开目录 / 打开浏览器」这类命令，也没有允许新增依赖（shell/opener 插件），
 * 因此计划 B6 里的「打开备份目录」实现为「显示完整路径 + 一键复制路径」。
 */
import { computed, h, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NDataTable,
  NDivider,
  NFormItem,
  NInput,
  NInputNumber,
  NSpace,
  NSwitch,
  NTag,
  NText,
} from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import * as ipc from "../api/ipc";
import type { BackupDto, Config } from "../api/ipc";
import { backupLabel, errorText, settingsPathValue } from "../api/ipc";
import { useAction } from "../composables/useAction";
import { useConfirm } from "../composables/useConfirm";
import { CC_SWITCH_LICENSE_TEXT, CC_SWITCH_SOURCE, CC_SWITCH_SOURCE_REF, CC_SWITCH_URL } from "../constants";
import { useConfigStore } from "../stores/config";

const store = useConfigStore();
const { isBusy, run, message } = useAction();
const { confirm } = useConfirm();

// ---------------------------------------------------------------- 端口与开关

const port = ref<number | null>(null);
const closeToTray = ref(true);
const restoreOnExit = ref(false);
const requestLogToFile = ref(false);

/**
 * 本地草稿只在**磁盘上的值真的变了**时重新灌入（M10）。
 *
 * 原来监听整个 `store.config` 对象：任何一次 refreshConfig()（例如在同一个页面里
 * 「重新生成令牌」「导入配置」）都会把用户还没保存的端口 / 开关改动弹回旧值。
 * 这里逐字段监听原始值 —— 与这些字段无关的刷新不再动草稿。
 *
 * **`ui.autostart` 不在这个 watcher 里**：它不是草稿，而是一个立即动作，
 * 真相在注册表（见下面「开机自启」一节）。用配置镜像去灌它会多出第二条真相来源。
 */
watch(
  [
    () => store.config?.gateway.port ?? null,
    () => store.config?.ui.closeToTray ?? null,
    () => store.config?.ui.restoreOnExit ?? null,
    () => store.config?.ui.requestLogToFile ?? null,
  ],
  () => {
    const config = store.config;
    if (!config) return;
    port.value = config.gateway.port;
    closeToTray.value = config.ui.closeToTray;
    restoreOnExit.value = config.ui.restoreOnExit;
    requestLogToFile.value = config.ui.requestLogToFile;
  },
  { immediate: true },
);

// ---------------------------------------------------------------- 开机自启（注册表为准）

/**
 * 「开机自启」开关**不是草稿，而是一个立即动作**：真相在 Windows 注册表里
 * （`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的 `CC Router`），
 * `config.json` 的 `ui.autostart` 只是它的**镜像**。
 *
 * 契约（已裁决，勿改）：
 *  1. 注册表是唯一事实来源；前端**绝不**拿配置去反写注册表。
 *  2. 注册表**只在用户拨动这个开关时**改变 —— 启动时不写，`saveSettings()` 时也不写
 *     （它只把当前值放进 payload，等于把镜像写成镜像自身的值，不改变系统状态）。
 *     理由：`ui.autostart` 的默认值是 `true`。若改成「启动时按配置对齐注册表」，
 *     用户只是打开一次应用，机器上就会被静默加上一条开机自启项 —— 一个用户没要求的系统级副作用。
 *  3. 所以开关在挂载时用 `autostart_get()`（读注册表）初始化，而不是读 `store.config.ui.autostart`；
 *     拨动失败时以注册表实测值回滚。界面永远显示系统真实状态，不会出现「显示开着、其实没写进去」。
 *
 * 代价（有意为之）：手改 `config.json` 里的 `autostart` 字段不会改变系统状态 ——
 * 一个数据文件不应该悄悄改动机器的启动项。
 */
const autostart = ref(true);
/** 最近一次从注册表读到的实测值：`autostart_set` 失败且再读也读不回来时的兜底。 */
let autostartObserved: boolean | null = null;
/** 首次读注册表还没回来：此时开关显示 loading（naive-ui 的 Switch 在 loading 时忽略点击），
 *  免得在真相未知时先亮一个「开」。 */
const autostartLoading = ref(true);

/**
 * 读注册表真相并同步到开关。
 * `notify` 为 false 时不弹错 —— 失败回滚路径上已经弹过一条主错误，避免同一件事弹两次。
 */
async function readAutostart(notify: boolean): Promise<boolean | null> {
  try {
    const enabled = await ipc.autostartGet();
    autostartObserved = enabled;
    autostart.value = enabled;
    return enabled;
  } catch (err) {
    if (notify) {
      message.error(`读取开机自启状态失败：${errorText(err)}`, { duration: 8000, closable: true });
    }
    return null;
  }
}

/** 用户拨动开关：立即写注册表，并以后端的**实测**返回值为准。 */
async function changeAutostart(next: boolean): Promise<void> {
  const previous = autostart.value;
  autostart.value = next; // 先给即时反馈，避免开关「按不动」；失败时下面会回滚
  const ok = await run(
    async () => {
      autostart.value = await ipc.autostartSet(next);
    },
    next ? "已开启开机自启" : "已关闭开机自启",
    "autostart",
  );
  if (ok) {
    autostartObserved = autostart.value;
    return;
  }
  // 失败：`run` 已经弹出后端错误文本（spec §5.7 不允许静默失败）。
  // 但开关绝不能停在「开」而注册表说「关」：以注册表实测值回滚；
  // 实测值也读不回来时，退回上一次实测值，最后才退回拨动前的值。
  const observed = await readAutostart(false);
  if (observed === null) autostart.value = autostartObserved ?? previous;
}

async function initAutostart(): Promise<void> {
  try {
    await readAutostart(true);
  } finally {
    autostartLoading.value = false;
  }
}

/**
 * 「立即重启网关」只在**已经保存的端口**与运行中的网关端口不一致时出现（M5）。
 * 不再看本地草稿：草稿还没保存就点，只会用旧端口重启，却提示「已按新端口重启」——
 * 那是一条假成功。保存成功后 `config.gateway.port` 变了，按钮才会出现。
 */
const restartNeeded = computed(() => {
  const config = store.config;
  if (!config || !store.gatewayRunning) return false;
  return store.gatewayStatus?.port !== config.gateway.port;
});

/** 端口输入框里改了但还没保存：给一句提示，免得「立即重启网关」看起来凭空消失了。 */
const portDirty = computed(() => {
  const config = store.config;
  return !!config && port.value !== null && port.value !== config.gateway.port;
});

function saveSettings() {
  const config = store.config;
  if (!config) {
    message.error("配置尚未加载，请稍后重试");
    return Promise.resolve(false);
  }
  if (port.value === null || port.value < 1024 || port.value > 65535) {
    message.warning("端口必须在 1024 – 65535 之间");
    return Promise.resolve(false);
  }
  return run(
    async () => {
      const next: Config = {
        ...config,
        gateway: { ...config.gateway, port: port.value as number },
        ui: {
          ...config.ui,
          closeToTray: closeToTray.value,
          autostart: autostart.value,
          restoreOnExit: restoreOnExit.value,
          requestLogToFile: requestLogToFile.value,
        },
      };
      await ipc.saveConfig(next);
      await store.refreshConfig();
    },
    "设置已保存",
    "save-settings",
  );
}

function restartGateway() {
  return run(
    async () => {
      await ipc.gatewayRestart();
      await store.refreshGateway();
      await store.refreshConfig();
    },
    "网关已按新端口重启",
    "restart",
  );
}

// ---------------------------------------------------------------- 令牌

const showToken = ref(false);

const maskedToken = computed(() => {
  const token = store.config?.gateway.localToken ?? "";
  if (showToken.value) return token;
  return token.length <= 12 ? "•".repeat(token.length) : `${token.slice(0, 7)}…${token.slice(-4)}`;
});

function copyText(text: string, label: string): void {
  if (!text) {
    message.warning(`${label}为空`);
    return;
  }
  const fallback = () => {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    document.body.removeChild(area);
  };
  try {
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).then(
        () => message.success(`${label}已复制`),
        () => {
          fallback();
          message.success(`${label}已复制`);
        },
      );
    } else {
      fallback();
      message.success(`${label}已复制`);
    }
  } catch (err) {
    message.error(`复制失败：${errorText(err)}`);
  }
}

async function regenerateToken() {
  const go = await confirm({
    title: "重新生成本地令牌",
    content:
      "旧令牌立即失效。若当前已接管 Claude Code，应用会同步重写 ~/.claude/settings.json 里的 ANTHROPIC_AUTH_TOKEN（属于接管更新，不会新建备份）。继续？",
    positiveText: "重新生成",
    danger: true,
  });
  if (!go) return;
  await run(
    async () => {
      const token = await ipc.tokenRegenerate();
      await store.refreshConfig();
      await store.refreshTakeover();
      showToken.value = true;
      message.success(`新令牌：${token}`);
    },
    undefined,
    "token",
  );
}

// ---------------------------------------------------------------- 备份

const backups = ref<BackupDto[]>([]);
const backupsLoading = ref(false);

async function loadBackups(): Promise<void> {
  backupsLoading.value = true;
  try {
    backups.value = await ipc.backupsList();
  } catch (err) {
    message.error(errorText(err), { duration: 8000, closable: true });
  } finally {
    backupsLoading.value = false;
  }
}

function formatDateTime(ts: string | null | undefined): string {
  if (!ts) return "—";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return ts;
  return date.toLocaleString();
}

function formatSize(bytes: number | null | undefined): string {
  if (typeof bytes !== "number") return "—";
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KB`;
}

const backupColumns: DataTableColumns<BackupDto> = [
  { title: "文件", key: "fileName", minWidth: 320, render: (row) => h("span", { class: "mono" }, backupLabel(row)) },
  {
    title: "时间",
    key: "time",
    width: 200,
    render: (row) => formatDateTime(row.modifiedAt ?? row.createdAt),
  },
  { title: "大小", key: "sizeBytes", width: 110, render: (row) => formatSize(row.sizeBytes) },
];

const backupsDir = computed(() => settingsPathValue(store.settingsPaths, "backups"));
const configPath = computed(() => settingsPathValue(store.settingsPaths, "config"));
const settingsPath = computed(() => settingsPathValue(store.settingsPaths, "settings"));
const agentsDir = computed(() => settingsPathValue(store.settingsPaths, "agents"));
const presetsUserPath = computed(() => settingsPathValue(store.settingsPaths, "presets"));
const logsDir = computed(() => settingsPathValue(store.settingsPaths, "logs"));

// ---------------------------------------------------------------- 导入 / 导出

const importInput = ref<HTMLInputElement | null>(null);

function exportConfig(stripKeys: boolean): void {
  const config = store.config;
  if (!config) {
    message.error("配置尚未加载");
    return;
  }
  const clone = JSON.parse(JSON.stringify(config)) as Config;
  if (stripKeys) {
    clone.providers = clone.providers.map((provider) => ({ ...provider, apiKey: "" }));
  }
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const blob = new Blob([JSON.stringify(clone, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `cc-router-config${stripKeys ? "-nokey" : ""}-${stamp}.json`;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  URL.revokeObjectURL(url);
  message.success(stripKeys ? "已导出（已移除密钥，可安全分享）" : "已导出（含明文密钥，请勿分享）");
}

function pickImportFile(): void {
  importInput.value?.click();
}

async function onImportFile(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  try {
    const text = await file.text();
    const parsed = JSON.parse(text) as Config;
    if (!parsed || typeof parsed !== "object" || !parsed.gateway || !Array.isArray(parsed.providers)) {
      message.error("这个文件不像是 cc-router 的 config.json（缺少 gateway / providers）");
      return;
    }
    const go = await confirm({
      title: "导入配置",
      content: `用 ${file.name} 覆盖当前配置？当前的服务商、密钥、角色绑定都会被替换。`,
      positiveText: "导入",
      danger: true,
    });
    if (!go) return;
    await run(
      async () => {
        await ipc.saveConfig(parsed);
        await store.refreshConfig();
        await store.refreshTakeover();
      },
      "配置已导入",
      "import",
    );
  } catch (err) {
    message.error(`读取或解析失败：${errorText(err)}`, { duration: 8000, closable: true });
  }
}

onMounted(() => {
  void initAutostart();
  void loadBackups();
  if (!store.settingsPaths) void store.refreshPaths();
  if (!store.appVersion) void store.refreshVersion();
});
</script>

<template>
  <div>
    <n-alert v-if="!store.config" type="warning" title="配置尚未加载">
      若持续如此，请检查 %APPDATA%\cc-router\config.json 是否被破坏（应用启动时校验失败会拒绝启动）。
    </n-alert>

    <n-card size="small" title="网关与行为">
      <n-form-item label="端口（1024 – 65535；改动后需要重启网关才生效）" :show-feedback="false">
        <n-space align="center">
          <n-input-number v-model:value="port" :min="1024" :max="65535" style="width: 160px" />
          <n-button type="primary" size="small" :loading="isBusy('save-settings')" @click="saveSettings">
            保存设置
          </n-button>
          <n-button
            v-if="restartNeeded"
            size="small"
            :loading="isBusy('restart')"
            @click="restartGateway"
          >
            立即重启网关
          </n-button>
          <n-text v-if="portDirty" depth="3">端口已修改，先点「保存设置」，重启按钮才会出现</n-text>
        </n-space>
      </n-form-item>
      <n-text depth="3">
        当前网关地址：<span class="mono">{{ store.gatewayUrl || "—" }}</span>
        （{{ store.gatewayRunning ? "运行中" : "未运行" }}）
      </n-text>

      <n-divider style="margin: 12px 0" />

      <n-space vertical :size="10">
        <n-space align="center" :size="10">
          <n-switch v-model:value="closeToTray" size="small" />
          <n-text>关闭窗口时最小化到托盘（关闭 = 直接退出）</n-text>
        </n-space>
        <n-space align="center" :size="10">
          <n-switch
            :value="autostart"
            :loading="autostartLoading || isBusy('autostart')"
            size="small"
            @update:value="changeAutostart"
          />
          <n-text>开机自启</n-text>
          <n-tag size="small" type="success">拨动即写入注册表</n-tag>
          <n-text depth="3">以注册表为准；手改 config.json 的 autostart 不会改变系统启动项</n-text>
        </n-space>
        <n-space align="center" :size="10">
          <n-switch v-model:value="restoreOnExit" size="small" />
          <n-text>退出应用时自动还原 Claude Code 配置</n-text>
        </n-space>
        <n-space align="center" :size="10">
          <n-switch v-model:value="requestLogToFile" size="small" />
          <n-text>把请求日志落盘到 logs\requests-YYYY-MM-DD.jsonl</n-text>
        </n-space>
      </n-space>
    </n-card>

    <n-card size="small" title="本地令牌" style="margin-top: 12px">
      <n-alert type="info" style="margin-bottom: 12px">
        令牌用于保护 <span class="mono">127.0.0.1</span> 上的网关：只有携带它的请求才会被转发。
        真正的厂商密钥<b>不会</b>写进 Claude Code 的 settings.json，那里只有这个令牌。
      </n-alert>
      <n-space align="center">
        <n-input :value="maskedToken" readonly style="width: 420px" />
        <n-button size="small" @click="showToken = !showToken">{{ showToken ? "隐藏" : "显示" }}</n-button>
        <n-button
          size="small"
          @click="copyText(store.config?.gateway.localToken ?? '', '令牌')"
        >
          复制
        </n-button>
        <n-button size="small" type="warning" :loading="isBusy('token')" @click="regenerateToken">
          重新生成
        </n-button>
      </n-space>
    </n-card>

    <n-card size="small" title="备份" style="margin-top: 12px">
      <template #header-extra>
        <n-space>
          <n-button size="small" :loading="backupsLoading" @click="loadBackups">刷新</n-button>
          <n-button size="small" :disabled="!backupsDir" @click="copyText(backupsDir, '备份目录路径')">
            复制备份目录路径
          </n-button>
        </n-space>
      </template>
      <n-text depth="3">
        备份目录：<span class="mono">{{ backupsDir || "（点击刷新读取）" }}</span>
      </n-text>
      <n-data-table
        :columns="backupColumns"
        :data="backups"
        :loading="backupsLoading"
        :bordered="false"
        size="small"
        :max-height="240"
        style="margin-top: 10px"
        :locale="{ empty: '还没有备份（接管 Claude Code 时会自动生成）' }"
      />
      <n-text depth="3">
        接管前会备份 ~/.claude/settings.json；修改或删除子 Agent 前会备份到 backups\agents\。
      </n-text>
    </n-card>

    <n-card size="small" title="配置导入 / 导出" style="margin-top: 12px">
      <n-space>
        <n-button size="small" @click="exportConfig(true)">导出（移除密钥）</n-button>
        <n-button size="small" @click="exportConfig(false)">导出（含密钥）</n-button>
        <n-button size="small" type="primary" :loading="isBusy('import')" @click="pickImportFile">
          导入配置…
        </n-button>
        <input
          ref="importInput"
          type="file"
          accept=".json,application/json"
          style="display: none"
          @change="onImportFile"
        />
      </n-space>
      <n-text depth="3" style="display: block; margin-top: 8px">
        导出内容就是 <span class="mono">config.json</span> 本身；「移除密钥」只是把
        <span class="mono">apiKey</span> 置空，便于分享与排查。
      </n-text>
    </n-card>

    <n-card size="small" title="文件位置" style="margin-top: 12px">
      <div v-for="item in [
        { label: '主配置（含明文密钥）', value: configPath },
        { label: 'Claude Code settings.json', value: settingsPath },
        { label: '子 Agent 目录', value: agentsDir },
        { label: '备份目录', value: backupsDir },
        { label: '预设用户覆盖文件', value: presetsUserPath },
        { label: '日志目录', value: logsDir },
      ]" :key="item.label" class="path-row">
        <n-text depth="3" style="width: 220px">{{ item.label }}</n-text>
        <span class="mono path-value">{{ item.value || "（读取中）" }}</span>
        <n-button size="tiny" quaternary :disabled="!item.value" @click="copyText(item.value, item.label)">
          复制
        </n-button>
      </div>
    </n-card>

    <n-card size="small" title="关于" style="margin-top: 12px">
      <n-space vertical :size="8">
        <n-text>CC Router 版本：<b>{{ store.appVersion }}</b></n-text>
        <n-text>
          厂家预设数据来源：
          <span class="mono">{{ CC_SWITCH_SOURCE }}</span>
          （ref <span class="mono">{{ CC_SWITCH_SOURCE_REF }}</span>，MIT License，Copyright © 2025 Jason Young）
        </n-text>
        <n-space align="center">
          <n-text class="mono">{{ CC_SWITCH_URL }}</n-text>
          <n-button size="tiny" quaternary @click="copyText(CC_SWITCH_URL, '项目地址')">复制地址</n-button>
        </n-space>
        <n-text depth="3">
          93 条预设来自该项目的 claudeProviderPresets 数据（已剥离联盟与追踪参数，未搬运任何推广关系）；
          其中仅 3 条在本机实测过，其余标注为「未在本机验证」。本应用自带 MIT 许可证全文如下：
        </n-text>
        <pre class="license-box">{{ CC_SWITCH_LICENSE_TEXT }}</pre>
      </n-space>
    </n-card>
  </div>
</template>

<style scoped>
.path-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 3px 0;
}
.path-value {
  flex: 1;
  word-break: break-all;
}
</style>

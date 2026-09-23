/**
 * 全局配置 / 运行时状态（Plan 3 B1）。
 *
 * - `config`：`get_config` 的快照（服务商、模型、角色、额外规则、ui、takeover）。
 * - `gatewayStatus`：网关运行状态（另兼作「最近一次请求数」来源）。
 * - `logs`：`recent_logs` 的最近 200 条，**用 1.5s 轮询刷新**（B1 明确要求：
 *   不引入后端事件推送，避免在并行开发期改后端）。
 *
 * 刷新失败不抛异常，而是记进 `loadError`：轮询每 1.5s 一次，若抛异常会变成
 * 未处理的 rejection 刷屏；视图侧真正需要感知失败的是**写操作**，那些走 `ipc.*`
 * 直接抛出并由 `useAction()` 弹出。
 */
import { computed, ref } from "vue";
import { defineStore } from "pinia";
import * as ipc from "../api/ipc";
import type {
  Config,
  GatewayStatus,
  LogEntry,
  ModelSpec,
  Preset,
  Provider,
  RoleName,
  SettingsPaths,
  TakeoverStatus,
  TargetDto,
} from "../api/ipc";
import { errorText } from "../api/ipc";
import { APP_VERSION_FALLBACK } from "../constants";

export const LOG_POLL_INTERVAL_MS = 1500;
/** 状态页展示的日志条数（spec §10.1 要求最近 500 条；计划 B2 要求显示最近 200 条）。 */
export const LOG_LIMIT = 200;

/** 展开后的「一个已配置模型」，供 ModelPicker / 角色页 / 别名展示复用。 */
export interface FlatModel {
  providerId: string;
  providerName: string;
  modelId: string;
  modelName: string;
  alias: string;
  context1m: boolean;
  contextWindow: number | null;
  maxTokens: number | null;
}

export const useConfigStore = defineStore("config", () => {
  const config = ref<Config | null>(null);
  const gatewayStatus = ref<GatewayStatus | null>(null);
  const logs = ref<LogEntry[]>([]);
  const presets = ref<Preset[]>([]);
  const takeover = ref<TakeoverStatus | null>(null);
  const settingsPaths = ref<SettingsPaths | null>(null);
  const appVersion = ref<string>(APP_VERSION_FALLBACK);
  const ready = ref(false);

  /**
   * 按来源记录刷新失败（而不是一个共享字符串）：`refreshAll()` 会并发跑四个刷新，
   * 共用一个字段会出现「一个失败被另一个成功清掉」的竞态，把错误藏起来。
   */
  const refreshErrors = ref<Record<string, string>>({});

  function setRefreshError(source: string, message: string | null): void {
    const next = { ...refreshErrors.value };
    if (message) next[source] = message;
    else delete next[source];
    refreshErrors.value = next;
  }

  const loadError = computed<string | null>(() => {
    const messages = Object.values(refreshErrors.value);
    return messages.length ? messages.join("\n") : null;
  });

  function dismissLoadError(): void {
    refreshErrors.value = {};
  }

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  /**
   * 正在跑的那一拍（`null` = 空闲）。1.5s 的定时器与状态页的「立即刷新」共用
   * `pollFastState()`：慢请求不会再叠罗汉 —— 后来的调用并入同一拍并等它结束（M9）。
   */
  let pollInFlight: Promise<void> | null = null;

  const providers = computed<Provider[]>(() => config.value?.providers ?? []);
  const models = computed<FlatModel[]>(() =>
    providers.value.flatMap((provider) =>
      provider.models.map((model) => ({
        providerId: provider.id,
        providerName: provider.name,
        modelId: model.id,
        modelName: model.name || model.id,
        alias: model.alias,
        context1m: model.context1m,
        contextWindow: model.contextWindow,
        maxTokens: model.maxTokens,
      })),
    ),
  );
  const aliases = computed<string[]>(() => models.value.map((model) => model.alias));
  const roleTargets = computed<Record<RoleName, TargetDto | null>>(() => ({
    main: config.value?.roles.main ?? null,
    fast: config.value?.roles.fast ?? null,
    subagent: config.value?.roles.subagent ?? null,
  }));
  const gatewayRunning = computed(() => gatewayStatus.value?.running === true);
  const gatewayUrl = computed(() => {
    const port = gatewayStatus.value?.port ?? config.value?.gateway.port ?? null;
    return port === null ? "" : `http://127.0.0.1:${port}`;
  });
  /** 接管已生效但网关没跑：Claude Code 此刻不可用（spec §7.4）。 */
  const takeoverWithoutGateway = computed(
    () => takeover.value?.state === "applied" && gatewayStatus.value?.running === false,
  );

  function providerById(id: string): Provider | null {
    return providers.value.find((provider) => provider.id === id) ?? null;
  }

  function modelSpecOf(providerId: string, modelId: string): ModelSpec | null {
    return providerById(providerId)?.models.find((model) => model.id === modelId) ?? null;
  }

  function modelOf(target: TargetDto | null): FlatModel | null {
    if (!target) return null;
    return (
      models.value.find(
        (model) => model.providerId === target.providerId && model.modelId === target.modelId,
      ) ?? null
    );
  }

  function aliasOf(target: TargetDto | null): string | null {
    return modelOf(target)?.alias ?? null;
  }

  function modelByAlias(alias: string | null): FlatModel | null {
    if (!alias) return null;
    const needle = alias.trim().toLowerCase().replace(/\[1m\]$/i, "");
    return models.value.find((model) => model.alias.toLowerCase() === needle) ?? null;
  }

  /** `"<provider名> / <模型显示名>"`，即写入 `*_MODEL_NAME` 的展示名（spec §7.1）。 */
  function displayNameOf(target: TargetDto | null): string | null {
    const model = modelOf(target);
    if (!model) return null;
    return `${model.providerName} / ${model.modelName}`;
  }

  // ------------------------------------------------------------ 刷新

  async function refreshConfig(): Promise<boolean> {
    try {
      config.value = await ipc.getConfig();
      setRefreshError("config", null);
      return true;
    } catch (err) {
      setRefreshError("config", `读取配置失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshGateway(): Promise<boolean> {
    try {
      gatewayStatus.value = await ipc.gatewayStatus();
      setRefreshError("gateway", null);
      return true;
    } catch (err) {
      setRefreshError("gateway", `读取网关状态失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshLogs(): Promise<boolean> {
    try {
      logs.value = await ipc.recentLogs(LOG_LIMIT);
      setRefreshError("logs", null);
      return true;
    } catch (err) {
      setRefreshError("logs", `读取请求日志失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshTakeover(): Promise<boolean> {
    try {
      takeover.value = await ipc.takeoverStatus();
      setRefreshError("takeover", null);
      return true;
    } catch (err) {
      setRefreshError("takeover", `读取接管状态失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshPresets(): Promise<boolean> {
    try {
      presets.value = await ipc.presetsList();
      setRefreshError("presets", null);
      return true;
    } catch (err) {
      setRefreshError("presets", `读取厂家预设失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshPaths(): Promise<boolean> {
    try {
      settingsPaths.value = await ipc.settingsPaths();
      setRefreshError("paths", null);
      return true;
    } catch (err) {
      setRefreshError("paths", `读取文件路径失败：${errorText(err)}`);
      return false;
    }
  }

  async function refreshVersion(): Promise<void> {
    appVersion.value = await ipc.getAppVersion();
  }

  /** 首屏：配置 + 网关状态 + 接管状态 + 日志（预设由服务商页按需拉）。 */
  async function refreshAll(): Promise<void> {
    await Promise.all([refreshConfig(), refreshGateway(), refreshTakeover(), refreshLogs()]);
    ready.value = true;
  }

  /** 状态页与日志页轮询：日志 + 网关状态（请求数、端口会变）。 */
  function pollFastState(): Promise<void> {
    if (pollInFlight) return pollInFlight;
    const tick = Promise.all([refreshLogs(), refreshGateway()]).then(() => undefined);
    pollInFlight = tick;
    const settle = () => {
      if (pollInFlight === tick) pollInFlight = null;
    };
    void tick.then(settle, settle);
    return tick;
  }

  function startLogPolling(): void {
    if (pollTimer !== null) return;
    pollTimer = setInterval(() => {
      void pollFastState();
    }, LOG_POLL_INTERVAL_MS);
  }

  function stopLogPolling(): void {
    if (pollTimer === null) return;
    clearInterval(pollTimer);
    pollTimer = null;
  }

  return {
    config,
    gatewayStatus,
    logs,
    presets,
    takeover,
    settingsPaths,
    appVersion,
    loadError,
    dismissLoadError,
    ready,
    providers,
    models,
    aliases,
    roleTargets,
    gatewayRunning,
    gatewayUrl,
    takeoverWithoutGateway,
    providerById,
    modelSpecOf,
    modelOf,
    aliasOf,
    modelByAlias,
    displayNameOf,
    refreshAll,
    refreshConfig,
    refreshGateway,
    refreshLogs,
    refreshTakeover,
    refreshPresets,
    refreshPaths,
    refreshVersion,
    pollFastState,
    startLogPolling,
    stopLogPolling,
  };
});

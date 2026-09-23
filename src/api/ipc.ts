/**
 * CC Router 前端**唯一**的 IPC 出口（Plan 3 Part B1）。
 *
 * 契约来源：`docs/superpowers/plans/2026-09-22-cc-router-03-ui-and-presets.md` §A4
 * （命令名与参数清单已钉死；Part A 正在并行实现，前端不生成、不读 Rust）。
 *
 * 本文件的三条约定：
 *  1. 每个命令一个类型化包装，别处**不允许**出现 `invoke`。
 *  2. 后端 `Result<_, String>` 的 `Err(String)` 在这里统一转成抛出的 `Error`
 *     （`errorText()` 负责把任意抛出物转成可读文本）。
 *  3. 参数用 camelCase（Tauri 把 JS 的 camelCase 映射到 Rust 的 snake_case 形参）；
 *     `Option<T>` 形参给 `null` 与缺省都表示 `None`，因此可选参数一律「有值才带」，
 *     避免把一个非 `Option` 的 Rust 形参撞成 `invalid type: null`。
 *
 * **字段名的不确定性（已知计划缺陷，见报告）**：A4 钉死了命令名与参数清单，但没钉死
 * `PresetDto` / `NewProviderDto` / `FetchPickDto` / `TestResultDto` / `FetchOutcomeDto` /
 * `BackupDto` / `SettingsPathsDto` 的**字段级形状**。本文件按 A1–A3 的 Rust 结构体定义
 * 加全仓惯例（`#[serde(rename_all = "camelCase")]`）推断；对无法唯一确定的少数形状
 * （`AttemptOutcome` 枚举的序列化形式、备份 DTO、可空附加字段）保留宽容归一化，
 * 以便 Part A 的字段名与推断不同时前端仍能显示真实值而不是空白。
 */
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { APP_VERSION_FALLBACK } from "../constants";

// ---------------------------------------------------------------- 配置模型（spec §5.2）

export type AuthStyle = "both" | "x-api-key" | "bearer";
export type RoleName = "main" | "fast" | "subagent";
export type UnknownModelPolicy = "default" | "error";
/** 子 Agent 的三选一（spec §8.2 / Plan 3 B5）。 */
export type AgentModelChoice = "inherit" | "subagent_default" | "alias";

export interface TargetDto {
  providerId: string;
  modelId: string;
}

export interface ModelSpec {
  id: string;
  name: string;
  alias: string;
  context1m: boolean;
  contextWindow: number | null;
  maxTokens: number | null;
}

/** 最近一次「一键获取模型」的元信息（`provider.modelsFetch`）。 */
export interface ModelsFetchInfo {
  lastAt: string;
  lastUrl: string;
  count: number;
}

export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  authStyle: AuthStyle;
  presetId: string | null;
  modelsUrl: string | null;
  modelsFetch: ModelsFetchInfo | null;
  models: ModelSpec[];
}

export interface Roles {
  main: TargetDto | null;
  fast: TargetDto | null;
  subagent: TargetDto | null;
}

export interface ExtraRoute {
  alias: string;
  providerId: string;
  modelId: string;
}

export interface GatewayConfig {
  bind: string;
  port: number;
  localToken: string;
  maxRequestBodyBytes: number;
  connectTimeoutMs: number;
  idleTimeoutMs: number;
}

export interface UiConfig {
  closeToTray: boolean;
  autostart: boolean;
  requestLogToFile: boolean;
  restoreOnExit: boolean;
}

export interface TakeoverConfigState {
  enabled: boolean;
  appliedAt: string | null;
  backupFile: string | null;
  settingsKeys: unknown;
  agentFiles: unknown;
  manifest?: unknown;
}

export interface Config {
  version: number;
  gateway: GatewayConfig;
  providers: Provider[];
  roles: Roles;
  extraRoutes: ExtraRoute[];
  onUnknownModel: UnknownModelPolicy;
  defaultTarget: TargetDto | null;
  takeover: TakeoverConfigState;
  ui: UiConfig;
}

// ---------------------------------------------------------------- 预设（A1 / spec §5.6）

export interface TemplateValue {
  label?: string | null;
  placeholder?: string | null;
  defaultValue?: string | null;
  editorValue?: string | null;
}

/** A1 未定义 `DefaultModel` 的具体形状，故两者都接受，用 `defaultModelId()` 归一。 */
export type PresetDefaultModel = string | { id?: string | null; name?: string | null };

export interface Preset {
  id: string;
  name: string;
  category: string;
  baseUrl: string;
  authStyle: AuthStyle;
  modelsUrl: string | null;
  apiKeyUrl: string | null;
  websiteUrl: string | null;
  icon: string | null;
  iconColor: string | null;
  apiKeyField: string | null;
  apiFormat: string | null;
  templateValues: Record<string, TemplateValue | string> | null;
  defaultEnv: Record<string, string>;
  supported: boolean;
  unsupportedReason: string | null;
  verifiedAt: string | null;
  defaultModels: PresetDefaultModel[];
}

// ---------------------------------------------------------------- 运行时状态

export interface GatewayStatus {
  running: boolean;
  port: number | null;
  requestsServed: number;
}

/** `recent_logs` 的一条（spec §6.7）。 */
export interface LogEntry {
  ts: string;
  method: string;
  path: string;
  requestedModel: string;
  matchedBy: string;
  role: string | null;
  alias: string;
  providerId: string;
  upstreamModel: string;
  status: number | null;
  stream: boolean;
  latencyMs: number;
  error: string | null;
}

export type TakeoverStateName = "applied" | "stale" | "not_applied";

export interface TakeoverStatus {
  state: TakeoverStateName;
  gatewayUrl: string;
  found: string | null;
  appliedAt: string | null;
}

export interface RestoreResult {
  path: string;
  changedKeys: string[];
}

export interface AgentInfo {
  path: string;
  name: string | null;
  description: string | null;
  model: string | null;
}

// ---------------------------------------------------------------- 命令的入参 / 出参

/** `provider_add` 的入参；除 `baseUrl` + `apiKey`（spec §5.8 的最少必填）外都可推导。 */
export interface NewProviderDto {
  baseUrl: string;
  apiKey: string;
  id?: string;
  name?: string;
  authStyle?: AuthStyle;
  presetId?: string;
  modelsUrl?: string;
}

/** `models_add_many` 的一条；字段名容忍见文件头。 */
export interface FetchPickDto {
  id: string;
  name?: string;
  contextWindow?: number;
  maxTokens?: number;
}

export interface TestResult {
  ok: boolean;
  status: number | null;
  latencyMs: number;
  message: string | null;
}

export interface FetchedModel {
  id: string;
  displayName: string | null;
  contextWindow: number | null;
  maxTokens: number | null;
  looksNonChat: boolean;
}

export interface FetchAttempt {
  url: string;
  outcome: unknown;
}

export interface FetchOutcome {
  models: FetchedModel[];
  usedUrl: string;
  attempts: FetchAttempt[];
}

/** `backups_list` 的一项；字段名容忍见 `backupLabel()`。 */
export interface BackupDto {
  fileName?: string | null;
  name?: string | null;
  path?: string | null;
  sizeBytes?: number | null;
  modifiedAt?: string | null;
  createdAt?: string | null;
}

/** `settings_paths` 的返回；字段名容忍见 `settingsPathValue()`。 */
export interface SettingsPaths {
  configPath?: string | null;
  settingsPath?: string | null;
  backupsDir?: string | null;
  agentsDir?: string | null;
}

// ---------------------------------------------------------------- 底层封装

function compact(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (value !== undefined && value !== null) out[key] = value;
  }
  return out;
}

/** 把任意抛出物（后端 `Err(String)`、`Error`、别的）转成可读文本。 */
export function errorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object") {
    try {
      return JSON.stringify(err);
    } catch {
      return String(err);
    }
  }
  return String(err);
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    // 后端返回 Err(String) 时，Tauri 直接以该字符串 reject；统一成 Error 便于 UI 处理。
    throw new Error(errorText(err));
  }
}

// ---------------------------------------------------------------- 配置 / 网关 / 日志

export function getConfig(): Promise<Config> {
  return call<Config>("get_config");
}

export function saveConfig(config: Config): Promise<void> {
  return call<void>("save_config", { config });
}

export function gatewayStatus(): Promise<GatewayStatus> {
  return call<GatewayStatus>("gateway_status");
}

export function gatewayStart(): Promise<number> {
  return call<number>("gateway_start");
}

export function gatewayStop(): Promise<void> {
  return call<void>("gateway_stop");
}

export function gatewayRestart(): Promise<number> {
  return call<number>("gateway_restart");
}

export function recentLogs(limit = 200): Promise<LogEntry[]> {
  return call<LogEntry[]>("recent_logs", { limit });
}

// ---------------------------------------------------------------- 接管（spec §7）

export function takeoverStatus(): Promise<TakeoverStatus> {
  return call<TakeoverStatus>("takeover_status");
}

export function takeoverApply(): Promise<TakeoverStatus> {
  return call<TakeoverStatus>("takeover_apply");
}

export function takeoverRestore(): Promise<RestoreResult> {
  return call<RestoreResult>("takeover_restore");
}

// ---------------------------------------------------------------- 预设（A1）

export function presetsList(): Promise<Preset[]> {
  return call<Preset[]>("presets_list");
}

// ---------------------------------------------------------------- 服务商与模型（A2）

export function providerAdd(provider: NewProviderDto): Promise<string> {
  // 展开成匿名对象字面量再 compact：接口类型没有索引签名，直接 cast 会被告知"不够重叠"。
  return call<string>("provider_add", { new: compact({ ...provider }) });
}

export function providerUpdate(provider: Provider): Promise<void> {
  return call<void>("provider_update", { provider });
}

export function providerRemove(id: string): Promise<void> {
  return call<void>("provider_remove", { id });
}

export function modelAdd(
  providerId: string,
  modelId: string,
  options: {
    name?: string | null;
    context1m?: boolean;
    contextWindow?: number | null;
    maxTokens?: number | null;
  } = {},
): Promise<string> {
  return call<string>(
    "model_add",
    compact({
      providerId,
      modelId,
      name: options.name,
      context1m: options.context1m ?? false,
      contextWindow: options.contextWindow,
      maxTokens: options.maxTokens,
    }),
  );
}

export function modelRemove(providerId: string, modelId: string): Promise<void> {
  return call<void>("model_remove", compact({ providerId, modelId }));
}

export function roleSet(role: RoleName, target: TargetDto | null): Promise<void> {
  return call<void>("role_set", compact({ role, target }));
}

export async function providerTest(providerId: string, modelId?: string | null): Promise<TestResult> {
  const raw = await call<{
    ok?: boolean;
    status?: number | null;
    latencyMs?: number | null;
    message?: string | null;
  }>("provider_test", compact({ providerId, modelId }));
  return {
    ok: raw?.ok === true,
    status: typeof raw?.status === "number" ? raw.status : null,
    latencyMs: typeof raw?.latencyMs === "number" ? raw.latencyMs : 0,
    message: typeof raw?.message === "string" ? raw.message : null,
  };
}

// ---------------------------------------------------------------- 一键获取模型（A3）

function normalizeFetchedModel(raw: unknown): FetchedModel | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const id = typeof o.id === "string" ? o.id : typeof o.name === "string" ? o.name : null;
  if (!id) return null;
  const display =
    typeof o.displayName === "string" ? o.displayName : typeof o.name === "string" ? o.name : null;
  const num = (v: unknown): number | null => (typeof v === "number" ? v : null);
  return {
    id,
    displayName: display && display !== id ? display : null,
    contextWindow: num(o.contextWindow) ?? num(o.context_length) ?? num(o.maxInputTokens),
    maxTokens: num(o.maxTokens) ?? num(o.max_output_tokens),
    looksNonChat: o.looksNonChat === true,
  };
}

function normalizeAttempt(raw: unknown): FetchAttempt | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const url = typeof o.url === "string" ? o.url : typeof o.URL === "string" ? o.URL : "";
  if (!url) return null;
  return { url, outcome: o.outcome };
}

export async function modelsFetch(providerId: string, modelsUrl?: string | null): Promise<FetchOutcome> {
  const raw = await call<{
    models?: unknown;
    usedUrl?: string | null;
    attempts?: unknown;
  }>("models_fetch", compact({ providerId, modelsUrl }));
  const models = Array.isArray(raw?.models)
    ? raw.models.map(normalizeFetchedModel).filter((m): m is FetchedModel => m !== null)
    : [];
  const attempts = Array.isArray(raw?.attempts)
    ? raw.attempts.map(normalizeAttempt).filter((a): a is FetchAttempt => a !== null)
    : [];
  return {
    models,
    usedUrl: typeof raw?.usedUrl === "string" ? raw.usedUrl : "",
    attempts,
  };
}

export function modelsAddMany(providerId: string, models: FetchPickDto[]): Promise<string[]> {
  return call<string[]>("models_add_many", { providerId, models });
}

/** 把一条拉取结果转成 `models_add_many` 的入参（空字段不带，避免撞上非 `Option` 形参）。 */
export function pickFromFetched(model: FetchedModel): FetchPickDto {
  const pick: FetchPickDto = { id: model.id };
  if (model.displayName) pick.name = model.displayName;
  if (typeof model.contextWindow === "number") pick.contextWindow = model.contextWindow;
  if (typeof model.maxTokens === "number") pick.maxTokens = model.maxTokens;
  return pick;
}

/** `AttemptOutcome`（`Http(u16)` / `Error(String)` / 已被 DTO 摊平的字符串或数字）→ 可读文本。 */
export function attemptOutcomeText(outcome: unknown): string {
  if (typeof outcome === "number") return `HTTP ${outcome}`;
  if (typeof outcome === "string") return outcome;
  if (outcome && typeof outcome === "object") {
    const o = outcome as Record<string, unknown>;
    const http = o.Http ?? o.http ?? o.HTTP;
    if (typeof http === "number") return `HTTP ${http}`;
    const message = o.Error ?? o.error;
    if (typeof message === "string") return message;
    try {
      return JSON.stringify(outcome);
    } catch {
      return String(outcome);
    }
  }
  return "未知结果";
}

// ---------------------------------------------------------------- 令牌 / 备份 / 路径

export function tokenRegenerate(): Promise<string> {
  return call<string>("token_regenerate");
}

export function backupsList(): Promise<BackupDto[]> {
  return call<BackupDto[]>("backups_list");
}

export function settingsPaths(): Promise<SettingsPaths> {
  return call<SettingsPaths>("settings_paths");
}

/** 备份列表的显示名；A4 未钉死字段名，故按候选顺序取名。 */
export function backupLabel(backup: BackupDto): string {
  const candidates = [backup.fileName, backup.name, backup.path];
  for (const value of candidates) {
    if (typeof value === "string" && value.trim()) {
      return value.split(/[\\/]/).pop() || value;
    }
  }
  return "（未知文件）";
}

/** `settings_paths` 的任一字段；A4 未钉死字段名，故按候选键取值。 */
export function settingsPathValue(paths: SettingsPaths | null, key: "config" | "settings" | "backups" | "agents"): string {
  if (!paths) return "";
  const table: Record<typeof key, Array<keyof SettingsPaths>> = {
    config: ["configPath"],
    settings: ["settingsPath"],
    backups: ["backupsDir"],
    agents: ["agentsDir"],
  };
  for (const field of table[key]) {
    const value = paths[field];
    if (typeof value === "string" && value.trim()) return value;
  }
  return "";
}

// ---------------------------------------------------------------- 子 Agent（spec §8）

export function agentsList(): Promise<AgentInfo[]> {
  return call<AgentInfo[]>("agents_list");
}

export function agentSetModel(
  path: string,
  choice: AgentModelChoice,
  alias?: string | null,
): Promise<void> {
  return call<void>("agent_set_model", compact({ path, choice, alias }));
}

export function agentCreate(input: {
  name: string;
  description: string;
  choice: AgentModelChoice;
  alias?: string | null;
  body: string;
}): Promise<string> {
  return call<string>(
    "agent_create",
    compact({
      name: input.name,
      description: input.description,
      choice: input.choice,
      alias: input.alias,
      body: input.body,
    }),
  );
}

export function agentDelete(path: string): Promise<void> {
  return call<void>("agent_delete", { path });
}

// ---------------------------------------------------------------- 应用信息

/**
 * 应用版本。A4 没有版本命令，`@tauri-apps/api/app` 的 `getVersion()` 走 core IPC；
 * 若该权限/命令不可用则退回 `APP_VERSION_FALLBACK`（与 `tauri.conf.json` 同值）。
 */
export async function getAppVersion(): Promise<string> {
  try {
    return await getVersion();
  } catch {
    return APP_VERSION_FALLBACK;
  }
}

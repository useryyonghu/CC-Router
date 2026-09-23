/**
 * 预设相关的纯函数（Plan 3 B3 / spec §5.6）——**只做展示与替换，不发 IPC**。
 *
 * 放在视图外面是为了让「分组 / 搜索 / 模板变量替换」这三段逻辑不散落在模板里，
 * 也便于将来加单测（前端当前没有测试框架，Plan 3 的验证工具是 `pnpm build`）。
 */
import type { AuthStyle, Preset, PresetDefaultModel, TemplateValue } from "./api/ipc";

const CATEGORY_LABELS: Record<string, string> = {
  official: "官方",
  cn_official: "国内官方",
  third_party: "第三方",
  aggregator: "聚合 / 中转",
  cloud_provider: "云厂商",
};

export function categoryLabel(category: string): string {
  return CATEGORY_LABELS[category] ?? category ?? "其它";
}

export interface PresetGroup {
  category: string;
  label: string;
  presets: Preset[];
}

/**
 * 按 `category` 分组。A1 已保证后端返回的顺序是 `official` → 合作伙伴 → 其它、
 * 组内按名称；这里只把 `official` 钉到最前（spec §10.2），其余保持后端顺序（`sort` 稳定）。
 */
export function groupPresets(presets: Preset[]): PresetGroup[] {
  const buckets = new Map<string, Preset[]>();
  for (const preset of presets) {
    const category = preset.category || "other";
    const bucket = buckets.get(category);
    if (bucket) bucket.push(preset);
    else buckets.set(category, [preset]);
  }
  return [...buckets.entries()]
    .map(([category, items]) => ({ category, label: categoryLabel(category), presets: items }))
    .sort((a, b) => rank(a.category) - rank(b.category));
}

function rank(category: string): number {
  return category === "official" ? 0 : 1;
}

/** 搜索框过滤：名称 / id / 分类 / baseUrl 任一命中即保留（大小写不敏感）。 */
export function filterPresets(presets: Preset[], query: string): Preset[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return presets;
  return presets.filter((preset) => {
    const haystack = [preset.name, preset.id, preset.category, categoryLabel(preset.category), preset.baseUrl]
      .filter((v): v is string => typeof v === "string")
      .join(" ")
      .toLowerCase();
    return haystack.includes(needle);
  });
}

/** 预设的「已验证」角标文案（spec §5.6 的诚实语义）。 */
export function presetVerificationText(preset: Preset): string {
  return preset.verifiedAt
    ? `本机已验证（${preset.verifiedAt}）`
    : "来自 cc-switch 预设，未在本机验证";
}

export function authStyleLabel(style: AuthStyle | null | undefined): string {
  switch (style) {
    case "x-api-key":
      return "只发 x-api-key";
    case "bearer":
      return "只发 Authorization: Bearer";
    case "both":
      return "同时发 x-api-key 与 Bearer";
    default:
      return "—";
  }
}

// ---------------------------------------------------------------- 模板变量（`${VAR}`）

function collectVariables(into: string[], text: string | null | undefined): void {
  if (!text) return;
  for (const match of text.matchAll(/\$\{([A-Za-z0-9_]+)\}/g)) {
    if (match[1] && !into.includes(match[1])) into.push(match[1]);
  }
}

/**
 * 需要向用户索要的变量 = **地址里真的会出现**的变量（`baseUrl` / `modelsUrl`）。
 *
 * 只收集这两处，因为 `resolvePresetFields()` 也只替换这两处，而 `NewProviderDto`
 * 没有 `env` 字段（后端 `NewProvider` 也不接受 env）：`defaultEnv` 里的 `${VAR}`
 * ——例如 AWS Bedrock 的 `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`——**填了也存不进**。
 * 与其索要一个注定被丢弃的密钥，不如不问（I3；诚实优先）。
 */
export function templateVariables(preset: Preset): string[] {
  const names: string[] = [];
  collectVariables(names, preset.baseUrl);
  collectVariables(names, preset.modelsUrl);
  return names;
}

/** 预设声明了、但本版本**不会保存**的变量（只出现在 `defaultEnv` / `templateValues` 里）。 */
export function unstoredTemplateVariables(preset: Preset): string[] {
  const used = templateVariables(preset);
  const declared: string[] = [];
  for (const value of Object.values(preset.defaultEnv ?? {})) collectVariables(declared, value);
  for (const key of Object.keys(preset.templateValues ?? {})) {
    if (key && !declared.includes(key)) declared.push(key);
  }
  return declared.filter((name) => !used.includes(name));
}

export function templateValueOf(preset: Preset, name: string): TemplateValue | null {
  const raw = preset.templateValues?.[name];
  if (!raw) return null;
  return typeof raw === "string" ? { label: raw } : raw;
}

export function templateLabel(preset: Preset, name: string): string {
  const value = templateValueOf(preset, name);
  const label = typeof value?.label === "string" && value.label.trim() ? value.label : name;
  return `${label}（\${${name}}）`;
}

export function templatePlaceholder(preset: Preset, name: string): string {
  const value = templateValueOf(preset, name);
  const text = value?.placeholder ?? value?.defaultValue ?? value?.editorValue ?? "";
  return typeof text === "string" ? text : "";
}

/**
 * 变量输入框的**初始值** —— 只取真正的默认值，**绝不用 `placeholder`**。
 *
 * `placeholder` 是给用户看的"示例"（例如 kat-coder 的 `ep-xxx-xxx`）。把它当成初始值会
 * 让用户直接点「替换并继续」就把示例写进 Base URL；更糟的是 `resolvePresetFields()`
 * 只把**空值**算作"没填"，于是那句"还有变量没填"的安全网永远不会触发 ——
 * 界面看起来填好了，实际填的是示例。
 */
export function templateInitialValue(preset: Preset, name: string): string {
  const value = templateValueOf(preset, name);
  const text = value?.defaultValue ?? value?.editorValue ?? "";
  return typeof text === "string" ? text : "";
}

/** 用输入值替换 `${VAR}`；未填的变量保留原样，并由 `unresolvedVariables()` 报出。 */
export function fillTemplate(text: string | null | undefined, values: Record<string, string>): string {
  if (!text) return "";
  return text.replace(/\$\{([A-Za-z0-9_]+)\}/g, (whole, name: string) => {
    const value = values[name];
    return value !== undefined && value.trim() !== "" ? value.trim() : whole;
  });
}

export interface ResolvedPresetFields {
  baseUrl: string;
  modelsUrl: string | null;
  unresolved: string[];
}

/** 把预设落地成可直接提交的字段（B3 的「替换 `${VAR}` 后再落地」）。 */
export function resolvePresetFields(preset: Preset, values: Record<string, string>): ResolvedPresetFields {
  const baseUrl = fillTemplate(preset.baseUrl, values);
  const rawModelsUrl = preset.modelsUrl ? fillTemplate(preset.modelsUrl, values) : "";
  // `templateVariables()` 只返回地址里的变量，所以「没填」与「地址里还留着 ${VAR}」等价。
  const unresolved = templateVariables(preset).filter((name) => {
    const value = values[name];
    return value === undefined || value.trim() === "";
  });
  return { baseUrl, modelsUrl: rawModelsUrl ? rawModelsUrl : null, unresolved };
}

// ---------------------------------------------------------------- 离线兜底模型

/** A1 未定义 `DefaultModel` 的形状，故字符串与对象都接受。 */
export function defaultModelId(model: PresetDefaultModel | null | undefined): string | null {
  if (typeof model === "string") return model.trim() ? model.trim() : null;
  if (model && typeof model === "object") {
    const candidate = model.id ?? model.name;
    if (typeof candidate === "string" && candidate.trim()) return candidate.trim();
  }
  return null;
}

export function defaultModelIds(preset: Preset | null | undefined): string[] {
  if (!preset) return [];
  const out: string[] = [];
  for (const model of preset.defaultModels ?? []) {
    const id = defaultModelId(model);
    if (id && !out.includes(id)) out.push(id);
  }
  return out;
}

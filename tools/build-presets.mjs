#!/usr/bin/env node
// 生成 src-tauri/presets.json —— Plan 3 A1 的一次性生成器（Node，无依赖）。
//
//   node tools/build-presets.mjs
//
// 输入：docs/reference/cc-switch-presets.raw.json（来自 farion1231/cc-switch，MIT，只读参考）
// 输出：src-tauri/presets.json（入库）
//
// 本脚本必须**可重复执行且结果一致**（幂等）：所有集合（defaultEnv、templateValues）都按
// 固定顺序写出，JSON 为 2 空格缩进 + 末尾换行，因此重跑后 `git diff --stat` 为空。
//
// 设计要点（对应 spec §5.6 与 Plan 3 A1）：
// 1. 剥离联盟/追踪参数（aff / ref / invitecode / ch / utm_*），**不搬运 cc-switch 的推广关系**；
//    剥离后仍是纯跳转/推广路径的 apiKeyUrl 置 null，UI 改为展示 websiteUrl。
// 2. supported:false 的 5 条保留但带原因（将来加协议转换可直接启用）。
// 3. verifiedAt 只填本机真正实测过的 3 条，其余一律 null（不谎称已验证）。
// 4. 预设绝不包含密钥：上游 env 里的 ANTHROPIC_AUTH_TOKEN / ANTHROPIC_API_KEY 都是空串，
//    本脚本原样保留（空串不是密钥），不新增任何密钥字段。

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const RAW_PATH = join(ROOT, 'docs', 'reference', 'cc-switch-presets.raw.json');
const OUT_PATH = join(ROOT, 'src-tauri', 'presets.json');

/** 联盟/追踪参数：整段删除（spec §5.6「链接处理」）。 */
const TRACKING_PARAM = /^(aff|ref|invitecode|ch|utm_)/i;

/** 明确的推广跳转路径前缀（多段用 `a/b` 表示）。 */
const REFERRAL_PATH_PREFIXES = ['go', 'i', 'r', 'invite', 'agent/register', 'ref'];

/** 取 defaultModels 的 env 键（Plan 3 A1 规则 8）。 */
const DEFAULT_MODEL_KEYS = [
  'ANTHROPIC_MODEL',
  'ANTHROPIC_DEFAULT_OPUS_MODEL',
  'ANTHROPIC_DEFAULT_SONNET_MODEL',
  'ANTHROPIC_DEFAULT_HAIKU_MODEL',
  'ANTHROPIC_SMALL_FAST_MODEL',
];

/** 本机实测过的 3 条（spec §5.6「verifiedAt 的诚实语义」）。 */
const VERIFIED_IDS = new Set(['claude-official', 'deepseek', 'xiaomi-mimo']);

/** 非 [a-z0-9-] → `-`，折叠，去首尾（Plan 3 A1 规则 1）。 */
function slug(name) {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '');
}

/** 重名冲突时加 `-2`、`-3`…（当前 93 条无冲突，脚本仍保持该行为）。 */
function makeUniqueIdFactory() {
  const used = new Map();
  return (name) => {
    const base = slug(name) || 'preset';
    const n = (used.get(base) ?? 0) + 1;
    used.set(base, n);
    return n === 1 ? base : `${base}-${n}`;
  };
}

function parseUrl(url) {
  try {
    return new URL(url);
  } catch {
    return null;
  }
}

/**
 * 删除查询串里的联盟/追踪参数，保留其它参数与 fragment。
 * 返回 `{ url, removed }`；`removed` 为被删参数个数（`url` 为 null 表示输入为空）。
 */
function stripTracking(url) {
  if (!url) return { url: null, removed: 0 };
  const parsed = parseUrl(url);
  if (!parsed) return { url, removed: 0 };
  const removed = [...parsed.searchParams.keys()].filter((k) => TRACKING_PARAM.test(k));
  if (removed.length === 0) return { url, removed: 0 };
  for (const key of removed) parsed.searchParams.delete(key);
  return { url: parsed.toString(), removed: removed.length };
}

/**
 * `apiKeyUrl` 是否已无法还原为厂商控制台的正常 URL（spec §5.6 规则 2）。
 * 判定顺序固定，便于 review：
 *  a. 路径形如 `/go/...`；
 *  b. 原本只是「根路径 + 追踪参数」（剥离后查询为空、且没有正常路径）；
 *  c. 路径命中明确的推广前缀（`/i/`、`/r/`、`/invite/`、`/agent/register/`…）；
 *  d. 单段不透明短码（段内出现大写字母，如 `/VjM74M`、`/nMvAvy`）。
 */
function isPureReferral(strippedUrl, rawUrl) {
  const parsed = parseUrl(strippedUrl);
  if (!parsed) return false;
  const segments = parsed.pathname.split('/').filter(Boolean);
  const lower = segments.map((s) => s.toLowerCase());
  const joined = lower.join('/');

  // 单段前缀（`go`/`i`/`r`）整段相等才算，避免把 `/google`、`/registration` 误判成推广链接。
  const hasPrefix = REFERRAL_PATH_PREFIXES.some((p) =>
    p.includes('/') ? joined.startsWith(p) : lower[0] === p,
  );
  if (hasPrefix) return true;

  const raw = parseUrl(rawUrl);
  if (raw && raw.search && !parsed.search && segments.length === 0) return true;

  if (segments.length === 1 && /[A-Z]/.test(segments[0])) return true;

  return false;
}

/** supported / unsupportedReason（Plan 3 A1 规则 3，优先级固定）。 */
function supportOf(raw) {
  const apiFormat = raw.apiFormat ?? 'anthropic';
  const requiresOAuth = raw.requiresOAuth === true;
  const providerType = raw.providerType ?? null;

  if (apiFormat === 'anthropic' && !requiresOAuth && !providerType) {
    return { supported: true, unsupportedReason: null };
  }
  if (requiresOAuth) {
    return { supported: false, unsupportedReason: '需要 OAuth，本期仅支持 API Key' };
  }
  if (apiFormat === 'gemini_native') {
    return { supported: false, unsupportedReason: '需要 Gemini 原生格式转换' };
  }
  if (apiFormat === 'openai_chat') {
    return { supported: false, unsupportedReason: '需要 OpenAI Chat 格式转换' };
  }
  if (apiFormat === 'openai_responses') {
    return { supported: false, unsupportedReason: '需要 OpenAI Responses 格式转换' };
  }
  return { supported: false, unsupportedReason: `需要 ${apiFormat} 格式转换` };
}

/**
 * defaultEnv 的值统一成字符串。
 *
 * 上游 `settingsConfig.env` 里 Longcat / MiniMax / MiniMax en 三条的
 * `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` 是数字 `1`（JS 里能用，但 env 的语义本就是字符串，
 * 且 Claude Code 的 settings.json 也只接受字符串）。Plan 3 A1 把 `defaultEnv` 定义为
 * `BTreeMap<String, String>`，所以这里按 JSON 标量形式转成字符串（`1` → `"1"`）；
 * 对象/数组/null 无法表达成 env，直接丢弃。
 */
function envValue(value) {
  if (typeof value === 'string') return value;
  if (value === null || typeof value === 'object') return null;
  return String(value);
}

function envOf(rawEnv) {
  const out = {};
  for (const key of Object.keys(rawEnv).sort()) {
    const value = envValue(rawEnv[key]);
    if (value !== null) out[key] = value;
  }
  return out;
}

/** templateValues 原样带上，但键序固定，便于幂等 diff。 */
function templateValuesOf(raw) {
  if (!raw.templateValues) return null;
  const out = {};
  for (const key of Object.keys(raw.templateValues).sort()) {
    const src = raw.templateValues[key] ?? {};
    const entry = {};
    if (src.label !== undefined) entry.label = src.label;
    if (src.placeholder !== undefined) entry.placeholder = src.placeholder;
    if (src.defaultValue !== undefined) entry.defaultValue = src.defaultValue;
    if (src.editorValue !== undefined) entry.editorValue = src.editorValue;
    out[key] = entry;
  }
  return out;
}

/** defaultModels：从 defaultEnv 的模型键取值、去重、保序（Plan 3 A1 规则 8）。 */
function defaultModelsOf(defaultEnv) {
  const seen = new Set();
  const out = [];
  for (const key of DEFAULT_MODEL_KEYS) {
    const value = defaultEnv[key];
    if (typeof value !== 'string') continue;
    const model = value.trim();
    if (value !== model || model === '' || model.includes('${')) continue;
    if (seen.has(model)) continue;
    seen.add(model);
    out.push({ id: model });
  }
  return out;
}

function convert(raw, uniqueId) {
  // 规则 2：baseUrl 取自 settingsConfig.env.ANTHROPIC_BASE_URL；Claude Official 的 env 是 {}。
  const rawEnv = (raw.settingsConfig && raw.settingsConfig.env) || {};
  const isClaudeOfficial = !rawEnv.ANTHROPIC_BASE_URL;
  const baseUrl = isClaudeOfficial ? 'https://api.anthropic.com' : rawEnv.ANTHROPIC_BASE_URL;
  const authStyle = isClaudeOfficial ? 'x-api-key' : 'both';
  const defaultEnv = isClaudeOfficial
    ? { ANTHROPIC_BASE_URL: 'https://api.anthropic.com' }
    : envOf(rawEnv);

  const website = stripTracking(raw.websiteUrl);
  const apiKey = stripTracking(raw.apiKeyUrl);
  const apiKeyUrl =
    apiKey.url && isPureReferral(apiKey.url, raw.apiKeyUrl) ? null : apiKey.url;

  const id = uniqueId(raw.name);
  const { supported, unsupportedReason } = supportOf(raw);

  return {
    id,
    name: raw.name,
    category: raw.category ?? 'third_party',
    primePartner: raw.primePartner === true,
    baseUrl,
    authStyle,
    modelsUrl: raw.modelsUrl ?? null,
    apiKeyUrl,
    websiteUrl: website.url,
    icon: raw.icon ?? null,
    iconColor: raw.iconColor ?? null,
    apiKeyField: raw.apiKeyField ?? 'ANTHROPIC_AUTH_TOKEN',
    apiFormat: raw.apiFormat ?? 'anthropic',
    templateValues: templateValuesOf(raw),
    defaultEnv,
    supported,
    unsupportedReason,
    verifiedAt: VERIFIED_IDS.has(id) ? '2026-09-22' : null,
    defaultModels: defaultModelsOf(defaultEnv),
  };
}

function main() {
  const raw = JSON.parse(readFileSync(RAW_PATH, 'utf8'));
  if (!Array.isArray(raw)) throw new Error(`${RAW_PATH} 顶层必须是数组`);

  const uniqueId = makeUniqueIdFactory();
  const presets = raw.map((preset) => convert(preset, uniqueId));

  const doc = {
    version: 1,
    source: 'farion1231/cc-switch',
    license: 'MIT',
    sourceRef: 'main',
    presets,
  };

  writeFileSync(OUT_PATH, `${JSON.stringify(doc, null, 2)}\n`, 'utf8');
  const unsupported = presets.filter((p) => !p.supported).length;
  const templated = presets.filter((p) => p.templateValues).length;
  const verified = presets.filter((p) => p.verifiedAt).length;
  const tracked = presets.filter(
    (p) =>
      [p.websiteUrl, p.apiKeyUrl, p.baseUrl].some(
        (u) => typeof u === 'string' && /[?&](aff|ref|invitecode|ch|utm_)/i.test(u),
      ),
  ).length;
  console.log(
    `presets.json: ${presets.length} 条（不支持 ${unsupported} / 模板 ${templated} / 已验证 ${verified} / 残留追踪参数 ${tracked}）`,
  );
}

main();

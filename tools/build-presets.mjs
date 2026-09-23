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
// 1. `websiteUrl` / `apiKeyUrl` 按**功能性参数白名单**清洗（见 FUNCTIONAL_PARAM），
//    其余参数一律删除并逐条打印；推广链接（路径段或参数值命中 PROMOTION_MARK）置 null，UI 改展示
//    websiteUrl。**不再用"追踪参数黑名单"** —— 理由见 sanitizeLink 的注释。
// 2. supported:false 的 7 条保留但带原因（将来加协议转换可直接启用）：
//    5 条来自 apiFormat / requiresOAuth 规则，2 条来自"Claude Code 被要求直连厂商"规则。
// 3. verifiedAt 只填本机真正实测过的 3 条，其余一律 null（不谎称已验证）。
// 4. 预设绝不包含密钥：上游 env 里的 ANTHROPIC_AUTH_TOKEN / ANTHROPIC_API_KEY 都是空串，
//    本脚本原样保留（空串不是密钥），不新增任何密钥字段。

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const RAW_PATH = join(ROOT, 'docs', 'reference', 'cc-switch-presets.raw.json');
const OUT_PATH = join(ROOT, 'src-tauri', 'presets.json');

/**
 * 功能性查询参数**白名单**：只有人类确认过"删了链接就指向错误页面"的参数才在这里。
 *
 *   `apikey`   火山控制台把 `{}` 以 `%7B%7D` 嵌在查询串里，删了就打不开密钥页
 *   `redirect` FennoAI 注册后的落地目标
 *   `tab`      AICodeWith 的 `tab=register` 直接开注册页
 *
 * 要新增必须写明理由：白名单是"默认删除"，加错一项就等于把一个未知参数放进交付物。
 */
const FUNCTIONAL_PARAM = new Set(['apikey', 'redirect', 'tab']);

/**
 * 推广归因标志：上游项目名（cc-switch / ccswitch / cc_switch 之外的连写形式）或它的代号。
 * 路径段与**参数值**都要查 —— 参数名是开放集合，黑名单补不完。
 */
const PROMOTION_MARK = /cc-?switch|ccs/i;

/**
 * 追踪参数名（**精确匹配** + `utm_` 前缀）。
 *
 * 只在 `baseUrl` 上还用得着：`baseUrl` 是厂家端点，spec §5.6 只要求剥离链接栏位，
 * 这里仅用于报告"交付物里还剩多少条带追踪参数的 URL"。
 *
 * 必须精确匹配：前缀匹配会让 `charset` / `channel` / `refresh` 这类正常参数被误报。
 */
const TRACKING_NAMES = new Set([
  'aff',
  'ref',
  'invitecode',
  'ic',
  'ytag',
  'ac',
  'rc',
  'from',
  'code',
  'source',
  'ch',
]);

function isTrackingName(name) {
  const key = name.toLowerCase();
  return TRACKING_NAMES.has(key) || key.startsWith('utm_');
}

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
 * 删除查询串里不在白名单内的参数，保留其它参数与 fragment。
 * 返回 `{ url, removed }`；`removed` 为被删参数个数（`url` 为 null 表示输入为空）。
 */
function stripNonFunctional(url) {
  if (!url) return { url: null, removed: 0, dropped: [] };
  const parsed = parseUrl(url);
  if (!parsed) return { url, removed: 0, dropped: [] };
  const dropped = [...parsed.searchParams.entries()].filter(
    ([k]) => !FUNCTIONAL_PARAM.has(k.toLowerCase()),
  );
  if (dropped.length === 0) return { url, removed: 0, dropped: [] };
  for (const [key] of dropped) parsed.searchParams.delete(key);
  return { url: parsed.toString(), removed: dropped.length, dropped };
}

/** 推广归因命中：**路径段**命中 `PROMOTION_MARK`（归因长在路径里，删参数也去不掉）。 */
function promotionPathHit(rawUrl) {
  const parsed = parseUrl(rawUrl);
  if (!parsed) return null;
  for (const segment of parsed.pathname.split('/').filter(Boolean)) {
    if (PROMOTION_MARK.test(segment)) return `/${segment}`;
  }
  return null;
}

/**
 * 推广归因命中：**白名单参数**的值命中 `PROMOTION_MARK`。
 *
 * 只看白名单参数。非白名单参数下一步就会被删掉，它的值带着归因一起消失 ——
 * 因此绝不能因为一个"反正要删的"参数值而 null 掉整条链接。上一版规则就是在这里错的：
 * `https://platform.kimi.com?aff=cc-switch` 的 Kimi 平台首页被整条 null，
 * 理由却是那个本来就要被删除的 `aff`。
 */
function promotionKeptParamHit(rawUrl) {
  const parsed = parseUrl(rawUrl);
  if (!parsed) return null;
  for (const [key, value] of parsed.searchParams) {
    if (FUNCTIONAL_PARAM.has(key.toLowerCase()) && PROMOTION_MARK.test(value)) {
      return `${key}=${value}`;
    }
  }
  return null;
}

/** 不透明短链：单段路径且段内出现大写字母（如 `/VjM74M`、`/nMvAvy`）——无从判断它落在哪。 */
function isOpaqueShortLink(url) {
  const parsed = parseUrl(url);
  if (!parsed) return false;
  const segments = parsed.pathname.split('/').filter(Boolean);
  return segments.length === 1 && /[A-Z]/.test(segments[0]);
}

/**
 * 清洗 `websiteUrl` / `apiKeyUrl`（spec §5.6「链接处理」）。
 *
 * **白名单，不是黑名单**：黑名单被独立复审打穿两次 —— 先漏了 7 个参数
 * （ac/rc/code/source/from/ic/ytag），补上后又漏了两条**完全不含参数**的推广链接
 * （PPIO 的 `/activity/ccswitch`、Qiniu 的 `/nMvAvy` 短链）。参数名是开放集合，
 * 黑名单永远补不完；"只保留人类确认过的功能性参数"才是收敛的规则。
 *
 * 四道判定，顺序固定：
 *  1. **路径段**命中推广标志 → `null`。归因长在路径里，清洗路径等于把链接掏空。
 *  2. **白名单参数的值**命中推广标志 → `null`。这个参数我们要保留，污点去不掉。
 *  3. 非白名单参数 → 删除；**值即使命中推广标志也只是随参数一起消失**，链接必须保留
 *     （`aff=cc-switch` 不该毁掉 Kimi 的首页）。有删除才重新序列化，否则原样保留。
 *  4. 纯推广跳转（仅 `apiKeyUrl`）与不透明短链（**两个栏位一视同仁**）→ `null`。
 *     第 4 条对两个栏位用同一判据，是为了避免 Qiniu 那种"同一条 URL、两个栏位两种判决"。
 *
 * 已知误报面（故意保守）：`ccs` 是裸子串，理论上可能出现在无关 token 里。
 * 对**路径**误判的代价只是少一条链接（保守方向，可接受）；对**保留参数的值**，
 * 现在这 93 条里不存在这种取值。真出现时应当收紧 `PROMOTION_MARK` 或给该参数
 * 加例外，而不是放宽整条规则。
 *
 * 每一次删除与每一次置 null 都写进 `log`，由 `main()` 打印出来供人工复核。
 */
function sanitizeLink(rawUrl, field, label, log) {
  if (!rawUrl) return null;
  if (!parseUrl(rawUrl)) return rawUrl;

  const pathHit = promotionPathHit(rawUrl);
  if (pathHit) {
    log(`  NULL ${label}.${field}: 推广路径 ${pathHit} ← ${rawUrl}`);
    return null;
  }
  const keptHit = promotionKeptParamHit(rawUrl);
  if (keptHit) {
    log(`  NULL ${label}.${field}: 保留参数带推广值（${keptHit}） ← ${rawUrl}`);
    return null;
  }

  const { url, dropped } = stripNonFunctional(rawUrl);
  if (dropped.length) {
    log(
      `  DROP ${label}.${field}: ${dropped.map(([k, v]) => `${k}=${v}`).join(', ')} ← ${rawUrl} ⇒ ${url}`,
    );
  }

  if (field === 'apiKeyUrl' && isPureReferral(url, rawUrl)) {
    log(`  NULL ${label}.${field}: 纯推广跳转/取密钥页已不可还原 ← ${rawUrl}`);
    return null;
  }
  if (isOpaqueShortLink(url)) {
    log(`  NULL ${label}.${field}: 不透明短链 ← ${rawUrl}`);
    return null;
  }
  return url;
}

/** 返回一条 URL 里所有参数名（小写）。 */
function paramNames(url) {
  const parsed = parseUrl(url);
  if (!parsed) return [];
  return [...parsed.searchParams.keys()].map((k) => k.toLowerCase());
}

/**
 * `apiKeyUrl` 是否已无法还原为厂商控制台的正常 URL（spec §5.6 规则 2）。
 * 判定顺序固定，便于 review：
 *  a. 路径形如 `/go/...`；
 *  b. 原本只是「根路径 + 追踪参数」（剥离后查询为空、且没有正常路径）；
 *  c. 路径命中明确的推广前缀（`/i/`、`/r/`、`/invite/`、`/agent/register/`…）；
 *  d. 单段不透明短码（段内出现大写字母，如 `/VjM74M`、`/nMvAvy`）。
 *
 * 已知误报面（规则 d，故意保留）：单段路径里含大写字母的**正常**控制台路径会被判成
 * 短链并置 null，例如 `/apiKeys`、`/apiKey`。当前数据集里没有这种取密钥页，所以不为此
 * 增加例外 —— 例外会让"不透明短链"这条规则变得可被任意路径说服。将来真出现时，
 * 正确做法是把该厂商加进"允许的单段路径"白名单，而不是放宽 d。
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

/**
 * 剥离后 URL 仍必须**可用**：不允许把一条 URL 剥成"只剩根路径、连查询串都没有"——
 * 这种 URL 已经指不到任何页面（页面只能靠参数定位），等于把"一键打开取密钥页"变成死链。
 * 这种情况必须由 [`isPureReferral`] 判成推广链接并置 null，而不是留下一个空壳。
 *
 * 只对 `apiKeyUrl` 断言：`websiteUrl` 的根路径就是厂商首页，本来就可点。
 */
function assertStillUsable(strippedUrl, rawUrl, label) {
  const parsed = parseUrl(strippedUrl);
  if (!parsed) return; // 解析不了的 URL 本脚本原样保留，不在此断言范围
  const hasPath = parsed.pathname !== '' && parsed.pathname !== '/';
  if (!hasPath && parsed.search === '') {
    throw new Error(
      `${label}: 剥离追踪参数后 URL 已不可用（只剩根路径且无查询串）：${rawUrl} → ${strippedUrl}`,
    );
  }
}

/** URL 里是否仍带追踪参数名（精确匹配，见 [`isTrackingName`]）。 */
function hasTracking(url) {
  const parsed = parseUrl(url);
  if (!parsed) return false;
  return [...parsed.searchParams.keys()].some((k) => isTrackingName(k));
}

/** URL 里是否有白名单之外的参数（清洗后必须为空）。 */
function hasNonFunctionalParam(url) {
  return paramNames(url).some((k) => !FUNCTIONAL_PARAM.has(k));
}

/** Claude Code 被要求"直连某厂商并使用厂商凭证"的标志键。 */
const DIRECT_VENDOR_KEY = /^CLAUDE_CODE_USE_(.+)$/;

/** supported / unsupportedReason（Plan 3 A1 规则 3，优先级固定）。 */
function supportOf(raw) {
  const apiFormat = raw.apiFormat ?? 'anthropic';
  const requiresOAuth = raw.requiresOAuth === true;
  const providerType = raw.providerType ?? null;
  const env = (raw.settingsConfig && raw.settingsConfig.env) || {};

  // 架构级阻塞优先：`CLAUDE_CODE_USE_*`（如 CLAUDE_CODE_USE_BEDROCK）只说明
  // "让 Claude Code 自己直连厂商"，而本网关只做 Anthropic Messages 的别名改写 + 透传
  // （`gateway/rewrite.rs` 只会拼 `{baseUrl}/v1/messages`）。这类厂商既不走
  // `/v1/messages`，也不用我们的本地令牌 —— 网关拦不住它，也没有协议转换能力。
  // 结构判定而不是点名判据：将来加 Vertex/其它直连厂商会自动落到同一条。
  const directVendor = Object.keys(env).find((k) => DIRECT_VENDOR_KEY.test(k));
  if (directVendor) {
    const raw = directVendor.match(DIRECT_VENDOR_KEY)[1];
    const vendor = raw.charAt(0) + raw.slice(1).toLowerCase();
    return {
      supported: false,
      unsupportedReason: `需要直连 ${vendor} 并使用厂商凭证（SigV4 / 专用调用路径），本版本网关仅做 Anthropic 格式透传`,
    };
  }

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

function convert(raw, uniqueId, log) {
  // 规则 2：baseUrl 取自 settingsConfig.env.ANTHROPIC_BASE_URL；Claude Official 的 env 是 {}。
  // 判据用**名字**而不是"env 里没有 baseUrl"：上游将来加一条 env 为空的预设时，
  // 后者会把它错当成官方、硬指向 api.anthropic.com。
  const rawEnv = (raw.settingsConfig && raw.settingsConfig.env) || {};
  const isClaudeOfficial = raw.name === 'Claude Official';
  const baseUrl = isClaudeOfficial ? 'https://api.anthropic.com' : rawEnv.ANTHROPIC_BASE_URL;
  const authStyle = isClaudeOfficial ? 'x-api-key' : 'both';
  const defaultEnv = isClaudeOfficial
    ? { ANTHROPIC_BASE_URL: 'https://api.anthropic.com' }
    : envOf(rawEnv);

  // 链接栏位：白名单清洗（详见 sanitizeLink）。
  const websiteUrl = sanitizeLink(raw.websiteUrl, 'websiteUrl', raw.name, log);
  const apiKeyUrl = sanitizeLink(raw.apiKeyUrl, 'apiKeyUrl', raw.name, log);
  // 清洗过的 apiKeyUrl 必须仍然指得到某个页面（详见 assertStillUsable）。
  if (apiKeyUrl && raw.apiKeyUrl && apiKeyUrl !== raw.apiKeyUrl) {
    assertStillUsable(apiKeyUrl, raw.apiKeyUrl, `${raw.name}.apiKeyUrl`);
  }

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
    websiteUrl,
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
  const log = [];
  const presets = raw.map((preset) => convert(preset, uniqueId, (line) => log.push(line)));

  const doc = {
    version: 1,
    source: 'farion1231/cc-switch',
    license: 'MIT',
    sourceRef: 'main',
    presets,
  };

  writeFileSync(OUT_PATH, `${JSON.stringify(doc, null, 2)}\n`, 'utf8');

  // 逐条打印每一次参数删除与每一次置 null：这份输出就是人工复核的记录
  // （`git diff -- src-tauri/presets.json` 只能看到"结果变了"，看不到"为什么"）。
  if (log.length) {
    console.log('链接栏位处理明细（人工复核用）：');
    for (const line of log) console.log(line);
  }

  const unsupported = presets.filter((p) => !p.supported).length;
  const templated = presets.filter((p) => p.templateValues).length;
  const verified = presets.filter((p) => p.verifiedAt).length;
  const nulled = presets.filter(
    (p) => (p.websiteUrl === null) !== (p.apiKeyUrl === null),
  ).length;
  const tracked = presets.filter(
    (p) => typeof p.baseUrl === 'string' && hasTracking(p.baseUrl),
  ).length;
  console.log(
    `presets.json: ${presets.length} 条（不支持 ${unsupported} / 模板 ${templated} / 已验证 ${verified} / baseUrl 残留追踪参数 ${tracked} / 链接栏位置 null ${nulled}）`,
  );

  // 本脚本是**唯一**做清洗的地方：websiteUrl / apiKeyUrl 里还剩白名单之外的参数，
  // 就说明清洗逻辑有漏，必须在这里炸掉而不是把残留写进交付物
  // （src-tauri/src/preset/mod.rs 的同名用例也守这一条）。
  const residue = presets.filter((p) =>
    [p.websiteUrl, p.apiKeyUrl].some((u) => typeof u === 'string' && hasNonFunctionalParam(u)),
  ).length;
  if (residue > 0) {
    throw new Error(
      `presets.json 仍有 ${residue} 条链接带白名单之外的参数：清洗逻辑有漏`,
    );
  }
}

main();

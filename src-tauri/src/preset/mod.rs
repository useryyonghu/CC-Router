//! 厂家预设目录（spec §5.6 / Plan 3 A1）。
//!
//! 预设是**数据，不是代码**：内置 [`presets_builtin`] 随应用打包；用户可在
//! `%APPDATA%\cc-router\presets.user.json` 追加或覆盖（同 `id` 覆盖内置），无需重新打包应用。
//!
//! 本模块不依赖 Tauri（硬约束），也不碰网络：只做「解析 → 合并 → 排序」。
//! 数据来源与 MIT 署名见 `docs/reference/cc-switch-LICENSE.txt`；生成器是 `tools/build-presets.mjs`。

use crate::config::AuthStyle;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 内置目录原文（`src-tauri/presets.json`，由 `tools/build-presets.mjs` 生成）。
pub fn presets_builtin() -> &'static str {
    include_str!("../../presets.json")
}

/// 用户覆盖文件：`%APPDATA%\cc-router\presets.user.json`。
pub fn presets_user_path() -> PathBuf {
    crate::app_paths::app_data_dir().join("presets.user.json")
}

/// 目录文件的信封（与内置文件同构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetFile {
    /// 手写的 `presets.user.json` 常常不带版本号：缺字段就按 1 处理，而不是退回"裸数组"解析
    /// 再报一句与文件无关的 `expected an array`。
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub presets: Vec<Preset>,
}

fn default_version() -> u32 {
    1
}

/// `templateValues` 的一个条目（上游字段：`label` / `placeholder` / `defaultValue` / `editorValue`）。
///
/// Plan 3 A1 只用 `Option<BTreeMap<String, TemplateValue>>` 指代它、没给字段表，
/// 这里按上游数据取最小完备集合（全部 `Option` + `default`，兼容上游缺字段的写法）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateValue {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub default_value: Option<String>,
    #[serde(default)]
    pub editor_value: Option<String>,
}

/// 拉取失败时的离线兜底候选（spec §5.7 失败三出路的第 3 条）。
///
/// Plan 3 A1 只说 `Vec<DefaultModel>` 而没给字段表；这里取最小完备集合：`id` 必填，
/// `name` 为可选的展示名（当前生成器只写 `id`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// 一条厂家预设。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    /// 稳定 slug（由 `name` 生成）；`config.providers[].presetId` 指向它。
    pub id: String,
    pub name: String,
    /// `official` | `cn_official` | `third_party` | `aggregator` | `cloud_provider`。
    pub category: String,
    /// 上游 `primePartner`：排序用的「合作伙伴」标记（spec §10.2 的 official → primePartner → 其它）。
    #[serde(default)]
    pub prime_partner: bool,
    /// 上游 Anthropic 兼容入口；含 `${VAR}` 时由前端用 `templateValues` 替换后再落地。
    pub base_url: String,
    pub auth_style: AuthStyle,
    #[serde(default)]
    pub models_url: Option<String>,
    /// 已剥离联盟/追踪参数；无法还原为厂商控制台正常 URL 时为 `None`（UI 改展示 `website_url`）。
    #[serde(default)]
    pub api_key_url: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_color: Option<String>,
    /// 该厂商习惯用的密钥变量名（`ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY`），仅供提示。
    #[serde(default)]
    pub api_key_field: Option<String>,
    #[serde(default)]
    pub api_format: Option<String>,
    #[serde(default)]
    pub template_values: Option<BTreeMap<String, TemplateValue>>,
    /// 上游 `settingsConfig.env` 原样（`Claude Official` 的 `{}` 例外：补上显式 baseUrl）。
    #[serde(default)]
    pub default_env: BTreeMap<String, String>,
    /// `false` = 本期网关无法服务（仍列出、由 UI 禁用并显示原因）。
    pub supported: bool,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
    /// 仅本机实测过的 3 条填日期，其余一律 `None`。
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub default_models: Vec<DefaultModel>,
}

/// 解析 + 合并 + 排序。
///
/// - `builtin_json`：内置目录原文（[`presets_builtin`]）。
/// - `user_json`：用户覆盖文件原文；同 `id` 覆盖内置，新 `id` 追加。既接受与内置同构的
///   信封 `{ "version": 1, "presets": [...] }`，也接受裸数组 `[...]`（手写文件更省事）。
pub fn load_presets(builtin_json: &str, user_json: Option<&str>) -> Result<Vec<Preset>> {
    let mut list = parse_envelope(builtin_json, Path::new("<builtin presets.json>"))?;

    if let Some(user) = user_json {
        let path = presets_user_path();
        for preset in parse_user(user, &path)? {
            match list.iter_mut().find(|p| p.id == preset.id) {
                Some(slot) => *slot = preset,
                None => list.push(preset),
            }
        }
    }

    list.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    Ok(list)
}

/// 从磁盘加载：内置 + `%APPDATA%\cc-router\presets.user.json`（不存在则只用内置）。
pub fn load_presets_on_disk() -> Result<Vec<Preset>> {
    let user_path = presets_user_path();
    let user = match std::fs::read_to_string(&user_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::io(user_path, e)),
    };
    load_presets(presets_builtin(), user.as_deref())
}

/// 排序键：`official` → 合作伙伴（`primePartner`）→ 其它；组内按名称，名称相同按 `id` 定序。
fn sort_key(p: &Preset) -> (u8, String, String) {
    let rank = if p.category == "official" {
        0
    } else if p.prime_partner {
        1
    } else {
        2
    };
    (rank, p.name.to_lowercase(), p.id.clone())
}

fn parse_envelope(json: &str, path: &Path) -> Result<Vec<Preset>> {
    let file: PresetFile = serde_json::from_str(json).map_err(|e| Error::Json {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(file.presets)
}

/// 解析用户文件：既接受与内置同构的信封，也接受裸数组（手写文件更省事）。
///
/// 两种写法都不成立时，报**信封**的错误：用户文件通常是信封写法，报数组的错误
/// （`expected an array`）会把"信封里某个字段写坏了"误导成"文件结构完全不对"。
fn parse_user(json: &str, path: &Path) -> Result<Vec<Preset>> {
    let envelope_err = match serde_json::from_str::<PresetFile>(json) {
        Ok(file) => return Ok(file.presets),
        Err(e) => e,
    };
    match serde_json::from_str::<Vec<Preset>>(json) {
        Ok(list) => Ok(list),
        Err(array_err) => Err(Error::Json {
            path: path.to_path_buf(),
            // 顶层是 `{` 就按信封报错；否则（裸数组写坏了）报数组的错误。
            source: if json.trim_start().starts_with('{') { envelope_err } else { array_err },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// 只读参考数据（`docs/reference/`，来自 farion1231/cc-switch，MIT）。
    const RAW_JSON: &str = include_str!("../../../docs/reference/cc-switch-presets.raw.json");

    fn builtin() -> Vec<Preset> {
        load_presets(presets_builtin(), None).expect("内置 presets.json 必须可解析")
    }

    fn find<'a>(list: &'a [Preset], id: &str) -> &'a Preset {
        list.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("预设 {id} 不存在"))
    }

    /// 取 URL 查询串里的参数名（小写）。不引入 URL 解析依赖，够用于本测试。
    fn query_param_names(url: &str) -> Vec<String> {
        let Some(after) = url.split_once('?').map(|(_, rest)| rest) else {
            return Vec::new();
        };
        let query = after.split('#').next().unwrap_or("");
        query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| kv.split('=').next().unwrap_or("").to_lowercase())
            .collect()
    }

    /// 取 URL 的路径（不含开头的 `/`）与查询串。同样不引入解析依赖。
    fn path_and_query(url: &str) -> (&str, &str) {
        let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        let (authority_and_path, query) = match rest.split_once('?') {
            Some((a, q)) => (a, q.split('#').next().unwrap_or("")),
            None => (rest, ""),
        };
        let path = authority_and_path.split_once('/').map(|(_, p)| p).unwrap_or("");
        (path, query)
    }

    /// 功能性查询参数**白名单**（与 `tools/build-presets.mjs` 的 `FUNCTIONAL_PARAM` 同一份）。
    ///
    /// 只有人类确认过"删了链接就指向错误页面"的参数才在这里：`apikey`（火山控制台把 `{}`
    /// 以 `%7B%7D` 嵌在查询串里）、`redirect`（FennoAI 注册后落地目标）、`tab`
    /// （AICodeWith 的 `tab=register`）。**要新增必须写明理由**：白名单的默认动作是删除。
    const FUNCTIONAL_PARAM: [&str; 3] = ["apikey", "redirect", "tab"];

    /// 追踪参数名（**精确匹配** + `utm_` 前缀）。只在 `baseUrl` 的残留检查里用得到：
    /// `baseUrl` 是厂家端点，spec §5.6 只要求清洗链接栏位。
    fn is_tracking(key_lower: &str) -> bool {
        const NAMES: [&str; 11] = [
            "aff", "ref", "invitecode", "ic", "ytag", "ac", "rc", "from", "code", "source", "ch",
        ];
        NAMES.contains(&key_lower) || key_lower.starts_with("utm_")
    }

    /// 推广归因标志：`cc-switch` / `ccswitch` / `ccs`（大小写不敏感）。
    fn is_promotion_mark(text: &str) -> bool {
        let lower = text.to_lowercase();
        ["ccswitch", "cc-switch", "ccs"].iter().any(|m| lower.contains(m))
    }

    /// 推广归因命中（**三段式规则里会导致整条置 null 的两种**）：
    ///  a. **路径段**命中 —— 归因长在路径里，清洗路径等于把链接掏空；
    ///  b. **白名单参数**的值命中 —— 这个参数我们要保留，污点去不掉。
    ///
    /// **非**白名单参数的值命中不算：那个参数下一步就被删掉，归因随它一起消失，
    /// 不该因此毁掉一条还能用的链接（`?aff=cc-switch` 不该杀掉 Kimi 的平台首页）。
    fn promotion_hit(url: &str) -> Option<String> {
        let (path, query) = path_and_query(url);
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            if is_promotion_mark(segment) {
                return Some(format!("path=/{segment}"));
            }
        }
        for kv in query.split('&').filter(|kv| !kv.is_empty()) {
            let (key, value) = kv.split_once('=').unwrap_or((kv, ""));
            if FUNCTIONAL_PARAM.contains(&key.to_lowercase().as_str()) && is_promotion_mark(value) {
                return Some(format!("kept-param={key}={value}"));
            }
        }
        None
    }

    /// 不透明短链：单段路径且段内出现大写字母（`/nMvAvy`）——无从判断它落在哪。
    fn is_opaque_short_link(url: &str) -> bool {
        let (path, _) = path_and_query(url);
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        segments.len() == 1 && segments[0].chars().any(|c| c.is_ascii_uppercase())
    }

    fn non_functional_params(url: &str) -> Vec<String> {
        query_param_names(url)
            .into_iter()
            .filter(|k| !FUNCTIONAL_PARAM.contains(&k.as_str()))
            .collect()
    }

    fn has_tracking(url: &str) -> bool {
        query_param_names(url).iter().any(|k| is_tracking(k))
    }

    #[test]
    fn loads_all_93_presets() {
        let list = builtin();
        assert_eq!(list.len(), 93, "spec §5.6：cc-switch 共 93 条预设，一条都不能丢");
        assert!(list.iter().all(|p| !p.name.trim().is_empty()));
        assert!(
            list.iter().any(|p| p.name == "DeepSeek" && p.base_url == "https://api.deepseek.com/anthropic"),
            "抽查一条 cn_official 预设"
        );
    }

    /// spec §5.6「链接处理」+ Plan 3 A1 规则 4：`websiteUrl` / `apiKeyUrl` 按**功能性参数
    /// 白名单**清洗（不再是黑名单）。
    ///
    /// 黑名单结构性失败过两次：先漏了 7 个参数，补上后又漏了两条**完全不含参数**的推广链接
    /// （PPIO 的 `/activity/ccswitch` 靠路径、Qiniu 的 `/nMvAvy` 靠短链）。参数名是开放集合，
    /// 黑名单永远补不完，所以规则改成"默认删除、白名单保留 + 推广启发式"。
    ///
    /// 本用例守**结果**（不重抄生成器的判定顺序）：
    /// 1. 白名单之外的参数零残留；2. 推广标志零残留；3. 有删除/置 null 就必须体现在产物里；
    /// 4. 两个链接栏位对同一条 URL 必须同判；5. 功能性参数在存活的链接里仍然保留。
    #[test]
    fn strips_affiliate_and_tracking_params() {
        let raw: Vec<serde_json::Value> = serde_json::from_str(RAW_JSON).unwrap();
        assert_eq!(raw.len(), 93);
        let list = builtin();
        let by_name: HashMap<&str, &Preset> =
            list.iter().map(|p| (p.name.as_str(), p)).collect();
        assert_eq!(by_name.len(), 93, "预设名称必须唯一，否则原始数据与本测试的映射会错位");

        // 夹具锚点用**精确值**（不是"下界"）：docs/reference/cc-switch-presets.raw.json 是
        // 只读的上游参考，2026-09-22 实测 47 条 URL / 38 条预设带非白名单参数；
        // 它被改动时应当有人来看一眼，而不是让本用例静静变成空转。
        let mut raw_with_params = 0usize;
        let mut raw_presets: HashSet<&str> = HashSet::new();
        let mut raw_param_names: HashSet<String> = HashSet::new();
        for entry in &raw {
            let name = entry["name"].as_str().unwrap();
            for field in ["websiteUrl", "apiKeyUrl"] {
                if let Some(url) = entry.get(field).and_then(|v| v.as_str()) {
                    raw_param_names.extend(query_param_names(url));
                    if !non_functional_params(url).is_empty() {
                        raw_with_params += 1;
                        raw_presets.insert(name);
                    }
                }
            }
        }
        assert_eq!(raw_with_params, 47, "只读夹具变了：带非白名单参数的 URL 应为 47（实测值）");
        assert_eq!(raw_presets.len(), 38, "只读夹具变了：带非白名单参数的预设应为 38（实测值）");
        // 夹具必须真的覆盖三个功能性参数，否则"保留它们"这两条断言就是空转
        for name in ["apikey", "redirect", "tab"] {
            assert!(
                raw_param_names.contains(name),
                "夹具里没有 {name}：白名单的保留行为无从被验证"
            );
        }
        // 也要有完全不带参数的链接，否则"原样保留"分支没被走过
        assert!(
            raw.iter().any(|e| ["websiteUrl", "apiKeyUrl"].iter().any(|f| {
                e.get(f)
                    .and_then(|v| v.as_str())
                    .map(|u| query_param_names(u).is_empty())
                    == Some(true)
            })),
            "夹具里应当也有完全不带参数的链接"
        );

        // 逐条断言**不变量**（不是"数了多少条被置 null"—— 那种计数在规则写错时照样能过）：
        for entry in &raw {
            let name = entry["name"].as_str().unwrap();
            let preset = by_name
                .get(name)
                .unwrap_or_else(|| panic!("原始数据里的预设 {name} 没有出现在内置目录里"));
            for field in ["websiteUrl", "apiKeyUrl"] {
                let raw_url = entry.get(field).and_then(|v| v.as_str());
                let shipped = if field == "websiteUrl" {
                    preset.website_url.as_deref()
                } else {
                    preset.api_key_url.as_deref()
                };
                if let Some(url) = shipped {
                    // 不变量 1：非白名单参数零残留
                    let residue = non_functional_params(url);
                    assert!(
                        residue.is_empty(),
                        "{name}.{field} 仍有白名单之外的参数 {residue:?}: {url}"
                    );
                    // 不变量 2/3：路径段与**保留参数的值**都不得命中推广标志
                    assert!(
                        promotion_hit(url).is_none(),
                        "{name}.{field} 仍命中推广标志（{}）: {url}",
                        promotion_hit(url).unwrap()
                    );
                }

                let Some(raw_url) = raw_url else { continue };
                // 不变量 4：原 URL 命中推广规则（路径 / 保留参数值）→ 产物必须是 null
                if promotion_hit(raw_url).is_some() {
                    assert_eq!(
                        shipped, None,
                        "{name}.{field} 命中推广规则（{}）就必须置 null，不能留下带归因的链接: {raw_url}",
                        promotion_hit(raw_url).unwrap()
                    );
                }
                // 不变量 5：删了非白名单参数就必须体现在产物里（"静默丢弃"在这里被挡住）。
                // 注意**不能**反过来要求"值命中推广标志就置 null"：那种参数是被删掉的，
                // 链接必须留下（见本用例末尾 Kimi / Volcengine 的定点断言）。
                if !non_functional_params(raw_url).is_empty() {
                    assert_ne!(
                        shipped, Some(raw_url),
                        "{name}.{field} 有非白名单参数，产物却与原文一样（参数没被删）"
                    );
                }
                // 不变量 6：不透明短链两个栏位同一判据（Qiniu 那条 URL 在两边都要 null）
                if is_opaque_short_link(raw_url) && promotion_hit(raw_url).is_none() {
                    assert_eq!(
                        shipped, None,
                        "{name}.{field} 是不透明短链，必须与另一个栏位得到同一判决: {raw_url}"
                    );
                }
            }
        }

        // 独立复审点名的两个案例：PPIO 靠**路径**（无参数可查）、Qiniu 靠短链，
        // 而 Qiniu 的两个栏位是**同一条 URL**，必须同判。
        let ppio = find(&list, "ppio");
        assert_eq!(
            ppio.api_key_url, None,
            "PPIO 的 /activity/ccswitch 是纯推广落地页（项目名在路径里），必须置 null"
        );
        assert_eq!(ppio.website_url.as_deref(), Some("https://ppio.com"));
        let qiniu = find(&list, "qiniu");
        assert_eq!(qiniu.api_key_url, None, "Qiniu 的短链取密钥页必须置 null");
        assert_eq!(
            qiniu.website_url, None,
            "Qiniu 的 websiteUrl 是**同一条**短链，必须得到与 apiKeyUrl 相同的判决"
        );

        // 功能性参数在存活的链接里必须保留（白名单不是"全删"）
        assert!(
            find(&list, "fennoai").api_key_url.as_deref().unwrap().contains("redirect="),
            "redirect 是功能性参数（注册后落地目标），必须保留"
        );
        assert!(
            find(&list, "aicodewith").api_key_url.as_deref().unwrap().contains("tab=register"),
            "tab=register 是功能性参数，必须保留"
        );

        // **`apikey` 之所以在白名单里，唯一理由就是火山控制台那条 URL**：它的取密钥页把 `{}`
        // 以 `%7B%7D` 嵌在查询串里，删了参数就打不开。三段式规则**不得**因为同一 URL 上
        // 那些"反正要删"的 utm_* 值提到 ccswitch 就把整条链接 null 掉 —— 那会让这条白名单
        // 形同虚设、且这个预设的取密钥入口在产品里彻底消失。
        let doubao = find(&list, "volcengine-doubao");
        let key_url = doubao
            .api_key_url
            .as_deref()
            .expect("Volcengine Doubao 的取密钥页必须保留（apikey 参数是它存在的理由）");
        assert!(key_url.contains("/apiKey"), "{key_url}");
        assert!(
            key_url.contains("apikey=%7B%7D"),
            "apikey=%7B%7D 必须原样保留（火山控制台用 %7B%7D 表示那个空占位符）: {key_url}"
        );
        assert_eq!(
            non_functional_params(key_url).len() + non_functional_params(key_url).len(),
            0,
            "除 apikey 外的参数（utm_* 等）必须全部删除: {key_url}"
        );
        assert_eq!(
            query_param_names(key_url),
            vec!["apikey".to_string()],
            "只剩 apikey 一个参数: {key_url}"
        );
        assert_eq!(
            doubao.website_url.as_deref(),
            Some(key_url),
            "websiteUrl 与 apiKeyUrl 是同一条 URL，判决必须一致"
        );

        // 三段式规则的**代价侧**：只有"归因长在路径里"和"保留参数带推广值"才置 null。
        // 下面这些链接的推广标志只出现在**即将被删除**的参数值里 —— 它们必须留下来，
        // 否则一条能用的链接会被一个本来就要删的参数毁掉（这正是上一版的错误）。
        assert_eq!(
            find(&list, "kimi").website_url.as_deref(),
            Some("https://platform.kimi.com/"),
            "Kimi 平台首页只因 ?aff=cc-switch 被 null 是错的：aff 本来就会被删掉"
        );
        assert_eq!(
            find(&list, "kimi-for-coding").website_url.as_deref(),
            Some("https://www.kimi.com/code/"),
        );
        assert_eq!(
            find(&list, "byteplus").api_key_url.as_deref(),
            Some("https://www.byteplus.com/en/product/modelark"),
            "BytePlus 的 ModelArk 产品页只因 utm_* 提到 ccswitch 被 null 是错的"
        );
        assert_eq!(
            find(&list, "compshare").api_key_url.as_deref(),
            Some("https://www.compshare.cn/coding-plan"),
        );
        assert_eq!(
            find(&list, "cubence").api_key_url.as_deref(),
            Some("https://cubence.com/signup"),
            "Cubence 的 code=CCSWITCH/source=ccs 都是被删的参数，链接必须留下"
        );
        let plan = find(&list, "agent-plan");
        assert_eq!(
            plan.website_url.as_deref(),
            Some("https://www.volcengine.com/activity/agentplan"),
        );
        assert_eq!(plan.api_key_url, plan.website_url);

        // baseUrl 是厂家端点，生成器不碰它的参数；这里确认交付物里确实没有追踪参数残留
        for p in &list {
            assert!(
                !has_tracking(&p.base_url),
                "{} 的 baseUrl 带追踪参数: {}",
                p.name,
                p.base_url
            );
        }
    }
    /// 不支持的条目必须仍在目录里、带原因（spec §5.6：保留而不是删掉，将来加协议转换可启用）。
    ///
    /// 5 条来自 `apiFormat` / `requiresOAuth` 规则；本轮新增的 2 条来自
    /// "Claude Code 被要求直连厂商（`CLAUDE_CODE_USE_*`）"这条**架构级**规则：
    /// 网关只会拼 `{baseUrl}/v1/messages`（`gateway/rewrite.rs`），既不做 SigV4 签名也没有
    /// Bedrock 专用调用路径，而且那条 env 里的 `CLAUDE_CODE_USE_BEDROCK=1` 根本不会落到
    /// Claude Code（`defaultEnv` 本版本不持久化）—— 它们不可能通过本网关工作。
    #[test]
    fn marks_exactly_7_presets_unsupported_with_reasons() {
        let list = builtin();
        let mut unsupported: Vec<&Preset> = list.iter().filter(|p| !p.supported).collect();
        unsupported.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = unsupported.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "AWS Bedrock (AKSK)",
                "AWS Bedrock (API Key)",
                "Codex",
                "Gemini Native",
                "GitHub Copilot",
                "Nvidia",
                "xAI (Grok)"
            ],
            "不支持的 7 条必须仍列出，且正好是这 7 条"
        );

        let reason = |name: &str| {
            unsupported
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .unsupported_reason
                .clone()
                .unwrap_or_else(|| panic!("{name} 必须带 unsupportedReason"))
        };
        assert_eq!(reason("Gemini Native"), "需要 Gemini 原生格式转换");
        assert_eq!(reason("Nvidia"), "需要 OpenAI Chat 格式转换");
        // GitHub Copilot 同时是 openai_chat 且 requiresOAuth：spec §5.6 / A1 规则 3 规定
        // 原因优先级里 OAuth 最高，所以它记的是 OAuth 而不是 Chat。
        assert_eq!(reason("GitHub Copilot"), "需要 OAuth，本期仅支持 API Key");
        assert_eq!(reason("Codex"), "需要 OAuth，本期仅支持 API Key");
        assert_eq!(reason("xAI (Grok)"), "需要 OAuth，本期仅支持 API Key");
        assert_eq!(
            unsupported.iter().filter(|p| p.unsupported_reason.as_deref() == Some("需要 OAuth，本期仅支持 API Key")).count(),
            3,
            "requiresOAuth 的正好 3 条"
        );

        // 两条 Bedrock：原因必须点名真正的阻塞（直连厂商 + 厂商凭证），而不是含糊的"格式转换"。
        for id in ["aws-bedrock-aksk", "aws-bedrock-api-key"] {
            let p = find(&list, id);
            assert!(!p.supported, "{id} 必须标记为不支持");
            let reason = p.unsupported_reason.as_deref().unwrap_or("");
            assert!(reason.contains("Bedrock"), "{id} 的原因要点名厂商: {reason}");
            assert!(reason.contains("厂商凭证"), "{id} 的原因要点名阻塞: {reason}");
        }

        assert_eq!(list.len() - unsupported.len(), 86, "其余 86 条 supported: true");
        for p in list.iter().filter(|p| p.supported) {
            assert!(p.unsupported_reason.is_none(), "{} 不该有不支持原因", p.name);
            // 这条才是"将来加 Vertex 预设也不会漏"的守卫：只要 Claude Code 被告知直连厂商
            // （`CLAUDE_CODE_USE_*`），本网关就不可能服务它 —— 绝不能标 supported。
            for key in p.default_env.keys() {
                assert!(
                    !key.starts_with("CLAUDE_CODE_USE_"),
                    "{} 标记为 supported，却让 Claude Code 直连厂商（{key}）：本网关无法服务直连厂商的请求",
                    p.name
                );
            }
        }
    }

    #[test]
    fn marks_exactly_3_presets_with_template_values() {
        let list = builtin();
        let mut with_template: Vec<&Preset> =
            list.iter().filter(|p| p.template_values.is_some()).collect();
        with_template.sort_by(|a, b| a.id.cmp(&b.id));
        let ids: Vec<&str> = with_template.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["aws-bedrock-aksk", "aws-bedrock-api-key", "kat-coder"]);

        let kat = find(&list, "kat-coder").template_values.as_ref().unwrap();
        let endpoint = kat.get("ENDPOINT_ID").expect("KAT-Coder 需要 ENDPOINT_ID");
        assert_eq!(endpoint.label.as_deref(), Some("Vanchin Endpoint ID"));
        assert!(
            find(&list, "kat-coder").base_url.contains("${ENDPOINT_ID}"),
            "含 templateValues 的预设，baseUrl 里必须留下占位符交给 UI 替换"
        );

        let bedrock = find(&list, "aws-bedrock-aksk").template_values.as_ref().unwrap();
        assert!(bedrock.contains_key("AWS_REGION"));
        assert!(bedrock.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(bedrock.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(find(&list, "aws-bedrock-aksk").base_url.contains("${AWS_REGION}"));
    }

    #[test]
    fn only_three_presets_are_verified() {
        let list = builtin();
        let mut verified: Vec<&Preset> = list.iter().filter(|p| p.verified_at.is_some()).collect();
        verified.sort_by(|a, b| a.id.cmp(&b.id));
        let ids: Vec<&str> = verified.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude-official", "deepseek", "xiaomi-mimo"]);
        for p in &verified {
            assert_eq!(p.verified_at.as_deref(), Some("2026-09-22"), "{}", p.name);
        }
        assert_eq!(
            find(&list, "xiaomi-mimo").base_url,
            "https://api.xiaomimimo.com/anthropic"
        );
        assert_eq!(
            find(&list, "xiaomi-mimo-token-plan-china").verified_at,
            None,
            "同在小米域名家族但未实测的那条不得标已验证"
        );
    }

    #[test]
    fn claude_official_has_explicit_anthropic_base_url() {
        let list = builtin();
        let official = find(&list, "claude-official");
        assert_eq!(official.base_url, "https://api.anthropic.com");
        assert_eq!(official.auth_style, AuthStyle::XApiKey);
        assert_eq!(
            official.default_env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://api.anthropic.com"),
            "官方预设的上游 env 原本是 {{}}，我们必须补上显式地址"
        );
        assert!(official.supported);
        // 判据是**名字**，不是"env 里没有 ANTHROPIC_BASE_URL"：后者会把将来任何一条
        // env 为空的预设也硬指向 api.anthropic.com。这里从产物侧钉住"只有它"。
        assert_eq!(
            list.iter().filter(|p| p.base_url == "https://api.anthropic.com").count(),
            1,
            "只有 Claude Official 可以指向 api.anthropic.com"
        );
        assert_eq!(official.id, "claude-official");
    }

    /// 手写 `presets.user.json` 的两条容错：
    /// 1. 信封缺 `version` 必须能读（`#[serde(default)]`），而不是掉进裸数组解析；
    /// 2. 信封里某字段写坏时报**信封**的错误（点名那个字段），而不是 `expected an array`。
    #[test]
    fn user_file_envelope_tolerates_missing_version_and_reports_the_envelope_error() {
        let body = r#"{"id":"my-custom","name":"My Custom","category":"third_party","baseUrl":"https://example.com/anthropic","authStyle":"both","supported":true}"#;

        let no_version = format!(r#"{{"presets":[{body}]}}"#);
        let list = load_presets(presets_builtin(), Some(&no_version)).unwrap();
        assert_eq!(list.len(), 94, "信封缺 version 也必须能读");
        assert_eq!(find(&list, "my-custom").name, "My Custom");

        // 信封里 presets 的类型写坏：错误必须来自**信封**解析（点名那个坏值），
        // 而不是"整份文件不是数组"（`invalid type: map`）这种把人带偏的消息。
        let broken = r#"{"version":1,"presets":"oops"}"#;
        let err = load_presets(presets_builtin(), Some(broken)).unwrap_err().to_string();
        assert!(
            err.contains("oops"),
            "错误要点名出问题的取值（信封错误）: {err}"
        );
        assert!(
            !err.contains("invalid type: map"),
            "不该把信封错误误报成裸数组错误: {err}"
        );

        // 裸数组写坏时仍然报数组的错误（那条路的提示才是有用的）
        let broken_array = r#"[{"id":"x"}"#;
        assert!(load_presets(presets_builtin(), Some(broken_array)).is_err());
    }

    #[test]
    fn user_override_replaces_builtin_by_id() {
        let user = r#"{
            "version": 1,
            "presets": [{
                "id": "deepseek",
                "name": "My DeepSeek",
                "category": "third_party",
                "baseUrl": "https://api.deepseek.com/anthropic",
                "authStyle": "both",
                "supported": true
            }]
        }"#;
        let list = load_presets(presets_builtin(), Some(user)).unwrap();
        assert_eq!(list.len(), 93, "同 id 覆盖而不是追加");
        assert_eq!(find(&list, "deepseek").name, "My DeepSeek");
        assert_eq!(find(&list, "deepseek").category, "third_party");
        assert_eq!(find(&list, "kimi").name, "Kimi", "其余 92 条必须原样保留");
    }

    #[test]
    fn user_file_accepts_bare_array_and_appends_new_ids() {
        let user = r#"[{
            "id": "my-custom",
            "name": "My Custom",
            "category": "third_party",
            "baseUrl": "https://example.com/anthropic",
            "authStyle": "both",
            "supported": true
        }]"#;
        let list = load_presets(presets_builtin(), Some(user)).unwrap();
        assert_eq!(list.len(), 94, "新 id 追加");
        assert_eq!(find(&list, "my-custom").name, "My Custom");
    }

    #[test]
    fn ids_are_unique_and_slug_shaped() {
        let list = builtin();
        let mut seen = HashSet::new();
        for p in &list {
            assert!(
                !p.id.is_empty()
                    && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && !p.id.starts_with('-')
                    && !p.id.ends_with('-'),
                "id 必须是 slug 形状: {}",
                p.id
            );
            assert!(seen.insert(p.id.clone()), "id 重复: {}", p.id);
        }
    }

    #[test]
    fn sorts_official_then_prime_partner_then_others() {
        let list = builtin();
        assert_eq!(list[0].id, "claude-official", "official 组永远排最前");

        let positions = |pred: &dyn Fn(&Preset) -> bool| {
            list.iter()
                .enumerate()
                .filter(|(_, p)| pred(p))
                .map(|(i, _)| i)
                .collect::<Vec<usize>>()
        };
        let last_official = *positions(&|p: &Preset| p.category == "official").last().unwrap();
        let partners = positions(&|p: &Preset| p.prime_partner);
        let first_other = *positions(&|p: &Preset| p.category != "official" && !p.prime_partner)
            .first()
            .unwrap();
        let partner_ids: Vec<&str> = list
            .iter()
            .filter(|p| p.prime_partner)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(partner_ids, ["kimi", "kimi-for-coding"], "上游 primePartner 共 2 条");
        assert!(last_official < *partners.first().unwrap(), "official 必须早于合作伙伴");
        assert!(
            *partners.last().unwrap() < first_other,
            "合作伙伴必须早于其它预设（spec §10.2）"
        );

        // 组内按名称排序
        let names: Vec<String> = list
            .iter()
            .filter(|p| p.category != "official" && !p.prime_partner)
            .map(|p| p.name.to_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "组内必须按名称排序");
    }

    /// 硬约束：预设绝不预填密钥。
    #[test]
    fn presets_never_embed_api_keys() {
        for p in builtin() {
            for key in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"] {
                if let Some(value) = p.default_env.get(key) {
                    assert!(
                        value.is_empty() || (value.starts_with("${") && value.ends_with('}')),
                        "{} 的 {key} 预填了非空值: {value:?}",
                        p.name
                    );
                }
            }
        }
        assert!(
            !presets_builtin().contains("\"apiKey\""),
            "预设结构里只允许 apiKeyUrl / apiKeyField，不允许承载密钥的 apiKey 字段"
        );
    }
}

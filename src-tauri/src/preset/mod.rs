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

fn parse_user(json: &str, path: &Path) -> Result<Vec<Preset>> {
    if let Ok(file) = serde_json::from_str::<PresetFile>(json) {
        return Ok(file.presets);
    }
    serde_json::from_str::<Vec<Preset>>(json).map_err(|e| Error::Json {
        path: path.to_path_buf(),
        source: e,
    })
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

    fn has_tracking(url: &str) -> bool {
        query_param_names(url)
            .iter()
            .any(|k| matches!(k.as_str(), "aff" | "ref" | "invitecode" | "ch") || k.starts_with("utm_"))
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

    /// spec §5.6「链接处理」+ Plan 3 A1 规则 4：一律剥离联盟/追踪参数，不搬运推广关系。
    #[test]
    fn strips_affiliate_and_tracking_params() {
        let raw: Vec<serde_json::Value> = serde_json::from_str(RAW_JSON).unwrap();
        assert_eq!(raw.len(), 93);
        let list = builtin();
        let by_name: HashMap<&str, &Preset> =
            list.iter().map(|p| (p.name.as_str(), p)).collect();
        assert_eq!(by_name.len(), 93, "预设名称必须唯一，否则原始数据与本测试的映射会错位");

        // 先证明本测试不是空转：原始数据里确实有一批带追踪参数的 URL。
        let mut raw_tracked = 0usize;
        for entry in &raw {
            for field in ["websiteUrl", "apiKeyUrl"] {
                if let Some(url) = entry.get(field).and_then(|v| v.as_str()) {
                    if has_tracking(url) {
                        raw_tracked += 1;
                    }
                }
            }
        }
        assert!(
            raw_tracked >= 26,
            "spec §5.6 记载至少 26 条 URL 带联盟/追踪参数，实测 {raw_tracked} 条：原始数据可能被改动"
        );

        for entry in &raw {
            let name = entry["name"].as_str().unwrap();
            let preset = by_name
                .get(name)
                .unwrap_or_else(|| panic!("原始数据里的预设 {name} 没有出现在内置目录里"));
            let urls: [(&str, Option<&str>); 3] = [
                ("baseUrl", Some(preset.base_url.as_str())),
                ("websiteUrl", preset.website_url.as_deref()),
                ("apiKeyUrl", preset.api_key_url.as_deref()),
            ];
            for (field, url) in urls {
                let Some(url) = url else { continue };
                assert!(!has_tracking(url), "{name}.{field} 仍带追踪参数: {url}");
                for marker in ["aff=", "invitecode=", "utm_"] {
                    assert!(
                        !url.contains(marker),
                        "{name}.{field} 仍含 {marker:?}: {url}"
                    );
                }
            }
            // 原来带参的 URL，剥离后必须与原文不同（或整条被置 null）。
            for field in ["websiteUrl", "apiKeyUrl"] {
                let Some(raw_url) = entry.get(field).and_then(|v| v.as_str()) else { continue };
                if !has_tracking(raw_url) {
                    continue;
                }
                let stripped = if field == "websiteUrl" {
                    preset.website_url.as_deref()
                } else {
                    preset.api_key_url.as_deref()
                };
                if let Some(stripped) = stripped {
                    assert_ne!(stripped, raw_url, "{name}.{field} 应已剥离追踪参数");
                }
            }
        }
    }

    #[test]
    fn marks_exactly_5_presets_unsupported_with_reasons() {
        let list = builtin();
        let mut unsupported: Vec<&Preset> = list.iter().filter(|p| !p.supported).collect();
        unsupported.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = unsupported.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Codex", "Gemini Native", "GitHub Copilot", "Nvidia", "xAI (Grok)"],
            "不支持的 5 条必须仍列出，且正好是这 5 条"
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

        assert_eq!(list.len() - unsupported.len(), 88, "其余 88 条 supported: true");
        for p in list.iter().filter(|p| p.supported) {
            assert!(p.unsupported_reason.is_none(), "{} 不该有不支持原因", p.name);
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

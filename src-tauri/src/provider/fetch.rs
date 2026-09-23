//! 一键获取模型列表（spec §5.7 / Plan 3 A3）。
//!
//! 多候选探测、命中即停；全部失败时**不允许静默失败**：错误信息里逐个列出候选 URL 与
//! 状态码/错误摘要，供 UI 展示 spec §5.7 的"失败三出路"。
//!
//! 本模块不依赖 Tauri；鉴权复用 `gateway::rewrite`（`both` 同发两种头），避免第二套实现漂移。

use super::{status_hint, REQUEST_TIMEOUT};
use crate::config::Provider;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// spec §5.7 第 4 条：剥离这些已知兼容子路径后得到"站点根"再试。
pub const COMPAT_SUFFIXES: [&str; 9] = [
    "/anthropic",
    "/api",
    "/api/anthropic",
    "/api/claudecode",
    "/coding",
    "/step_plan",
    "/api/coding",
    "/api/compatible",
    "/api/plan",
];

/// 疑似非对话模型的 `id` 子串（spec §5.7）：UI 默认不勾选但可见。
const NON_CHAT_MARKERS: [&str; 9] = [
    "embed",
    "embedding",
    "rerank",
    "tts",
    "whisper",
    "audio",
    "image",
    "moderation",
    "dall-e",
];

/// 列表里的一个模型（字段可缺省；`contextWindow` / `maxTokens` 只用于界面展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// `id` 命中 [`NON_CHAT_MARKERS`] 时为 true。
    pub looks_non_chat: bool,
}

/// 单个候选的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// 拿到了 HTTP 响应但没命中（非 2xx，或 2xx 但列表为空）。
    Http(u16),
    /// 连接/超时/解析失败的错误摘要。
    Error(String),
}

impl AttemptOutcome {
    pub fn describe(&self) -> String {
        match self {
            AttemptOutcome::Http(status) => format!("HTTP {status}"),
            AttemptOutcome::Error(message) => message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchAttempt {
    pub url: String,
    pub outcome: AttemptOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    pub models: Vec<FetchedModel>,
    pub used_url: String,
    /// 含命中的那一次（按尝试顺序）。
    pub attempts: Vec<FetchAttempt>,
}

/// 候选 URL（去重、保序，spec §5.7）：
/// 1. `provider.models_url` 有值 → **只**返回它；
/// 2. `modelsFetch.lastUrl`（上次试通的地址）优先；
/// 3. `{baseUrl}/v1/models`、`{baseUrl}/models`；
/// 4. 剥离 [`COMPAT_SUFFIXES`] 后的站点根，各试 `/v1/models` 与 `/models`。
pub fn candidate_urls(provider: &Provider) -> Vec<String> {
    if let Some(explicit) = provider
        .models_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        return vec![explicit.to_string()];
    }

    let mut out: Vec<String> = Vec::new();
    if let Some(last) = provider
        .models_fetch
        .as_ref()
        .map(|f| f.last_url.trim())
        .filter(|u| !u.is_empty())
    {
        push_unique(&mut out, last.to_string());
    }

    let base = provider.base_url.trim().trim_end_matches('/');
    if !base.is_empty() {
        push_unique(&mut out, format!("{base}/v1/models"));
        push_unique(&mut out, format!("{base}/models"));
        for suffix in COMPAT_SUFFIXES {
            if let Some(root) = base.strip_suffix(suffix) {
                let root = root.trim_end_matches('/');
                if root.is_empty() {
                    continue;
                }
                push_unique(&mut out, format!("{root}/v1/models"));
                push_unique(&mut out, format!("{root}/models"));
            }
        }
    }
    out
}

/// 解析模型列表响应：Anthropic / OpenAI 两种形状（共用 `data[].id`）与裸数组都接受。
pub fn parse_models(bytes: &[u8]) -> Result<Vec<FetchedModel>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| Error::Gateway(format!("模型列表响应不是合法 JSON: {e}")))?;
    let entries = match &value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => map
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| Error::Gateway("模型列表响应里没有 data 数组".to_string()))?,
        other => {
            return Err(Error::Gateway(format!(
                "模型列表响应形状不认识（既不是数组也不是对象）: {other}"
            )))
        }
    };

    let mut out: Vec<FetchedModel> = Vec::new();
    for item in entries {
        let Some(obj) = item.as_object() else { continue };
        let Some(id) = obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if out.iter().any(|m| m.id == id) {
            continue; // 上游偶尔会重复列同一条；去重避免 UI 与 add_model 都出错
        }
        let display_name = ["display_name", "name"]
            .iter()
            .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(FetchedModel {
            id: id.to_string(),
            display_name,
            context_window: first_u64(obj, &["context_length", "context_window", "max_input_tokens"]),
            max_tokens: first_u64(obj, &["max_output_tokens", "max_tokens"]),
            looks_non_chat: looks_non_chat(id),
        });
    }
    Ok(out)
}

/// 依次尝试候选，命中即停（2xx 且解析出非空列表）。
///
/// 返回 `(成功的 outcome, 全部尝试记录)`：全部失败时 outcome 为 `None`，但 `attempts`
/// 完整（这也是 [`fetch_models`] 生成的错误信息的内容来源）。
pub async fn fetch_models_attempts(
    client: &reqwest::Client,
    provider: &Provider,
) -> (Option<FetchOutcome>, Vec<FetchAttempt>) {
    let mut attempts: Vec<FetchAttempt> = Vec::new();
    for url in candidate_urls(provider) {
        match probe(client, provider, &url).await {
            Probe::Hit(status, models) => {
                attempts.push(FetchAttempt {
                    url: url.clone(),
                    outcome: AttemptOutcome::Http(status),
                });
                let outcome = FetchOutcome {
                    models,
                    used_url: url,
                    attempts: attempts.clone(),
                };
                return (Some(outcome), attempts);
            }
            Probe::Miss(status) => attempts.push(FetchAttempt {
                url,
                outcome: AttemptOutcome::Http(status),
            }),
            Probe::Failed(message) => attempts.push(FetchAttempt {
                url,
                outcome: AttemptOutcome::Error(message),
            }),
        }
    }
    (None, attempts)
}

/// 拉取模型列表；全部候选失败时返回 `Err`，错误正文含每一次尝试的 URL 与结果。
pub async fn fetch_models(
    client: &reqwest::Client,
    provider: &Provider,
) -> Result<FetchOutcome> {
    let (outcome, attempts) = fetch_models_attempts(client, provider).await;
    outcome.ok_or_else(|| Error::Gateway(failure_summary(&attempts)))
}

/// 失败信息（spec §5.7「失败处理是一等公民」）：逐个候选 + 常见原因 + 三条出路。
pub fn failure_summary(attempts: &[FetchAttempt]) -> String {
    let mut out = String::from("全部候选模型列表接口都失败了：");
    for attempt in attempts {
        out.push_str(&format!(
            "\n  - {} → {}",
            attempt.url,
            attempt.outcome.describe()
        ));
        if let AttemptOutcome::Http(status) = attempt.outcome {
            if let Some(hint) = status_hint(status) {
                out.push_str(&format!("（{hint}）"));
            }
        }
    }
    if attempts.is_empty() {
        out.push_str("\n  （没有任何候选 URL：请检查 Base URL 是否为空）");
    }
    out.push_str(
        "\n可以：① 手动填入完整的模型列表 URL 重试；② 手动输入模型名；③ 改用该预设的 defaultModels 作为离线兜底。",
    );
    out
}

enum Probe {
    Hit(u16, Vec<FetchedModel>),
    Miss(u16),
    Failed(String),
}

async fn probe(client: &reqwest::Client, provider: &Provider, url: &str) -> Probe {
    // 复用网关的鉴权注入：both 同发 x-api-key 与 Authorization: Bearer。
    let target = crate::routing::resolve::ResolvedTarget {
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        auth_style: provider.auth_style,
        upstream_model: String::new(),
        alias: String::new(),
        matched_by: crate::routing::resolve::MatchedBy::Passthrough,
        roles: Vec::new(),
        context_1m: false,
    };
    let headers = crate::gateway::rewrite::build_headers(&target, &hyper::HeaderMap::new()).headers;

    let mut request = client.get(url).timeout(REQUEST_TIMEOUT);
    for (name, value) in headers.iter() {
        request = request.header(name.as_str(), value.as_bytes());
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            return Probe::Failed(if e.is_timeout() {
                format!("超时（{}s）", REQUEST_TIMEOUT.as_secs())
            } else if e.is_connect() {
                format!("连接失败: {e}")
            } else {
                format!("请求失败: {e}")
            })
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Probe::Miss(status);
    }
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return Probe::Failed(format!("HTTP {status} 但读取响应失败: {e}")),
    };
    match parse_models(&bytes) {
        Ok(models) if !models.is_empty() => Probe::Hit(status, models),
        // 2xx 但没有模型：这个候选不算命中，继续试下一个。
        Ok(_) => Probe::Miss(status),
        Err(e) => Probe::Failed(format!("HTTP {status} 但响应无法解析: {e}")),
    }
}

fn push_unique(out: &mut Vec<String>, url: String) {
    if !out.contains(&url) {
        out.push(url);
    }
}

fn looks_non_chat(id: &str) -> bool {
    let lower = id.to_lowercase();
    NON_CHAT_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn first_u64(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let Some(value) = obj.get(*key) else { continue };
        if let Some(n) = value.as_u64() {
            return Some(n);
        }
        if let Some(s) = value.as_str() {
            if let Ok(n) = s.trim().parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthStyle, ModelsFetch};
    use std::collections::HashSet;

    fn provider(base: &str) -> Provider {
        Provider {
            id: "custom".into(),
            name: "Custom".into(),
            base_url: base.into(),
            api_key: "sk-real".into(),
            auth_style: AuthStyle::Both,
            preset_id: None,
            models_url: None,
            models_fetch: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn candidate_urls_prefers_explicit_models_url() {
        let mut p = provider("https://api.deepseek.com/anthropic");
        p.models_url = Some("https://api.deepseek.com/models".into());
        assert_eq!(
            candidate_urls(&p),
            vec!["https://api.deepseek.com/models".to_string()],
            "显式 modelsUrl 存在时只试它，不做任何探测"
        );

        p.models_url = Some("   ".into());
        assert!(candidate_urls(&p).len() > 1, "空白值等于未设置");
    }

    #[test]
    fn candidate_urls_strips_known_compat_subpaths() {
        let urls = candidate_urls(&provider("https://api.deepseek.com/anthropic"));
        assert_eq!(urls[0], "https://api.deepseek.com/anthropic/v1/models");
        assert_eq!(urls[1], "https://api.deepseek.com/anthropic/models");
        assert!(
            urls.contains(&"https://api.deepseek.com/models".to_string()),
            "spec §5.7 的验收点：DeepSeek 的 modelsUrl 没有 /v1，必须能探测到它：{urls:?}"
        );
        assert!(urls.contains(&"https://api.deepseek.com/v1/models".to_string()));
        assert_eq!(urls.len(), 4);

        // 多段后缀各自剥离：/api/anthropic 与 /anthropic 都要试
        let urls = candidate_urls(&provider("https://open.bigmodel.cn/api/anthropic"));
        assert!(urls.contains(&"https://open.bigmodel.cn/api/models".to_string()), "{urls:?}");
        assert!(urls.contains(&"https://open.bigmodel.cn/models".to_string()), "{urls:?}");

        // 没有已知后缀 → 只有 base 的两条
        let urls = candidate_urls(&provider("https://api.example.com/v1"));
        assert_eq!(
            urls,
            vec![
                "https://api.example.com/v1/v1/models".to_string(),
                "https://api.example.com/v1/models".to_string()
            ]
        );

        // 去重且保序
        let seen: HashSet<&String> = urls.iter().collect();
        assert_eq!(seen.len(), urls.len());
    }

    /// spec §5.7：试通的 URL 记进 `modelsFetch.lastUrl`，"下次优先试它"。
    #[test]
    fn candidate_urls_prefers_last_successful_url() {
        let mut p = provider("https://api.deepseek.com/anthropic");
        p.models_fetch = Some(ModelsFetch {
            last_at: "2026-09-22T10:00:00+08:00".into(),
            last_url: "https://api.deepseek.com/models".into(),
            count: 12,
        });
        let urls = candidate_urls(&p);
        assert_eq!(urls[0], "https://api.deepseek.com/models");
        assert_eq!(
            urls.iter().filter(|u| *u == "https://api.deepseek.com/models").count(),
            1,
            "优先项不得重复出现在候选里"
        );
        assert_eq!(urls.len(), 4);
    }

    #[test]
    fn parses_anthropic_openai_and_bare_array_shapes() {
        let anthropic =
            br#"{"data":[{"type":"model","id":"claude-sonnet-5","display_name":"Claude Sonnet 5"}],"has_more":false}"#;
        let models = parse_models(anthropic).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-5");
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Sonnet 5"));

        let openai =
            br#"{"object":"list","data":[{"id":"deepseek-v4-pro","object":"model","created":1}]}"#;
        let models = parse_models(openai).unwrap();
        assert_eq!(models[0].id, "deepseek-v4-pro");
        assert_eq!(models[0].display_name, None);

        let bare = br#"[{"id":"a"},{"id":"b","name":"B display"}]"#;
        let models = parse_models(bare).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].display_name.as_deref(), Some("B display"), "name 是 display_name 的兜底");

        // 缺字段：没有 id / id 只有空白的条目跳过，其余照收
        let missing = br#"{"data":[{"display_name":"no id"},{"id":"   "},{"id":"ok"}]}"#;
        let models = parse_models(missing).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ok");

        // 空列表：解析成功但为空（由调用方当作"未命中"继续试下一个候选）
        assert!(parse_models(br#"{"data":[]}"#).unwrap().is_empty());
        assert!(parse_models(br#"[]"#).unwrap().is_empty());

        // 重复 id 去重
        let dup = parse_models(br#"[{"id":"x"},{"id":"x","display_name":"X2"}]"#).unwrap();
        assert_eq!(dup.len(), 1);
        assert_eq!(dup[0].display_name, None, "保留第一条");

        // 认不出来的形状 → Err，而不是静默空列表
        assert!(parse_models(b"<html>502</html>").is_err());
        assert!(parse_models(br#"{"object":"list"}"#).is_err());
        assert!(parse_models(b"42").is_err());
    }

    #[test]
    fn extracts_context_window_from_either_naming() {
        let bytes = br#"{"data":[
            {"id":"a","context_length":200000,"max_output_tokens":8192},
            {"id":"b","context_window":"131072","max_tokens":"4096"},
            {"id":"c","max_input_tokens":1000000},
            {"id":"d","context_window":null,"max_output_tokens":null}
        ]}"#;
        let models = parse_models(bytes).unwrap();
        assert_eq!((models[0].context_window, models[0].max_tokens), (Some(200_000), Some(8192)));
        assert_eq!(
            (models[1].context_window, models[1].max_tokens),
            (Some(131_072), Some(4096)),
            "字符串形式的数字也接受"
        );
        assert_eq!((models[2].context_window, models[2].max_tokens), (Some(1_000_000), None));
        assert_eq!((models[3].context_window, models[3].max_tokens), (None, None));
    }

    #[test]
    fn flags_non_chat_models() {
        let bytes = br#"[{"id":"text-embedding-3"},{"id":"bge-reranker-v2"},{"id":"whisper-1"},
            {"id":"gpt-image-1"},{"id":"dall-e-3"},{"id":"tts-1"},{"id":"omni-moderation-latest"},
            {"id":"claude-sonnet-5"},{"id":"deepseek-v4-pro"}]"#;
        let models = parse_models(bytes).unwrap();
        let non_chat: Vec<&str> = models.iter().filter(|m| m.looks_non_chat).map(|m| m.id.as_str()).collect();
        assert_eq!(
            non_chat,
            ["text-embedding-3", "bge-reranker-v2", "whisper-1", "gpt-image-1", "dall-e-3", "tts-1", "omni-moderation-latest"]
        );
    }

    #[tokio::test]
    async fn fetch_stops_at_first_candidate_with_a_non_empty_list() {
        let mock = crate::provider::mock::MockUpstream::start(|path| match path {
            "/v1/models" => (404, r#"{"error":"not found"}"#.to_string()),
            "/models" => (200, r#"{"data":[{"id":"m1"},{"id":"m2"}]}"#.to_string()),
            other => (500, format!(r#"{{"error":"unexpected {other}"}}"#)),
        })
        .await;
        let p = provider(&mock.base_url);
        let client = reqwest::Client::new();

        let outcome = fetch_models(&client, &p).await.unwrap();
        assert_eq!(outcome.models.len(), 2);
        assert_eq!(outcome.used_url, format!("{}/models", mock.base_url));
        assert_eq!(outcome.attempts.len(), 2, "命中即停：第三条候选不应被尝试");
        assert_eq!(outcome.attempts[0].outcome, AttemptOutcome::Http(404));
        assert_eq!(outcome.attempts[1].outcome, AttemptOutcome::Http(200));

        // 鉴权复用 gateway::rewrite：both 同发两种头，且不发客户端令牌
        let sent = mock.requests();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].api_key.as_deref(), Some("sk-real"));
        assert_eq!(sent[0].authorization.as_deref(), Some("Bearer sk-real"));
        mock.shutdown().await;
    }

    #[tokio::test]
    async fn fetch_reports_every_attempt_on_total_failure() {
        let mock = crate::provider::mock::MockUpstream::start(|_path| {
            (404, r#"{"error":{"message":"no such endpoint"}}"#.to_string())
        })
        .await;
        let p = provider(&mock.base_url);
        let client = reqwest::Client::new();
        let expected = candidate_urls(&p);

        let (outcome, attempts) = fetch_models_attempts(&client, &p).await;
        assert!(outcome.is_none(), "全部失败时没有 outcome");
        assert_eq!(attempts.len(), expected.len(), "每个候选都要留下一条尝试记录");
        assert_eq!(
            attempts.iter().map(|a| a.url.clone()).collect::<Vec<_>>(),
            expected,
            "尝试顺序必须与候选顺序一致"
        );
        assert!(attempts.iter().all(|a| a.outcome == AttemptOutcome::Http(404)));

        let err = fetch_models(&client, &p).await.unwrap_err().to_string();
        for url in &expected {
            assert!(err.contains(url.as_str()), "错误信息必须逐个列出候选 URL：{err}");
        }
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("厂商不提供该接口"), "常见原因要直接解释（spec §5.7）：{err}");
        assert!(err.contains("①") && err.contains("②") && err.contains("③"), "三出路必须写清：{err}");
        mock.shutdown().await;
    }

    #[tokio::test]
    async fn fetch_records_transport_failures_as_error_attempts() {
        // 端口 99999 超出 u16：reqwest 立刻报 URL 解析错误。刻意不用"连一个没人监听的端口"，
        // 那在 Windows 上会被当成 SYN 被丢弃而要走满 TCP 重传（单测会慢十几秒）。
        let p = provider("http://127.0.0.1:99999/anthropic");
        let client = reqwest::Client::new();

        let (outcome, attempts) = fetch_models_attempts(&client, &p).await;
        assert!(outcome.is_none());
        assert_eq!(attempts.len(), 4, "候选清单见 candidate_urls_strips_known_compat_subpaths");
        assert!(
            attempts.iter().all(|a| matches!(a.outcome, AttemptOutcome::Error(_))),
            "传输层失败必须记为 Error 摘要：{attempts:?}"
        );
        let err = fetch_models(&client, &p).await.unwrap_err().to_string();
        assert!(err.contains("请求失败"), "{err}");
        assert!(err.contains("127.0.0.1"), "{err}");
    }
}

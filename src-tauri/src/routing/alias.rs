use std::collections::HashSet;

pub const ALIAS_PREFIX: &str = "ccr-";
const MAX_ALIAS_LEN: usize = 64;
const ONE_M_MARKER: &str = "[1m]";

/// 剥离结尾的 `[1M]`（大小写不敏感，允许尾随空白）。返回 (清理后的名字, 是否带标记)。
/// 上游 cc-switch 的 `hasClaudeOneMMarker` / `stripClaudeOneMMarker` 语义与此一致。
pub fn strip_1m_marker(model: &str) -> (String, bool) {
    let trimmed_end = model.trim_end();
    if trimmed_end.to_lowercase().ends_with(ONE_M_MARKER) {
        let cut = trimmed_end.len() - ONE_M_MARKER.len();
        (trimmed_end[..cut].trim_end().to_string(), true)
    } else {
        (model.to_string(), false)
    }
}

/// 转小写 → 非 `[a-z0-9._-]` 替换为 `-` → 折叠连续 `-` → 去首尾 `-`。
pub fn slugify(input: &str) -> String {
    let lowered = input.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_dash = false;
    for ch in lowered.chars() {
        let keep = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_';
        if keep {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

/// 默认别名 `ccr-<providerId>-<modelId>`；超长则截断并追加 6 位 hash；冲突则追加 -2/-3…
pub fn generate_alias(provider_id: &str, model_id: &str, taken: &HashSet<String>) -> String {
    let (model_no_1m, _) = strip_1m_marker(model_id);
    let base = format!("{}{}-{}", ALIAS_PREFIX, slugify(provider_id), slugify(&model_no_1m));
    let base = if base.len() > MAX_ALIAS_LEN {
        let keep = MAX_ALIAS_LEN - 7;
        let truncated = base.chars().take(keep).collect::<String>();
        format!("{}-{}", truncated.trim_end_matches('-'), fnv1a_hex(&base))
    } else {
        base
    };
    if !taken.contains(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 由 baseUrl 的域名推导 provider id（`https://api.moonshot.cn/anthropic` → `moonshot`）。
pub fn derive_provider_id(base_url: &str, taken: &HashSet<String>) -> String {
    let raw = base_url
        .trim()
        .strip_prefix("https://")
        .or_else(|| base_url.trim().strip_prefix("http://"))
        .unwrap_or(base_url.trim());
    let host_port = raw.split('/').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or(host_port);
    let mut labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    // 去掉常见前缀与顶级域，取最有辨识度的一段
    while labels.len() > 1
        && matches!(labels[0], "api" | "www" | "open" | "gateway" | "router" | "cc" | "chat")
    {
        labels.remove(0);
    }
    if labels.len() > 1 {
        labels.pop(); // 去掉 TLD
    }
    let candidate = slugify(labels.first().copied().unwrap_or(host));
    let candidate = if candidate.is_empty() { "custom".to_string() } else { candidate };
    let mut candidate: String = candidate.chars().take(32).collect();
    candidate = candidate.trim_matches('-').to_string();
    if candidate.is_empty() {
        candidate = "custom".to_string();
    }
    if !taken.contains(&candidate) {
        return candidate;
    }
    let mut n = 2u32;
    loop {
        let c = format!("{candidate}-{n}");
        if !taken.contains(&c) {
            return c;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn strips_1m_marker_case_insensitively_with_trailing_space() {
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1M]"), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1m]"), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("mimo-v2.6-pro[1M]  "), ("mimo-v2.6-pro".into(), true));
        assert_eq!(strip_1m_marker("ccr-kimi-k3"), ("ccr-kimi-k3".into(), false));
        assert_eq!(strip_1m_marker("weird[1M]name"), ("weird[1M]name".into(), false));
    }

    #[test]
    fn slugify_replaces_and_collapses() {
        assert_eq!(slugify("Kimi K3"), "kimi-k3");
        assert_eq!(slugify("mimo-v2.6-pro"), "mimo-v2.6-pro");
        assert_eq!(slugify("a__b"), "a__b");
        assert_eq!(slugify("a//b::c"), "a-b-c");
        assert_eq!(slugify("---x---"), "x");
        assert_eq!(slugify("中文名"), "");
    }

    #[test]
    fn generates_default_alias_and_strips_marker_from_model_id() {
        assert_eq!(generate_alias("kimi", "k3", &none()), "ccr-kimi-k3");
        assert_eq!(generate_alias("xiaomi", "mimo-v2.6-pro[1M]", &none()), "ccr-xiaomi-mimo-v2.6-pro");
    }

    #[test]
    fn resolves_alias_collisions_by_appending_index() {
        let mut taken = HashSet::new();
        taken.insert("ccr-kimi-k3".to_string());
        assert_eq!(generate_alias("kimi", "k3", &taken), "ccr-kimi-k3-2");
        taken.insert("ccr-kimi-k3-2".to_string());
        assert_eq!(generate_alias("kimi", "k3", &taken), "ccr-kimi-k3-3");
    }

    #[test]
    fn truncates_overlong_alias_and_keeps_it_within_limit() {
        let long_model = "x".repeat(120);
        let alias = generate_alias("provider", &long_model, &none());
        assert!(alias.len() <= MAX_ALIAS_LEN, "alias too long: {}", alias.len());
        assert!(alias.starts_with(ALIAS_PREFIX));
        assert!(!alias.ends_with('-'));
    }

    #[test]
    fn derives_provider_id_from_hostname() {
        assert_eq!(derive_provider_id("https://api.moonshot.cn/anthropic", &none()), "moonshot");
        assert_eq!(derive_provider_id("https://api.deepseek.com/anthropic", &none()), "deepseek");
        assert_eq!(derive_provider_id("https://api.xiaomimimo.com/anthropic", &none()), "xiaomimimo");
        assert_eq!(derive_provider_id("https://open.bigmodel.cn/api/anthropic", &none()), "bigmodel");
        // IPv4 主机名：只保留首段（"127" 已满足 ^[a-z0-9][a-z0-9-]{0,31}$）。
        // 这是刻意的简化——本地 provider 的 id 通常由用户改写，不值得为它做多段拼接。
        assert_eq!(derive_provider_id("http://127.0.0.1:9000/v1", &none()), "127");
    }

    #[test]
    fn derive_provider_id_resolves_collisions_and_handles_junk() {
        let mut taken = HashSet::new();
        taken.insert("moonshot".to_string());
        assert_eq!(derive_provider_id("https://api.moonshot.cn/anthropic", &taken), "moonshot-2");
        assert_eq!(derive_provider_id("https://武/", &none()), "custom");
    }
}

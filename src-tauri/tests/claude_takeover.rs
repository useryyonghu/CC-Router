//! `claude` 模块的端到端测试：settings.json 接管/还原（spec §7）与子 Agent
//! frontmatter 的 `model` 管理（spec §8）。
//!
//! 所有夹具都在 `tempfile::tempdir()` 里构造，**绝不触碰真实的 `~/.claude/`**；
//! 需要路径的地方一律显式传入，绝不调用 `claude_settings_path()` / `agents_dir()`。

use cc_router::claude::agents::{
    create_agent, delete_agent, list_agents, restore_agent, set_model, set_model_recorded,
    ModelChoice,
};
use cc_router::claude::settings::{
    apply_takeover, apply_takeover_at, build_env_updates, restore, takeover_state, RestorePath,
    TakeoverState as FileState, REMOVED_KEYS,
};
use cc_router::config::{
    AuthStyle, Config, GatewayConfig, ModelSpec, Provider, Roles, TakeoverState, Target, UiConfig,
    UnknownModelPolicy, DEFAULT_BIND, DEFAULT_MAX_BODY_BYTES,
};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// 真实形状夹具：cc-switch 写出的、指向小米 MiMo 的 `~/.claude/settings.json`
/// （顶层有 `attribution`，`env` 里有 MiMo 的 baseUrl、`*_MODEL` 与 `*_MODEL_NAME`、
/// `CLAUDE_CODE_SUBAGENT_MODEL`、`API_TIMEOUT_MS`、`CLAUDE_CODE_EFFORT_LEVEL`）。
///
/// 与真实文件的两处刻意差异：额外加入 spec §7.1 要求**移除**的三个旧键
/// （`ANTHROPIC_MODEL` / `ANTHROPIC_API_KEY` / `ANTHROPIC_SMALL_FAST_MODEL`），
/// 否则"移除"规则无从被夹具覆盖。
const REAL_SHAPE: &str = r#"{
  "attribution": {
    "commit": "",
    "pr": ""
  },
  "env": {
    "ANTHROPIC_API_KEY": "sk-mimo-should-be-removed",
    "ANTHROPIC_AUTH_TOKEN": "sk-mimo-fixture-00000000000000000000000000000",
    "ANTHROPIC_BASE_URL": "https://api.xiaomimimo.com/anthropic",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "mimo-v2.6-pro[1M]",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": "mimo-v2.6-pro",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "mimo-v2.6-pro",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "mimo-v2.6-pro",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "mimo-v2.6-pro[1M]",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "mimo-v2.6-pro",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "mimo-v2.6-pro[1M]",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "mimo-v2.6-pro",
    "ANTHROPIC_MODEL": "mimo-v2.6-pro",
    "ANTHROPIC_SMALL_FAST_MODEL": "mimo-v2.6-pro",
    "API_TIMEOUT_MS": "600000",
    "CLAUDE_CODE_EFFORT_LEVEL": "high",
    "CLAUDE_CODE_SUBAGENT_MODEL": "mimo-v2.6-flash[1M]"
  }
}
"#;

fn provider(id: &str, name: &str, models: &[(&str, &str, &str)]) -> Provider {
    Provider {
        id: id.to_string(),
        name: name.to_string(),
        base_url: format!("https://api.{id}.example/anthropic"),
        api_key: format!("{id}-key"),
        auth_style: AuthStyle::Both,
        preset_id: None,
        models_url: None,
        models_fetch: None,
        models: models
            .iter()
            .map(|(mid, display, alias)| ModelSpec {
                id: mid.to_string(),
                name: display.to_string(),
                alias: alias.to_string(),
                context_1m: false,
                context_window: None,
                max_tokens: None,
            })
            .collect(),
    }
}

/// 三个槽位都绑定：main=Kimi/K3、fast+subagent=DeepSeek/Flash。
fn demo_cfg() -> Config {
    Config {
        version: 1,
        gateway: GatewayConfig {
            bind: DEFAULT_BIND.to_string(),
            port: 8787,
            local_token: "sk-ccr-fixture-token".to_string(),
            max_request_body_bytes: DEFAULT_MAX_BODY_BYTES,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 300_000,
        },
        providers: vec![
            provider("kimi", "Kimi 官方", &[("k3", "Kimi K3", "ccr-kimi-k3")]),
            provider(
                "deepseek",
                "DeepSeek",
                &[("ds-flash", "DeepSeek V4 Flash", "ccr-ds-flash")],
            ),
        ],
        roles: Roles {
            main: Some(Target { provider_id: "kimi".into(), model_id: "k3".into() }),
            fast: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
            subagent: Some(Target { provider_id: "deepseek".into(), model_id: "ds-flash".into() }),
        },
        extra_routes: Vec::new(),
        on_unknown_model: UnknownModelPolicy::Default,
        default_target: None,
        takeover: TakeoverState::default(),
        ui: UiConfig::default(),
    }
}

fn env_of(path: &Path) -> Value {
    let doc: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    doc.get("env").cloned().unwrap_or(Value::Null)
}

fn pre_baks(backups: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(backups)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".pre.bak"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

// ---------------------------------------------------------------- Task 2

/// AC8 主用例：真实形状夹具 → 接管 → 还原必须**逐字节相同**。
#[test]
fn takeover_then_restore_is_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, REAL_SHAPE).unwrap();
    let before = fs::read(&path).unwrap();
    let before_json: Value = serde_json::from_slice(&before).unwrap();

    // fast 未绑定：HAIKU 的两个键既不能写、也不能删。
    let mut cfg = demo_cfg();
    cfg.roles.fast = None;

    let manifest = apply_takeover(&cfg, &path, &backups).unwrap();
    assert!(manifest.env_existed);
    assert!(manifest.file_existed);
    assert_eq!(manifest.keys.len(), build_env_updates(&cfg).len() + REMOVED_KEYS.len());

    let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let env = after.get("env").expect("env");
    assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-ccr-fixture-token");
    for removed in REMOVED_KEYS {
        assert!(env.get(removed).is_none(), "{removed} 必须被移除");
    }
    for fam in ["OPUS", "SONNET", "FABLE"] {
        assert_eq!(
            env[format!("ANTHROPIC_DEFAULT_{fam}_MODEL")],
            "ccr-kimi-k3",
            "{fam} 属于 main 家族"
        );
        assert_eq!(
            env[format!("ANTHROPIC_DEFAULT_{fam}_MODEL_NAME")],
            "Kimi 官方 / Kimi K3",
            "{fam} 的人类可读展示名"
        );
    }
    assert_eq!(env["CLAUDE_CODE_SUBAGENT_MODEL"], "ccr-ds-flash");
    // 未绑定槽位：原值原样保留（既没写也没删）
    assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "mimo-v2.6-pro");
    assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"], "mimo-v2.6-pro");
    // 顶层其它键逐字节不动
    assert_eq!(after["attribution"], before_json["attribution"]);

    let out = restore(&manifest, &path, &backups).unwrap();
    assert_eq!(out.path, RestorePath::Verbatim);
    assert_eq!(fs::read(&path).unwrap(), before, "AC8：还原后必须逐字节相同");
}

/// spec §7.3：接管前整个文件都不存在 → 还原后必须删除该文件。
#[test]
fn takeover_then_restore_when_file_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    let cfg = demo_cfg();
    assert!(!path.exists());

    let manifest = apply_takeover(&cfg, &path, &backups).unwrap();
    assert!(!manifest.file_existed);
    assert!(!manifest.env_existed);
    assert_eq!(manifest.pre_bytes_file, "", "没有 pre 快照可存");
    assert!(path.exists(), "接管后必须创建文件");
    assert_eq!(env_of(&path)["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");

    let out = restore(&manifest, &path, &backups).unwrap();
    assert_eq!(out.path, RestorePath::Verbatim);
    assert!(!path.exists(), "原来整个文件都不存在 → 还原后必须删除它");
}

/// spec §7.1：`env` 中其它键一律不改。
#[test]
fn takeover_preserves_other_env_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, REAL_SHAPE).unwrap();
    let cfg = demo_cfg();

    apply_takeover(&cfg, &path, &backups).unwrap();

    let env = env_of(&path);
    assert_eq!(env["API_TIMEOUT_MS"], "600000");
    assert_eq!(env["CLAUDE_CODE_EFFORT_LEVEL"], "high");
    let before_json: Value = serde_json::from_str(REAL_SHAPE).unwrap();
    for (k, v) in before_json["env"].as_object().unwrap() {
        if k == "ANTHROPIC_BASE_URL"
            || k == "ANTHROPIC_AUTH_TOKEN"
            || k.starts_with("ANTHROPIC_DEFAULT_")
            || k == "CLAUDE_CODE_SUBAGENT_MODEL"
            || REMOVED_KEYS.contains(&k.as_str())
        {
            continue;
        }
        assert_eq!(&env[k], v, "非己方键 {k} 必须原样保留");
    }
}

/// 三个槽位全未绑定：只写 baseUrl 与 token，其余键一个都不写。
#[test]
fn unbound_roles_write_no_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, "{}").unwrap();
    let mut cfg = demo_cfg();
    cfg.roles = Roles::default();

    let updates = build_env_updates(&cfg);
    assert_eq!(
        updates.keys().cloned().collect::<Vec<_>>(),
        vec!["ANTHROPIC_AUTH_TOKEN".to_string(), "ANTHROPIC_BASE_URL".to_string()]
    );

    apply_takeover(&cfg, &path, &backups).unwrap();
    let env = env_of(&path);
    let keys: Vec<&String> = env.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec![
            &"ANTHROPIC_AUTH_TOKEN".to_string(),
            &"ANTHROPIC_BASE_URL".to_string()
        ],
        "未绑定的槽位不得写入任何键"
    );
}

/// spec §7.1 关键设计 7：JSON 非法 → 报错并且**不写任何东西**（连备份都不产生）。
#[test]
fn malformed_json_is_rejected_and_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, b"{ not json").unwrap();
    let cfg = demo_cfg();

    let err = apply_takeover(&cfg, &path, &backups).unwrap_err();
    assert!(
        matches!(err, cc_router::error::Error::Json { .. }),
        "非法 JSON 必须报 Json 错误，实际: {err:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"{ not json");
    assert!(!backups.exists(), "接管失败时不得留下任何备份");
}

/// 己方要动的键原值不是字符串 → 拒绝接管（无法用清单精确还原，宁可不做）。
#[test]
fn non_string_owned_key_is_rejected_and_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, "{\n  \"env\": {\n    \"ANTHROPIC_BASE_URL\": 5\n  }\n}\n").unwrap();
    let before = fs::read(&path).unwrap();
    let cfg = demo_cfg();

    let err = apply_takeover(&cfg, &path, &backups).unwrap_err();
    assert!(
        matches!(err, cc_router::error::Error::ConfigInvalid(_)),
        "必须 fail-closed 而不是静默丢值，实际: {err:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!backups.exists());
}

/// 接管后被别的程序改过（例如 Claude Code 自己加了键）→ 用清单合并式回放，
/// 己方键恢复原值，外部新增键保留（spec §7.3）。
#[test]
fn restore_after_external_edit_uses_manifest_and_keeps_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, REAL_SHAPE).unwrap();
    let cfg = demo_cfg();
    let manifest = apply_takeover(&cfg, &path, &backups).unwrap();

    let mut edited: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edited["env"]["NEW_KEY"] = json!("added-by-claude-code");
    fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&edited).unwrap())).unwrap();

    let out = restore(&manifest, &path, &backups).unwrap();
    assert_eq!(out.path, RestorePath::MergedManifest);
    let env = env_of(&path);
    assert_eq!(env["NEW_KEY"], "added-by-claude-code", "外部新增键不得被删");
    assert_eq!(env["ANTHROPIC_BASE_URL"], "https://api.xiaomimimo.com/anthropic");
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-mimo-fixture-00000000000000000000000000000");
    assert_eq!(env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "mimo-v2.6-pro[1M]");
    assert_eq!(env["ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"], "mimo-v2.6-pro");
    assert_eq!(env["ANTHROPIC_MODEL"], "mimo-v2.6-pro", "原先存在的被移除键必须写回");
    assert_eq!(env["ANTHROPIC_API_KEY"], "sk-mimo-should-be-removed");
    assert_eq!(env["ANTHROPIC_SMALL_FAST_MODEL"], "mimo-v2.6-pro");
    assert_eq!(env["CLAUDE_CODE_SUBAGENT_MODEL"], "mimo-v2.6-flash[1M]");
    assert!(
        out.changed_keys.contains(&"ANTHROPIC_BASE_URL".to_string()),
        "changed_keys 必须列出真正被改回的键: {:?}",
        out.changed_keys
    );
    assert_eq!(out.changed_keys.first().map(String::as_str), Some("ANTHROPIC_API_KEY"));
}

/// spec §7.4：`ANTHROPIC_BASE_URL` 是否仍等于本网关地址 → applied / stale / not_applied。
#[test]
fn takeover_state_detects_applied_stale_and_not_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    let cfg = demo_cfg();

    assert_eq!(takeover_state(&cfg, &path), FileState::NotApplied, "文件不存在");

    fs::write(&path, "{\n  \"env\": {}\n}\n").unwrap();
    assert_eq!(takeover_state(&cfg, &path), FileState::NotApplied, "没有 baseUrl 键");

    apply_takeover(&cfg, &path, &backups).unwrap();
    assert_eq!(takeover_state(&cfg, &path), FileState::Applied);

    let mut doc: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    doc["env"]["ANTHROPIC_BASE_URL"] = json!("https://api.xiaomimimo.com/anthropic");
    fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())).unwrap();
    assert_eq!(
        takeover_state(&cfg, &path),
        FileState::Stale { found: "https://api.xiaomimimo.com/anthropic".to_string() }
    );

    fs::write(&path, b"not json at all").unwrap();
    assert_eq!(takeover_state(&cfg, &path), FileState::NotApplied, "无法解析时不得宣称已接管");
}

/// spec §7.2 步骤 3：同一次接管只备份一次 —— pre 备份必须始终是**接管前**的字节，
/// 否则第二次接管会把"已接管内容"当成原始状态，精确还原当场失效。
#[test]
fn backup_files_are_written_once_per_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, REAL_SHAPE).unwrap();
    let before = fs::read(&path).unwrap();
    let cfg = demo_cfg();
    let stamp = "20260922T153000";

    let m1 = apply_takeover_at(&cfg, &path, &backups, stamp).unwrap();
    assert_eq!(m1.pre_bytes_file, "settings.json.20260922T153000.pre.bak");
    assert_eq!(m1.post_bytes_file, "settings.json.20260922T153000.post.bak");
    let after_first = fs::read(&path).unwrap();
    assert_eq!(fs::read(backups.join(&m1.pre_bytes_file)).unwrap(), before);
    assert_eq!(fs::read(backups.join(&m1.post_bytes_file)).unwrap(), after_first);

    let m2 = apply_takeover_at(&cfg, &path, &backups, stamp).unwrap();
    assert_eq!(m2.pre_bytes_file, m1.pre_bytes_file);
    assert_eq!(
        fs::read(backups.join(&m2.pre_bytes_file)).unwrap(),
        before,
        "同一 stamp 的第二次接管不得覆盖 pre 备份"
    );
    assert_eq!(pre_baks(&backups), vec!["settings.json.20260922T153000.pre.bak".to_string()]);

    restore(&m2, &path, &backups).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// pre 快照丢失（用户手工清理了 backups）时，清单回放仍然可用。
#[test]
fn restore_falls_back_to_manifest_when_backups_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    fs::write(&path, REAL_SHAPE).unwrap();
    let before = fs::read(&path).unwrap();
    let cfg = demo_cfg();
    let manifest = apply_takeover(&cfg, &path, &backups).unwrap();

    fs::remove_dir_all(&backups).unwrap();
    let out = restore(&manifest, &path, &backups).unwrap();
    assert_eq!(out.path, RestorePath::MergedManifest);
    let env = env_of(&path);
    assert_eq!(env["ANTHROPIC_BASE_URL"], "https://api.xiaomimimo.com/anthropic");
    assert_eq!(env["CLAUDE_CODE_SUBAGENT_MODEL"], "mimo-v2.6-flash[1M]");
    assert_eq!(env["ANTHROPIC_MODEL"], "mimo-v2.6-pro");
    // 夹具本身就是 2 空格缩进 + 键序排序 + 末尾单换行，与本应用的写出格式一致，
    // 因此这条路径在这个夹具上也逐字节相等（但 AC8 的保证来自 Verbatim 路径）。
    assert_eq!(fs::read(&path).unwrap(), before);
}

// ---------------------------------------------------------------- Task 3

/// 一个带 frontmatter 与正文的普通子 Agent 文件（LF）。
const AGENT_LF: &str = "---\nname: reviewer\ndescription: 代码审查\n---\n\n你是审查者。\n";

/// CRLF + 末尾**没有**换行：还原能否逐字节相等全看这一条。
const AGENT_CRLF_NO_TRAILING: &str =
    "---\r\nname: crlf-agent\r\ndescription: 换行风格测试\r\n---\r\nbody without trailing newline";

/// 完全没有 frontmatter 的文件。
const AGENT_NO_FRONTMATTER: &str = "# 纯正文\n\n没有 frontmatter 的 agent 文件。\n";

/// 去掉所有 `model:` 行后的文本（用于证明"只有 model 行变了"）。
fn without_model_lines(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_end_matches('\r').starts_with("model:"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_text(path: &Path) -> String {
    String::from_utf8(fs::read(path).unwrap()).unwrap()
}

#[test]
fn set_model_replace_insert_remove() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reviewer.md");
    let backups = dir.path().join("backups");
    fs::write(&path, AGENT_LF).unwrap();

    // 插入（原本没有 model 行）
    set_model(&path, ModelChoice::Alias("ccr-kimi-k3"), &backups).unwrap();
    let inserted = read_text(&path);
    assert!(inserted.contains("model: ccr-kimi-k3\n"), "{inserted}");
    assert_eq!(
        without_model_lines(&inserted),
        without_model_lines(AGENT_LF),
        "插入只允许影响 model 行"
    );

    // 替换
    set_model(&path, ModelChoice::Inherit, &backups).unwrap();
    let replaced = read_text(&path);
    assert!(replaced.contains("model: inherit\n"), "{replaced}");
    assert!(!replaced.contains("ccr-kimi-k3"));
    assert_eq!(without_model_lines(&replaced), without_model_lines(AGENT_LF));

    // 删除
    set_model(&path, ModelChoice::SubagentDefault, &backups).unwrap();
    assert_eq!(read_text(&path), AGENT_LF, "删掉 model 行后必须回到原样");
}

#[test]
fn set_model_preserves_crlf_and_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");

    // CRLF + 无末尾换行
    let crlf = dir.path().join("crlf.md");
    fs::write(&crlf, AGENT_CRLF_NO_TRAILING).unwrap();
    set_model(&crlf, ModelChoice::Alias("ccr-x"), &backups).unwrap();
    let text = read_text(&crlf);
    assert!(text.contains("model: ccr-x\r\n"), "插入的行必须用 CRLF: {text:?}");
    assert!(!text.ends_with('\n'), "末尾无换行必须保持: {text:?}");
    assert_eq!(text.replace("model: ccr-x\r\n", ""), AGENT_CRLF_NO_TRAILING);

    // LF + 有末尾换行
    let lf = dir.path().join("lf.md");
    fs::write(&lf, AGENT_LF).unwrap();
    set_model(&lf, ModelChoice::Alias("ccr-x"), &backups).unwrap();
    let text = read_text(&lf);
    assert!(text.contains("model: ccr-x\n"));
    assert!(!text.contains('\r'), "不得把 LF 文件改成 CRLF: {text:?}");
    assert!(text.ends_with('\n'), "末尾换行必须保持");
    assert_eq!(text.replace("model: ccr-x\n", ""), AGENT_LF);
}

#[test]
fn set_model_preserves_other_frontmatter_and_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tools.md");
    let backups = dir.path().join("backups");
    let original = "---\nname: r\ndescription: d\ntools: Read, Grep\nmodel: old-alias\n---\n\nSome body\n---\nnot frontmatter\n- list item\n";
    fs::write(&path, original).unwrap();

    set_model(&path, ModelChoice::Alias("ccr-new"), &backups).unwrap();
    let text = read_text(&path);
    assert!(text.contains("tools: Read, Grep"));
    assert!(text.contains("description: d"));
    assert!(text.contains("\n---\nnot frontmatter\n- list item\n"), "正文里的 --- 与连字符行不得被当成 frontmatter");
    assert_eq!(text.replace("model: ccr-new", "model: old-alias"), original);
}

#[test]
fn agent_round_trip_is_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");

    for (i, original) in [AGENT_LF, AGENT_CRLF_NO_TRAILING, AGENT_NO_FRONTMATTER]
        .into_iter()
        .enumerate()
    {
        let path = dir.path().join(format!("agent{i}.md"));
        fs::write(&path, original).unwrap();
        let entry = set_model_recorded(&path, ModelChoice::Alias("ccr-kimi-k3"), &backups).unwrap();
        assert!(entry.existed && entry.backup_file.is_some());
        restore_agent(&entry, &backups).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            original.as_bytes(),
            "case {i} 必须逐字节还原"
        );
    }
}

/// 备份被清理掉时，还原清单本身必须仍然足够精确 —— 否则 spec §8.3 的"还原清单"
/// 就只是备份的别名，而备份是会被清理的。
#[test]
fn restore_agent_reconstructs_when_backup_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");

    for (i, original) in [AGENT_LF, AGENT_CRLF_NO_TRAILING, AGENT_NO_FRONTMATTER]
        .into_iter()
        .enumerate()
    {
        let path = dir.path().join(format!("no-bak{i}.md"));
        fs::write(&path, original).unwrap();
        let entry = set_model_recorded(&path, ModelChoice::Alias("ccr-kimi-k3"), &backups).unwrap();
        fs::remove_file(backups.join(entry.backup_file.as_ref().unwrap())).unwrap();

        restore_agent(&entry, &backups).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            original.as_bytes(),
            "case {i} 必须仅凭清单逐字节还原"
        );
    }
}

#[test]
fn create_then_delete_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    let backups = dir.path().join("backups");

    let path = create_agent(
        &agents,
        "code-reviewer",
        "审查代码",
        ModelChoice::Alias("ccr-kimi-k3"),
        "你是代码审查专家。\n",
        &backups,
    )
    .unwrap();
    assert_eq!(path, agents.join("code-reviewer.md"));
    let text = read_text(&path);
    assert!(text.starts_with("---\n"), "{text:?}");
    assert!(text.contains("name: code-reviewer\n"));
    assert!(text.contains("description: 审查代码\n"));
    assert!(text.contains("model: ccr-kimi-k3\n"));
    assert!(text.ends_with("你是代码审查专家。\n"));

    delete_agent(&path, &backups).unwrap();
    assert!(!path.exists());
    let baks: Vec<String> = fs::read_dir(backups.join("agents"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        baks.iter().any(|n| n.starts_with("code-reviewer.md.") && n.ends_with(".bak")),
        "删除前必须先备份: {baks:?}"
    );
}

#[test]
fn list_agents_is_recursive_and_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    fs::create_dir_all(agents.join("z-sub")).unwrap();
    fs::create_dir_all(agents.join("a-sub")).unwrap();
    fs::write(agents.join("z-sub/one.md"), "---\nname: One\n---\n正文\n").unwrap();
    fs::write(agents.join("a-sub/two.md"), "---\nname: Two\nmodel: ccr-x\n---\n正文\n").unwrap();
    fs::write(agents.join("top.md"), "---\nname: Top\ndescription: 顶层\n---\n正文\n").unwrap();
    fs::write(agents.join("ignore.txt"), "not markdown").unwrap();

    let list = list_agents(&agents).unwrap();
    assert_eq!(list.len(), 3, "必须递归列出 **/*.md，且忽略非 .md: {list:?}");
    assert!(
        list.windows(2).all(|w| w[0].path <= w[1].path),
        "必须按路径排序: {:?}",
        list.iter().map(|a| &a.path).collect::<Vec<_>>()
    );
    let one = list.iter().find(|a| a.name.as_deref() == Some("One")).unwrap();
    assert_eq!(one.model, None);
    assert_eq!(one.model_line, None);
    let two = list.iter().find(|a| a.name.as_deref() == Some("Two")).unwrap();
    assert_eq!(two.model.as_deref(), Some("ccr-x"));
    assert_eq!(two.model_line, Some(3), "model 行在第 3 行（1 起）");
    let top = list.iter().find(|a| a.name.as_deref() == Some("Top")).unwrap();
    assert_eq!(top.description.as_deref(), Some("顶层"));
    assert_eq!(top.model, None);

    // 目录不存在时返回空表，而不是报错（首次使用时 agents/ 目录可能还没有）
    assert!(list_agents(&dir.path().join("nope")).unwrap().is_empty());
}

#[test]
fn no_frontmatter_file_gains_one_for_alias_but_not_for_default() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");

    let path = dir.path().join("plain.md");
    fs::write(&path, AGENT_NO_FRONTMATTER).unwrap();
    set_model(&path, ModelChoice::Alias("ccr-kimi-k3"), &backups).unwrap();
    let text = read_text(&path);
    let expected_prefix = "---\nname: plain\nmodel: ccr-kimi-k3\n---\n";
    assert!(text.starts_with(expected_prefix), "{text:?}");
    assert!(text.ends_with(AGENT_NO_FRONTMATTER), "正文必须原样保留");

    let also = dir.path().join("plain-inherit.md");
    fs::write(&also, AGENT_NO_FRONTMATTER).unwrap();
    set_model(&also, ModelChoice::Inherit, &backups).unwrap();
    assert!(read_text(&also).starts_with("---\nname: plain-inherit\nmodel: inherit\n---\n"));

    // SubagentDefault：没有 model 行可删 → 一个字节都不改
    let untouched = dir.path().join("plain-default.md");
    fs::write(&untouched, AGENT_NO_FRONTMATTER).unwrap();
    set_model(&untouched, ModelChoice::SubagentDefault, &backups).unwrap();
    assert_eq!(fs::read(&untouched).unwrap(), AGENT_NO_FRONTMATTER.as_bytes());
}

// ---------------------------------------------------------------- Task 4

/// 接管清单要存进 `config.json` 的 `takeover.manifest`：**应用重启后还原仍必须逐字节相等**。
/// 这里用 tempdir + 显式路径走完整往返（IPC 需要 Tauri 运行时，逻辑在本层验证）。
#[test]
fn manifest_survives_config_round_trip_and_still_restores_byte_exact() {
    use cc_router::claude::settings::TakeoverManifest;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let backups = dir.path().join("backups");
    let config_path = dir.path().join("config.json");
    fs::write(&path, REAL_SHAPE).unwrap();
    let before = fs::read(&path).unwrap();
    let mut cfg = demo_cfg();

    let manifest = apply_takeover(&cfg, &path, &backups).unwrap();
    assert_ne!(fs::read(&path).unwrap(), before, "接管必须真的改了文件");

    // 模拟 takeover_apply 落盘配置
    cfg.takeover = TakeoverState {
        enabled: true,
        applied_at: Some(manifest.applied_at.clone()),
        backup_file: Some(manifest.pre_bytes_file.clone()),
        manifest: Some(serde_json::to_value(&manifest).unwrap()),
        ..TakeoverState::default()
    };
    cc_router::config::store::atomic_write_json(&config_path, &cfg).unwrap();

    // 模拟应用重启：从磁盘重新加载配置，取回清单
    let reloaded = cc_router::config::store::load_from(&config_path).unwrap();
    assert!(reloaded.takeover.enabled);
    assert_eq!(reloaded.takeover.backup_file.as_deref(), Some(manifest.pre_bytes_file.as_str()));
    let stored: TakeoverManifest =
        serde_json::from_value(reloaded.takeover.manifest.clone().expect("manifest 必须落盘")).unwrap();
    assert_eq!(stored, manifest, "清单往返不得有任何损失");

    let out = restore(&stored, &path, &backups).unwrap();
    assert_eq!(out.path, RestorePath::Verbatim);
    assert_eq!(fs::read(&path).unwrap(), before, "重启后还原仍必须逐字节相同");
}

/// `takeover.manifest` 是本计划新加的字段：加入之前写的 config.json 必须仍能反序列化。
#[test]
fn legacy_config_without_manifest_field_still_deserializes() {
    let legacy = serde_json::json!({
        "version": 1,
        "gateway": {
            "bind": "127.0.0.1",
            "port": 8787,
            "localToken": "sk-ccr-legacy",
            "maxRequestBodyBytes": 134217728,
            "connectTimeoutMs": 10000,
            "idleTimeoutMs": 300000
        },
        "providers": [],
        "roles": {},
        "extraRoutes": [],
        "onUnknownModel": "default",
        "defaultTarget": null,
        "takeover": {
            "enabled": false,
            "appliedAt": null,
            "backupFile": null,
            "settingsKeys": null,
            "agentFiles": null
        },
        "ui": {
            "closeToTray": true,
            "autostart": false,
            "requestLogToFile": false,
            "restoreOnExit": false
        }
    });

    let cfg: Config = serde_json::from_value(legacy).unwrap();
    assert_eq!(cfg.takeover.manifest, None);
    assert!(!cfg.takeover.enabled);
    assert_eq!(cfg.gateway.port, 8787);
}



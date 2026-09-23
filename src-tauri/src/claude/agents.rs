//! `~/.claude/agents/**/*.md` 的 frontmatter `model:` 行管理（spec §8）。
//!
//! # 唯一改动点
//!
//! 只改 frontmatter 内的 `model:` 行；frontmatter 其它字段与正文**逐字节保留**，
//! 并保持原文件的 CRLF/LF 与"文件末尾是否有换行"。这是还原能逐字节相等的前提。
//!
//! 边界约定（计划未规定，这里明确下来）：
//! - frontmatter = 文件第一行是 `---`（可带 `\r`）且能找到下一个 `---` 行。
//!   **未闭合的 `---` 不视为 frontmatter**。
//! - 只认行首（第 0 列）的 `model:`，`model_name:` 之类不会被误伤。
//! - 没有 frontmatter 的文件在 `Alias`/`Inherit` 时于**文件开头**插入一整块
//!   `---\nname: <文件名主干>\nmodel: …\n---\n`；`SubagentDefault` 时不动。

use super::atomic_write_bytes;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `~/.claude/agents`。
pub fn agents_dir() -> PathBuf {
    crate::claude::settings::claude_dir().join("agents")
}

/// 子 Agent 备份落在 `<backups_root>/agents/`（spec §8.2）。
pub fn backups_dir(backups_root: &Path) -> PathBuf {
    backups_root.join("agents")
}

/// 每个子 Agent 的 `model` 三选一（spec §8.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelChoice<'a> {
    /// 跟随主模型 → `model: inherit`。
    Inherit,
    /// 使用 subagent 默认 → **删除** `model:` 行（走 `CLAUDE_CODE_SUBAGENT_MODEL`）。
    SubagentDefault,
    /// 指定模型 → `model: <别名>`。别名是否在别名表里由 UI 负责（Plan 3），这里不校验。
    Alias(&'a str),
}

impl<'a> ModelChoice<'a> {
    fn written_value(self) -> Option<&'a str> {
        match self {
            ModelChoice::Inherit => Some("inherit"),
            ModelChoice::SubagentDefault => None,
            ModelChoice::Alias(alias) => Some(alias),
        }
    }
}

/// 列表用的轻量视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFile {
    pub path: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    /// `model:` 行在文件中的行号（**1 起**）。没有该行时为 `None`。
    pub model_line: Option<usize>,
}

/// 还原清单条目（spec §8.3）。
///
/// 与 spec 的 `{path, existed, modelLine}` 相比多了四个字段，都是为了在**没有备份**时
/// 也能逐字节还原：`had_frontmatter`（原本没有 frontmatter 时要删掉我们插入的整块）、
/// `crlf` / `trailing_newline`（重建插入行时的换行风格）、`backup_file`（有备份时优先
/// 逐字节回放）、`model_line_index`（把被删掉的 `model:` 行插回**原位置**而不是紧贴 `---`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentManifestEntry {
    pub path: PathBuf,
    pub existed: bool,
    /// 原始 `model:` 行文本（**含其行尾换行符**；原本没有该行时为 `None`）。
    #[serde(default)]
    pub model_line: Option<String>,
    /// 原始 `model:` 行的行号（**1 起**；原本没有该行时为 `None`）。
    ///
    /// 降级还原（备份不可用）时靠它把行插回原位：只看 `model_line` 文本时，只能把行塞在
    /// `---` 之后，而原位置可能在第 3、第 5 行 —— 还原出来的文件与原文不同。
    #[serde(default)]
    pub model_line_index: Option<usize>,
    #[serde(default)]
    pub crlf: bool,
    #[serde(default)]
    pub trailing_newline: bool,
    #[serde(default)]
    pub had_frontmatter: bool,
    /// 改动前的字节备份，路径相对 `backups_root`（如 `agents/a.md-<path-hash>.<stamp>.bak`）。
    #[serde(default)]
    pub backup_file: Option<String>,
}

/// spec §8.3 的 `takeover.agentFiles`：**以文件路径为键**的还原清单。
pub type AgentManifestMap = std::collections::BTreeMap<String, AgentManifestEntry>;

/// 递归列出 `agents_dir/**/*.md`，按路径排序。目录不存在时返回空表（首次使用时的常态）。
pub fn list_agents(agents_dir: &Path) -> Result<Vec<AgentFile>> {
    let mut out = Vec::new();
    if !agents_dir.exists() {
        return Ok(out);
    }
    collect(agents_dir, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<AgentFile>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir.to_path_buf(), e))?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.extension().map(|e| e.eq_ignore_ascii_case("md")).unwrap_or(false) {
            out.push(read_agent(&path)?);
        }
    }
    subdirs.sort();
    for sub in subdirs {
        collect(&sub, out)?;
    }
    Ok(())
}

fn read_agent(path: &Path) -> Result<AgentFile> {
    let text = read_text(path)?;
    let lines = split_lines(&text);
    let frontmatter = frontmatter_range(&lines);
    let (name, description, model, model_line) = match frontmatter {
        Some((open, close)) => (
            frontmatter_value(&lines, open, close, "name"),
            frontmatter_value(&lines, open, close, "description"),
            frontmatter_value(&lines, open, close, "model"),
            (open + 1..close).find(|&i| model_line_value(line_body(lines[i])).is_some()).map(|i| i + 1),
        ),
        None => (None, None, None, None),
    };
    Ok(AgentFile { path: path.to_path_buf(), name, description, model, model_line })
}

/// 三种选择各写一次（spec §8.2）。
pub fn set_model(path: &Path, choice: ModelChoice<'_>, backups_dir: &Path) -> Result<()> {
    set_model_recorded(path, choice, backups_dir).map(|_| ())
}

/// [`set_model`] 的记录版：同时返回还原清单条目，供 spec §8.3 的还原用。
pub fn set_model_recorded(
    path: &Path,
    choice: ModelChoice<'_>,
    backups_dir: &Path,
) -> Result<AgentManifestEntry> {
    set_model_recorded_at(path, choice, backups_dir, &super::backup_stamp())
}

/// 与 [`set_model_recorded`] 相同，但可显式指定备份时间戳。
///
/// 存在的理由与 `settings::apply_takeover_at` 一样：只有能固定 stamp，才能确定性地测出
/// "两个不同目录下的同名文件在同一次操作里不得共用备份" 这条不变量。
pub fn set_model_recorded_at(
    path: &Path,
    choice: ModelChoice<'_>,
    backups_dir: &Path,
    stamp: &str,
) -> Result<AgentManifestEntry> {
    let original = read_text(path)?;
    let lines = split_lines(&original);
    let frontmatter = frontmatter_range(&lines);
    let model_index = frontmatter
        .and_then(|(open, close)| (open + 1..close).find(|&i| model_line_value(line_body(lines[i])).is_some()));

    let entry = AgentManifestEntry {
        path: path.to_path_buf(),
        existed: true,
        model_line: model_index.map(|i| lines[i].to_string()),
        model_line_index: model_index.map(|i| i + 1),
        crlf: detect_newline(&original) == "\r\n",
        trailing_newline: original.ends_with('\n'),
        had_frontmatter: frontmatter.is_some(),
        backup_file: None,
    };

    let rewritten = rewrite(&original, choice, path)?;
    if rewritten == original {
        // 没有任何改动（例如 SubagentDefault 而文件本来就没有 model 行）→ 不写盘、不备份。
        return Ok(entry);
    }

    let backup_file = write_backup_once(path, original.as_bytes(), backups_dir, stamp)?;
    atomic_write_bytes(path, rewritten.as_bytes())?;
    Ok(AgentManifestEntry { backup_file: Some(backup_file), ..entry })
}

/// 新建子 Agent（spec §8.1：该目录可能是空的，必须支持新建）。
///
/// `_backups_dir` 保留在签名里与其它 API 一致：新建时原文件并不存在，没有东西可备份。
pub fn create_agent(
    agents_dir: &Path,
    name: &str,
    description: &str,
    choice: ModelChoice<'_>,
    body: &str,
    _backups_dir: &Path,
) -> Result<PathBuf> {
    validate_agent_name(name)?;
    let path = agents_dir.join(format!("{name}.md"));
    if path.exists() {
        return Err(Error::ConfigInvalid(format!("子 Agent 已存在：{}", path.display())));
    }

    let nl = "\n";
    let mut text = format!("---{nl}name: {name}{nl}description: {description}{nl}");
    if let Some(value) = choice.written_value() {
        text.push_str(&format!("model: {value}{nl}"));
    }
    text.push_str(&format!("---{nl}"));
    text.push_str(body);
    atomic_write_bytes(&path, text.as_bytes())?;
    Ok(path)
}

/// [`create_agent`] 的记录版：同时返回还原清单条目（spec §8.3）。
///
/// `existed: false` —— 这个文件原本不存在，还原时必须**删除**它（删之前先备份，
/// 见 [`restore_agent`]：绝不无声销毁内容）。
pub fn create_agent_recorded(
    agents_dir: &Path,
    name: &str,
    description: &str,
    choice: ModelChoice<'_>,
    body: &str,
    backups_dir: &Path,
) -> Result<AgentManifestEntry> {
    let path = create_agent(agents_dir, name, description, choice, body, backups_dir)?;
    Ok(AgentManifestEntry {
        path,
        existed: false,
        model_line: None,
        model_line_index: None,
        crlf: false,
        trailing_newline: false,
        had_frontmatter: false,
        backup_file: None,
    })
}

/// 删除子 Agent：**先备份再删除**（spec §8.2）。
pub fn delete_agent(path: &Path, backups_dir: &Path) -> Result<()> {
    delete_agent_at(path, backups_dir, &super::backup_stamp())
}

/// 与 [`delete_agent`] 相同，但可显式指定备份时间戳（理由见 [`set_model_recorded_at`]）。
pub fn delete_agent_at(path: &Path, backups_dir: &Path, stamp: &str) -> Result<()> {
    delete_agent_recorded_at(path, backups_dir, stamp).map(|_| ())
}

/// [`delete_agent`] 的记录版：同时返回还原清单条目（spec §8.3）。
///
/// 条目只带 `existed: true` 与备份文件名：删除是不可逆的内容销毁，**只能**靠备份逐字节回放；
/// 清单里的 `model_line` 之类无法重建被删掉的正文，所以不写进条目假装降级路径可用。
pub fn delete_agent_recorded(path: &Path, backups_dir: &Path) -> Result<AgentManifestEntry> {
    delete_agent_recorded_at(path, backups_dir, &super::backup_stamp())
}

/// 与 [`delete_agent_recorded`] 相同，但可显式指定备份时间戳。
pub fn delete_agent_recorded_at(
    path: &Path,
    backups_dir: &Path,
    stamp: &str,
) -> Result<AgentManifestEntry> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    let backup_file = write_backup_once(path, &bytes, backups_dir, stamp)?;
    std::fs::remove_file(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    Ok(AgentManifestEntry {
        path: path.to_path_buf(),
        existed: true,
        model_line: None,
        model_line_index: None,
        crlf: false,
        trailing_newline: false,
        had_frontmatter: false,
        backup_file: Some(backup_file),
    })
}

/// 还原清单的键：能用规范路径就用规范路径。
///
/// 同一文件的两种写法（大小写、`..`、符号链接、`\\?\` 前缀）必须命中**同一条**记录：
/// 否则"首次写入优先"的去重失效 —— 第二次改动另起一条记录，还原时两条都会被回放，
/// 结果停在中间态而不是改动前的原状。因此调用方要在**改动文件之前**取键（文件还在，
/// canonicalize 能成功）；新建文件只能在其写盘之后取键。
pub fn manifest_key(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let raw = resolved.to_string_lossy().to_string();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string()
}

/// 删除备份文件前的**目录守卫**（spec §9 的同一套思路）。
///
/// 界面会把备份文件路径回传上来，而这个路径直接进 `remove_file` —— 没有守卫的话，
/// 一个被污染/错误的前端参数就能删掉任意文件。所以两侧都 canonicalize：
/// 目标必须是 `backups/agents` **之内**的普通文件，目录本身也拒绝。
/// 任一侧无法规范化（目录不存在、文件不存在）即 fail-closed。
pub fn ensure_within_backups_dir(backups_agents_dir: &Path, target: &Path) -> Result<()> {
    let root = backups_agents_dir
        .canonicalize()
        .map_err(|e| Error::io(backups_agents_dir.to_path_buf(), e))?;
    let resolved = target
        .canonicalize()
        .map_err(|e| Error::io(target.to_path_buf(), e))?;
    if resolved == root || !resolved.starts_with(&root) {
        return Err(Error::Io {
            path: target.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "拒绝操作备份目录之外的文件",
            ),
        });
    }
    Ok(())
}

/// 批量还原（spec §8.3：一键还原要把清单里的每个子 Agent 文件都回放回去）。
///
/// 返回 `(成功还原的路径, 失败描述)`。**不因为一个文件失败就提前中断**：否则排在后面的文件
/// 会永远停在改动后的状态，而调用方只看到一个错误。
pub fn restore_agents<'a, I>(entries: I, backups_dir: &Path) -> (Vec<PathBuf>, Vec<String>)
where
    I: IntoIterator<Item = &'a AgentManifestEntry>,
{
    let mut restored = Vec::new();
    let mut failed = Vec::new();
    for entry in entries {
        match restore_agent(entry, backups_dir) {
            Ok(()) => restored.push(entry.path.clone()),
            Err(e) => failed.push(format!("{}: {e}", entry.path.display())),
        }
    }
    (restored, failed)
}

/// 按清单还原一个子 Agent（spec §8.3）。
pub fn restore_agent(entry: &AgentManifestEntry, backups_dir: &Path) -> Result<()> {
    if !entry.existed {
        // 我们新建的文件：先备份再删除，绝不无声销毁。
        if entry.path.exists() {
            let bytes = std::fs::read(&entry.path).map_err(|e| Error::io(entry.path.clone(), e))?;
            write_backup_once(&entry.path, &bytes, backups_dir, &super::backup_stamp())?;
            std::fs::remove_file(&entry.path).map_err(|e| Error::io(entry.path.clone(), e))?;
        }
        return Ok(());
    }

    if let Some(name) = &entry.backup_file {
        let backup = backups_dir.join(name);
        if let Ok(bytes) = std::fs::read(&backup) {
            return atomic_write_bytes(&entry.path, &bytes);
        }
    }

    // 备份不可用 → 仅凭清单重建（只依赖"改动点只有 model 行"这一不变量）。
    let text = read_text(&entry.path)?;
    let rebuilt = rebuild(&text, entry)?;
    atomic_write_bytes(&entry.path, rebuilt.as_bytes())
}

fn validate_agent_name(name: &str) -> Result<()> {
    let bad = name.trim() != name
        || name.is_empty()
        || name == "."
        || name == ".."
        || name.contains("..")
        || name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || name.chars().any(|c| c.is_control());
    if bad {
        return Err(Error::ConfigInvalid(format!(
            "子 Agent 名称非法：{name:?}（不得为空、不得含路径分隔符或 .. ）"
        )));
    }
    Ok(())
}

/// 纵深防御（spec §9）：确认 `candidate` 真的落在 `agents_dir` 之内。
///
/// IPC 只往这里传前端给的路径字符串，若不校验，一个拼接错误的路径就能改写/删除用户
/// 任意可写文件。**两侧都先 canonicalize**，因此下面三类逃逸都会被拒：
/// `..` 回退、指向别处的绝对路径、符号链接/junction 跳转。
///
/// `candidate` 必须存在（canonicalize 需要真实文件）——本函数服务于"改/删已存在的子 Agent"，
/// 新建路径由 [`create_agent`] 自己从 `agents_dir` 拼出。`agents_dir` 不存在时同样报错
/// （边界无法确立 → fail-closed）。
pub fn ensure_within_agents_dir(agents_dir: &Path, candidate: &Path) -> Result<()> {
    let root = agents_dir.canonicalize().map_err(|e| Error::io(agents_dir.to_path_buf(), e))?;
    let target = candidate
        .canonicalize()
        .map_err(|e| Error::io(candidate.to_path_buf(), e))?;
    if target.starts_with(&root) {
        Ok(())
    } else {
        Err(Error::ConfigInvalid(format!(
            "拒绝操作 {}：它不在子 Agent 目录 {} 之内",
            candidate.display(),
            agents_dir.display()
        )))
    }
}

// ------------------------------------------------------------ 文本手术

fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    String::from_utf8(bytes).map_err(|_| {
        Error::ConfigInvalid(format!("{} 不是 UTF-8 文本，拒绝改动", path.display()))
    })
}

/// 每行**含其行尾换行符**（最后一行可能没有）。重新拼接即得到原文本。
fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn line_body(line: &str) -> &str {
    line.strip_suffix('\n').map(|s| s.strip_suffix('\r').unwrap_or(s)).unwrap_or(line)
}

fn eol_of(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// 文件级换行风格：出现 `\r\n` 即认为 CRLF。
fn detect_newline(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// 第 0 列且形如 `model: …` 的行 → 返回去掉前缀后的值。`model_name:` 不算。
fn model_line_value(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("model:")?;
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(rest.trim())
    } else {
        None
    }
}

/// frontmatter = 首行 `---` 到下一个 `---` 行（两者都含）。未闭合时视为没有 frontmatter。
fn frontmatter_range(lines: &[&str]) -> Option<(usize, usize)> {
    if lines.is_empty() || line_body(lines[0]) != "---" {
        return None;
    }
    (1..lines.len()).find(|&i| line_body(lines[i]) == "---").map(|close| (0, close))
}

fn frontmatter_value(lines: &[&str], open: usize, close: usize, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in &lines[open + 1..close] {
        let body = line_body(line);
        let Some(rest) = body.strip_prefix(&prefix) else { continue };
        if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(unquote(rest.trim()));
        }
    }
    None
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// 只重写 frontmatter 里的 `model:` 行，其余字节原样搬运。
fn rewrite(text: &str, choice: ModelChoice<'_>, path: &Path) -> Result<String> {
    let lines = split_lines(text);
    let nl = detect_newline(text);
    let value = choice.written_value();

    let Some((open, close)) = frontmatter_range(&lines) else {
        return match value {
            Some(v) => {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut out = format!("---{nl}name: {stem}{nl}model: {v}{nl}---{nl}");
                out.push_str(text);
                Ok(out)
            }
            None => Ok(text.to_string()),
        };
    };

    let model_index =
        (open + 1..close).find(|&i| model_line_value(line_body(lines[i])).is_some());

    let mut out = String::with_capacity(text.len() + 32);
    for (i, line) in lines.iter().enumerate() {
        match (model_index, value) {
            (Some(idx), Some(v)) if i == idx => {
                // 保留原行的行尾：原行没有换行符（文件末尾）时也不新增，
                // 否则"文件末尾是否有换行"会变。
                out.push_str(&format!("model: {v}{}", eol_of(line)));
            }
            (Some(idx), None) if i == idx => {}
            _ => {
                out.push_str(line);
                if i == open {
                    if let (None, Some(v)) = (model_index, value) {
                        out.push_str(&format!("model: {v}{nl}"));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// 仅凭清单重建（备份不可用时的降级路径）。
fn rebuild(text: &str, entry: &AgentManifestEntry) -> Result<String> {
    let lines = split_lines(text);

    let Some((open, close)) = frontmatter_range(&lines) else {
        return Ok(text.to_string());
    };
    let model_index =
        (open + 1..close).find(|&i| model_line_value(line_body(lines[i])).is_some());

    // 原本没有 frontmatter：当前这一整块是我们插进去的（`---` / `name:` / `model:` / `---`），
    // 必须连块删除，只删 `model:` 行会留下残缺的 frontmatter。
    if !entry.had_frontmatter && looks_like_inserted_block(&lines) {
        let mut out = String::with_capacity(text.len());
        for line in &lines[4..] {
            out.push_str(line);
        }
        return Ok(out);
    }

    // 当前**没有** `model:` 行（`SubagentDefault` 把它删掉了）→ 把原行插回**原位置**。
    // 依据是清单里的 `model_line_index`；只凭行文本时"第 4 行"会被插成"第 2 行"，
    // 还原结果与原文不再逐字节相等。旧清单没有行号时退化为"紧跟 `---` 之后"（旧行为）。
    if model_index.is_none() {
        let Some(original) = &entry.model_line else {
            return Ok(text.to_string());
        };
        let insert_at = entry
            .model_line_index
            .map(|n| n.saturating_sub(1))
            .filter(|at| *at <= lines.len())
            // 没有行号（或行号越界）：插在开标签 `---` 之后。
            .unwrap_or(open + 1)
            // 绝不插到开标签之前：那会破坏 frontmatter。
            .max(1);
        let mut out = String::with_capacity(text.len() + original.len());
        for (i, line) in lines.iter().enumerate() {
            if i == insert_at {
                out.push_str(original);
            }
            out.push_str(line);
        }
        if insert_at >= lines.len() {
            out.push_str(original);
        }
        return Ok(out);
    }

    let mut out = String::with_capacity(text.len() + 32);
    for (i, line) in lines.iter().enumerate() {
        match (model_index, &entry.model_line) {
            (Some(idx), Some(original)) if i == idx => out.push_str(original),
            (Some(idx), None) if i == idx => {}
            _ => out.push_str(line),
        }
    }
    Ok(out)
}

/// 开头是否正是 [`rewrite`] 在无 frontmatter 时插入的 4 行块。
fn looks_like_inserted_block(lines: &[&str]) -> bool {
    lines.len() >= 4
        && line_body(lines[0]) == "---"
        && line_body(lines[1]).starts_with("name:")
        && model_line_value(line_body(lines[2])).is_some()
        && line_body(lines[3]) == "---"
}

/// 备份键：以**规范化的完整路径**为依据，而不是裸文件名。
///
/// 子 Agent 目录是递归的（spec §8.1）：`alpha/reviewer.md` 与 `beta/reviewer.md` 若都按
/// `reviewer.md` 命名，第二次备份会被 write-if-absent 静默跳过，而两次的清单记录指向同一个
/// 备份文件 —— 还原时会把 alpha 的字节写进 beta，且 beta 的原文从未被备份（不可恢复）。
///
/// **完整路径的哈希是无条件附加的**，不是"只在超长时才加"：扁平化把所有非
/// `[A-Za-z0-9.\-_]` 字符压成 `_`，因此 `/`、`\`、空格与**本来就存在的 `_`** 不可区分 ——
/// `agents/a/b.md` 与 `agents/a_b.md`、`agents/reviewer pro.md` 与 `agents/reviewer_pro.md`
/// 都得到同一个 71 字符的扁平串。按长度阈值决定是否加哈希，等于让最常见的短路径落进
/// 上面那条不可恢复的碰撞路径；只有无条件哈希才能让映射（在 64 位 FNV-1a 的强度内）单射。
/// 备份名使用的**扁平化**规则：`[A-Za-z0-9.\-_]` 之外一律换成 `_`（于是 `/`、`\`、空格与
/// 本来存在的 `_` 不可区分 —— 单独用它不是单射，所以备份名还要再附完整路径的哈希）。
///
/// 抽成函数是为了**从备份名反查原路径**（`agent_backups_list`）：备份名里带着扁平化后的
/// 完整路径，用同一条规则把已知路径扁平化后比对，就能把"那个看不懂的文件名"还原成"哪个 agent"。
pub(crate) fn flatten_for_backup(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    for ch in text.chars() {
        // 非 ASCII 字符也替换成 '_'：结果保证是纯 ASCII，后面按字节切片才安全。
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            flat.push(ch);
        } else {
            flat.push('_');
        }
    }
    flat
}

/// 把路径按 `backup_name` 同一套规则规范化（canonicalize → 去掉 `\\?\` 前缀）后扁平化。
pub(crate) fn flatten_path(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let raw = resolved.to_string_lossy().to_string();
    let text = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    flatten_for_backup(text)
}

fn backup_name(path: &Path, stamp: &str) -> String {
    // canonicalize 让同一文件的不同写法（大小写、`..`、符号链接）落到同一个键上，
    // 使"同一文件同一 stamp 只备份一次"仍然成立。
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let raw = resolved.to_string_lossy().to_string();
    let text = raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string();

    let mut flat = flatten_for_backup(&text);
    let hash = fnv1a_hex16(&text);
    if flat.len() > 120 {
        // 文件名长度上限（Windows 255）：保留信息量最大的尾部，并附**完整路径**的哈希
        // 以保证截断后仍然唯一。
        let tail = flat[flat.len() - 80..].to_string();
        flat = format!("{tail}-{hash}");
    } else {
        // 短路径分支同样必须附哈希：扁平化本身不是单射。
        flat.push('-');
        flat.push_str(&hash);
    }
    format!("agents/{flat}.{stamp}.bak")
}

fn fnv1a_hex16(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 备份一次：同一个 `(文件, 时间戳)` 只备份一次，绝不覆盖已有备份
/// （覆盖会把"已改动的内容"当成原始状态）。
fn write_backup_once(path: &Path, bytes: &[u8], backups_root: &Path, stamp: &str) -> Result<String> {
    let relative = backup_name(path, stamp);
    let full = backups_root.join(&relative);
    if !full.exists() {
        atomic_write_bytes(&full, bytes)?;
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 备份键必须**按路径**区分同名文件，且长度受控。
    #[test]
    fn backup_name_is_unique_per_path_and_length_bounded() {
        let a = Path::new(r"C:\x\agents\alpha\reviewer.md");
        let b = Path::new(r"C:\x\agents\beta\reviewer.md");
        // 这些路径并不存在 → canonicalize 失败 → 回退到原样字符串，仍然可区分。
        assert_ne!(
            backup_name(a, "S"),
            backup_name(b, "S"),
            "不同目录下的同名文件必须得到不同的备份键"
        );
        assert!(
            backup_name(a, "S").contains("reviewer.md"),
            "备份名要保留文件名以便人工辨认: {}",
            backup_name(a, "S")
        );
        assert!(backup_name(a, "S").ends_with(".S.bak"));

        // 超长路径（截断分支）仍必须唯一且不超过文件名长度上限
        let long_a = format!(r"C:\{}.md", "d".repeat(400));
        let long_b = format!(r"C:\{}x.md", "d".repeat(400));
        let name = backup_name(Path::new(&long_a), "20260922T153000");
        let file_name = name.rsplit('/').next().unwrap();
        assert!(file_name.len() < 200, "备份文件名过长（{}）: {file_name}", file_name.len());
        assert_ne!(
            backup_name(Path::new(&long_a), "S"),
            backup_name(Path::new(&long_b), "S"),
            "截断后仍必须靠路径哈希保持唯一"
        );
    }

    /// CRITICAL：扁平化会把**所有**非 `[A-Za-z0-9.\-_]` 字符压成 `_`，其中的 `/`、`\`、空格
    /// 与**本来就存在的 `_`** 是同一个结果。若只在扁平串超过 120 字符时才附路径哈希，那么
    /// 最常见的短路径恰好落在碰撞区（实测 `...\agents\a\b.md` 与 `...\agents\a_b.md` 的扁平串
    /// 都是 71 字符）：两个不同文件得到同一个备份键 ⇒ `write_backup_once` 静默跳过第二次备份，
    /// 而两份清单都指向第一个备份 ⇒ `restore_agent` 把第一个文件的字节写进第二个文件，
    /// 第二个文件的原文永久丢失。
    #[test]
    fn backup_name_is_injective_for_paths_that_flatten_alike() {
        let pairs = [
            (r"C:\x\agents\a\b.md", r"C:\x\agents\a_b.md"),
            (r"C:\x\agents\reviewer pro.md", r"C:\x\agents\reviewer_pro.md"),
            // 非 ASCII 字符同样被逐个扁平化成 '_'，所以**同长度的中文名**曾互相碰撞
            // （两个字符的 `评审` 与 `测试` 都变成 `__`），`评审\a.md` 与 `评审_a.md` 也是。
            (r"C:\x\agents\评审.md", r"C:\x\agents\测试.md"),
            (r"C:\x\agents\评审\a.md", r"C:\x\agents\评审_a.md"),
        ];
        for (left, right) in pairs {
            let ln = backup_name(Path::new(left), "S");
            let rn = backup_name(Path::new(right), "S");
            assert_ne!(
                ln, rn,
                "{left} 与 {right} 扁平化后同名，备份键必须仍然可区分（否则一方永不备份）"
            );
        }
    }

    #[test]
    fn ensure_within_agents_dir_accepts_inside_and_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(agents.join("sub")).unwrap();
        let inside = agents.join("sub/ok.md");
        std::fs::write(&inside, "---\nname: ok\n---\n").unwrap();
        let outside_dir = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("bad.md");
        std::fs::write(&outside, "---\nname: bad\n---\n").unwrap();
        // 规范化之后等价于 outside 的".."路径
        let traversal = agents.join("sub/../../elsewhere/bad.md");

        assert!(ensure_within_agents_dir(&agents, &inside).is_ok(), "目录内的文件必须放行");
        assert!(ensure_within_agents_dir(&agents, &agents).is_ok(), "目录自身也算在内");
        assert!(
            ensure_within_agents_dir(&agents, &outside).is_err(),
            "目录外的绝对路径必须拒绝"
        );
        assert!(
            ensure_within_agents_dir(&agents, &traversal).is_err(),
            "`..` 回退逃逸必须拒绝"
        );
        let err = ensure_within_agents_dir(&agents, &outside).unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)), "必须是清晰的拒绝错误: {err:?}");

        // 无法 canonicalize（不存在）→ 拒绝，而不是放行
        assert!(
            ensure_within_agents_dir(&agents, &agents.join("nope.md")).is_err(),
            "不存在的候选路径无法确立边界，必须 fail-closed"
        );
        // 目录不存在 → 拒绝（边界无法确立）
        assert!(ensure_within_agents_dir(&dir.path().join("no-such-dir"), &inside).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn ensure_within_agents_dir_rejects_symlink_escape() {        use std::os::windows::fs::symlink_file;
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let outside = dir.path().join("secret.md");
        std::fs::write(&outside, "---\nname: secret\n---\n").unwrap();
        let link = agents.join("link.md");
        if symlink_file(&outside, &link).is_err() {
            // 未开启开发者模式/无权限时无法创建符号链接；此时该子断言无法执行。
            // 这不是"静默通过"：其余三类逃逸已在另一个测试里被真实覆盖。
            eprintln!("skip: 本机不允许创建符号链接（需要开发者模式）");
            return;
        }
        assert!(
            ensure_within_agents_dir(&agents, &link).is_err(),
            "指向目录外的符号链接必须被拒（canonicalize 后落在 agents 之外）"
        );
    }

    /// 删除备份前的目录守卫：这是"界面回传路径 → `remove_file`"这条链上唯一的防线。
    #[test]
    fn backup_guard_allows_inside_and_rejects_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let inside = agents.join("reviewer.md-29d8bf14ee6ea9ea.20260923T135817.bak");
        std::fs::write(&inside, b"backup").unwrap();
        let outside = dir.path().join("settings.json.pre.bak");
        std::fs::write(&outside, b"secret").unwrap();

        assert!(
            ensure_within_backups_dir(&agents, &inside).is_ok(),
            "备份目录之内的普通文件应当允许删除"
        );
        assert!(
            ensure_within_backups_dir(&agents, &outside).is_err(),
            "备份目录之外的文件必须被拒 —— 否则一个被污染的参数就能删掉任意文件"
        );
        assert!(
            ensure_within_backups_dir(&agents, &agents).is_err(),
            "目录自身必须被拒"
        );
        assert!(
            ensure_within_backups_dir(&agents, &agents.join("不存在的.bak")).is_err(),
            "不存在的路径必须 fail-closed"
        );
        assert!(
            ensure_within_backups_dir(&agents, &inside.join("..").join("..").join("settings.json.pre.bak"))
                .is_err(),
            "`..` 逃逸必须被拒"
        );
    }
}

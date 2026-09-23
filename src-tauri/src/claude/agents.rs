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
/// 与 spec 的 `{path, existed, modelLine}` 相比多了三个字段，都是为了在**没有备份**时
/// 也能逐字节还原：`had_frontmatter`（原本没有 frontmatter 时要删掉我们插入的整块）、
/// `crlf` / `trailing_newline`（重建插入行时的换行风格）、`backup_file`（有备份时优先
/// 逐字节回放）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentManifestEntry {
    pub path: PathBuf,
    pub existed: bool,
    /// 原始 `model:` 行文本（**含其行尾换行符**；原本没有该行时为 `None`）。
    #[serde(default)]
    pub model_line: Option<String>,
    #[serde(default)]
    pub crlf: bool,
    #[serde(default)]
    pub trailing_newline: bool,
    #[serde(default)]
    pub had_frontmatter: bool,
    /// 改动前的字节备份，路径相对 `backups_root`（如 `agents/a.md.<stamp>.bak`）。
    #[serde(default)]
    pub backup_file: Option<String>,
}

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
    let original = read_text(path)?;
    let lines = split_lines(&original);
    let frontmatter = frontmatter_range(&lines);
    let model_index = frontmatter
        .and_then(|(open, close)| (open + 1..close).find(|&i| model_line_value(line_body(lines[i])).is_some()));

    let entry = AgentManifestEntry {
        path: path.to_path_buf(),
        existed: true,
        model_line: model_index.map(|i| lines[i].to_string()),
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

    let backup_file = write_backup_once(path, original.as_bytes(), backups_dir)?;
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

/// 删除子 Agent：**先备份再删除**（spec §8.2）。
pub fn delete_agent(path: &Path, backups_dir: &Path) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    write_backup_once(path, &bytes, backups_dir)?;
    std::fs::remove_file(path).map_err(|e| Error::io(path.to_path_buf(), e))
}

/// 按清单还原一个子 Agent（spec §8.3）。
pub fn restore_agent(entry: &AgentManifestEntry, backups_dir: &Path) -> Result<()> {
    if !entry.existed {
        // 我们新建的文件：先备份再删除，绝不无声销毁。
        if entry.path.exists() {
            let bytes = std::fs::read(&entry.path).map_err(|e| Error::io(entry.path.clone(), e))?;
            write_backup_once(&entry.path, &bytes, backups_dir)?;
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

    let mut out = String::with_capacity(text.len() + 32);
    for (i, line) in lines.iter().enumerate() {
        match (model_index, &entry.model_line) {
            (Some(idx), Some(original)) if i == idx => out.push_str(original),
            (Some(idx), None) if i == idx => {}
            _ => {
                out.push_str(line);
                if i == open {
                    if let (None, Some(original)) = (model_index, &entry.model_line) {
                        out.push_str(original);
                    }
                }
            }
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

/// 备份一次：同一个 `(文件, 时间戳)` 只备份一次，绝不覆盖已有备份
/// （覆盖会把"已改动的内容"当成原始状态）。
fn write_backup_once(path: &Path, bytes: &[u8], backups_root: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| Error::ConfigInvalid(format!("无法取文件名：{}", path.display())))?;
    let relative = format!("agents/{file_name}.{}.bak", super::backup_stamp());
    let full = backups_root.join(&relative);
    if !full.exists() {
        atomic_write_bytes(&full, bytes)?;
    }
    Ok(relative)
}

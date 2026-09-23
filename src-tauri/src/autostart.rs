//! 开机自启：写/删 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下名为
//! `CC Router` 的字符串值（spec §12 / ledger R40 修正）。
//!
//! # 为什么走 `reg.exe` 而不是 Tauri autostart 插件
//!
//! Plan 4 的硬约束是"不新增依赖"，插件要求新增一个 Cargo 依赖；而 HKCU 写入本来就不需要
//! 管理员权限，`reg.exe` 是 Windows 自带、随系统版本稳定存在的接口。所以这里用
//! `std::process::Command` 驱动 `reg.exe`，并加 `CREATE_NO_WINDOW` 以免弹出控制台黑框。
//!
//! # 本模块不依赖 `tauri`
//!
//! 项目约定：核心逻辑保持与 Tauri 无关，普通 `cargo test` 就能覆盖。因此
//! 纯逻辑（参数拼装、`reg query` 输出解析）与"真的去动注册表"的薄壳分开：
//! 前者任何平台都可测，后者才 `#[cfg(windows)]`。
//!
//! # 注册表是唯一事实来源（裁定，见 ledger R40 修正）
//!
//! `ui.autostart` 只是注册表的**镜像**（缓存），**绝不用它反写注册表**：
//!
//! - 注册表**只在用户调用 [`set_enabled`]（即拨动设置页开关）时**才改变；
//! - **启动时不写注册表**，`save_config` 也**不写**注册表（没有任何隐藏副作用）；
//! - `config_get` 以注册表实测值覆盖 `ui.autostart`，所以界面永远显示系统真实状态；
//! - 推论：**手改 `config.json` 里的 `autostart` 字段不会生效**。这是刻意的 ——
//!   系统级状态不应该被一个数据文件悄悄改变。
//!
//! 反方案（"启动时按配置意图对齐注册表"）被否决的原因：`UiConfig::default()` 里
//! `autostart: true`，所以"打开一次应用"就会在用户的 HKCU Run 里静默写入一条开机自启项 ——
//! 那是用户没有要求的系统级副作用。

/// 注册表值名。固定，不做成配置项：改名会让旧值变成孤儿条目。
pub const VALUE_NAME: &str = "CC Router";

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`（`reg.exe` 接受的短写形式）。
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

// ---------------------------------------------------------------- 纯逻辑（可跨平台测试）

/// 把 exe 绝对路径包成注册表数据（spec §12：**值 = 带引号的当前 exe 绝对路径**）。
///
/// 引号是必须的：路径可能含空格，不加引号 Windows 会去解析 argv 而不是整个路径。
pub fn quoted_path(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// `reg.exe query <key> /v <name>` 的参数表。
pub fn query_argv() -> Vec<String> {
    ["query", RUN_KEY, "/v", VALUE_NAME]
        .map(str::to_string)
        .to_vec()
}

/// `reg.exe add <key> /v <name> /t REG_SZ /d <data> /f` 的参数表。
///
/// `/f` 是"已存在就覆盖"：不加的话 `reg add` 会交互式问 `Value CC Router exists, overwrite(Yes/No)?`，
/// 而我们是用 `CREATE_NO_WINDOW` 启动的、没有 stdin 可以回答。
pub fn add_argv(data: &str) -> Vec<String> {
    ["add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", data, "/f"]
        .map(str::to_string)
        .to_vec()
}

/// `reg.exe delete <key> /v <name> /f` 的参数表。
pub fn delete_argv() -> Vec<String> {
    ["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]
        .map(str::to_string)
        .to_vec()
}

/// 把 `reg query` 输出里的一行拆成 `(值名, 数据)`；不是"值行"则返回 `None`。
///
/// `reg.exe` 用空白对齐输出三列，且**中间的空格数不固定**，所以按"第一个 `REG_*` 记号"
/// 切分：记号左边合起来是值名（我们的值名 `CC Router` 自带一个空格，按空白切会碎掉），
/// 右边合起来是数据。键路径表头行与"找不到"错误行都没有 `REG_*` 记号，自然被排除。
fn split_reg_line(line: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let kind_at = tokens.iter().position(|t| t.starts_with("REG_"))?;
    if kind_at == 0 {
        // 没有值名，只有一个类型记号 —— 不是值行。
        return None;
    }
    Some((tokens[..kind_at].join(" "), tokens[kind_at + 1..].join(" ")))
}

/// 从完整 `reg query` 输出里取出 `value_name` 的数据；**值不存在则返回 `None`**。
///
/// 判据是"输出里出现了这个名字的值行"，与 `reg.exe` 的退出码无关：值不存在时 `reg.exe`
/// 以退出码 1 + 一行错误信息结束，而那行解析不出值行 —— 两种情况都归到 `None`。
pub fn parse_query_stdout(stdout: &str, value_name: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(split_reg_line)
        .find(|(name, _)| name.eq_ignore_ascii_case(value_name))
        .map(|(_, data)| data)
}

/// [`parse_query_stdout`] 的布尔版：注册表里到底有没有这条开机自启。
pub fn is_enabled_from_stdout(stdout: &str) -> bool {
    parse_query_stdout(stdout, VALUE_NAME).is_some()
}

// ---------------------------------------------------------------- 平台薄壳

#[cfg(windows)]
mod platform {
    use super::{add_argv, delete_argv, is_enabled_from_stdout, query_argv, quoted_path};
    use std::process::{Command, Output};

    /// `CREATE_NO_WINDOW`：`reg.exe` 是控制台程序，不屏蔽窗口就会闪一个黑框。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(argv: &[String]) -> std::io::Result<Output> {
        use std::os::windows::process::CommandExt;
        Command::new("reg.exe")
            .args(argv)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    /// 注册表里现在**实际**有没有 `CC Router` 这条自启项。
    ///
    /// 读失败（`reg.exe` 起不来等）按"没有"处理并写明原因：读不到时既不能说"已开启"，
    /// 也不该让设置页因为一次读取失败就整个报错。
    pub fn is_enabled() -> bool {
        match reg(&query_argv()) {
            Ok(out) => {
                // 注意：`reg.exe` 按控制台代码页输出，非 ASCII 路径可能被 lossy 解码替换。
                // 我们只看"有没有这一行"，值名与 `REG_SZ` 都是 ASCII，因此不受影响。
                is_enabled_from_stdout(&String::from_utf8_lossy(&out.stdout))
            }
            Err(e) => {
                eprintln!("[cc-router] 读取开机自启注册表失败（按“未开启”处理）：{e}");
                false
            }
        }
    }

    /// 写/删注册表项，返回**实测**状态（不是请求值）。
    ///
    /// 写失败时不能假装成功：返回实测值，前端据此把开关拨回去并报错。
    pub fn set_enabled(enabled: bool) -> bool {
        let argv = if enabled {
            match std::env::current_exe() {
                Ok(exe) => add_argv(&quoted_path(&exe)),
                Err(e) => {
                    eprintln!("[cc-router] 取当前 exe 路径失败，开机自启未改动：{e}");
                    return is_enabled();
                }
            }
        } else {
            delete_argv()
        };

        if let Err(e) = reg(&argv) {
            eprintln!("[cc-router] 修改开机自启注册表失败：{e}");
        }
        is_enabled()
    }
}

#[cfg(windows)]
pub use platform::{is_enabled, set_enabled};

/// 非 Windows 目标：没有 HKCU Run 可写，功能恒为关闭（保证其它目标仍能编译）。
#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

/// 非 Windows 目标：没有 HKCU Run 可写，不改动任何东西，如实返回"未开启"。
#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `reg query` 在值存在时的真实版式（列之间用空格对齐，宽度不固定）。
    const FOUND: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    CC Router    REG_SZ    \"C:\\Program Files\\CC Router\\cc-router.exe\"\r\n\r\n";

    /// 值不存在时 `reg.exe` 的真实输出（退出码 1）。
    const NOT_FOUND: &str = "\r\n错误: 系统找不到指定的注册表项或值。\r\n\r\n";

    /// 键存在、但没有我们这一条时的输出（别的程序的自启项）。
    const OTHER_VALUES: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    OneDrive    REG_SZ    \"C:\\Users\\me\\OneDrive.exe\" /background\r\n    CC Router X    REG_SZ    \"C:\\other.exe\"\r\n\r\n";

    #[test]
    fn quoted_path_wraps_in_quotes_and_keeps_spaces() {
        let got = quoted_path(Path::new(r"C:\Program Files\CC Router\cc-router.exe"));
        assert_eq!(got, "\"C:\\Program Files\\CC Router\\cc-router.exe\"");
        assert!(got.starts_with('"') && got.ends_with('"'));
    }

    #[test]
    fn query_argv_targets_the_documented_value() {
        assert_eq!(
            query_argv(),
            vec![
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "CC Router",
            ]
        );
    }

    #[test]
    fn add_argv_writes_a_quoted_reg_sz_and_forces_overwrite() {
        let data = quoted_path(Path::new(r"C:\a b\cc-router.exe"));
        assert_eq!(
            add_argv(&data),
            vec![
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "CC Router",
                "/t",
                "REG_SZ",
                "/d",
                "\"C:\\a b\\cc-router.exe\"",
                "/f",
            ]
        );
    }

    #[test]
    fn delete_argv_removes_exactly_that_value() {
        assert_eq!(
            delete_argv(),
            vec![
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "CC Router",
                "/f",
            ]
        );
    }

    #[test]
    fn splits_a_value_line_whose_name_contains_a_space() {
        assert_eq!(
            split_reg_line("    CC Router    REG_SZ    \"C:\\a\\cc-router.exe\""),
            Some(("CC Router".to_string(), "\"C:\\a\\cc-router.exe\"".to_string()))
        );
    }

    #[test]
    fn ignores_header_and_error_lines() {
        assert_eq!(split_reg_line(r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run"), None);
        assert_eq!(split_reg_line("错误: 系统找不到指定的注册表项或值。"), None);
        assert_eq!(split_reg_line(""), None);
        assert_eq!(split_reg_line("    REG_SZ"), None, "只有类型记号、没有值名不算值行");
    }

    #[test]
    fn detects_the_value_when_present() {
        assert!(is_enabled_from_stdout(FOUND));
        assert_eq!(
            parse_query_stdout(FOUND, VALUE_NAME).as_deref(),
            Some("\"C:\\Program Files\\CC Router\\cc-router.exe\"")
        );
    }

    #[test]
    fn reports_absent_for_the_not_found_error_output() {
        assert!(!is_enabled_from_stdout(NOT_FOUND));
        assert_eq!(parse_query_stdout(NOT_FOUND, VALUE_NAME), None);
    }

    /// 别的程序的自启项（包括名字带我们前缀的 `CC Router X`）不能被误认为我们那条。
    #[test]
    fn does_not_match_other_values_in_the_same_key() {
        assert!(!is_enabled_from_stdout(OTHER_VALUES));
        assert_eq!(parse_query_stdout(OTHER_VALUES, "OneDrive").as_deref(), Some("\"C:\\Users\\me\\OneDrive.exe\" /background"));
    }

    /// 数据里带空格 / 非 ASCII 路径时，仍然只靠"这一行在不在"判断。
    #[test]
    fn tolerates_space_and_non_ascii_in_the_data() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    CC Router    REG_SZ    \"C:\\用户目录\\cc router\\cc-router.exe\" --flag\r\n\r\n";
        assert!(is_enabled_from_stdout(out));
        assert_eq!(
            parse_query_stdout(out, VALUE_NAME).as_deref(),
            Some("\"C:\\用户目录\\cc router\\cc-router.exe\" --flag")
        );
    }

    /// 值存在但数据为空也仍然是"已开启"（判据是值存在，不是数据非空）。
    #[test]
    fn empty_data_still_counts_as_enabled() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    CC Router    REG_SZ\r\n\r\n";
        assert!(is_enabled_from_stdout(out));
    }
}

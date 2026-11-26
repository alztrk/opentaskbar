//! Application jump-list launch targets (C# `ApplicationJumpListService.cs` /
//! `NativeJumpListSession.cs` parity).
//!
//! `BuildLaunchInfo` carries a working directory and a `use_shell_execute` flag. `.lnk`
//! arguments are split with Explorer-compatible quoting rules; native shell execute uses
//! `ShellExecuteW` so document associations and elevated verbs keep working.

use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// One resolved jump-list/item target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildLaunchInfo {
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub use_shell_execute: bool,
    pub display_name: String,
}

impl BuildLaunchInfo {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            arguments: String::new(),
            working_directory: String::new(),
            use_shell_execute: true,
            display_name: String::new(),
        }
    }

    /// Whether launching needs shell semantics (document/URL/elevation).
    pub fn needs_shell(&self) -> bool {
        self.use_shell_execute
            || self.executable.starts_with("shell:")
            || self.executable.starts_with("ms-settings:")
            || self.executable.starts_with("http://")
            || self.executable.starts_with("https://")
            || self.executable.eq_ignore_ascii_case("explorer.exe")
    }

    /// Launch via `ShellExecuteW` (non-Shell targets fall back to `CreateProcess`-like `Command`).
    pub fn launch(&self) -> bool {
        let path = self.executable.trim();
        if path.is_empty() {
            return false;
        }
        if !self.needs_shell() {
            let mut command = std::process::Command::new(path);
            if !self.arguments.is_empty() {
                // Split using Explorer quoting rules before handing to Command.
                command.args(split_arguments(&self.arguments));
            }
            if !self.working_directory.is_empty() {
                command.current_dir(&self.working_directory);
            }
            return command.spawn().is_ok();
        }
        let wide_path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let wide_args: Vec<u16> = self.arguments.encode_utf16().chain(Some(0)).collect();
        let wide_dir: Vec<u16> = self.working_directory.encode_utf16().chain(Some(0)).collect();
        let dir_ptr = if self.working_directory.is_empty() {
            std::ptr::null()
        } else {
            wide_dir.as_ptr()
        };
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let rc = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                wide_path.as_ptr(),
                wide_args.as_ptr(),
                dir_ptr,
                SW_SHOWNORMAL,
            )
        };
        rc as isize > 32
    }
}

/// Explorer-style argument splitter: honours `"` and `\"`, drops empty tokens.
pub fn split_arguments(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    let mut started = false;
    let chars: Vec<char> = args.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if escape {
            current.push(c);
            escape = false;
            started = true;
            continue;
        }
        match c {
            '\\' if chars.get(i) == Some(&'"') => {
                escape = true;
                started = true;
            }
            '\\' => {
                current.push('\\');
                started = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if escape {
        current.push('\\');
    }
    if started {
        out.push(current);
    }
    out
}

/// Quotes an argument for a jump-list command line (C# `BuildQuotedArguments`).
pub fn quote_argument(arg: &str) -> String {
    if !arg.is_empty()
        && !arg.chars().any(|c| c.is_whitespace() || c == '"')
    {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Derives a working directory for a launcher: explicit value, else the executable's folder.
pub fn resolve_working_directory(executable: &str, explicit: &str) -> String {
    if !explicit.trim().is_empty() {
        return explicit.trim().to_owned();
    }
    Path::new(executable.trim())
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Explorer special-case: `explorer.exe /select,<path>` needs shell execute and `/select,`.
pub fn explorer_select(path: &str) -> BuildLaunchInfo {
    let mut info = BuildLaunchInfo::new("explorer.exe");
    info.arguments = format!("/select,{}", quote_argument(path));
    info.working_directory = resolve_working_directory("explorer.exe", "");
    info.use_shell_execute = true;
    info.display_name = PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Open location".to_owned());
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_launch_info_shell_execute_and_working_dir() {
        let mut info = BuildLaunchInfo::new(r"C:\Program Files\App\app.exe");
        info.arguments = "--flag \"two words\"".into();
        info.working_directory = r"C:\Program Files\App".into();
        assert!(info.needs_shell());
        info.use_shell_execute = false;
        assert!(!info.needs_shell());
        assert_eq!(
            resolve_working_directory(r"C:\Tools\x.exe", ""),
            r"C:\Tools"
        );
        assert_eq!(resolve_working_directory("x.exe", r"D:\work"), r"D:\work");
    }

    #[test]
    fn argument_splitting_and_quoting() {
        assert_eq!(
            split_arguments(r#"--path "C:\Program Files\x" --n 2"#),
            vec!["--path", r"C:\Program Files\x", "--n", "2"]
        );
        assert_eq!(split_arguments(r#"a\"b c"#), vec![r#"a"b"#, "c"]);
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument(r#"a "quoted" path"#), r#""a \"quoted\" path""#);
    }

    #[test]
    fn explorer_select_uses_shell_execute() {
        let info = explorer_select(r"C:\Users\me\file.txt");
        assert!(info.needs_shell());
        assert!(info.arguments.starts_with("/select,"));
        assert_eq!(info.display_name, "file.txt");
    }
}

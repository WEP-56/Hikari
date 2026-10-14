//! Conservative policy checks, not an operating-system sandbox.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Careful,
    Autonomous,
}
impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Careful => "审慎",
            Self::Autonomous => "自主",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Careful => "ta 会在关键行动前与你确认。",
            Self::Autonomous => "ta 会自主处理常规操作，仅在重要事项前确认。",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask(String),
    Deny(String),
}

/// Resolve existing ancestors (including Windows junctions) before policy checks.
pub fn resolve_path(root: &Path, input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("路径不能为空".into());
    }
    let raw = Path::new(input);
    let joined = if raw.is_absolute() {
        raw.to_owned()
    } else {
        root.join(raw)
    };
    let mut ancestor = joined.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(ancestor.file_name().ok_or("路径不完整")?.to_owned());
        ancestor = ancestor.parent().ok_or("路径不存在")?;
    }
    let mut resolved = ancestor.canonicalize().map_err(|e| e.to_string())?;
    for part in tail.into_iter().rev() {
        if part == ".." || part == "." {
            return Err("路径包含未解析的父目录".into());
        }
        resolved.push(part);
    }
    Ok(resolved)
}

pub fn sensitive(path: &Path) -> bool {
    let lower = path.to_string_lossy().replace('\\', "/").to_lowercase();
    [
        "/.ssh",
        "/.aws",
        "/.codex",
        "/.hikari",
        "/windows",
        "/program files",
        "/programdata",
        "/etc",
        "/dev",
        "/proc",
        "/sys",
        "/credentials",
        "/.env",
        "/id_rsa",
        "/id_ed25519",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

pub fn assess(
    mode: PermissionMode,
    root: &Path,
    name: &str,
    args: &Value,
) -> Result<(Decision, Option<PathBuf>), String> {
    if matches!(
        name,
        "read_file" | "read" | "write" | "edit" | "ls" | "find" | "grep"
    ) {
        let path = resolve_path(root, args["path"].as_str().ok_or("缺少 path")?)?;
        let decision = if sensitive(&path) {
            Decision::Ask("访问或修改敏感资源".into())
        } else if mode == PermissionMode::Careful
            && !path.starts_with(root.canonicalize().map_err(|e| e.to_string())?)
        {
            Decision::Ask("访问工作区外路径".into())
        } else {
            Decision::Allow
        };
        return Ok((decision, Some(path)));
    }
    let decision = match name {
        "shell" => assess_command(mode, root, args["command"].as_str().ok_or("缺少 command")?),
        "todo_list"
        | "clarify"
        | "process_manage"
        | "tool_search"
        | "artifact_read"
        | "session_read"
        | "session_search"
        | "web_search"
        | "web_fetch"
        | "recall"
        | "search_observations"
        | "expand"
        | "forget" => Decision::Allow,
        // Includes every MCP and future extension tool. Hints never grant authority.
        _ => Decision::Ask("外部工具或扩展操作需要确认".into()),
    };
    Ok((decision, None))
}

fn words(command: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for c in command.chars() {
        if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote.is_none()
            && (c.is_whitespace() || matches!(c, ';' | '&' | '|' | '>' | '<' | '(' | ')'))
        {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                result.push(c.to_string());
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

/// A stable snapshot of identifiable shell resources, shown with an approval and
/// rechecked after the wait. This is not a proof about arbitrary script behavior.
pub fn shell_targets(root: &Path, command: &str) -> Vec<PathBuf> {
    fn collect(root: &Path, command: &str, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 8 {
            return;
        }
        for word in words(command) {
            if word.contains(char::is_whitespace) {
                collect(root, &word, depth + 1, out);
            }
            let candidate = word.split_once('=').map_or(word.as_str(), |(_, v)| v);
            if candidate.is_empty()
                || candidate.starts_with('-')
                || matches!(candidate, "&" | "|" | ";" | ">" | "<" | "(" | ")")
            {
                continue;
            }
            if let Ok(path) = resolve_path(root, candidate) {
                if path.exists() || candidate.contains(['/', '\\', ':']) {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    collect(root, command, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

fn catastrophic(command: &str) -> bool {
    let lower = command.to_lowercase().replace('^', "").replace('`', "");
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){:|:&};:") || lower.contains("--no-preserve-root") {
        return true;
    }
    let tokens = words(&lower);
    // Inspect encoded PowerShell payloads even inside a pipeline or compound command.
    for (i, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "-encodedcommand" | "-enc" | "-ec") {
            use base64::Engine;
            if let Some(encoded) = tokens.get(i + 1) {
                // Base64 is case sensitive, so obtain the original token.
                let original = words(command);
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD
                    .decode(original.get(i + 1).unwrap_or(encoded))
                {
                    let text: Vec<_> = bytes
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    if catastrophic(&String::from_utf16_lossy(&text)) {
                        return true;
                    }
                }
            }
        }
    }
    // Shell payloads may be quoted as a single token. Inspect them as code as well.
    let physical_target = tokens.iter().any(|t| {
        let Some(target) = t.strip_prefix("of=") else {
            return false;
        };
        target.starts_with("\\\\.\\physicaldrive")
            || ["/dev/sd", "/dev/vd", "/dev/hd", "/dev/nvme", "/dev/mmcblk"]
                .iter()
                .any(|p| target.starts_with(p))
    });
    if tokens
        .iter()
        .any(|t| t.contains(char::is_whitespace) && t != &lower && catastrophic(t))
    {
        return true;
    }
    let system_drive = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".into())
        .to_lowercase();
    let system_root = std::env::var("SystemRoot")
        .unwrap_or_else(|_| "C:\\Windows".into())
        .to_lowercase();
    let root_target = |t: &str| {
        let t = t.trim().trim_matches(['\'', '"']).replace('/', "\\");
        if t == "*" || t == "." || t == ".." {
            return false;
        }
        let t = t
            .trim_start_matches("\\\\?\\")
            .trim_end_matches(['\\', '*', '.']);
        let resolved = Path::new(t).canonicalize().ok().map(|p| {
            p.to_string_lossy()
                .to_lowercase()
                .trim_start_matches("\\\\?\\")
                .trim_end_matches('\\')
                .to_owned()
        });
        t.is_empty()
            || t == system_drive
            || t == system_root
            || t == format!("{system_root}\\system32")
            || t == format!("{system_root}\\syswow64")
            || resolved
                .as_ref()
                .is_some_and(|p| p == &system_drive || p == &system_root)
            || matches!(
                t,
                "%systemdrive%"
                    | "%systemroot%"
                    | "$env:systemdrive"
                    | "$env:systemroot"
                    | "${env:systemdrive}"
                    | "${env:systemroot}"
            )
    };
    let deleting = tokens.iter().any(|t| {
        matches!(
            t.rsplit(['/', '\\'])
                .next()
                .unwrap_or(t)
                .trim_end_matches(".exe"),
            "rm" | "rd" | "rmdir" | "del" | "erase" | "remove-item"
        )
    });
    if deleting && tokens.iter().any(|t| root_target(t)) {
        return true;
    }
    let formatting = tokens.iter().any(|t| {
        let exe = t
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(t)
            .trim_end_matches(".exe");
        exe == "format" || exe == "format.com" || exe == "format-volume" || exe.starts_with("mkfs")
    });
    #[cfg(unix)]
    let mounted_root = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|info| {
            info.lines().find_map(|line| {
                let (mount, source) = line.split_once(" - ")?;
                if mount.split_whitespace().nth(4) == Some("/") {
                    source.split_whitespace().nth(1).map(str::to_owned)
                } else {
                    None
                }
            })
        });
    #[cfg(not(unix))]
    let mounted_root: Option<String> = None;
    if formatting
        && tokens.iter().any(|t| {
            root_target(t)
                || mounted_root.as_ref() == Some(t)
                || (t == system_drive.trim_end_matches(':')
                    && tokens.iter().any(|t| t == "-driveletter"))
                || matches!(t.as_str(), "/dev/root")
        })
    {
        return true;
    }
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "dd" | "diskpart" | "clear-disk"))
        && ((physical_target
            && (lower.contains("if=/dev/zero") || lower.contains("if=/dev/urandom")))
            || lower.contains("clean all")
            || lower.contains("clear-disk"))
    {
        return true;
    }
    false
}

pub fn assess_command(mode: PermissionMode, root: &Path, command: &str) -> Decision {
    inspect_command(mode, root, command, 0)
}
fn inspect_command(mode: PermissionMode, root: &Path, command: &str, depth: usize) -> Decision {
    if depth > 8 {
        return Decision::Ask("包装命令层数过多".into());
    }
    if catastrophic(command) {
        return Decision::Deny("禁止灾难性磁盘或系统清除操作".into());
    }
    let tokens = words(command);
    let lower = command.to_lowercase();
    if tokens.iter().any(|t| {
        matches!(
            t.to_lowercase()
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(t)
                .trim_end_matches(".exe"),
            "taskkill" | "kill" | "pkill" | "stop-process"
        )
    }) && (lower.contains("hikari")
        || tokens.iter().any(|t| t == &std::process::id().to_string()))
    {
        return Decision::Deny("禁止终止 Hikari 主进程；请使用 process_manage 管理登记进程".into());
    }
    let Some(first) = tokens.first() else {
        return Decision::Ask("空命令".into());
    };
    let executable = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_lowercase();
    let executable = executable.trim_end_matches(".exe");
    if matches!(executable, "powershell" | "pwsh") {
        if let Some(i) = tokens.iter().position(|t| {
            matches!(
                t.to_lowercase().as_str(),
                "-encodedcommand" | "-enc" | "-ec"
            )
        }) {
            use base64::Engine;
            if let Some(encoded) = tokens.get(i + 1) {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                    let utf16: Vec<_> = bytes
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    return inspect_command(
                        mode,
                        root,
                        &String::from_utf16_lossy(&utf16),
                        depth + 1,
                    );
                }
            }
            return Decision::Ask("无法解析编码命令".into());
        }
    }
    if matches!(
        executable,
        "cmd" | "powershell" | "pwsh" | "bash" | "sh" | "zsh" | "ksh" | "wsl"
    ) {
        if let Some(i) = tokens.iter().position(|t| {
            matches!(
                t.to_lowercase().as_str(),
                "/c" | "/k" | "-command" | "-c" | "-lc"
            )
        }) {
            return inspect_command(mode, root, &tokens[i + 1..].join(" "), depth + 1);
        }
        return Decision::Ask("无法确定包装命令的实际动作".into());
    }
    // Do not try to prove arbitrary scripts or compound shell syntax harmless.
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), ";" | "&" | "|" | ">" | "<" | "(" | ")"))
        || command.contains(['$', '%', '`', '^', '\n', '\r'])
    {
        return Decision::Ask("复合命令、变量、管道或重定向需要确认".into());
    }
    if matches!(
        executable,
        "rm" | "rd"
            | "rmdir"
            | "del"
            | "erase"
            | "remove-item"
            | "chmod"
            | "chown"
            | "icacls"
            | "cacls"
            | "dd"
            | "format"
            | "diskpart"
            | "systemctl"
            | "sc"
            | "stop-service"
            | "stop-process"
            | "taskkill"
            | "kill"
            | "pkill"
            | "tee"
            | "sed"
            | "install"
            | "curl"
            | "wget"
            | "invoke-expression"
            | "iex"
    ) || executable.starts_with("mkfs")
    {
        return Decision::Ask("删除、敏感修改、系统控制或外部动作需要确认".into());
    }
    let safe = match executable {
        "echo" | "pwd" | "dir" | "ls" | "cat" | "type" | "head" | "tail" | "rg" | "grep"
        | "findstr" | "get-content" | "get-childitem" | "get-location" | "whoami" => true,
        "git" => tokens
            .get(1)
            .is_some_and(|t| matches!(t.as_str(), "status" | "diff" | "log" | "show" | "ls-files")),
        "node" => tokens
            .get(1)
            .is_some_and(|t| matches!(t.as_str(), "--check" | "-c")),
        _ => false,
    };
    if tokens.iter().any(|t| {
        matches!(
            t.to_lowercase().split('=').next().unwrap_or(t),
            "--pre" | "--ext-diff" | "--textconv" | "--follow" | "-l" | "-recurse" | "/s"
        )
    }) {
        return Decision::Ask("外部处理器或递归命令可能扩大执行和访问范围".into());
    }
    if !safe {
        return Decision::Ask("命令含可执行动作，无法可靠确定范围".into());
    }
    // Option values may be paths; inspect every token, not only the first executable.
    for token in tokens.iter().skip(1) {
        let candidate = token.split_once('=').map_or(token.as_str(), |(_, v)| v);
        if candidate.starts_with('-') {
            continue;
        }
        if sensitive(Path::new(candidate)) {
            return Decision::Ask("命令访问敏感资源".into());
        }
        match resolve_path(root, candidate) {
            Ok(path) => {
                if sensitive(&path) {
                    return Decision::Ask("命令实际路径指向敏感资源".into());
                }
                if mode == PermissionMode::Careful
                    && !path.starts_with(root.canonicalize().unwrap_or_else(|_| root.to_owned()))
                {
                    return Decision::Ask("命令可能访问工作区外或链接目标".into());
                }
            }
            Err(_) if Path::new(candidate).is_absolute() || candidate.contains("..") => {
                return Decision::Ask("命令目标无法可靠解析".into())
            }
            _ => {}
        }
    }
    Decision::Allow
}

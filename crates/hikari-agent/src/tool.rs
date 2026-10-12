use std::fs;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::provider::ToolDef;

const OUTPUT_CAP: usize = 16_000;
#[path = "file_tools.rs"]
mod file_tools;
use file_tools::FileTool;

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    fn call(&self, arguments: &str) -> std::result::Result<String, String>;
    fn output_limit(&self) -> usize {
        OUTPUT_CAP
    }
    fn call_with_abort(
        &self,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<String, String> {
        if abort.load(Ordering::Relaxed) {
            return Err("已取消工具调用".into());
        }
        self.call(arguments)
    }
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    defs: Vec<ToolDef>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        let defs = tools
            .iter()
            .map(|tool| ToolDef {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters(),
            })
            .collect();
        Self { tools, defs }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// 本机两个工具：在工作区里跑命令，和读工作区里的文本。
    pub fn workspace(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self::new(vec![
            Arc::new(ShellTool { root: root.clone() }),
            Arc::new(ReadTool { root: root.clone() }),
            Arc::new(FileTool {
                root: root.clone(),
                kind: "read",
            }),
            Arc::new(FileTool {
                root: root.clone(),
                kind: "write",
            }),
            Arc::new(FileTool {
                root: root.clone(),
                kind: "edit",
            }),
            Arc::new(FileTool {
                root: root.clone(),
                kind: "ls",
            }),
            Arc::new(FileTool {
                root: root.clone(),
                kind: "find",
            }),
            Arc::new(FileTool { root, kind: "grep" }),
        ])
    }

    pub fn defs(&self) -> &[ToolDef] {
        &self.defs
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> std::result::Result<(), String> {
        if self.tools.iter().any(|t| t.name() == tool.name()) {
            return Err(format!("重复工具名：{}", tool.name()));
        }
        self.defs.push(ToolDef {
            name: tool.name().into(),
            description: tool.description().into(),
            parameters: tool.parameters(),
        });
        self.tools.push(tool);
        Ok(())
    }

    pub fn call_with_abort(
        &self,
        name: &str,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<String, String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| format!("没有名为 {name} 的工具"))?;
        tool.call_with_abort(arguments, abort)
            .map(|s| cap_chars(&s, tool.output_limit()))
            .map_err(|s| cap_chars(&s, OUTPUT_CAP))
    }

    pub fn call(&self, name: &str, arguments: &str) -> std::result::Result<String, String> {
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return Err(format!("没有名为 {name} 的工具"));
        };
        match tool.call(arguments) {
            Ok(text) => Ok(cap_chars(&text, tool.output_limit())),
            Err(err) => Err(cap_chars(&err, OUTPUT_CAP)),
        }
    }
}

struct ShellTool {
    root: PathBuf,
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "在工作区目录里执行一条命令，返回输出。用来查看或修改本机文件。一次只做一件事，不要把输出逐行当成结果。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        let command = required_string(arguments, "command")?;
        run_command(&self.root, &command, &AtomicBool::new(false))
    }
    fn call_with_abort(
        &self,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<String, String> {
        run_command(&self.root, &required_string(arguments, "command")?, abort)
    }
}

struct ReadTool {
    root: PathBuf,
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "读取工作区里的一个文本文件。path 相对工作区。不要用它读目录。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区的路径" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        let path = required_string(arguments, "path")?;
        read_workspace_file(&self.root, &path)
    }
}

fn required_string(arguments: &str, key: &str) -> std::result::Result<String, String> {
    let value: Value = serde_json::from_str(arguments)
        .map_err(|_| format!("参数要是 JSON，例如 {{\"{key}\":\"...\"}}"))?;
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {key}"))
}

fn run_command(
    root: &Path,
    command: &str,
    abort: &AtomicBool,
) -> std::result::Result<String, String> {
    if command.len() > 8_000 {
        return Err("命令太长".into());
    }
    let mut child = Command::new("cmd.exe")
        .arg("/C")
        .arg(command)
        .current_dir(root)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("命令没有启动。{err}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = thread::spawn(move || read_capped(stdout, 12_000));
    let err_handle = thread::spawn(move || read_capped(stderr, 4_000));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None)
                if abort.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(30) =>
            {
                #[cfg(windows)]
                {
                    let _ = Command::new("taskkill")
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .creation_flags(0x08000000)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(if abort.load(Ordering::Relaxed) {
                    "命令已取消"
                } else {
                    "命令超过 30 秒，停掉了"
                }
                .into());
            }
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(err) => return Err(format!("命令中断了。{err}")),
        }
    };
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    let mut text = String::new();
    if !stdout.is_empty() {
        text.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    if text.is_empty() {
        text.push_str(if status.success() {
            "完成，没有输出。"
        } else {
            "命令失败了，没有输出。"
        });
    }
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        text.push_str(&format!("\n退出码 {code}"));
        return Err(text);
    }
    Ok(text)
}

fn read_capped(reader: Option<impl Read>, cap: usize) -> String {
    let Some(mut reader) = reader else {
        return String::new();
    };
    let mut buf = [0u8; 4096];
    let mut kept = Vec::new();
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if kept.len() < cap {
                    let take = n.min(cap - kept.len());
                    kept.extend_from_slice(&buf[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text.push_str("\n…截断了");
    }
    text
}

fn read_workspace_file(root: &Path, user_path: &str) -> std::result::Result<String, String> {
    let full = resolve_inside(root, user_path)?;
    let meta = fs::metadata(&full).map_err(|err| format!("读不到。{err}"))?;
    if meta.is_dir() {
        return Err("这是目录，不是文件".into());
    }
    if meta.len() > 1_000_000 {
        return Err("文件太大".into());
    }
    let bytes = fs::read(&full).map_err(|err| format!("读不到。{err}"))?;
    if bytes.iter().take(800).any(|byte| *byte == 0) {
        return Err("这是二进制文件".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut out: String = text.chars().take(32_000).collect();
    if text.chars().count() > 32_000 {
        out.push_str("\n…后面截断了");
    }
    Ok(out)
}

fn resolve_inside(root: &Path, user_path: &str) -> std::result::Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|err| format!("工作区打不开。{err}"))?;
    let raw = Path::new(user_path.trim());
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    let full = if joined.exists() {
        joined
            .canonicalize()
            .map_err(|err| format!("路径打不开。{err}"))?
    } else {
        let mut ancestor = joined.as_path();
        let mut tail = Vec::new();
        while !ancestor.exists() {
            tail.push(ancestor.file_name().ok_or("路径不完整")?.to_os_string());
            ancestor = ancestor.parent().ok_or("路径不存在")?;
        }
        let mut full = ancestor.canonicalize().map_err(|e| e.to_string())?;
        for part in tail.into_iter().rev() {
            if part == ".." || part == "." {
                return Err("路径不能包含未解析的父目录".into());
            }
            full.push(part);
        }
        full
    };
    if !full.starts_with(&root) {
        return Err("路径超出工作区".into());
    }
    Ok(full)
}

fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("\n…截断了");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_runs_in_the_workspace() {
        let registry = ToolRegistry::workspace(std::env::temp_dir());
        let out = registry
            .call("shell", r#"{"command":"echo hikari-ok"}"#)
            .unwrap();
        assert!(out.contains("hikari-ok"), "{out}");
    }

    #[test]
    fn read_file_rejects_paths_outside_the_workspace() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let registry = ToolRegistry::workspace(&root);
        let out = registry
            .call("read_file", r#"{"path":"Cargo.toml"}"#)
            .unwrap();
        assert!(out.contains("hikari-agent"), "{out}");
        let outside = registry.call("read_file", r#"{"path":"../preview/Cargo.toml"}"#);
        assert!(outside.is_err(), "{outside:?}");
    }
}

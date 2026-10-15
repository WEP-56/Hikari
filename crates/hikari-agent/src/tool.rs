use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    fn target_path(&self, _arguments: &str) -> std::result::Result<Option<PathBuf>, String> {
        Ok(None)
    }
    /// Exact resolved target approved by the host; ordinary extension tools ignore it.
    fn call_authorized(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        _target: Option<&Path>,
    ) -> std::result::Result<String, String> {
        self.call_with_abort(arguments, abort)
    }
    fn call_output(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        target: Option<&Path>,
    ) -> Result<crate::ToolOutput, String> {
        self.call_authorized(arguments, abort, target)
            .map(crate::ToolOutput::text)
    }
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
    deferred: BTreeMap<String, String>,
    loaded: Mutex<Vec<String>>,
    artifacts: Option<PathBuf>,
    root: Option<PathBuf>,
    execution: Option<Arc<crate::execution::Execution>>,
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
        Self {
            tools,
            defs,
            deferred: BTreeMap::new(),
            loaded: Mutex::new(Vec::new()),
            artifacts: None,
            root: None,
            execution: None,
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// 本机两个工具：在工作区里跑命令，和读工作区里的文本。
    pub fn workspace(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        let mut registry = Self::new(vec![
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
            Arc::new(FileTool {
                root: root.clone(),
                kind: "grep",
            }),
        ]);
        registry.root = Some(root);
        registry
    }

    pub fn enable_execution(
        &mut self,
        execution: Arc<crate::execution::Execution>,
    ) -> std::result::Result<(), String> {
        execution.register(self)?;
        self.execution = Some(execution);
        Ok(())
    }
    pub fn execution(&self) -> Option<&Arc<crate::execution::Execution>> {
        self.execution.as_ref()
    }

    /// Child agents share implementations and the workspace, but never loaded-tool state,
    /// parent conversation, memory access, or the parent's task-bound session tools.
    pub fn fork_for_execution(
        &self,
        execution: Arc<crate::execution::Execution>,
    ) -> std::result::Result<Self, String> {
        let allowed = |name: &str| {
            !matches!(
                name,
                "delegate_task"
                    | "todo_list"
                    | "clarify"
                    | "process_manage"
                    | "session_search"
                    | "session_read"
            ) && !crate::memory::is_memory_tool(name)
        };
        let mut fork = Self {
            tools: self
                .tools
                .iter()
                .filter(|t| allowed(t.name()))
                .cloned()
                .collect(),
            defs: self
                .defs
                .iter()
                .filter(|t| allowed(&t.name))
                .cloned()
                .collect(),
            deferred: self
                .deferred
                .iter()
                .filter(|(n, _)| allowed(n))
                .map(|(n, v)| (n.clone(), v.clone()))
                .collect(),
            loaded: Mutex::new(Vec::new()),
            artifacts: self.artifacts.clone(),
            root: self.root.clone(),
            execution: None,
        };
        fork.enable_execution(execution)?;
        Ok(fork)
    }

    pub fn enable_delegation(
        &mut self,
        provider: Arc<dyn crate::Provider>,
        system_prompt: String,
        context_limit_tokens: u32,
        attachments: Option<crate::AttachmentStore>,
    ) -> std::result::Result<(), String> {
        let execution = self.execution.clone().ok_or("委派需要会话执行底座")?;
        let template = Arc::new(self.fork_for_execution(execution.clone())?);
        self.register(Arc::new(crate::delegate::DelegateTool::new(
            execution,
            provider,
            template,
            system_prompt,
            context_limit_tokens,
            attachments,
        )))
    }

    pub(crate) fn summary_output(&self, text: String) -> String {
        self.finish_output(text, OUTPUT_CAP)
    }

    pub fn defs(&self) -> &[ToolDef] {
        &self.defs
    }

    pub fn visible_defs(&self) -> Vec<ToolDef> {
        let loaded = self.loaded.lock().unwrap();
        let mut defs: Vec<_> = self
            .defs
            .iter()
            .filter(|d| !self.deferred.contains_key(&d.name))
            .cloned()
            .collect();
        defs.extend(
            loaded
                .iter()
                .filter_map(|name| self.defs.iter().find(|d| &d.name == name))
                .cloned(),
        );
        defs
    }
    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    pub fn restore_loaded(&self, tools: &[ToolDef]) {
        *self.loaded.lock().unwrap() = tools
            .iter()
            .filter(|d| self.deferred.contains_key(&d.name))
            .map(|d| d.name.clone())
            .collect();
    }

    pub fn register_deferred(
        &mut self,
        tool: Arc<dyn Tool>,
        namespace: String,
    ) -> std::result::Result<(), String> {
        let name = tool.name().to_owned();
        self.register(tool)?;
        self.deferred.insert(name, namespace);
        if !self.defs.iter().any(|d| d.name == "tool_search") {
            self.defs.push(crate::tool_search::definition());
        }
        Ok(())
    }

    pub fn enable_artifacts(&mut self, root: PathBuf) -> std::result::Result<(), String> {
        if self.defs.iter().any(|d| d.name == "artifact_read") {
            return Err("重复工具名：artifact_read".into());
        }
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        self.artifacts = Some(root);
        self.defs.push(ToolDef { name: "artifact_read".into(), description: "按字符范围继续读取本会话的长工具输出。offset 从 0 开始，返回 next_offset。".into(), parameters: json!({"type":"object","properties":{"id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":12000}},"required":["id"],"additionalProperties":false}) });
        Ok(())
    }

    fn finish_output(&self, text: String, cap: usize) -> String {
        if text.chars().count() <= cap {
            return text;
        }
        if let Some(root) = &self.artifacts {
            use sha2::{Digest, Sha256};
            let id = format!("{:x}", Sha256::digest(text.as_bytes()));
            match fs::write(root.join(format!("{id}.txt")), &text) {
                Ok(()) => {
                    return format!(
                        "{}\n[完整输出已保存：artifact_read id={id}，offset={cap}，共 {} 字符]",
                        text.chars().take(cap).collect::<String>(),
                        text.chars().count()
                    );
                }
                Err(e) => return format!("{}\n[完整输出保存失败：{e}]", cap_chars(&text, cap)),
            }
        }
        cap_chars(&text, cap)
    }

    // Rich results share one allowance, including the continuation marker itself.
    fn finish_rich_text(&self, text: String, cap: usize) -> String {
        if text.chars().count() <= cap {
            return text;
        }
        let marker = match self.save_artifact(&text) {
            Ok(id) => format!(
                "\n[完整输出已保存：artifact_read id={id}，offset=0，共 {} 字符]",
                text.chars().count()
            ),
            Err(note) => format!("\n[输出已截断；{note}]"),
        };
        let preview_cap = cap.saturating_sub(marker.chars().count());
        text.chars()
            .take(preview_cap)
            .chain(marker.chars())
            .take(cap)
            .collect()
    }

    fn save_artifact(&self, text: &str) -> Result<String, &'static str> {
        use sha2::{Digest, Sha256};
        let root = self
            .artifacts
            .as_ref()
            .ok_or("未启用 artifact 存储，完整结果不可恢复")?;
        let id = format!("{:x}", Sha256::digest(text.as_bytes()));
        fs::write(root.join(format!("{id}.txt")), text).map_err(|_| "完整结果保存失败")?;
        Ok(id)
    }

    fn read_artifact(&self, arguments: &str) -> std::result::Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let id = args["id"].as_str().ok_or("缺少 id")?;
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("artifact id 无效".into());
        }
        let root = self.artifacts.as_ref().ok_or("未启用 artifact 存储")?;
        let text = fs::read_to_string(root.join(format!("{id}.txt"))).map_err(|e| e.to_string())?;
        let offset = args
            .get("offset")
            .map(|v| v.as_u64().ok_or("offset 无效"))
            .transpose()?
            .unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .map(|v| {
                v.as_u64()
                    .filter(|n| (1..=12000).contains(n))
                    .ok_or("limit 需要在 1–12000 内")
            })
            .transpose()?
            .unwrap_or(6000) as usize;
        let part: String = text.chars().skip(offset).take(limit).collect();
        let total = text.chars().count();
        let next = offset.saturating_add(part.chars().count()).min(total);
        Ok(json!({"id":id,"offset":offset,"text":part,"next_offset":next,"total_chars":total,"has_more":next<total}).to_string())
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> std::result::Result<(), String> {
        if matches!(tool.name(), "tool_search" | "artifact_read")
            || self.defs.iter().any(|t| t.name == tool.name())
        {
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
    ) -> Result<String, String> {
        self.call_output(name, arguments, abort).and_then(|output| {
            if output.failure.is_some() {
                Err(output.text)
            } else {
                Ok(output.text)
            }
        })
    }
    pub fn call_output(
        &self,
        name: &str,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<crate::ToolOutput, String> {
        if abort.load(Ordering::Relaxed) {
            return Err("已取消工具调用".into());
        }
        if name == "tool_search" {
            return crate::tool_search::search(&self.defs, &self.deferred, &self.loaded, arguments)
                .map(crate::ToolOutput::text);
        }
        if name == "artifact_read" {
            return self.read_artifact(arguments).map(crate::ToolOutput::text);
        }
        if self.deferred.contains_key(name)
            && !self.loaded.lock().unwrap().iter().any(|n| n == name)
        {
            return Err("工具尚未加载，请先调用 tool_search".into());
        }
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| format!("没有名为 {name} 的工具"))?;
        let mut target = None;
        if let Some(root) = &self.root {
            let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
            if name == "shell" && args["command"].as_str().is_some_and(|c| c.len() > 8000) {
                return Err("命令太长".into());
            }
            let mode = self
                .execution
                .as_ref()
                .map_or(Default::default(), |e| e.active_mode());
            let external_target = tool.target_path(arguments)?;
            let (decision, resolved) = if matches!(name, "load_skill" | "skill_manage") {
                let path = external_target.as_ref().ok_or("Skill 目标路径无效")?;
                let (decision, target) = crate::permission::assess(
                    mode,
                    root,
                    if name == "load_skill" || args["action"] == "history" {
                        "read_file"
                    } else {
                        "write"
                    },
                    &json!({"path":path}),
                )?;
                (
                    if name == "skill_manage" && args["action"] != "history" {
                        if mode == crate::permission::PermissionMode::Autonomous {
                            if args["action"] == "delete" {
                                crate::permission::Decision::Ask("删除 Skill 需要确认".into())
                            } else {
                                crate::permission::Decision::Allow
                            }
                        } else {
                            crate::permission::Decision::Ask(
                                "Skill 管理将修改可执行指令及资源".into(),
                            )
                        }
                    } else {
                        decision
                    },
                    target,
                )
            } else {
                crate::permission::assess(mode, root, name, &args)?
            };
            target = resolved;
            match decision {
                crate::permission::Decision::Allow => {}
                crate::permission::Decision::Deny(reason) => {
                    return Err(format!("硬性拦截：{reason}"));
                }
                crate::permission::Decision::Ask(reason) => {
                    let resources = if name == "shell" {
                        crate::permission::shell_targets(
                            root,
                            args["command"].as_str().unwrap_or_default(),
                        )
                    } else {
                        vec![]
                    };
                    let reason = if resources.is_empty() {
                        reason
                    } else {
                        format!(
                            "{reason}\n已解析的命令资源：{}",
                            resources
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join("；")
                        )
                    };
                    let execution = self
                        .execution
                        .as_ref()
                        .ok_or_else(|| format!("需要本次审批：{reason}"))?;
                    execution.approve(
                        root.clone(),
                        name,
                        arguments,
                        target.clone(),
                        reason,
                        abort,
                    )?;
                    if name == "shell"
                        && resources
                            != crate::permission::shell_targets(
                                root,
                                args["command"].as_str().unwrap_or_default(),
                            )
                    {
                        return Err("命令目标路径已改变，需要重新审批".into());
                    }
                }
            }
            if abort.load(Ordering::Relaxed) {
                return Err("已取消，操作未执行".into());
            }
            if name == "shell" && args.get("background").and_then(Value::as_bool) == Some(true) {
                let execution = self.execution.as_ref().ok_or("后台进程需要会话执行底座")?;
                return execution
                    .spawn_process_with_resources(
                        root.clone(),
                        required_string(arguments, "command")?,
                        abort,
                        self.tools
                            .iter()
                            .filter(|tool| tool.name() == "load_skill")
                            .cloned()
                            .collect(),
                    )
                    .map(crate::ToolOutput::text);
            }
        }
        tool.call_output(arguments, abort, target.as_deref())
            .map(|mut output| {
                let cap = tool.output_limit();
                let failure_chars = output.failure.as_ref().map_or(0, |failure| {
                    serde_json::to_string(failure).unwrap().chars().count()
                });
                let structured_chars = output.structured.as_ref().map_or(0, |value| value.to_string().chars().count());
                if failure_chars > cap / 4
                    && failure_chars.saturating_add(structured_chars).saturating_add(output.text.chars().count()) > cap
                {
                    let failure = output.failure.as_mut().unwrap();
                    let serialized = serde_json::to_string(failure).unwrap();
                    failure.message = match self.save_artifact(&serialized) {
                        Ok(id) => format!("完整错误已保存：artifact_read id={id} offset=0，共 {} 字符（JSON，包含 kind 和 message）。", serialized.chars().count()),
                        Err(note) => format!("错误详情超过工具输出预算；{note}。"),
                    };
                    if serde_json::to_string(&failure.kind).unwrap().chars().count() > 256 {
                        failure.kind = "truncated_failure".into();
                    }
                    output.truncated = true;
                }
                let failure_chars = output.failure.as_ref().map_or(0, |failure| {
                    serde_json::to_string(failure).unwrap().chars().count()
                });
                if let Some(structured) = output.structured.as_ref() {
                    let serialized = structured.to_string();
                    let total_chars = serialized.chars().count();
                    if total_chars.saturating_add(output.text.chars().count()).saturating_add(failure_chars) > cap {
                        output.truncated = true;
                        output.structured = Some(if let Some(root) = &self.artifacts {
                            use sha2::{Digest, Sha256};
                            let id = format!("{:x}", Sha256::digest(serialized.as_bytes()));
                            match fs::write(root.join(format!("{id}.txt")), &serialized) {
                                Ok(()) => json!({
                                    "artifact_ref":{"tool":"artifact_read","id":id,"offset":0,"total_chars":total_chars,"format":"json"},
                                    "note":"结构化结果超过工具输出预算，完整 JSON 已保存，可按字符分页读取。"
                                }),
                                Err(error) => json!({"truncated":true,"total_chars":total_chars,"note":format!("结构化结果超过工具输出预算，完整结果保存失败：{error}")}),
                            }
                        } else {
                            json!({"truncated":true,"total_chars":total_chars,"note":"结构化结果超过工具输出预算；未启用 artifact 存储，完整结构化结果不可恢复。"})
                        });
                    }
                }
                // Count serialized failures too: MCP failures can duplicate the
                // entire text in message or return an arbitrarily large kind.
                let structured_chars = output.structured.as_ref().map_or(0, |value| value.to_string().chars().count());
                let text_cap = cap.saturating_sub(structured_chars).saturating_sub(failure_chars);
                output.truncated |= output.text.chars().count() > text_cap;
                output.text = if output.structured.is_some() || output.failure.is_some() {
                    self.finish_rich_text(output.text, text_cap)
                } else {
                    self.finish_output(output.text, text_cap)
                };
                output
            })
            .map_err(|s| self.finish_output(s, OUTPUT_CAP))
    }

    pub fn call(&self, name: &str, arguments: &str) -> std::result::Result<String, String> {
        self.call_with_abort(name, arguments, &AtomicBool::new(false))
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
        "在会话工作目录执行命令。默认前台执行，30 秒超时；长任务使用 background=true 登记后台进程并用 process_manage 查询。cwd 不构成系统沙箱；所有调用经过权限判断。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "background": { "type": "boolean", "description": "true 登记为本会话后台进程，立即返回 task_id，用 process_manage 读取或停止" }
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
    fn call_authorized(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        target: Option<&Path>,
    ) -> std::result::Result<String, String> {
        if abort.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let path = authorized_path(&self.root, &required_string(arguments, "path")?, target)?;
        read_text_file(&path)
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
    let mut child = crate::execution::shell_command(command)
        .current_dir(root)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("命令没有启动。{err}"))?;
    let job = match crate::process_job::ProcessJob::attach(&child) {
        Ok(job) => job,
        Err(e) => {
            crate::execution::kill_tree(&mut child);
            return Err(format!("无法登记进程树，已停止：{e}"));
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = thread::spawn(move || read_capped(stdout, 8 * 1024 * 1024));
    let err_handle = thread::spawn(move || read_capped(stderr, 8 * 1024 * 1024));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None)
                if abort.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(30) =>
            {
                job.terminate();
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
    job.terminate();
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
    read_text_file(&full)
}
fn read_text_file(full: &Path) -> std::result::Result<String, String> {
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
    Ok(text.into_owned())
}

fn authorized_path(
    root: &Path,
    input: &str,
    approved: Option<&Path>,
) -> std::result::Result<PathBuf, String> {
    if let Some(approved) = approved {
        let current = crate::permission::resolve_path(root, input)?;
        if current != approved {
            return Err("目标路径已改变，需要重新审批".into());
        }
        Ok(current)
    } else {
        resolve_inside(root, input)
    }
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
        let outside = registry.call("read_file", r#"{"path":"../hikari/Cargo.toml"}"#);
        assert!(outside.is_err(), "{outside:?}");
    }
}

//! Session-owned execution state. Context compaction never owns this state.
use crate::{permission::PermissionMode, tool::Tool, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
}
impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub status: TodoStatus,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionBudget {
    pub elapsed_seconds: u64,
    pub tokens: u64,
    pub concurrent_tasks: usize,
    pub child_tasks: usize,
    pub output_bytes: usize,
}
impl Default for ExecutionBudget {
    fn default() -> Self {
        Self {
            elapsed_seconds: 1800,
            tokens: 1_000_000,
            concurrent_tasks: 4,
            child_tasks: 16,
            output_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTask {
    pub session_id: String,
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub kind: String,
    pub workspace: PathBuf,
    pub model: String,
    pub permission: PermissionMode,
    pub status: TaskStatus,
    pub started_at: String,
    pub elapsed_ms: u64,
    pub usage: Usage,
    pub command: Option<String>,
    pub pid: Option<u32>,
    pub result: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub task_id: String,
    pub text: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionSnapshot {
    pub mode: PermissionMode,
    pub budget: ExecutionBudget,
    pub todos: Vec<TodoItem>,
    pub tasks: Vec<ExecutionTask>,
    pub results: Vec<ExecutionResult>,
    pub next_id: u64,
}
impl ExecutionSnapshot {
    /// Never replay effects or kill a reused PID after restart.
    pub fn recover(&mut self) {
        for task in &mut self.tasks {
            if !task.status.terminal() {
                task.status = TaskStatus::Failed;
                task.pid = None;
                task.result = Some("应用重启，执行已中断；未重跑，也未根据旧 PID 操作进程".into());
                self.results.push(ExecutionResult {
                    task_id: task.task_id.clone(),
                    text: task.result.clone().unwrap(),
                });
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionKind {
    Approval,
    Clarify,
}
#[derive(Debug, Clone)]
pub struct Interaction {
    pub id: u64,
    pub task_id: String,
    pub kind: InteractionKind,
    pub question: String,
    pub options: Vec<String>,
    pub tool: String,
    pub arguments: String,
    pub workspace: PathBuf,
    pub target: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub enum Answer {
    Allow,
    Reject,
    Text(String),
    Cancel,
}
struct Pending {
    request: Interaction,
    answer: Option<Answer>,
}
struct State {
    snapshot: ExecutionSnapshot,
    pending: Option<Pending>,
    active: Option<String>,
    revision: u64,
}
struct Process {
    child: Mutex<Child>,
    job: crate::process_job::ProcessJob,
    output: Mutex<Vec<u8>>,
    stopped: AtomicBool,
    overflow: AtomicBool,
    readers: std::sync::atomic::AtomicUsize,
}
pub struct Execution {
    session: String,
    state: Mutex<State>,
    processes: Mutex<BTreeMap<String, Arc<Process>>>,
    clock: Mutex<BTreeMap<String, Instant>>,
}
impl Execution {
    pub fn new(session: String, mut snapshot: ExecutionSnapshot) -> Arc<Self> {
        snapshot.recover();
        Arc::new(Self {
            session,
            state: Mutex::new(State {
                snapshot,
                pending: None,
                active: None,
                revision: 0,
            }),
            processes: Mutex::new(BTreeMap::new()),
            clock: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn snapshot(&self) -> ExecutionSnapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    pub fn revision(&self) -> u64 {
        self.state.lock().unwrap().revision
    }
    pub fn mode(&self) -> PermissionMode {
        self.state.lock().unwrap().snapshot.mode
    }
    pub fn set_mode(&self, mode: PermissionMode) {
        let mut s = self.state.lock().unwrap();
        s.snapshot.mode = mode;
        s.revision += 1;
    }
    pub fn pending(&self) -> Option<Interaction> {
        self.state
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .map(|p| p.request.clone())
    }
    pub fn answer(&self, id: u64, answer: Answer) -> bool {
        let mut s = self.state.lock().unwrap();
        if let Some(p) = &mut s.pending {
            if p.request.id == id && p.answer.is_none() {
                p.answer = Some(answer);
                s.revision += 1;
                return true;
            }
        }
        false
    }
    pub fn active(&self) -> Option<String> {
        self.state.lock().unwrap().active.clone()
    }
    pub fn active_mode(&self) -> PermissionMode {
        let s = self.state.lock().unwrap();
        s.snapshot
            .tasks
            .iter()
            .find(|t| Some(&t.task_id) == s.active.as_ref())
            .map_or(s.snapshot.mode, |t| t.permission)
    }
    pub fn begin(&self, workspace: PathBuf, model: String) -> Result<String, String> {
        let mut s = self.state.lock().unwrap();
        if s.active.is_some() {
            return Err("本会话已有执行任务".into());
        }
        if s.snapshot
            .tasks
            .iter()
            .filter(|t| !t.status.terminal())
            .count()
            >= s.snapshot.budget.concurrent_tasks
        {
            return Err("已达到会话并发预算".into());
        }
        s.snapshot.next_id += 1;
        let id = format!("{}-task-{}", self.session, s.snapshot.next_id);
        let permission = s.snapshot.mode;
        s.snapshot.tasks.push(ExecutionTask {
            session_id: self.session.clone(),
            task_id: id.clone(),
            parent_task_id: None,
            kind: "agent".into(),
            workspace,
            model,
            permission,
            status: TaskStatus::Running,
            started_at: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: 0,
            usage: Usage::default(),
            command: None,
            pid: None,
            result: None,
        });
        s.active = Some(id.clone());
        s.revision += 1;
        self.clock
            .lock()
            .unwrap()
            .insert(id.clone(), Instant::now());
        Ok(id)
    }
    pub fn check_budget(&self, usage: Usage) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        let id = s.active.clone().ok_or("没有活动任务")?;
        let elapsed = self
            .clock
            .lock()
            .unwrap()
            .get(&id)
            .map_or(Duration::ZERO, Instant::elapsed);
        let budget = s.snapshot.budget.clone();
        if let Some(t) = s.snapshot.tasks.iter_mut().find(|t| t.task_id == id) {
            t.usage = usage;
            t.elapsed_ms = elapsed.as_millis() as u64;
        }
        if elapsed.as_secs() >= budget.elapsed_seconds {
            return Err("任务耗时预算已用完".into());
        }
        if usage.total() >= budget.tokens {
            return Err("任务 token 预算已用完".into());
        }
        Ok(())
    }
    pub fn check_deadline(&self) -> Result<(), String> {
        let s = self.state.lock().unwrap();
        let id = s.active.as_ref().ok_or("没有活动任务")?;
        if self
            .clock
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|c| c.elapsed().as_secs() >= s.snapshot.budget.elapsed_seconds)
        {
            Err("任务耗时预算已用完".into())
        } else {
            Ok(())
        }
    }
    pub fn finish(&self, error: Option<&str>, cancelled: bool, usage: Usage) {
        let mut s = self.state.lock().unwrap();
        let Some(id) = s.active.take() else {
            return;
        };
        if let Some(t) = s.snapshot.tasks.iter_mut().find(|t| t.task_id == id) {
            t.status = if cancelled {
                TaskStatus::Cancelled
            } else if error.is_some() {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };
            t.result = error.map(str::to_owned);
            t.usage = usage;
            t.elapsed_ms = self
                .clock
                .lock()
                .unwrap()
                .remove(&id)
                .map_or(0, |c| c.elapsed().as_millis() as u64);
        }
        s.pending = None;
        s.revision += 1;
        drop(s);
        if cancelled || error.is_some() {
            self.stop_children(&id);
        }
    }
    pub fn take_results(&self) -> Vec<ExecutionResult> {
        let mut s = self.state.lock().unwrap();
        let results = std::mem::take(&mut s.snapshot.results);
        if !results.is_empty() {
            s.revision += 1;
        }
        results
    }
    pub fn wait(
        &self,
        mut request: Interaction,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<Answer, String> {
        {
            let mut s = self.state.lock().unwrap();
            if s.pending.is_some() {
                return Err("已有等待用户的请求".into());
            }
            request.task_id = s.active.clone().ok_or("没有活动任务")?;
            s.snapshot.next_id += 1;
            request.id = s.snapshot.next_id;
            if let Some(t) = s
                .snapshot
                .tasks
                .iter_mut()
                .find(|t| t.task_id == request.task_id)
            {
                t.status = TaskStatus::WaitingUser;
            }
            s.pending = Some(Pending {
                request,
                answer: None,
            });
            s.revision += 1;
        }
        let started = Instant::now();
        let result = loop {
            if abort.load(Ordering::Relaxed) {
                break Err("已取消，操作未执行".into());
            }
            if started.elapsed() >= timeout {
                break Err("等待用户超时，操作未执行".into());
            }
            if let Err(e) = self.check_budget(self.current_usage()) {
                break Err(e);
            }
            if let Some(answer) = self
                .state
                .lock()
                .unwrap()
                .pending
                .as_mut()
                .and_then(|p| p.answer.take())
            {
                break Ok(answer);
            }
            std::thread::sleep(Duration::from_millis(40));
        };
        let mut s = self.state.lock().unwrap();
        if let Some(pending) = s.pending.take() {
            if let Some(t) = s
                .snapshot
                .tasks
                .iter_mut()
                .find(|t| t.task_id == pending.request.task_id)
            {
                t.status = TaskStatus::Running;
            }
        }
        s.revision += 1;
        result
    }
    fn current_usage(&self) -> Usage {
        let s = self.state.lock().unwrap();
        s.snapshot
            .tasks
            .iter()
            .find(|t| Some(&t.task_id) == s.active.as_ref())
            .map_or(Usage::default(), |t| t.usage)
    }
    pub fn approve(
        &self,
        root: PathBuf,
        tool: &str,
        args: &str,
        target: Option<PathBuf>,
        reason: String,
        abort: &AtomicBool,
    ) -> Result<(), String> {
        match self.wait(
            Interaction {
                id: 0,
                task_id: String::new(),
                kind: InteractionKind::Approval,
                question: reason,
                options: vec![],
                tool: tool.into(),
                arguments: args.into(),
                workspace: root,
                target,
            },
            Duration::from_secs(300),
            abort,
        )? {
            Answer::Allow => Ok(()),
            Answer::Reject => Err("用户拒绝，操作未执行".into()),
            _ => Err("已取消，操作未执行".into()),
        }
    }
    pub fn register(self: &Arc<Self>, registry: &mut crate::ToolRegistry) -> Result<(), String> {
        for kind in ["todo_list", "clarify", "process_manage"] {
            registry.register(Arc::new(SessionTool {
                execution: self.clone(),
                kind,
            }))?;
        }
        Ok(())
    }
    pub fn spawn_process(
        self: &Arc<Self>,
        root: PathBuf,
        command: String,
        abort: &AtomicBool,
    ) -> Result<String, String> {
        let mut s = self.state.lock().unwrap();
        let parent = s.active.clone().ok_or("没有活动任务")?;
        let budget = s.snapshot.budget.clone();
        if s.snapshot
            .tasks
            .iter()
            .filter(|t| !t.status.terminal())
            .count()
            >= budget.concurrent_tasks
        {
            return Err("已达到会话并发预算".into());
        }
        if s.snapshot
            .tasks
            .iter()
            .filter(|t| t.parent_task_id.as_ref() == Some(&parent))
            .count()
            >= budget.child_tasks
        {
            return Err("已达到本任务子任务预算".into());
        }
        if abort.load(Ordering::Relaxed) {
            return Err("已取消，进程未启动".into());
        }
        let mut cmd = shell_command(&command);
        let mut child = cmd
            .current_dir(&root)
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let job = match crate::process_job::ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(e) => {
                kill_tree(&mut child);
                return Err(format!("无法登记进程树，已停止：{e}"));
            }
        };
        if abort.load(Ordering::Relaxed) {
            job.terminate();
            kill_tree(&mut child);
            return Err("已取消，进程已停止".into());
        }
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let process = Arc::new(Process {
            child: Mutex::new(child),
            job,
            output: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            overflow: AtomicBool::new(false),
            readers: std::sync::atomic::AtomicUsize::new(2),
        });
        s.snapshot.next_id += 1;
        let id = format!("{}-process-{}", self.session, s.snapshot.next_id);
        let model = s
            .snapshot
            .tasks
            .iter()
            .find(|t| t.task_id == parent)
            .map(|t| t.model.clone())
            .unwrap_or_default();
        let permission = s
            .snapshot
            .tasks
            .iter()
            .find(|t| t.task_id == parent)
            .unwrap()
            .permission;
        s.snapshot.tasks.push(ExecutionTask {
            session_id: self.session.clone(),
            task_id: id.clone(),
            parent_task_id: Some(parent),
            kind: "process".into(),
            workspace: root,
            model,
            permission,
            status: TaskStatus::Running,
            started_at: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: 0,
            usage: Usage::default(),
            command: Some(command),
            pid: Some(pid),
            result: None,
        });
        s.revision += 1;
        drop(s);
        self.processes
            .lock()
            .unwrap()
            .insert(id.clone(), process.clone());
        read_output(stdout, process.clone(), budget.output_bytes);
        read_output(stderr, process.clone(), budget.output_bytes);
        let owner = self.clone();
        let task_id = id.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = loop {
                let mut child = process.child.lock().unwrap();
                match child.try_wait() {
                    Ok(Some(status)) => {
                        break if process.stopped.load(Ordering::Relaxed) {
                            (TaskStatus::Cancelled, "进程已停止".into())
                        } else if status.success() {
                            (TaskStatus::Completed, "进程完成，退出码 0".into())
                        } else {
                            (
                                TaskStatus::Failed,
                                format!("进程失败，退出码 {:?}", status.code()),
                            )
                        }
                    }
                    Err(e) => {
                        kill_tree(&mut child);
                        break (TaskStatus::Failed, e.to_string());
                    }
                    _ => {}
                }
                if process.overflow.load(Ordering::Relaxed)
                    || started.elapsed().as_secs() >= budget.elapsed_seconds
                {
                    kill_tree(&mut child);
                    break (
                        TaskStatus::Failed,
                        "进程输出或耗时预算已用完，已停止".into(),
                    );
                }
                drop(child);
                std::thread::sleep(Duration::from_millis(40));
            };
            process.job.terminate();
            // Descendants may inherit pipe handles. Bound the drain wait and kill the tree.
            let drain = Instant::now();
            while process.readers.load(Ordering::Acquire) != 0
                && drain.elapsed() < Duration::from_secs(2)
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            if process.readers.load(Ordering::Acquire) != 0 {
                kill_tree(&mut process.child.lock().unwrap());
            }
            let mut s = owner.state.lock().unwrap();
            if let Some(t) = s.snapshot.tasks.iter_mut().find(|t| t.task_id == task_id) {
                t.status = outcome.0;
                t.pid = None;
                t.result = Some(outcome.1.clone());
                t.elapsed_ms = started.elapsed().as_millis() as u64;
            }
            s.snapshot.results.push(ExecutionResult {
                task_id,
                text: outcome.1,
            });
            s.revision += 1;
        });
        Ok(json!({"task_id":id,"pid":pid,"status":"running"}).to_string())
    }
    pub fn stop(&self, id: &str) -> Result<(), String> {
        let processes = self.processes.lock().unwrap();
        let process = processes
            .get(id)
            .ok_or("只能停止本会话登记的进程；旧 PID 不可操作")?;
        let mut child = process.child.lock().unwrap();
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            process.stopped.store(true, Ordering::Relaxed);
            process.job.terminate();
            kill_tree(&mut child);
        }
        Ok(())
    }
    fn stop_children(&self, parent: &str) {
        let ids: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .snapshot
            .tasks
            .iter()
            .filter(|t| t.parent_task_id.as_deref() == Some(parent) && !t.status.terminal())
            .map(|t| t.task_id.clone())
            .collect();
        for id in ids {
            let _ = self.stop(&id);
            self.stop_children(&id);
        }
    }
    pub fn cancel_all(&self) {
        let ids: Vec<_> = self.processes.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id);
        }
    }
    pub fn output(&self, id: &str, offset: usize, limit: usize) -> Result<String, String> {
        let processes = self.processes.lock().unwrap();
        let p = processes
            .get(id)
            .ok_or("进程输出不可用；重启后未重连旧 PID")?;
        let data = p.output.lock().unwrap();
        let text = String::from_utf8_lossy(&data);
        let part: String = text.chars().skip(offset).take(limit).collect();
        let total = text.chars().count();
        let next = offset.saturating_add(part.chars().count()).min(total);
        Ok(json!({"task_id":id,"text":part,"next_offset":next,"total_chars":total,"has_more":next<total,"truncated":p.overflow.load(Ordering::Relaxed)}).to_string())
    }
}
pub(crate) fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("cmd.exe");
        // Attach the suspended process before it can spawn descendants.
        cmd.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{command}\""))
            .creation_flags(0x08000000 | 0x00000004);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}
pub(crate) fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn read_output(reader: Option<impl Read + Send + 'static>, process: Arc<Process>, cap: usize) {
    std::thread::spawn(move || {
        if let Some(mut reader) = reader {
            let mut buffer = [0u8; 4096];
            let mut pending = Vec::new();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buffer[..n]);
                        loop {
                            match std::str::from_utf8(&pending) {
                                Ok(text) => {
                                    append_output(&process, text, cap);
                                    pending.clear();
                                    break;
                                }
                                Err(error) => {
                                    let valid = error.valid_up_to();
                                    append_output(
                                        &process,
                                        std::str::from_utf8(&pending[..valid]).unwrap(),
                                        cap,
                                    );
                                    pending.drain(..valid);
                                    if let Some(invalid) = error.error_len() {
                                        pending.drain(..invalid);
                                        append_output(&process, "�", cap);
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !pending.is_empty() {
                append_output(&process, &String::from_utf8_lossy(&pending), cap);
            }
        }
        process.readers.fetch_sub(1, Ordering::Release);
    });
}
fn append_output(process: &Process, text: &str, cap: usize) {
    let mut output = process.output.lock().unwrap();
    let mut take = text.len().min(cap.saturating_sub(output.len()));
    while !text.is_char_boundary(take) {
        take -= 1;
    }
    output.extend_from_slice(&text.as_bytes()[..take]);
    if take < text.len() {
        process.overflow.store(true, Ordering::Relaxed);
    }
}
struct SessionTool {
    execution: Arc<Execution>,
    kind: &'static str,
}
impl Tool for SessionTool {
    fn name(&self) -> &str {
        self.kind
    }
    fn description(&self) -> &str {
        match self.kind {
        "todo_list" => "维护本会话待办，action=list 或 replace；replace 传完整 items，每项含 id/text/status（pending/in_progress/completed）。独立保存，不依赖压缩摘要。",
        "clarify" => "向用户提问并等待回答。question 必填，options 可选，timeout_seconds 1–600，默认 300。仅阻塞本会话，返回 answered/rejected/cancelled/timeout/failed。",
        _ => "管理本会话通过 shell background=true 登记的进程。action=list/status/read/stop；read 的 offset/limit 为字符分页，返回 next_offset。不能操作任意 PID。",
    }
    }
    fn parameters(&self) -> Value {
        match self.kind {
            "todo_list" => {
                json!({"type":"object","properties":{"action":{"type":"string","enum":["list","replace"]},"items":{"type":"array","maxItems":100,"items":{"type":"object","properties":{"id":{"type":"string"},"text":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["id","text","status"],"additionalProperties":false}}},"required":["action"],"additionalProperties":false})
            }
            "clarify" => {
                json!({"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","maxItems":8,"items":{"type":"string"}},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}},"required":["question"],"additionalProperties":false})
            }
            _ => {
                json!({"type":"object","properties":{"action":{"type":"string","enum":["list","status","read","stop"]},"task_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":12000}},"required":["action"],"additionalProperties":false})
            }
        }
    }
    fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_with_abort(arguments, &AtomicBool::new(false))
    }
    fn call_with_abort(&self, arguments: &str, abort: &AtomicBool) -> Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        if abort.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        match self.kind {
            "todo_list" => {
                let mut s = self.execution.state.lock().unwrap();
                match args["action"].as_str() {
                    Some("list") => {}
                    Some("replace") => {
                        let items: Vec<TodoItem> = serde_json::from_value(args["items"].clone())
                            .map_err(|e| e.to_string())?;
                        let mut ids = std::collections::HashSet::new();
                        if items.len() > 100
                            || items.iter().any(|i| {
                                i.id.trim().is_empty()
                                    || i.id.len() > 100
                                    || i.text.trim().is_empty()
                                    || i.text.chars().count() > 2000
                                    || !ids.insert(&i.id)
                            })
                        {
                            return Err("待办过多、内容无效或 id 重复".into());
                        }
                        s.snapshot.todos = items;
                        s.revision += 1;
                    }
                    _ => return Err("action 需要 list 或 replace".into()),
                }
                Ok(json!({"items":s.snapshot.todos}).to_string())
            }
            "clarify" => {
                let question = args["question"]
                    .as_str()
                    .filter(|q| !q.trim().is_empty() && q.chars().count() <= 4000)
                    .ok_or("question 无效")?;
                let options: Vec<String> = args
                    .get("options")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default();
                if options.len() > 8
                    || options
                        .iter()
                        .any(|o| o.trim().is_empty() || o.chars().count() > 500)
                {
                    return Err("options 无效".into());
                }
                let timeout = args
                    .get("timeout_seconds")
                    .map(|v| {
                        v.as_u64()
                            .filter(|n| (1..=600).contains(n))
                            .ok_or("timeout_seconds 无效")
                    })
                    .transpose()?
                    .unwrap_or(300);
                let root = self
                    .execution
                    .snapshot()
                    .tasks
                    .iter()
                    .rev()
                    .find(|t| t.kind == "agent")
                    .map(|t| t.workspace.clone())
                    .unwrap_or_default();
                let result = self.execution.wait(
                    Interaction {
                        id: 0,
                        task_id: String::new(),
                        kind: InteractionKind::Clarify,
                        question: question.into(),
                        options,
                        tool: "clarify".into(),
                        arguments: arguments.into(),
                        workspace: root,
                        target: None,
                    },
                    Duration::from_secs(timeout),
                    abort,
                );
                Ok(match result {
                    Ok(Answer::Text(text)) => json!({"status":"answered","answer":text}),
                    Ok(Answer::Reject) => json!({"status":"rejected"}),
                    Ok(_) => json!({"status":"cancelled"}),
                    Err(e) => json!({"status":if abort.load(Ordering::Relaxed){"cancelled"} else if e.contains("超时"){"timeout"}else{"failed"},"reason":e}),
                }.to_string())
            }
            _ => {
                let action = args["action"].as_str().ok_or("缺少 action")?;
                if action == "list" {
                    return Ok(json!({"tasks":self.execution.snapshot().tasks.into_iter().filter(|t|t.kind=="process").collect::<Vec<_>>()}).to_string());
                }
                let id = args["task_id"].as_str().ok_or("缺少 task_id")?;
                let task = self
                    .execution
                    .snapshot()
                    .tasks
                    .into_iter()
                    .find(|t| t.task_id == id && t.kind == "process")
                    .ok_or("本会话没有该进程任务")?;
                match action {
                    "status" => Ok(serde_json::to_string(&task).unwrap()),
                    "read" => {
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
                                    .ok_or("limit 无效")
                            })
                            .transpose()?
                            .unwrap_or(6000) as usize;
                        self.execution.output(id, offset, limit)
                    }
                    "stop" => {
                        self.execution.stop(id)?;
                        Ok(json!({"task_id":id,"status":"stop_requested"}).to_string())
                    }
                    _ => Err("action 无效".into()),
                }
            }
        }
    }
}

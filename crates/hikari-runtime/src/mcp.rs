//! MCP client: initialize, paginated discovery, tools/call, cancellation and connection lifetime.
use hikari_agent::tool::Tool;
use hikari_agent::{AgentError, Result, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        bearer_token_env: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub transport: McpTransport,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn yes() -> bool {
    true
}
fn timeout() -> u64 {
    30
}
impl McpServerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || self.name.len() > 24
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err(AgentError::new(
                "MCP 名称需为 1–24 个英文字母、数字或下划线",
            ));
        }
        match &self.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(AgentError::new("MCP 启动命令不能为空"))
            }
            McpTransport::StreamableHttp { url, .. }
                if !url.starts_with("https://") && !url.starts_with("http://") =>
            {
                return Err(AgentError::new("MCP 地址需要 http:// 或 https://"))
            }
            _ => {}
        }
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err(AgentError::new("MCP 超时需要在 1–300 秒内"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct McpStatus {
    pub name: String,
    pub state: String,
    pub tools: Vec<String>,
    pub error: Option<String>,
}
struct Connected {
    config: McpServerConfig,
    client: Arc<Mutex<Client>>,
    tools: Vec<RemoteToolDef>,
}
#[derive(Default)]
pub struct McpManager {
    operation: Mutex<()>,
    connections: Mutex<BTreeMap<String, Connected>>,
    statuses: Mutex<BTreeMap<String, McpStatus>>,
}
impl McpManager {
    pub fn statuses(&self) -> Vec<McpStatus> {
        self.statuses.lock().unwrap().values().cloned().collect()
    }
    fn status(&self, name: &str, state: &str, tools: Vec<String>, error: Option<String>) {
        self.statuses.lock().unwrap().insert(
            name.into(),
            McpStatus {
                name: name.into(),
                state: state.into(),
                tools,
                error,
            },
        );
    }
    /// Run on a background worker, never the GPUI thread.
    pub fn connect(&self, config: &McpServerConfig, workspace: &Path) -> Result<()> {
        let _operation = self.operation.lock().unwrap();
        self.connect_inner(config, workspace)
    }
    fn connect_inner(&self, config: &McpServerConfig, workspace: &Path) -> Result<()> {
        config.validate()?;
        self.disconnect_inner(&config.name);
        if !config.enabled {
            self.status(&config.name, "disabled", Vec::new(), None);
            return Ok(());
        }
        self.status(&config.name, "connecting", Vec::new(), None);
        let result = (|| {
            let mut client = Client::connect(config, workspace)?;
            let abort = AtomicBool::new(false);
            let init=client.rpc("initialize",json!({"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"hikari","version":"0.1.0"}}),&abort)?;
            let version = init["protocolVersion"]
                .as_str()
                .ok_or_else(|| AgentError::new("MCP initialize 缺少协议版本"))?;
            if !["2024-11-05", "2025-03-26", VERSION].contains(&version) {
                return Err(AgentError::new(format!("不支持 MCP 协议版本 {version}")));
            }
            client.version = version.into();
            client.notify("notifications/initialized", json!({}))?;
            let mut tools = Vec::new();
            if init["capabilities"].get("tools").is_some() {
                let mut cursor: Option<String> = None;
                let mut seen = std::collections::HashSet::new();
                loop {
                    let page = client.rpc(
                        "tools/list",
                        cursor
                            .as_ref()
                            .map(|c| json!({"cursor":c}))
                            .unwrap_or(json!({})),
                        &abort,
                    )?;
                    let page_tools = page["tools"]
                        .as_array()
                        .ok_or_else(|| AgentError::new("MCP tools/list 缺少工具列表"))?;
                    for tool in page_tools {
                        let name = tool["name"]
                            .as_str()
                            .filter(|n| !n.is_empty())
                            .ok_or_else(|| AgentError::new("MCP 工具没有名称"))?;
                        if !seen.insert(name.to_string()) {
                            continue;
                        }
                        let alias = format!("mcp_{}_{}", config.name, normalize_name(name));
                        if tools.iter().any(|t: &RemoteToolDef| t.alias == alias) {
                            return Err(AgentError::new("MCP 工具名称映射发生冲突"));
                        }
                        tools.push(RemoteToolDef {
                            name: name.into(),
                            alias,
                            description: tool["description"].as_str().unwrap_or(name).into(),
                            schema: tool
                                .get("inputSchema")
                                .cloned()
                                .unwrap_or(json!({"type":"object"})),
                        });
                    }
                    let next = page["nextCursor"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    if next.is_none() {
                        break;
                    }
                    if next == cursor || tools.len() > 2048 {
                        return Err(AgentError::new("MCP 工具分页无法结束"));
                    }
                    cursor = next;
                }
            }
            let names = tools.iter().map(|t| t.alias.clone()).collect();
            self.connections.lock().unwrap().insert(
                config.name.clone(),
                Connected {
                    config: config.clone(),
                    client: Arc::new(Mutex::new(client)),
                    tools,
                },
            );
            self.status(&config.name, "connected", names, None);
            Ok(())
        })();
        if let Err(e) = &result {
            self.status(&config.name, "failed", Vec::new(), Some(e.message.clone()));
        }
        result
    }
    pub fn disconnect(&self, name: &str) {
        let _operation = self.operation.lock().unwrap();
        self.disconnect_inner(name);
    }
    fn disconnect_inner(&self, name: &str) {
        let connection = self.connections.lock().unwrap().remove(name);
        if let Some(connection) = connection {
            connection.client.lock().unwrap().close();
        }
        self.status(&name, "disconnected", Vec::new(), None);
    }
    pub fn reconcile(&self, configs: &[McpServerConfig], workspace: &Path) {
        let _operation = self.operation.lock().unwrap();
        let old: Vec<_> = self.connections.lock().unwrap().keys().cloned().collect();
        for name in old {
            if !configs.iter().any(|c| c.name == name && c.enabled) {
                self.disconnect_inner(&name);
            }
        }
        for config in configs {
            let same = self
                .connections
                .lock()
                .unwrap()
                .get(&config.name)
                .is_some_and(|c| c.config == *config);
            if !same && config.enabled {
                let _ = self.connect_inner(config, workspace);
            }
            if !config.enabled {
                self.status(&config.name, "disabled", Vec::new(), None);
            }
        }
    }
    pub fn register(&self, registry: &mut ToolRegistry) -> std::result::Result<(), String> {
        for connection in self.connections.lock().unwrap().values() {
            for def in &connection.tools {
                registry.register(Arc::new(RemoteTool {
                    def: def.clone(),
                    client: connection.client.clone(),
                }))?;
            }
        }
        Ok(())
    }
}
fn normalize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

#[derive(Clone)]
struct RemoteToolDef {
    name: String,
    alias: String,
    description: String,
    schema: Value,
}
struct RemoteTool {
    def: RemoteToolDef,
    client: Arc<Mutex<Client>>,
}
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.def.alias
    }
    fn description(&self) -> &str {
        &self.def.description
    }
    fn parameters(&self) -> Value {
        self.def.schema.clone()
    }
    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        self.call_with_abort(arguments, &AtomicBool::new(false))
    }
    fn call_with_abort(
        &self,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let result = self
            .client
            .lock()
            .map_err(|_| "MCP 连接已失效")?
            .rpc(
                "tools/call",
                json!({"name":self.def.name,"arguments":args}),
                abort,
            )
            .map_err(|e| e.message)?;
        let mut text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|block| {
                if block["type"] == "text" {
                    block["text"].as_str().unwrap_or("").to_string()
                } else {
                    format!(
                        "[MCP {} content]",
                        block["type"].as_str().unwrap_or("unknown")
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(structured) = result.get("structuredContent") {
            text.push_str(&format!("\n{structured}"));
        }
        if result["isError"].as_bool().unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

struct Client {
    transport: Transport,
    next_id: u64,
    version: String,
    timeout: Duration,
    closed: bool,
}
enum Transport {
    Stdio {
        child: Child,
        input: ChildStdin,
        output: mpsc::Receiver<std::result::Result<Value, String>>,
    },
    Http {
        http: ureq::Agent,
        url: String,
        token: String,
        session: Option<String>,
    },
}
impl Client {
    fn connect(config: &McpServerConfig, workspace: &Path) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_seconds);
        let transport = match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut process = Command::new(command);
                process
                    .args(args)
                    .envs(env)
                    .current_dir(workspace)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                process.creation_flags(0x08000000);
                let mut child = process
                    .spawn()
                    .map_err(|e| AgentError::new(format!("MCP 启动失败：{e}")))?;
                let input = child.stdin.take().unwrap();
                let output = child.stdout.take().unwrap();
                let (tx, rx) = mpsc::sync_channel(64);
                thread::spawn(move || {
                    let mut reader = BufReader::new(output);
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => break,
                            Ok(_) if line.len() > 16 * 1024 * 1024 => {
                                let _ = tx.send(Err("MCP 消息过大".into()));
                                break;
                            }
                            Ok(_) => {
                                if line.trim().is_empty() {
                                    continue;
                                }
                                if tx
                                    .send(
                                        serde_json::from_str(&line)
                                            .map_err(|e| format!("MCP stdout 不是 JSON-RPC：{e}")),
                                    )
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e.to_string()));
                                break;
                            }
                        }
                    }
                });
                Transport::Stdio {
                    child,
                    input,
                    output: rx,
                }
            }
            McpTransport::StreamableHttp {
                url,
                bearer_token_env,
            } => {
                let token = match bearer_token_env.as_ref().filter(|s| !s.is_empty()) {
                    Some(name) => std::env::var(name)
                        .map_err(|_| AgentError::new(format!("MCP 密钥环境变量 {name} 未设置")))?,
                    None => String::new(),
                };
                Transport::Http {
                    http: ureq::AgentBuilder::new()
                        .timeout_connect(timeout)
                        .timeout_read(timeout)
                        .timeout_write(timeout)
                        .build(),
                    url: url.clone(),
                    token,
                    session: None,
                }
            }
        };
        Ok(Self {
            transport,
            next_id: 0,
            version: VERSION.into(),
            timeout,
            closed: false,
        })
    }
    fn send_stdio(input: &mut ChildStdin, value: &Value) -> Result<()> {
        writeln!(input, "{value}")
            .and_then(|_| input.flush())
            .map_err(|e| AgentError::new(format!("MCP 写入失败：{e}")))
    }
    fn http_request(
        http: &ureq::Agent,
        url: &str,
        token: &str,
        session: Option<&str>,
        version: &str,
    ) -> ureq::Request {
        let mut request = http
            .post(url)
            .set("Accept", "application/json, text/event-stream")
            .set("MCP-Protocol-Version", version);
        if let Some(session) = session {
            request = request.set("Mcp-Session-Id", session);
        }
        if !token.is_empty() {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        request
    }
    fn rpc(&mut self, method: &str, params: Value, abort: &AtomicBool) -> Result<Value> {
        if self.closed {
            return Err(AgentError::new("MCP 已断开，请重新连接"));
        }
        if abort.load(Ordering::Relaxed) {
            return Err(AgentError::new("MCP 调用已取消"));
        }
        self.next_id += 1;
        let id = self.next_id;
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let response = match &mut self.transport {
            Transport::Stdio { input, output, .. } => {
                Self::send_stdio(input, &body)?;
                let start = Instant::now();
                loop {
                    if abort.load(Ordering::Relaxed) || start.elapsed() > self.timeout {
                        let _ = Self::send_stdio(
                            input,
                            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"Cancelled by Hikari"}}),
                        );
                        return Err(AgentError::new(if abort.load(Ordering::Relaxed) {
                            "MCP 调用已取消"
                        } else {
                            "MCP 调用超时"
                        }));
                    }
                    let frame = match output.recv_timeout(Duration::from_millis(40)) {
                        Ok(value) => value.map_err(AgentError::new)?,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => return Err(AgentError::new("MCP 进程已退出")),
                    };
                    if let Some(server_method) = frame["method"].as_str() {
                        if frame.get("id").is_some() {
                            let reply = if server_method == "ping" {
                                json!({"jsonrpc":"2.0","id":frame["id"],"result":{}})
                            } else {
                                json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32601,"message":"Client capability not supported"}})
                            };
                            Self::send_stdio(input, &reply)?;
                        }
                        continue;
                    }
                    if frame["id"] == id {
                        break frame;
                    }
                }
            }
            Transport::Http {
                http,
                url,
                token,
                session,
            } => {
                let response =
                    Self::http_request(http, url, token, session.as_deref(), &self.version)
                        .send_json(body)
                        .map_err(|e| redacted_http_error(e, token))?;
                if method == "initialize" {
                    *session = response.header("Mcp-Session-Id").map(str::to_string);
                }
                let mut result = None;
                hikari_agent::transport::read_json_stream(response, abort, |frame| {
                    if frame["id"] == id {
                        result = Some(frame);
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                })?;
                result.ok_or_else(|| AgentError::new("MCP 没有返回匹配的请求结果"))?
            }
        };
        if let Some(error) = response.get("error") {
            return Err(AgentError::new(
                error["message"].as_str().unwrap_or("MCP 请求失败"),
            ));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| AgentError::new("MCP 响应缺少 result"))
    }
    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let body = json!({"jsonrpc":"2.0","method":method,"params":params});
        match &mut self.transport {
            Transport::Stdio { input, .. } => Self::send_stdio(input, &body),
            Transport::Http {
                http,
                url,
                token,
                session,
            } => Self::http_request(http, url, token, session.as_deref(), &self.version)
                .send_json(body)
                .map(|_| ())
                .map_err(|e| redacted_http_error(e, token)),
        }
    }
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        match &mut self.transport {
            Transport::Stdio { child, .. } => {
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
            }
            Transport::Http {
                http,
                url,
                token,
                session,
            } => {
                if let Some(id) = session.take() {
                    let mut request = http
                        .delete(url)
                        .set("Mcp-Session-Id", &id)
                        .set("MCP-Protocol-Version", &self.version);
                    if !token.is_empty() {
                        request = request.set("Authorization", &format!("Bearer {token}"));
                    }
                    let _ = request.call();
                }
            }
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}
fn redacted_http_error(error: ureq::Error, token: &str) -> AgentError {
    let mut message = match error {
        ureq::Error::Status(status, _) => {
            format!("MCP HTTP 请求失败（{status}），请检查连接和认证")
        }
        ureq::Error::Transport(e) => format!("MCP 连接失败：{e}"),
    };
    if !token.is_empty() {
        message = message.replace(token, "•••");
    }
    AgentError::new(message)
}

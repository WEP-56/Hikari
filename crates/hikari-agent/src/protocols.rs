//! Protocol adapters around Pi's event and replay semantics.
use crate::{
    AgentError, Context, Message, Provider, ProviderMetadata, Result, StopReason, StreamEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiProtocol {
    #[default]
    ChatCompletions,
    Responses,
    Anthropic,
}

impl ApiProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::ChatCompletions => "OpenAI Chat Completions",
            Self::Responses => "OpenAI Responses",
            Self::Anthropic => "Anthropic Messages",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::Responses => "responses",
            Self::Anthropic => "messages",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub protocol: ApiProtocol,
    pub base_url: String,
    pub model: String,
    /// Resolve credentials from the process environment; never persist the value.
    pub api_key_env: String,
    pub max_output_tokens: u32,
    pub context_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub compatibility: CompatibilityConfig,
    pub timeout_seconds: u64,
}
/// Gateways vary in their Chat Completions and Responses dialects.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompatibilityConfig {
    pub max_tokens_field: String,
    pub stream_usage: bool,
    pub system_role: String,
    pub responses_store: bool,
    pub reasoning: bool,
}
impl Default for CompatibilityConfig {
    fn default() -> Self {
        Self {
            max_tokens_field: "max_completion_tokens".into(),
            stream_usage: true,
            system_role: "system".into(),
            responses_store: false,
            reasoning: true,
        }
    }
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            protocol: ApiProtocol::ChatCompletions,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4.1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            max_output_tokens: 8192,
            context_tokens: 128_000,
            reasoning_effort: None,
            headers: BTreeMap::new(),
            compatibility: CompatibilityConfig::default(),
            timeout_seconds: 60,
        }
    }
}

impl ProviderConfig {
    pub fn validate(&self) -> Result<()> {
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://"))
            || self.model.trim().is_empty()
        {
            return Err(AgentError::new("请填写 HTTP(S) 模型地址和模型名称"));
        }
        if self.context_tokens < 1024
            || self.context_tokens > 100_000_000
            || self.max_output_tokens == 0
            || self.max_output_tokens > self.context_tokens
        {
            return Err(AgentError::new(
                "上下文需要在 1024–100000000 内；最大输出需要大于 0 且不超过上下文",
            ));
        }
        if !(1..=600).contains(&self.timeout_seconds) {
            return Err(AgentError::new("请求超时需要在 1–600 秒内"));
        }
        if !["max_tokens", "max_completion_tokens"]
            .contains(&self.compatibility.max_tokens_field.as_str())
            || !["system", "developer", "user"].contains(&self.compatibility.system_role.as_str())
        {
            return Err(AgentError::new("接口兼容项无效"));
        }
        if self
            .reasoning_effort
            .as_deref()
            .is_some_and(|s| !["minimal", "low", "medium", "high", "xhigh"].contains(&s))
        {
            return Err(AgentError::new(
                "思考挡位需要为 minimal / low / medium / high / xhigh",
            ));
        }
        if self.protocol == ApiProtocol::Anthropic && self.compatibility.reasoning {
            if let Some(effort) = self.reasoning_effort.as_deref() {
                if self.max_output_tokens <= thinking_budget(effort) {
                    return Err(AgentError::new("Anthropic 最大输出必须大于所选思考预算（low/minimal 1024、medium 2048、high 4096、xhigh 8192）"));
                }
            }
        }
        validate_headers(&self.headers)
    }
}
fn thinking_budget(effort: &str) -> u32 {
    match effort {
        "minimal" | "low" => 1024,
        "medium" => 2048,
        "xhigh" => 8192,
        _ => 4096,
    }
}

/// Secret headers use ${ENV_NAME}, optionally preceded by `Bearer `.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<()> {
    for (name, value) in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || value.contains(['\r', '\n'])
        {
            return Err(AgentError::new("请求头名称或内容无效"));
        }
        let lower = name.to_ascii_lowercase();
        if (lower.contains("authorization")
            || lower.contains("key")
            || lower.contains("token")
            || lower.contains("cookie"))
            && !value.contains("${")
        {
            return Err(AgentError::new(
                "认证请求头请使用环境变量引用，例如 Bearer ${MY_API_KEY}，密钥不能写入配置",
            ));
        }
        // Validate references without resolving them, so offline saving works.
        let mut tail = value.as_str();
        while let Some((_, rest)) = tail.split_once("${") {
            let (var, next) = rest
                .split_once('}')
                .ok_or_else(|| AgentError::new("请求头环境变量引用缺少 }"))?;
            if var.is_empty() || !var.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(AgentError::new("请求头环境变量名称无效"));
            }
            tail = next;
        }
    }
    Ok(())
}
pub fn resolve_headers(headers: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    validate_headers(headers)?;
    headers
        .iter()
        .map(|(name, value)| {
            let mut tail = value.as_str();
            let mut resolved = String::new();
            while let Some((prefix, rest)) = tail.split_once("${") {
                resolved.push_str(prefix);
                let (var, next) = rest.split_once('}').unwrap();
                let secret = std::env::var(var)
                    .map_err(|_| AgentError::new(format!("缺少请求头环境变量：{var}")))?;
                if secret.contains(['\r', '\n']) {
                    return Err(AgentError::new("请求头环境变量包含换行"));
                }
                resolved.push_str(&secret);
                tail = next;
            }
            resolved.push_str(tail);
            Ok((name.clone(), resolved))
        })
        .collect()
}

/// Model discovery is explicit and shares the provider's authentication and headers.
pub fn fetch_models(config: &ProviderConfig, key: &str) -> Result<Vec<String>> {
    config.validate()?;
    let base = config.base_url.trim().trim_end_matches('/');
    let base = base
        .strip_suffix(&format!("/{}", config.protocol.path()))
        .unwrap_or(base);
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(config.timeout_seconds))
        .build();
    let headers = resolve_headers(&config.headers)?;
    let mut ids = std::collections::BTreeSet::new();
    let mut after: Option<String> = None;
    // Anthropic uses cursor pagination; bound broken gateways.
    for _ in 0..100 {
        let mut request = http.get(&format!("{base}/models"));
        if config.protocol == ApiProtocol::Anthropic {
            request = request
                .set("anthropic-version", "2023-06-01")
                .set("x-api-key", key)
                .query("limit", "100");
            if let Some(cursor) = &after {
                request = request.query("after_id", cursor);
            }
        } else if !key.is_empty() {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        for (name, value) in &headers {
            request = request.set(name, value);
        }
        let mut secrets: Vec<&str> = headers.values().map(String::as_str).collect();
        secrets.push(key);
        let value: Value = request
            .call()
            .map_err(|e| crate::transport::http_error(e, &secrets))?
            .into_json()
            .map_err(|_| AgentError::new("模型列表不是有效 JSON"))?;
        let data = value["data"]
            .as_array()
            .ok_or_else(|| AgentError::new("模型列表缺少 data 数组；此服务可能不支持模型发现"))?;
        for item in data {
            if let Some(id) = item["id"].as_str().filter(|id| !id.trim().is_empty()) {
                ids.insert(id.to_owned());
            }
        }
        if config.protocol != ApiProtocol::Anthropic || value["has_more"] != true {
            return Ok(ids.into_iter().collect());
        }
        let cursor = value["last_id"]
            .as_str()
            .ok_or_else(|| AgentError::new("模型分页缺少 last_id"))?
            .to_owned();
        if after.as_ref() == Some(&cursor) {
            return Err(AgentError::new("模型分页游标重复"));
        }
        after = Some(cursor);
    }
    Err(AgentError::new("模型分页超过 100 页"))
}

pub struct ProtocolProvider {
    config: ProviderConfig,
    key: String,
    http: ureq::Agent,
}
impl ProtocolProvider {
    pub fn new(config: ProviderConfig, key: impl Into<String>) -> Result<Self> {
        config.validate()?;
        let timeout = config.timeout_seconds;
        Ok(Self {
            config,
            key: key.into(),
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(15))
                .timeout_read(Duration::from_secs(timeout))
                .timeout_write(Duration::from_secs(30))
                .build(),
        })
    }
}

impl Provider for ProtocolProvider {
    fn stream(
        &self,
        context: &Context<'_>,
        abort: &AtomicBool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<()> {
        if abort.load(Ordering::Relaxed) {
            return Ok(());
        }
        let endpoint = endpoint(&self.config);
        if self.key.is_empty()
            && !crate::openai::allows_empty_key(&endpoint)
            && !self.config.headers.keys().any(|h| {
                matches!(
                    h.to_ascii_lowercase().as_str(),
                    "authorization" | "x-api-key"
                )
            })
        {
            return Err(AgentError::new(
                "还没有密钥。在设置里填写或设置密钥环境变量。",
            ));
        }
        let mut request = self.http.post(&endpoint).set("Accept", "text/event-stream");
        if self.config.protocol == ApiProtocol::Anthropic {
            request = request.set("anthropic-version", "2023-06-01");
            if !self.key.is_empty() {
                request = request.set("x-api-key", &self.key);
            }
        } else if !self.key.is_empty() {
            request = request.set("Authorization", &format!("Bearer {}", self.key));
        }
        let headers = resolve_headers(&self.config.headers)?;
        for (name, value) in &headers {
            request = request.set(name, value);
        }
        let body = request_body(&self.config, context);
        let mut attempts = 0;
        let response = loop {
            match request.clone().send_json(body.clone()) {
                Ok(response) => break response,
                Err(error)
                    if attempts < 2
                        && matches!(
                            &error,
                            ureq::Error::Status(429 | 500 | 502 | 503 | 504, _)
                        ) =>
                {
                    attempts += 1;
                    for _ in 0..(attempts * 10) {
                        if abort.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(error) => {
                    let mut secrets: Vec<&str> = headers.values().map(String::as_str).collect();
                    secrets.push(&self.key);
                    return Err(crate::transport::http_error(error, &secrets));
                }
            }
        };
        let mut decoder = Decoder::new(self.config.protocol);
        crate::transport::read_json_stream(response, abort, |value| {
            let events = decoder.decode(value)?;
            for event in events {
                on_event(event);
            }
            // Chat usage is sent after finish_reason in a separate frame.
            Ok(decoder.finished && self.config.protocol != ApiProtocol::ChatCompletions)
        })?;
        if !decoder.finished && !abort.load(Ordering::Relaxed) {
            return Err(AgentError::new("模型流提前结束，未收到完成事件。"));
        }
        Ok(())
    }
}

pub fn endpoint(config: &ProviderConfig) -> String {
    let base = config.base_url.trim().trim_end_matches('/');
    if base.ends_with(config.protocol.path()) {
        base.to_string()
    } else {
        format!("{base}/{}", config.protocol.path())
    }
}

pub fn request_body(config: &ProviderConfig, context: &Context<'_>) -> Value {
    match config.protocol {
        ApiProtocol::ChatCompletions => {
            let mut body = crate::openai::request_body(&config.model, context);
            body[&config.compatibility.max_tokens_field] = json!(config.max_output_tokens);
            if !config.compatibility.stream_usage {
                body.as_object_mut().unwrap().remove("stream_options");
            }
            for message in body["messages"].as_array_mut().unwrap() {
                if message["role"] == "system" {
                    message["role"] = json!(config.compatibility.system_role);
                }
            }
            if let Some(effort) = config
                .reasoning_effort
                .as_ref()
                .filter(|_| config.compatibility.reasoning)
            {
                body["reasoning_effort"] = json!(effort);
            }
            body
        }
        ApiProtocol::Responses => {
            let mut input = Vec::new();
            for message in context.messages {
                match message {
                    Message::User { text } => input.push(json!({"role":"user","content":text})),
                    Message::Assistant { text, tool_calls, provider_metadata, .. } => {
                        if let Some(meta) = provider_metadata.as_ref().filter(|m| m.protocol == "responses") {
                            input.extend(meta.items.clone());
                        } else {
                            if !text.is_empty() { input.push(json!({"role":"assistant","content":text})); }
                            for call in tool_calls {
                                let call_id = call.id.split('|').next().unwrap_or(&call.id);
                                input.push(json!({"type":"function_call","call_id":call_id,"name":call.name,"arguments":call.arguments}));
                            }
                        }
                    }
                    Message::ToolResult { tool_call_id, text, .. } => input.push(json!({
                        "type":"function_call_output","call_id":tool_call_id.split('|').next().unwrap_or(tool_call_id),"output":text
                    })),
                    Message::ContextSummary { text } => input.push(json!({"role":"developer","content":text})),
                }
            }
            let mut body = json!({"model":config.model,"instructions":context.system_prompt,"input":input,
                "stream":true,"store":config.compatibility.responses_store,"max_output_tokens":config.max_output_tokens,
                "include":["reasoning.encrypted_content"]});
            if !config.compatibility.reasoning {
                body.as_object_mut().unwrap().remove("include");
            }
            if !context.tools.is_empty() {
                body["tools"] = json!(context
                    .tools
                    .iter()
                    .map(|t| json!({"type":"function","name":t.name,
                    "description":t.description,"parameters":t.parameters,"strict":false}))
                    .collect::<Vec<_>>());
            }
            if let Some(effort) = config
                .reasoning_effort
                .as_ref()
                .filter(|_| config.compatibility.reasoning)
            {
                body["reasoning"] = json!({"effort":effort,"summary":"auto"});
            }
            body
        }
        ApiProtocol::Anthropic => {
            let mut messages: Vec<Value> = Vec::new();
            for message in context.messages {
                let (role, content) = match message {
                    Message::User { text } | Message::ContextSummary { text } => {
                        ("user", vec![json!({"type":"text","text":text})])
                    }
                    Message::Assistant {
                        text,
                        tool_calls,
                        provider_metadata,
                        ..
                    } => {
                        let mut content = Vec::new();
                        if let Some(meta) = provider_metadata
                            .as_ref()
                            .filter(|m| m.protocol == "anthropic")
                        {
                            content.extend(meta.items.clone());
                        } else {
                            if !text.is_empty() {
                                content.push(json!({"type":"text","text":text}));
                            }
                            for call in tool_calls {
                                content.push(json!({"type":"tool_use","id":call.id,"name":call.name,
                                "input":serde_json::from_str::<Value>(&call.arguments).unwrap_or(json!({}))}));
                            }
                        }
                        if content.is_empty() {
                            continue;
                        }
                        ("assistant", content)
                    }
                    Message::ToolResult {
                        tool_call_id,
                        text,
                        is_error,
                        ..
                    } => (
                        "user",
                        vec![json!({
                            "type":"tool_result","tool_use_id":tool_call_id,"content":text,"is_error":is_error
                        })],
                    ),
                };
                if let Some(previous) = messages.last_mut().filter(|v| v["role"] == role) {
                    previous["content"].as_array_mut().unwrap().extend(content);
                } else {
                    messages.push(json!({"role":role,"content":content}));
                }
            }
            let mut body = json!({"model":config.model,"system":context.system_prompt,"messages":messages,
                "stream":true,"max_tokens":config.max_output_tokens});
            if !context.tools.is_empty() {
                body["tools"] = json!(context
                    .tools
                    .iter()
                    .map(|t| json!({
                        "name":t.name,"description":t.description,"input_schema":t.parameters
                    }))
                    .collect::<Vec<_>>());
            }
            if let Some(effort) = config
                .reasoning_effort
                .as_deref()
                .filter(|_| config.compatibility.reasoning)
            {
                let budget = thinking_budget(effort);
                if config.max_output_tokens > budget {
                    body["thinking"] = json!({"type":"enabled","budget_tokens":budget});
                }
            }
            body
        }
    }
}

/// Pi-style stateful assembly: stable tool slots, terminal snapshots and native reasoning replay.
pub struct Decoder {
    protocol: ApiProtocol,
    pub finished: bool,
    input: u64,
    output: u64,
    items: BTreeMap<usize, Value>,
    calls: BTreeMap<usize, (String, String, String)>,
    text_seen: bool,
}
impl Decoder {
    pub fn new(protocol: ApiProtocol) -> Self {
        Self {
            protocol,
            finished: false,
            input: 0,
            output: 0,
            items: BTreeMap::new(),
            calls: BTreeMap::new(),
            text_seen: false,
        }
    }
    pub fn decode(&mut self, value: Value) -> Result<Vec<StreamEvent>> {
        if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
            return Err(AgentError::new(message));
        }
        match self.protocol {
            ApiProtocol::ChatCompletions => {
                let events = crate::openai::parse_data_line(&value.to_string())?;
                self.finished |= events.iter().any(|e| matches!(e, StreamEvent::Done(_)));
                Ok(events)
            }
            ApiProtocol::Responses => self.responses(value),
            ApiProtocol::Anthropic => self.anthropic(value),
        }
    }
    fn metadata(&self) -> StreamEvent {
        StreamEvent::Metadata(ProviderMetadata {
            protocol: match self.protocol {
                ApiProtocol::Responses => "responses",
                _ => "anthropic",
            }
            .into(),
            items: self.items.values().cloned().collect(),
        })
    }
    fn responses(&mut self, v: Value) -> Result<Vec<StreamEvent>> {
        let mut out = Vec::new();
        let kind = v["type"].as_str().unwrap_or("");
        let index = v["output_index"]
            .as_u64()
            .map(|i| i as usize)
            .or_else(|| {
                self.items
                    .iter()
                    .find(|(_, item)| item["id"] == v["item_id"])
                    .map(|(i, _)| *i)
            })
            .unwrap_or(0);
        match kind {
            "response.output_text.delta" | "response.refusal.delta" => {
                self.text_seen = true;
                out.push(StreamEvent::Text(v["delta"].as_str().unwrap_or("").into()));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => out.push(
                StreamEvent::Thinking(v["delta"].as_str().unwrap_or("").into()),
            ),
            "response.output_item.added" | "response.output_item.done" => {
                let item = v["item"].clone();
                if item["type"] == "function_call" {
                    let args = item["arguments"].as_str().unwrap_or("").to_string();
                    let slot = self.calls.entry(index).or_insert_with(|| {
                        (
                            item["call_id"].as_str().unwrap_or("").into(),
                            item["name"].as_str().unwrap_or("").into(),
                            String::new(),
                        )
                    });
                    let delta = args.strip_prefix(&slot.2).unwrap_or("").to_string();
                    if !args.is_empty() {
                        slot.2 = args;
                    }
                    out.push(StreamEvent::ToolDelta {
                        index,
                        id: Some(slot.0.clone()),
                        name: Some(slot.1.clone()),
                        arguments: delta,
                    });
                }
                self.items.insert(index, item);
            }
            "response.function_call_arguments.delta" => {
                let args = v["delta"].as_str().unwrap_or("").to_string();
                if let Some(slot) = self.calls.get_mut(&index) {
                    slot.2.push_str(&args);
                }
                out.push(StreamEvent::ToolDelta {
                    index,
                    id: None,
                    name: None,
                    arguments: args,
                });
            }
            "response.failed" | "error" => {
                return Err(AgentError::new(
                    v.pointer("/response/error/message")
                        .or(v.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("模型请求失败"),
                ))
            }
            "response.completed" | "response.done" | "response.incomplete" | ""
                if v.get("output").is_some() || !kind.is_empty() =>
            {
                let response = v.get("response").unwrap_or(&v);
                if let Some(items) = response["output"].as_array() {
                    for (i, item) in items.iter().enumerate() {
                        if !self.text_seen && item["type"] == "message" {
                            for part in item["content"].as_array().into_iter().flatten() {
                                if let Some(text) = part["text"].as_str() {
                                    out.push(StreamEvent::Text(text.into()));
                                }
                            }
                        }
                        if item["type"] == "function_call" && !self.calls.contains_key(&i) {
                            out.push(StreamEvent::ToolDelta {
                                index: i,
                                id: item["call_id"].as_str().map(str::to_string),
                                name: item["name"].as_str().map(str::to_string),
                                arguments: item["arguments"].as_str().unwrap_or("").into(),
                            });
                        }
                        self.items.insert(i, item.clone());
                    }
                }
                out.push(self.metadata());
                out.push(StreamEvent::Usage {
                    input: response["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    output: response["usage"]["output_tokens"].as_u64().unwrap_or(0),
                });
                let stop = if response["incomplete_details"]["reason"] == "max_output_tokens" {
                    StopReason::Length
                } else if kind == "response.incomplete" {
                    StopReason::Error
                } else if self
                    .items
                    .values()
                    .any(|item| item["type"] == "function_call")
                {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                };
                out.push(StreamEvent::Done(stop));
                self.finished = true;
            }
            _ => {}
        }
        Ok(out)
    }
    fn anthropic(&mut self, v: Value) -> Result<Vec<StreamEvent>> {
        let mut out = Vec::new();
        let index = v["index"].as_u64().unwrap_or(0) as usize;
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                self.input = u["input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let item = v["content_block"].clone();
                if item["type"] == "tool_use" {
                    self.calls.insert(
                        index,
                        (
                            item["id"].as_str().unwrap_or("").into(),
                            item["name"].as_str().unwrap_or("").into(),
                            String::new(),
                        ),
                    );
                    out.push(StreamEvent::ToolDelta {
                        index,
                        id: item["id"].as_str().map(str::to_string),
                        name: item["name"].as_str().map(str::to_string),
                        arguments: String::new(),
                    });
                }
                if let Some(text) = item["text"].as_str().filter(|s| !s.is_empty()) {
                    out.push(StreamEvent::Text(text.into()));
                }
                self.items.insert(index, item);
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" | "thinking_delta" | "signature_delta" => {
                        let field = match d["type"].as_str() {
                            Some("thinking_delta") => "thinking",
                            Some("signature_delta") => "signature",
                            _ => "text",
                        };
                        let delta = d[field].as_str().unwrap_or("");
                        if let Some(item) = self.items.get_mut(&index) {
                            let mut text = item[field].as_str().unwrap_or("").to_string();
                            text.push_str(delta);
                            item[field] = json!(text);
                        }
                        if field == "text" {
                            out.push(StreamEvent::Text(delta.into()));
                        }
                        if field == "thinking" {
                            out.push(StreamEvent::Thinking(delta.into()));
                        }
                    }
                    "input_json_delta" => {
                        let args = d["partial_json"].as_str().unwrap_or("");
                        if let Some(slot) = self.calls.get_mut(&index) {
                            slot.2.push_str(args);
                        }
                        out.push(StreamEvent::ToolDelta {
                            index,
                            id: None,
                            name: None,
                            arguments: args.into(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(slot) = self.calls.get(&index) {
                    if !slot.2.is_empty() {
                        let input = serde_json::from_str::<Value>(&slot.2)
                            .map_err(|e| AgentError::new(format!("工具参数未完成：{e}")))?;
                        if let Some(item) = self.items.get_mut(&index) {
                            item["input"] = input;
                        }
                    }
                }
            }
            "message_delta" => {
                self.output = v["usage"]["output_tokens"].as_u64().unwrap_or(self.output);
                let stop = match v["delta"]["stop_reason"].as_str() {
                    Some("tool_use") => StopReason::ToolUse,
                    Some("max_tokens") => StopReason::Length,
                    Some("refusal") => StopReason::Error,
                    Some("pause_turn") => StopReason::PauseTurn,
                    _ => StopReason::Stop,
                };
                out.push(StreamEvent::Usage {
                    input: self.input,
                    output: self.output,
                });
                out.push(StreamEvent::Done(stop));
            }
            "message_stop" => {
                out.push(self.metadata());
                self.finished = true;
            }
            "message" => {
                self.items = v["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned()
                    .enumerate()
                    .collect();
                for (i, item) in &self.items {
                    if let Some(text) = item["text"].as_str() {
                        out.push(StreamEvent::Text(text.into()));
                    }
                    if item["type"] == "tool_use" {
                        out.push(StreamEvent::ToolDelta {
                            index: *i,
                            id: item["id"].as_str().map(str::to_string),
                            name: item["name"].as_str().map(str::to_string),
                            arguments: item["input"].to_string(),
                        });
                    }
                }
                out.push(self.metadata());
                out.push(StreamEvent::Usage {
                    input: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    output: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
                });
                out.push(StreamEvent::Done(if v["stop_reason"] == "tool_use" {
                    StopReason::ToolUse
                } else if v["stop_reason"] == "max_tokens" {
                    StopReason::Length
                } else {
                    StopReason::Stop
                }));
                self.finished = true;
            }
            _ => {}
        }
        Ok(out)
    }
}

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
        }
    }
}

pub struct ProtocolProvider {
    config: ProviderConfig,
    key: String,
    http: ureq::Agent,
}
impl ProtocolProvider {
    pub fn new(config: ProviderConfig, key: impl Into<String>) -> Result<Self> {
        if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
            return Err(AgentError::new("请填写模型地址和名字。"));
        }
        Ok(Self {
            config,
            key: key.into(),
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(15))
                .timeout_read(Duration::from_secs(60))
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
        if self.key.is_empty() && !crate::openai::allows_empty_key(&endpoint) {
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
                Err(error) => return Err(crate::transport::http_error(error, &[&self.key])),
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
            body["max_completion_tokens"] = json!(config.max_output_tokens);
            if let Some(effort) = &config.reasoning_effort {
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
                "stream":true,"store":false,"max_output_tokens":config.max_output_tokens,
                "include":["reasoning.encrypted_content"]});
            if !context.tools.is_empty() {
                body["tools"] = json!(context
                    .tools
                    .iter()
                    .map(|t| json!({"type":"function","name":t.name,
                    "description":t.description,"parameters":t.parameters,"strict":false}))
                    .collect::<Vec<_>>());
            }
            if let Some(effort) = &config.reasoning_effort {
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
            if let Some(effort) = config.reasoning_effort.as_deref() {
                let budget = match effort {
                    "low" => 1024,
                    "medium" => 2048,
                    _ => 4096,
                };
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

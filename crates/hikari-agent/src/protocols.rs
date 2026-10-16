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
    /// Maximum total attempts before a request-opening failure is returned.
    pub max_attempts: Option<u32>,
    /// Maximum SSE silence before the stream is considered stalled.
    pub idle_timeout_seconds: Option<u64>,
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
    pub tool_disclosure: ToolDisclosure,
    /// Some Anthropic-compatible gateways only accept a string system prompt.
    pub prompt_cache: bool,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDisclosure {
    #[default]
    Compatible,
    ResponsesAdditionalTools,
    ResponsesToolSearch,
    AnthropicInlineTools,
}
impl Default for CompatibilityConfig {
    fn default() -> Self {
        Self {
            max_tokens_field: "max_completion_tokens".into(),
            stream_usage: true,
            system_role: "system".into(),
            responses_store: false,
            reasoning: true,
            tool_disclosure: ToolDisclosure::Compatible,
            prompt_cache: true,
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
            max_attempts: Some(11),
            idle_timeout_seconds: Some(90),
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
        if self.max_attempts.is_some_and(|value| !(1..=100).contains(&value)) {
            return Err(AgentError::new("请求重试次数需要在 1–100 次内"));
        }
        if self.idle_timeout_seconds.is_some_and(|value| !(2..=3600).contains(&value)) {
            return Err(AgentError::new("流空闲超时需要在 2–3600 秒内"));
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
        let timeout_seconds = config.timeout_seconds;
        Ok(Self {
            config,
            key: key.into(),
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(timeout_seconds))
                // ureq applies the read timeout while waiting for response
                // headers as well as while reading the SSE body. Keep this
                // wide enough for provider queueing/first-token latency;
                // body idle handling remains explicit below.
                .timeout_read(Duration::from_secs(timeout_seconds))
                .timeout_write(Duration::from_secs(30))
                .build(),
        })
    }
}

impl Provider for ProtocolProvider {
    fn input_tokens(&self, context: &Context<'_>) -> u32 {
        crate::provider::estimate_request(context, |context| request_body(&self.config, context))
    }
    fn output_reserve(&self) -> u32 {
        self.config.max_output_tokens
    }
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
        if self.config.protocol == ApiProtocol::Anthropic
            && (body["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|m| m["role"] == "system"))
                || body["tools"].as_array().is_some_and(|tools| {
                    tools
                        .iter()
                        .any(|t| t["name"] == "__hikari_deferred_placeholder__")
                }))
        {
            let beta = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            request = request.set(
                "anthropic-beta",
                &if beta
                    .split(',')
                    .any(|value| value.trim() == "inline-tools-2026-09-15")
                {
                    beta.to_string()
                } else if beta.is_empty() {
                    "inline-tools-2026-09-15".into()
                } else {
                    format!("{beta},inline-tools-2026-09-15")
                },
            );
        }
        let policy = crate::transport::RetryPolicy {
            max_attempts: self.config.max_attempts.unwrap_or(11),
            ..Default::default()
        };
        let mut attempt = 1u32;
        let response = loop {
            match request.clone().send_json(body.clone()) {
                Ok(response) => break response,
                Err(error) if attempt < policy.max_attempts && crate::transport::is_retryable(&error) => {
                    let retry_after = match &error {
                        ureq::Error::Status(_, response) => crate::transport::parse_retry_after(response),
                        ureq::Error::Transport(_) => None,
                    };
                    let delay = crate::transport::retry_delay_ms(&policy, attempt, retry_after);
                    attempt += 1;
                    let mut remaining = delay;
                    while remaining > 0 {
                        if abort.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        let slice = remaining.min(100);
                        std::thread::sleep(Duration::from_millis(slice));
                        remaining -= slice;
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
        crate::transport::read_json_stream_with_idle(response, abort, Duration::from_secs(self.config.idle_timeout_seconds.unwrap_or(90)), |value| {
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

/// Project rich tool metadata into text fields understood by all three APIs.
/// The transcript's text is authoritative after artifact truncation.
pub(crate) fn tool_result_text(message: &Message) -> String {
    let Message::ToolResult {
        text,
        is_error,
        output,
        ..
    } = message
    else {
        return message.text().to_owned();
    };
    if let Some(output) = output {
        if output.structured.is_some()
            || output.failure.is_some()
            || output.truncated
            || output.retryable
            || *is_error
        {
            return json!({
                "text":text,
                "structured":output.structured,
                "failure":output.failure,
                "is_error":*is_error || output.failure.is_some(),
                "truncated":output.truncated,
                "retryable":output.retryable
            })
            .to_string();
        }
    } else if *is_error {
        return json!({"text":text,"is_error":true}).to_string();
    }
    text.to_owned()
}

pub fn request_body(config: &ProviderConfig, context: &Context<'_>) -> Value {
    let changes = catalog_changes(context);
    let latest_matches = context
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::ToolCatalog { tools } => Some(tools.as_slice() == context.tools),
            _ => None,
        })
        .unwrap_or(false);
    let native = latest_matches
        && match (config.protocol, config.compatibility.tool_disclosure) {
            (
                ApiProtocol::Responses,
                ToolDisclosure::ResponsesAdditionalTools | ToolDisclosure::ResponsesToolSearch,
            ) => changes.as_ref().is_some_and(|c| {
                c.iter().all(|(_, added, removed)| {
                    removed.is_empty() && added.iter().all(|t| context.tools.contains(t))
                })
            }),
            (ApiProtocol::Anthropic, ToolDisclosure::AnthropicInlineTools) => changes
                .as_ref()
                .is_some_and(|c| !c.is_empty() && !c[0].1.is_empty()),
            _ => false,
        };
    let initial = changes
        .as_ref()
        .and_then(|c| c.first())
        .map(|c| c.1.as_slice())
        .unwrap_or(context.tools);
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
            for (index, message) in context.messages.iter().enumerate() {
                match message {
                    Message::ToolCatalog { .. } => {
                        if native {
                            if let Some((_, added, _)) = changes.as_ref().unwrap().iter().skip(1).find(|c| c.0 == index) {
                                if added.is_empty() { continue; }
                                if config.compatibility.tool_disclosure == ToolDisclosure::ResponsesAdditionalTools {
                                    input.push(json!({"type":"additional_tools","role":"developer","tools":added.iter().map(|t| responses_tool(t, false)).collect::<Vec<_>>()}));
                                } else {
                                    let id = format!("hikari_tool_load_{index}");
                                    input.push(json!({"type":"tool_search_call","call_id":id,"execution":"client","status":"completed","arguments":{"query":added.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(" "),"limit":added.len()}}));
                                    input.push(json!({"type":"tool_search_output","call_id":id,"execution":"client","status":"completed","tools":added.iter().map(|t| responses_tool(t, true)).collect::<Vec<_>>()}));
                                }
                            }
                        }
                    },
                    Message::User { text, attachments, .. } => input.push(json!({"role":"user","content":if attachments.is_empty(){json!(text)}else{crate::attachment::content_parts(text, attachments, "responses")}})),
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
                    Message::ToolResult { tool_call_id, attachments, .. } => input.push(json!({
                        "type":"function_call_output","call_id":tool_call_id.split('|').next().unwrap_or(tool_call_id),"output":if attachments.is_empty(){json!(tool_result_text(message))}else{crate::attachment::content_parts(&tool_result_text(message), attachments,"responses")}
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
            let tools = if native { initial } else { context.tools };
            if !tools.is_empty() {
                body["tools"] = json!(tools
                    .iter()
                    .map(|t| responses_tool(t, false))
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
            let mut pending = Vec::new();
            for (index, message) in context.messages.iter().enumerate() {
                if matches!(message, Message::Assistant { .. }) {
                    messages.append(&mut pending);
                }
                let (role, content) = match message {
                    Message::ToolCatalog { .. } => {
                        if native {
                            if let Some((_, added, removed)) = changes
                                .as_ref()
                                .unwrap()
                                .iter()
                                .skip(1)
                                .find(|c| c.0 == index)
                            {
                                let mut blocks: Vec<_> = removed.iter().map(|t| json!({"type":"tool_removal","tool":{"type":"tool_reference","name":t.name}})).collect();
                                blocks.extend(added.iter().map(|t| json!({"type":"tool_addition","tool":{"type":"tool_definition","definition":anthropic_tool(t)}})));
                                if !blocks.is_empty() {
                                    pending.push(json!({"role":"system","content":blocks}));
                                }
                            }
                        }
                        continue;
                    }
                    Message::User { text, .. } | Message::ContextSummary { text } => (
                        "user",
                        crate::attachment::content_parts(text, message.attachments(), "anthropic")
                            .as_array()
                            .unwrap()
                            .clone(),
                    ),
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
                        is_error,
                        output,
                        ..
                    } => (
                        "user",
                        vec![json!({
                            "type":"tool_result","tool_use_id":tool_call_id,"content":if message.attachments().is_empty(){json!(tool_result_text(message))}else{crate::attachment::content_parts(&tool_result_text(message),message.attachments(),"anthropic")},"is_error":*is_error || output.as_ref().is_some_and(|value|value.failure.is_some())
                        })],
                    ),
                };
                if let Some(previous) = messages.last_mut().filter(|v| v["role"] == role) {
                    previous["content"].as_array_mut().unwrap().extend(content);
                } else {
                    messages.push(json!({"role":role,"content":content}));
                }
            }
            messages.append(&mut pending);
            // Keep cache breakpoints stable: one after the system prefix and one
            // on the last user block, independent of the number of tool rounds.
            let system = if config.compatibility.prompt_cache {
                json!([{"type":"text","text":context.system_prompt,"cache_control":{"type":"ephemeral"}}])
            } else {
                json!(context.system_prompt)
            };
            if config.compatibility.prompt_cache {
                for last_user in messages.iter_mut().rev().filter(|m| m["role"] == "user") {
                    if let Some(block) = last_user["content"].as_array_mut().and_then(|blocks| blocks.iter_mut().rev().find(|b| b["type"] == "text")) {
                        block["cache_control"] = json!({"type":"ephemeral"});
                        break;
                    }
                }
            }
            let mut body = json!({"model":config.model,"system":system,"messages":messages,
                "stream":true,"max_tokens":config.max_output_tokens});
            let tools = if native { initial } else { context.tools };
            if !tools.is_empty() {
                let mut definitions: Vec<_> = tools.iter().map(anthropic_tool).collect();
                if native {
                    definitions.push(json!({"name":"__hikari_deferred_placeholder__","description":"Reserved placeholder. Never call.","input_schema":{"type":"object","properties":{}},"defer_loading":true}));
                }
                body["tools"] = json!(definitions);
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

fn responses_tool(tool: &crate::provider::ToolDef, deferred: bool) -> Value {
    let mut definition = json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false});
    if deferred {
        definition["defer_loading"] = json!(true);
    }
    definition
}

fn anthropic_tool(tool: &crate::provider::ToolDef) -> Value {
    json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters})
}

fn catalog_changes(
    context: &Context<'_>,
) -> Option<
    Vec<(
        usize,
        Vec<crate::provider::ToolDef>,
        Vec<crate::provider::ToolDef>,
    )>,
> {
    let mut previous: Option<Vec<crate::provider::ToolDef>> = None;
    let mut changes = Vec::new();
    for (index, message) in context.messages.iter().enumerate() {
        let Message::ToolCatalog { tools } = message else {
            continue;
        };
        let old = previous.take().unwrap_or_default();
        let added = tools
            .iter()
            .filter(|tool| old.iter().find(|item| item.name == tool.name) != Some(*tool))
            .cloned()
            .collect();
        let removed = old
            .iter()
            .filter(|tool| !tools.iter().any(|item| item.name == tool.name))
            .cloned()
            .collect();
        changes.push((index, added, removed));
        previous = Some(tools.clone());
    }
    (!changes.is_empty()).then_some(changes)
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

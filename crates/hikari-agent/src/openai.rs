use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{AgentError, Result};
use crate::model::{Message, StopReason};
use crate::provider::{Context, Provider, StreamEvent, ToolDef};

/// OpenAI Chat Completions 兼容接口。地址、模型和密钥都由调用方给。
pub struct OpenAiProvider {
    endpoint: String,
    model: String,
    api_key: String,
    http: ureq::Agent,
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self> {
        let base_url = base_url.into();
        let model = model.into();
        if base_url.trim().is_empty() {
            return Err(AgentError::new("还没有模型地址。"));
        }
        if model.trim().is_empty() {
            return Err(AgentError::new("还没有模型名字。"));
        }
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(30))
            .build();
        Ok(Self {
            endpoint: chat_completions_url(&base_url),
            model: model.trim().to_string(),
            api_key: api_key.into().trim().to_string(),
            http,
        })
    }
}

impl Provider for OpenAiProvider {
    fn stream(
        &self,
        context: &Context<'_>,
        abort: &AtomicBool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<()> {
        if abort.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.api_key.is_empty() && !allows_empty_key(&self.endpoint) {
            return Err(AgentError::new("还没有密钥。在设置里填上再试。"));
        }

        let body = request_body(&self.model, context);
        let mut request = self
            .http
            .post(&self.endpoint)
            .set("Accept", "text/event-stream")
            .set("Content-Type", "application/json");
        if !self.api_key.is_empty() {
            request = request.set("Authorization", &format!("Bearer {}", self.api_key));
        }
        let response = request
            .send_json(body)
            .map_err(|err| map_ureq(err, &self.api_key))?;
        read_response(response, abort, on_event)
    }
}

pub fn chat_completions_url(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

pub(crate) fn allows_empty_key(endpoint: &str) -> bool {
    let authority = endpoint
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(""))
        .unwrap_or("");
    authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
}

pub(crate) fn request_body(model: &str, context: &Context<'_>) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": wire_messages(context.system_prompt, context.messages),
        "stream_options": { "include_usage": true },
    });
    if !context.tools.is_empty() {
        body["tools"] = Value::Array(context.tools.iter().map(wire_tool).collect());
    }
    body
}

fn wire_tool(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }
    })
}

fn wire_messages(system: &str, messages: &[Message]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for message in messages {
        match message {
            Message::User { text } => out.push(json!({"role": "user", "content": text})),
            Message::ContextSummary { text } => out.push(json!({"role":"system","content":text})),
            Message::Assistant {
                text, tool_calls, ..
            } => {
                let mut wire = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                });
                if !tool_calls.is_empty() {
                    wire["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                let arguments = if call.arguments.is_empty() {
                                    "{}"
                                } else {
                                    call.arguments.as_str()
                                };
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": { "name": call.name, "arguments": arguments },
                                })
                            })
                            .collect(),
                    );
                }
                out.push(wire);
            }
            Message::ToolResult {
                tool_call_id, text, ..
            } => out.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": text,
            })),
        }
    }
    out
}

fn read_response(
    response: ureq::Response,
    abort: &AtomicBool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<()> {
    let mut reader = BufReader::new(response.into_reader());
    let mut first = String::new();
    if reader
        .read_line(&mut first)
        .map_err(|err| AgentError::new(format!("读取中断了。{err}")))?
        == 0
    {
        return Ok(());
    }
    let trimmed = first.trim();
    if trimmed.starts_with('{') {
        let mut rest = String::new();
        reader
            .read_to_string(&mut rest)
            .map_err(|err| AgentError::new(format!("读取中断了。{err}")))?;
        let mut body = trimmed.to_string();
        body.push_str(&rest);
        return emit_all(&parse_data_line(&body)?, on_event);
    }
    emit_line(trimmed, abort, on_event)?;
    let lines = reader.lines();
    for line in lines {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        let line = line.map_err(|err| AgentError::new(format!("读取中断了。{err}")))?;
        if emit_line(line.trim(), abort, on_event)? {
            break;
        }
    }
    Ok(())
}

fn emit_all(events: &[StreamEvent], on_event: &mut dyn FnMut(StreamEvent)) -> Result<()> {
    for event in events {
        on_event(event.clone());
    }
    Ok(())
}

/// `true` 表示流结束。
fn emit_line(
    line: &str,
    abort: &AtomicBool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<bool> {
    if abort.load(Ordering::Relaxed) || line.is_empty() || line.starts_with(':') {
        return Ok(abort.load(Ordering::Relaxed));
    }
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(true);
    }
    emit_all(&parse_data_line(data)?, on_event)?;
    Ok(false)
}

pub fn parse_data_line(data: &str) -> Result<Vec<StreamEvent>> {
    let value: Value = serde_json::from_str(data)
        .map_err(|err| AgentError::new(format!("模型回了读不懂的内容。{err}")))?;
    if let Some(message) = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return Err(AgentError::new(message.to_string()));
    }

    let mut out = Vec::new();
    if let Some(usage) = value.get("usage") {
        let input = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input > 0 || output > 0 {
            out.push(StreamEvent::Usage { input, output });
        }
    }

    let Some(choice) = value.pointer("/choices/0") else {
        return Ok(out);
    };
    if let Some(delta) = choice.get("delta") {
        push_content(delta, &mut out);
    }
    if let Some(message) = choice.get("message") {
        push_content(message, &mut out);
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        out.push(StreamEvent::Done(parse_stop(reason)));
    }
    Ok(out)
}

fn push_content(node: &Value, out: &mut Vec<StreamEvent>) {
    if let Some(text) = node.get("content").and_then(json_text) {
        if !text.is_empty() {
            out.push(StreamEvent::Text(text));
        }
    }
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = node.get(key).and_then(json_text) {
            if !text.is_empty() {
                out.push(StreamEvent::Thinking(text));
            }
        }
    }
    if let Some(calls) = node.get("tool_calls").and_then(Value::as_array) {
        for (fallback, call) in calls.iter().enumerate() {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize)
                .unwrap_or(fallback);
            out.push(StreamEvent::ToolDelta {
                index,
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string),
                name: call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string),
                arguments: arguments_of(call),
            });
        }
    }
}

fn arguments_of(call: &Value) -> String {
    match call.pointer("/function/arguments") {
        Some(Value::String(text)) => text.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                } else if let Some(text) = part.as_str() {
                    out.push_str(text);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn parse_stop(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::Length,
        "tool_calls" | "tool_use" => StopReason::ToolUse,
        "content_filter" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn map_ureq(err: ureq::Error, key: &str) -> AgentError {
    let redact = |mut text: String| {
        if !key.is_empty() {
            text = text.replace(key, "•••");
        }
        if text.chars().count() > 400 {
            text = text.chars().take(400).collect();
        }
        text
    };
    match err {
        ureq::Error::Status(code, response) => {
            let body = redact(response.into_string().unwrap_or_default());
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(body);
            AgentError::new(format!("模型没有接上（{code}）。{message}"))
        }
        ureq::Error::Transport(err) => {
            AgentError::new(format!("模型没有接上。{}", redact(err.to_string())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_chat_completions_once() {
        assert_eq!(
            chat_completions_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://example.test/v1/chat/completions"),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn parses_text_tool_call_and_usage() {
        let text =
            parse_data_line(r#"{"choices":[{"delta":{"content":"你好"},"finish_reason":null}]}"#)
                .unwrap();
        assert_eq!(text, vec![StreamEvent::Text("你好".into())]);

        let tool = parse_data_line(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"command\":\"dir\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#,
        )
        .unwrap();
        assert!(tool.iter().any(|event| matches!(
            event,
            StreamEvent::ToolDelta { name: Some(name), arguments, .. }
                if name == "shell" && arguments.contains("dir")
        )));
        assert!(tool.contains(&StreamEvent::Done(StopReason::ToolUse)));
        assert!(tool.contains(&StreamEvent::Usage {
            input: 5,
            output: 3
        }));
    }

    #[test]
    fn local_endpoint_may_omit_the_key() {
        assert!(allows_empty_key(
            "http://127.0.0.1:11434/v1/chat/completions"
        ));
        assert!(!allows_empty_key(
            "https://api.openai.com/v1/chat/completions"
        ));
    }
}

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::pi::sse::SseParser;
use crate::{AgentError, Result};

/// Shared framing for model protocols and Streamable HTTP MCP.
pub fn read_json_stream(
    response: ureq::Response,
    abort: &AtomicBool,
    mut emit: impl FnMut(serde_json::Value) -> Result<bool>,
) -> Result<()> {
    let content_type = response.header("Content-Type").unwrap_or("").to_string();
    let mut reader = BufReader::new(response.into_reader());
    if !content_type.contains("text/event-stream") {
        let mut body = String::new();
        reader
            .by_ref()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut body)
            .map_err(|e| AgentError::new(format!("读取响应失败：{e}")))?;
        emit(
            serde_json::from_str(&body)
                .map_err(|e| AgentError::new(format!("响应不是 JSON：{e}")))?,
        )?;
        return Ok(());
    }
    let mut parser = SseParser::new();
    let mut line = String::new();
    loop {
        if abort.load(Ordering::Relaxed) {
            return Ok(());
        }
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| AgentError::new(format!("流读取中断：{e}")))?;
        let events = if n == 0 {
            parser.flush().into_iter().collect()
        } else {
            parser.feed(&line)
        };
        for event in events {
            if event.data == "[DONE]" {
                return Ok(());
            }
            if event.event == "error" && !event.data.starts_with('{') {
                return Err(AgentError::new(event.data));
            }
            if emit(
                serde_json::from_str(&event.data)
                    .map_err(|e| AgentError::new(format!("流事件不是 JSON：{e}")))?,
            )? {
                return Ok(());
            }
        }
        if n == 0 {
            return Ok(());
        }
    }
}

pub(crate) fn http_error(error: ureq::Error, secrets: &[&str]) -> AgentError {
    let mut text = match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or(body);
            format!("请求失败（{code}）：{message}")
        }
        ureq::Error::Transport(e) => format!("连接失败：{e}"),
    };
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        text = text.replace(secret, "•••");
    }
    AgentError::new(text.chars().take(500).collect::<String>())
}

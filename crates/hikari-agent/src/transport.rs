use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pi::sse::SseParser;
use crate::{AgentError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_ms: u64,
    pub max_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 11, base_ms: 500, max_ms: 32_000 }
    }
}

pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32, retry_after_s: Option<u64>) -> u64 {
    if let Some(seconds) = retry_after_s {
        return seconds.saturating_mul(1_000);
    }
    let shift = attempt.saturating_sub(1).min(10);
    let base = policy.base_ms.saturating_mul(1u64 << shift).min(policy.max_ms);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos() as u64);
    let jitter = seed % (base / 4 + 1);
    base.saturating_add(jitter)
}

pub fn is_retryable(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Status(code, _) => matches!(*code, 408 | 409 | 429 | 500..=599),
        ureq::Error::Transport(error) => {
            let text = format!("{error:?}").to_ascii_lowercase();
            ["timeout", "timedout", "wouldblock", "reset", "broken pipe", "closed", "eof"]
                .iter()
                .any(|needle| text.contains(needle))
        }
    }
}

pub fn parse_retry_after(response: &ureq::Response) -> Option<u64> {
    response.header("retry-after").and_then(|value| value.trim().parse().ok())
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
}

/// Shared framing for model protocols and Streamable HTTP MCP.
pub fn read_json_stream(
    response: ureq::Response,
    abort: &AtomicBool,
    emit: impl FnMut(serde_json::Value) -> Result<bool>,
) -> Result<()> {
    read_json_stream_with_idle(response, abort, Duration::from_secs(90), emit)
}

pub fn read_json_stream_with_idle(
    response: ureq::Response,
    abort: &AtomicBool,
    idle_timeout: Duration,
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
    let mut last_data = std::time::Instant::now();
    loop {
        if abort.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(error) if is_timeout(&error) => {
                if abort.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if last_data.elapsed() >= idle_timeout {
                    return Err(AgentError::new("流空闲超时，未收到新数据。"));
                }
                continue;
            }
            Err(error) => return Err(AgentError::new(format!("流读取中断：{error}"))),
        };
        if n > 0 {
            last_data = std::time::Instant::now();
        }
        let mut events = Vec::new();
        while let Some(newline) = line.find('\n') {
            let complete: String = line.drain(..=newline).collect();
            events.extend(parser.feed(&complete));
        }
        if n == 0 {
            if !line.is_empty() {
                events.extend(parser.feed(&line));
                line.clear();
            }
            events.extend(parser.flush());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_exponential_capped_and_jittered() {
        let policy = RetryPolicy::default();
        let first = retry_delay_ms(&policy, 1, None);
        let second = retry_delay_ms(&policy, 2, None);
        assert!(first >= 500 && first <= 625);
        assert!(second >= 1_000 && second <= 1_250);
        assert!(retry_delay_ms(&policy, 20, None) <= 40_000);
    }

    #[test]
    fn retry_after_overrides_local_backoff() {
        assert_eq!(retry_delay_ms(&RetryPolicy::default(), 1, Some(7)), 7_000);
    }
}

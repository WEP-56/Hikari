// Ported from pi_agent_rust b6fa9e18b, src/sse.rs. See licenses/pi_agent_rust.LICENSE.
//! Server-Sent Events (SSE) parser for asupersync HTTP client.
//!
//! Implements the SSE protocol (text/event-stream) on top of asupersync's
//! HTTP client for streaming LLM responses.

use std::borrow::Cow;

const MAX_EVENT_DATA_BYTES: usize = 100 * 1024 * 1024;
const MAX_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// Local parser failures must not masquerade as an upstream `event: error`.
/// The byte-stream adapter turns these into terminal I/O errors; the direct
/// parser API emits one diagnostic event and then refuses further input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SseParseError {
    EventDataLimit,
    BufferLimit,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_crlf_multiline_and_bom() {
        let mut parser = SseParser::new();
        assert!(parser
            .feed("\u{feff}event: content_block_delta\r")
            .is_empty());
        assert!(parser.feed("\ndata: {\"a\":\r\ndata: 1}\r\n").is_empty());
        let events = parser.feed("\r\n");
        assert_eq!(events[0].event, "content_block_delta");
        assert_eq!(events[0].data, "{\"a\":\n1}");
    }
    #[test]
    fn oversized_frame_does_not_emit_truncated_payload() {
        let mut parser = SseParser::with_max_event_data_bytes(3);
        let events = parser.feed("data: too large\n\n");
        assert_eq!(events[0].event, "error");
        assert!(parser.feed("data: ok\n\n").is_empty());
        assert!(parser.flush().is_none());
    }
}

impl SseParseError {
    const fn message(self) -> &'static str {
        match self {
            Self::EventDataLimit => "SSE event data limit exceeded",
            Self::BufferLimit => "SSE buffer limit exceeded",
        }
    }

    fn into_event(self) -> SseEvent {
        SseEvent {
            event: Cow::Borrowed("error"),
            data: self.message().to_string(),
            ..SseEvent::default()
        }
    }
}

/// A parsed SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type (from "event:" field, defaults to "message").
    pub event: Cow<'static, str>,
    /// Event data (from "data:" field(s), joined with newlines).
    pub data: String,
    /// Last event ID (from "id:" field).
    pub id: Option<String>,
    /// Whether this event itself contained an `id:` field. The SSE protocol
    /// carries the last ID forward to later events, so consumers that use IDs
    /// as replay checkpoints must distinguish an explicit checkpoint from an
    /// inherited value.
    pub id_was_explicit: bool,
    /// Retry interval hint in milliseconds (from "retry:" field).
    pub retry: Option<u64>,
}

impl Default for SseEvent {
    fn default() -> Self {
        Self {
            event: Cow::Borrowed("message"),
            data: String::new(),
            id: None,
            id_was_explicit: false,
            retry: None,
        }
    }
}

/// Parser state for SSE stream.
#[derive(Debug)]
pub struct SseParser {
    buffer: String,
    current: SseEvent,
    has_data: bool,
    /// Whether we've already stripped the BOM from the first feed.
    bom_checked: bool,
    /// Number of bytes in `buffer` that have already been scanned for newlines.
    scanned_len: usize,
    /// Per-event data accumulation cap in bytes.
    max_event_data_bytes: usize,
    /// A local limit failure poisons the parser until a new one is created.
    failed: bool,
}

impl Default for SseParser {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            current: SseEvent::default(),
            has_data: false,
            bom_checked: false,
            scanned_len: 0,
            max_event_data_bytes: MAX_EVENT_DATA_BYTES,
            failed: false,
        }
    }
}

impl SseParser {
    /// Create a new SSE parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a parser with a custom per-event data cap (for testing).
    #[cfg(test)]
    fn with_max_event_data_bytes(limit: usize) -> Self {
        Self {
            max_event_data_bytes: limit,
            ..Self::default()
        }
    }

    /// Intern common SSE event type names to avoid per-event String allocation.
    /// LLM streaming APIs use a fixed set of event types; matching them to
    /// `Cow::Borrowed` static strings eliminates one allocation per event.
    #[inline]
    fn intern_event_type(value: &str) -> Cow<'static, str> {
        match value {
            // Anthropic streaming events
            "message" => Cow::Borrowed("message"),
            "message_start" => Cow::Borrowed("message_start"),
            "message_stop" => Cow::Borrowed("message_stop"),
            "message_delta" => Cow::Borrowed("message_delta"),
            "content_block_start" => Cow::Borrowed("content_block_start"),
            "content_block_delta" => Cow::Borrowed("content_block_delta"),
            "content_block_stop" => Cow::Borrowed("content_block_stop"),
            // OpenAI Responses API streaming events
            "response.completed" => Cow::Borrowed("response.completed"),
            "response.done" => Cow::Borrowed("response.done"),
            "response.failed" => Cow::Borrowed("response.failed"),
            "response.incomplete" => Cow::Borrowed("response.incomplete"),
            "response.output_text.delta" => Cow::Borrowed("response.output_text.delta"),
            "response.output_text.done" => Cow::Borrowed("response.output_text.done"),
            "response.output_item.added" => Cow::Borrowed("response.output_item.added"),
            "response.output_item.done" => Cow::Borrowed("response.output_item.done"),
            "response.content_part.done" => Cow::Borrowed("response.content_part.done"),
            "response.function_call_arguments.delta" => {
                Cow::Borrowed("response.function_call_arguments.delta")
            }
            "response.reasoning_text.delta" => Cow::Borrowed("response.reasoning_text.delta"),
            "response.reasoning_text.done" => Cow::Borrowed("response.reasoning_text.done"),
            "response.reasoning_summary_text.delta" => {
                Cow::Borrowed("response.reasoning_summary_text.delta")
            }
            "response.reasoning_summary_text.done" => {
                Cow::Borrowed("response.reasoning_summary_text.done")
            }
            "response.reasoning_summary_part.done" => {
                Cow::Borrowed("response.reasoning_summary_part.done")
            }
            "response.created" => Cow::Borrowed("response.created"),
            // Common shared events
            "ping" => Cow::Borrowed("ping"),
            "error" => Cow::Borrowed("error"),
            _ => Cow::Owned(value.to_string()),
        }
    }

    #[inline]
    fn append_data_line(
        current: &mut SseEvent,
        value: &str,
        has_data: &mut bool,
        max_event_data_bytes: usize,
    ) -> Result<(), SseParseError> {
        let projected_len = current
            .data
            .len()
            .saturating_add(value.len())
            .saturating_add(1);
        if projected_len > max_event_data_bytes {
            // Dropping only this line can turn tool arguments into different
            // valid JSON or a truncated response into a completion event.
            // Reject the entire frame and all later input instead.
            return Err(SseParseError::EventDataLimit);
        }
        current.data.push_str(value);
        current.data.push('\n');
        *has_data = true;
        Ok(())
    }

    #[inline]
    fn parse_retry(value: &str) -> Option<u64> {
        if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            value.parse().ok()
        } else {
            None
        }
    }

    /// Process a single line of SSE data.
    fn process_line(
        line: &str,
        current: &mut SseEvent,
        has_data: &mut bool,
        max_event_data_bytes: usize,
    ) -> Result<(), SseParseError> {
        // Complete metadata lines must not bypass the incomplete-line cap.
        // Check before allocating an event name or a replay ID, which is also
        // carried forward to subsequent events. Data has its own event budget.
        if line.len() > MAX_BUFFER_SIZE && (line.starts_with("id:") || line.starts_with("event:")) {
            return Err(SseParseError::BufferLimit);
        }
        if let Some(rest) = line.strip_prefix(':') {
            // Comment line - ignore (but could be used for keep-alive)
            let _ = rest;
        } else if let Some((field, value)) = line.split_once(':') {
            // Field: value
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => current.event = Self::intern_event_type(value),
                "data" => Self::append_data_line(current, value, has_data, max_event_data_bytes)?,
                "id" if !value.contains('\0') => {
                    current.id = Some(value.to_string());
                    current.id_was_explicit = true;
                }
                "retry" => current.retry = Self::parse_retry(value),
                _ => {} // Unknown field - ignore
            }
        } else {
            // Field with no value
            match line {
                "event" => current.event = Cow::Borrowed(""),
                "data" => Self::append_data_line(current, "", has_data, max_event_data_bytes)?,
                "id" => {
                    current.id = Some(String::new());
                    current.id_was_explicit = true;
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[inline]
    fn reset_current_for_next_event(current: &mut SseEvent) {
        current.event = Cow::Borrowed("message");
        current.data.clear();
        current.id_was_explicit = false;
    }

    #[inline]
    fn carry_forward_event_state(current: &SseEvent) -> SseEvent {
        SseEvent {
            id: current.id.clone(),
            retry: current.retry,
            ..Default::default()
        }
    }

    #[inline]
    fn discard_after_failure(&mut self) {
        self.buffer = String::new();
        self.current = SseEvent::default();
        self.has_data = false;
        self.scanned_len = 0;
        self.failed = true;
    }

    /// Process complete lines from `source`, dispatching events via `emit`.
    /// Returns the byte offset of the first unconsumed byte.
    #[inline]
    fn process_source<F>(
        source: &str,
        scan_start: usize,
        bom_checked: &mut bool,
        current: &mut SseEvent,
        has_data: &mut bool,
        max_event_data_bytes: usize,
        emit: &mut F,
    ) -> Result<usize, SseParseError>
    where
        F: FnMut(SseEvent),
    {
        let bytes = source.as_bytes();
        let mut start = 0usize;
        let mut search_pos = scan_start;

        // Strip UTF-8 BOM from the beginning of the stream (SSE spec compliance).
        if !*bom_checked && !source.is_empty() {
            *bom_checked = true;
            if source.starts_with('\u{FEFF}') {
                start = 3;
                if search_pos < 3 {
                    search_pos = 3;
                }
            }
        }

        // Use memchr2 to find either \r or \n
        while let Some(rel_pos) = memchr::memchr2(b'\r', b'\n', &bytes[search_pos..]) {
            let pos = search_pos + rel_pos;
            let b = bytes[pos];

            let line_end;
            let next_start;

            if b == b'\n' {
                // Bare LF
                line_end = pos;
                next_start = pos + 1;
            } else {
                // Found \r
                if pos + 1 < source.len() {
                    line_end = pos;
                    next_start = if bytes[pos + 1] == b'\n' {
                        // CRLF
                        pos + 2
                    } else {
                        // Bare CR
                        pos + 1
                    };
                } else {
                    // CR at end of buffer - wait for more data to check for \n
                    break;
                }
            }

            let line = &source[start..line_end];
            start = next_start;
            search_pos = next_start;

            if line.is_empty() {
                // Blank line = event boundary
                if *has_data {
                    // Trim trailing newline from data
                    if current.data.ends_with('\n') {
                        current.data.pop();
                    }
                    // Per SSE spec, an empty event name dispatches as "message".
                    if current.event.is_empty() {
                        current.event = Cow::Borrowed("message");
                    }
                    let next_event = Self::carry_forward_event_state(current);
                    emit(std::mem::take(current));
                    *current = next_event;
                    *has_data = false;
                } else {
                    Self::reset_current_for_next_event(current);
                }
            } else {
                Self::process_line(line, current, has_data, max_event_data_bytes)?;
            }
        }

        Ok(start)
    }

    /// Feed data to the parser and emit any complete events to `emit`.
    fn feed_into<F>(&mut self, data: &str, emit: F) -> Result<(), SseParseError>
    where
        F: FnMut(SseEvent),
    {
        if self.failed {
            return Ok(());
        }
        let result = self.feed_validated(data, emit);
        if result.is_err() {
            self.discard_after_failure();
        }
        result
    }

    fn feed_validated<F>(&mut self, data: &str, mut emit: F) -> Result<(), SseParseError>
    where
        F: FnMut(SseEvent),
    {
        if self.buffer.is_empty() {
            // Fast path: process data directly without copying to buffer.
            let consumed = Self::process_source(
                data,
                0,
                &mut self.bom_checked,
                &mut self.current,
                &mut self.has_data,
                self.max_event_data_bytes,
                &mut emit,
            )?;
            if consumed < data.len() {
                if data.len() - consumed > MAX_BUFFER_SIZE {
                    return Err(SseParseError::BufferLimit);
                }
                self.buffer.push_str(&data[consumed..]);
            }
        } else {
            // Slow path: parse against a temporary combined source so we only
            // retain the truly unconsumed tail instead of a giant drained buffer.
            let mut combined = std::mem::take(&mut self.buffer);
            combined.push_str(data);
            // Re-scan from the last safe point (minus 1 to handle split CRLF).
            let scan_start = self.scanned_len.saturating_sub(1);
            let consumed = Self::process_source(
                &combined,
                scan_start,
                &mut self.bom_checked,
                &mut self.current,
                &mut self.has_data,
                self.max_event_data_bytes,
                &mut emit,
            )?;
            if consumed < combined.len() {
                if combined.len() - consumed > MAX_BUFFER_SIZE {
                    return Err(SseParseError::BufferLimit);
                }
                // Build buffer fresh from truly unprocessed tail only (no duplication).
                self.buffer = combined[consumed..].to_string();
            }
        }
        // Whether we drained or not, the entire remaining buffer has been scanned.
        self.scanned_len = self.buffer.len();
        Ok(())
    }

    /// Feed data to the parser and extract any complete events.
    ///
    /// Returns a vector of parsed events. Events are delimited by blank lines.
    /// A local size-limit failure emits one `error` event after any complete
    /// prefix events. The offending event is never emitted, and subsequent
    /// `feed`/`flush` calls are empty; create a new parser for a new stream.
    pub fn feed(&mut self, data: &str) -> Vec<SseEvent> {
        let mut events = Vec::with_capacity(4);
        if let Err(error) = self.feed_into(data, |event| events.push(event)) {
            events.push(error.into_event());
        }
        events
    }

    /// Check if the parser has any pending data.
    pub const fn has_pending(&self) -> bool {
        !self.buffer.is_empty() || self.has_data
    }

    /// Flush any pending event (called when stream ends).
    /// A local limit failure emits one diagnostic, never a truncated event.
    pub fn flush(&mut self) -> Option<SseEvent> {
        self.try_flush()
            .unwrap_or_else(|error| Some(error.into_event()))
    }

    fn try_flush(&mut self) -> Result<Option<SseEvent>, SseParseError> {
        if self.failed {
            return Ok(None);
        }
        // First, process any remaining buffer content that doesn't end with newline
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.scanned_len = 0;
            let line = line.trim_end_matches('\r');
            if let Err(error) = Self::process_line(
                line,
                &mut self.current,
                &mut self.has_data,
                self.max_event_data_bytes,
            ) {
                self.discard_after_failure();
                return Err(error);
            }
        }

        if self.has_data {
            if self.current.data.ends_with('\n') {
                self.current.data.pop();
            }
            if self.current.event.is_empty() {
                self.current.event = Cow::Borrowed("message");
            }
            let event = std::mem::take(&mut self.current);
            self.current = SseEvent::default();
            self.has_data = false;
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }
}

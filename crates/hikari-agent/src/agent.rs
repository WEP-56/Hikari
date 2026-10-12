use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use crate::memory::{ConsolidateState, Memory, RecallItem, RetainState};
use crate::model::{Message, ToolCall, Usage};
use crate::provider::{Context, Provider, StreamEvent};

pub const DEFAULT_CONTEXT_TOKENS: u32 = 128_000;
const COMPACT_KEEP: usize = 8;
pub const RECALL_TITLE: &str = "Recalled memory";

pub struct TurnHost {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<crate::tool::ToolRegistry>,
    pub memory: Arc<dyn Memory>,
    pub abort: Arc<AtomicBool>,
}

pub struct TurnRequest {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub retained_until: usize,
    pub query: String,
    pub system_prompt: String,
    pub context_limit_tokens: u32,
    pub max_tool_rounds: usize,
}

/// Pi's steering/follow-up boundary: steering after tools, follow-up after a reply.
#[derive(Default)]
pub struct TurnControl {
    queues: Mutex<(VecDeque<String>, VecDeque<String>)>,
}
impl TurnControl {
    pub fn steer(&self, text: impl Into<String>) {
        self.queues.lock().unwrap().0.push_back(text.into());
    }
    pub fn follow_up(&self, text: impl Into<String>) {
        self.queues.lock().unwrap().1.push_back(text.into());
    }
    pub fn pending(&self) -> usize {
        let queues = self.queues.lock().unwrap();
        queues.0.len() + queues.1.len()
    }
    fn take_steering(&self) -> Vec<String> {
        self.queues.lock().unwrap().0.drain(..).collect()
    }
    fn take_follow_up(&self) -> Option<String> {
        self.queues.lock().unwrap().1.pop_front()
    }
    pub fn drain_pending(&self) -> Vec<String> {
        let mut queues = self.queues.lock().unwrap();
        let mut pending: Vec<_> = queues.0.drain(..).collect();
        pending.extend(queues.1.drain(..));
        pending
    }
}

/// 前端只看这些事件。记忆三条都是主机发的，不是模型输出。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    InputDelivered {
        text: String,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        text: String,
        thinking: String,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: String,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        text: String,
        is_error: bool,
    },
    TurnEnd {
        message: Message,
        tool_results: Vec<Message>,
    },
    AgentEnd {
        messages: Vec<Message>,
        error: Option<String>,
        usage: Usage,
        retained_until: usize,
    },
    AutoCompactionStart {
        reason: String,
    },
    AutoCompactionEnd {
        error: Option<String>,
    },
    MemoryRecall {
        query: String,
        items: Vec<RecallItem>,
    },
    MemoryRetain {
        document_id: String,
        state: RetainState,
        kept: u32,
        dropped: u32,
        error: Option<String>,
    },
    MemoryConsolidation {
        state: ConsolidateState,
        created: u32,
        updated: u32,
        error: Option<String>,
    },
}

pub fn companion_prompt(name: &str, thinking: &str) -> String {
    let name = {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            "光"
        } else {
            trimmed
        }
    };
    let effort = match thinking {
        "关闭" => "不要展开思考过程，直接回答。",
        "低" => "先想一句，再回答。回复仍然短。",
        "中" => "想清楚再答，但不要写成清单，除非对方要。",
        "高" | "最高" => {
            "遇到要办事的请求，先把步骤想清楚，再用工具去做。回复给对方的话仍然短。"
        }
        _ => "想清楚再答。回复短一点。",
    };
    format!(
        "你是 Hikari，一个待在身边的个人助手，名字是{name}。先陪着，再办事。说话短，像在旁边，不催。需要看本机或跑命令时用工具，不要假装已经做过。{effort}"
    )
}

/// 跑完用户这一轮：召回只改这一次的系统提示，工具结果写回 session，结束后才 retain。
pub fn run_turn(host: &TurnHost, request: TurnRequest, on_event: impl FnMut(AgentEvent)) {
    run_turn_with_control(host, request, &TurnControl::default(), on_event);
}

pub fn run_turn_with_control(
    host: &TurnHost,
    mut request: TurnRequest,
    control: &TurnControl,
    mut on_event: impl FnMut(AgentEvent),
) {
    request.max_tool_rounds =
        crate::pi::iterations::clamp_max_tool_iterations(Some(request.max_tool_rounds));
    if request.retained_until > request.messages.len() {
        request.retained_until = request.messages.len();
    }
    let mut usage = Usage::default();
    let mut tool_results = Vec::new();
    let mut last_assistant = Message::assistant("");
    let mut error = None;
    let mut rounds = 0usize;
    let mut recovery = crate::pi::turn_recovery::TurnRecoveryState::new(
        crate::pi::turn_recovery::TurnRecoveryMode::Conservative,
    );
    let mut pause_continuations = 0;
    let mut handoff_warned = false;

    loop {
        if host.abort.load(Ordering::Relaxed) {
            error = Some("已停下。".to_string());
            if !request.messages.iter().any(is_assistant) {
                push_assistant(&mut request, &mut last_assistant, "已停下。", &mut on_event);
            }
            break;
        }

        for text in control.take_steering() {
            on_event(AgentEvent::InputDelivered { text: text.clone() });
            request.query = text.clone();
            request.messages.push(Message::user(text));
        }

        // 窗口满之前先把还没提交的原文交出去，再丢掉旧消息。摘要不会写进 session。
        if let Err(message) = maybe_compact(host, &mut request, &mut on_event) {
            error = Some(message);
            break;
        }

        let recalled = if host.memory.enabled() {
            host.memory.recall(&request.query)
        } else {
            Vec::new()
        };
        if host.memory.enabled() {
            on_event(AgentEvent::MemoryRecall {
                query: request.query.clone(),
                items: recalled.clone(),
            });
        }
        // 召回只活在这一次请求里。下一轮重新召回，不进 messages。
        let system = with_recall(&request.system_prompt, &recalled);
        let context = Context {
            system_prompt: &system,
            messages: &request.messages,
            tools: host.tools.defs(),
        };

        let mut draft = AssistantDraft::default();
        let mut started = false;
        let stream_result = host.provider.stream(&context, &host.abort, &mut |event| {
            match event {
                StreamEvent::Text(delta) => draft.text.push_str(&delta),
                StreamEvent::Thinking(delta) => draft.thinking.push_str(&delta),
                StreamEvent::ToolDelta {
                    index,
                    id,
                    name,
                    arguments,
                } => draft.push_tool(index, id, name, arguments),
                StreamEvent::Usage { input, output } => {
                    draft.usage = Usage { input, output };
                }
                StreamEvent::Done(reason) => draft.stop = reason,
                StreamEvent::Metadata(metadata) => draft.metadata = Some(metadata),
            }
            if !draft.text.is_empty() || !draft.thinking.is_empty() {
                if !started {
                    started = true;
                    on_event(AgentEvent::MessageStart {
                        message: draft.message(),
                    });
                }
                on_event(AgentEvent::MessageUpdate {
                    text: draft.text.clone(),
                    thinking: draft.thinking.clone(),
                });
            }
        });

        if let Err(err) = stream_result {
            let notice = format!("没有连上。{err}");
            if draft.text.is_empty() {
                draft.text = notice.clone();
            } else {
                draft.text.push_str("\n\n");
                draft.text.push_str(&notice);
            }
            error = Some(err.message);
        } else if host.abort.load(Ordering::Relaxed)
            && draft.text.is_empty()
            && draft.calls.is_empty()
        {
            draft.text = "已停下。".to_string();
            error = Some("已停下。".to_string());
        }

        // prompt_tokens 是这一次请求的全文，不是增量。输出才累加。
        if draft.usage.input > usage.input {
            usage.input = draft.usage.input;
        }
        usage.output = usage.output.saturating_add(draft.usage.output);
        let message = draft.message();
        if !started {
            on_event(AgentEvent::MessageStart {
                message: message.clone(),
            });
        }
        on_event(AgentEvent::MessageEnd {
            message: message.clone(),
        });
        last_assistant = message.clone();
        request.messages.push(message.clone());

        let calls = match &message {
            Message::Assistant { tool_calls, .. } => tool_calls.clone(),
            _ => Vec::new(),
        };
        if error.is_some()
            || host.abort.load(Ordering::Relaxed)
            || draft.stop == crate::model::StopReason::Error
        {
            if draft.stop == crate::model::StopReason::Error && error.is_none() {
                error = Some("模型未能完成本轮请求".into());
            }
            for call in &calls {
                request.messages.push(Message::tool_result(
                    &call.id,
                    &call.name,
                    "调用未执行：本轮已中断",
                    true,
                ));
            }
            if host.abort.load(Ordering::Relaxed) {
                error = Some("已停下。".to_string());
            }
            break;
        }
        // Pi resubmits pause_turn verbatim; these are server tools, not local calls.
        if draft.stop == crate::model::StopReason::PauseTurn {
            if pause_continuations < 3 {
                pause_continuations += 1;
                continue;
            }
            error = Some("模型暂停的续接次数已达上限".into());
            break;
        }
        if calls.is_empty() {
            if let Some(action) = recovery.evaluate(draft.stop, &draft.text) {
                request.messages.push(Message::user(action.nudge_text));
                continue;
            }
            if let Some(text) = control.take_follow_up() {
                on_event(AgentEvent::TurnEnd {
                    message: last_assistant.clone(),
                    tool_results: std::mem::take(&mut tool_results),
                });
                retain_new(host, &mut request, &mut on_event);
                on_event(AgentEvent::InputDelivered { text: text.clone() });
                request.query = text.clone();
                request.messages.push(Message::user(text));
                recovery = crate::pi::turn_recovery::TurnRecoveryState::new(
                    crate::pi::turn_recovery::TurnRecoveryMode::Conservative,
                );
                rounds = 0;
                handoff_warned = false;
                continue;
            }
            break;
        }
        if rounds >= request.max_tool_rounds {
            for call in &calls {
                request.messages.push(Message::tool_result(
                    &call.id,
                    &call.name,
                    "工具轮数已达上限，调用未执行",
                    true,
                ));
            }
            push_assistant(
                &mut request,
                &mut last_assistant,
                "工具一轮接一轮，先停在这里。",
                &mut on_event,
            );
            break;
        }
        rounds += 1;
        for call in calls {
            if host.abort.load(Ordering::Relaxed) {
                error = Some("已停下。".to_string());
                request.messages.push(Message::tool_result(
                    &call.id,
                    &call.name,
                    "调用未执行：用户已取消",
                    true,
                ));
                continue;
            }
            let arguments = if call.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                call.arguments.clone()
            };
            on_event(AgentEvent::ToolExecutionStart {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                args: arguments.clone(),
            });
            let (text, is_error) =
                match host
                    .tools
                    .call_with_abort(&call.name, &arguments, &host.abort)
                {
                    Ok(text) => (text, false),
                    Err(err) => (err, true),
                };
            let result = Message::tool_result(&call.id, &call.name, text.clone(), is_error);
            on_event(AgentEvent::ToolExecutionEnd {
                tool_call_id: call.id,
                tool_name: call.name,
                text,
                is_error,
            });
            tool_results.push(result.clone());
            request.messages.push(result);
        }
        if !handoff_warned
            && crate::pi::iterations::should_warn_at_iteration_threshold(
                rounds,
                request.max_tool_rounds,
            )
        {
            handoff_warned = true;
            request.messages.push(Message::user(
                crate::pi::iterations::iteration_handoff_steering_text(
                    rounds,
                    request.max_tool_rounds,
                ),
            ));
        }
    }

    on_event(AgentEvent::TurnEnd {
        message: last_assistant,
        tool_results,
    });
    retain_new(host, &mut request, &mut on_event);
    on_event(AgentEvent::AgentEnd {
        messages: request.messages,
        error,
        usage,
        retained_until: request.retained_until,
    });
}

fn push_assistant(
    request: &mut TurnRequest,
    last: &mut Message,
    text: &str,
    on_event: &mut impl FnMut(AgentEvent),
) {
    let message = Message::assistant(text);
    on_event(AgentEvent::MessageStart {
        message: message.clone(),
    });
    on_event(AgentEvent::MessageUpdate {
        text: text.to_string(),
        thinking: String::new(),
    });
    on_event(AgentEvent::MessageEnd {
        message: message.clone(),
    });
    request.messages.push(message.clone());
    *last = message;
}

fn is_assistant(message: &Message) -> bool {
    matches!(message, Message::Assistant { .. })
}

/// 压缩失败时返回错误，调用方不再丢原文，也不再请求模型。
fn maybe_compact(
    host: &TurnHost,
    request: &mut TurnRequest,
    on_event: &mut impl FnMut(AgentEvent),
) -> std::result::Result<(), String> {
    if !over_budget(&request.messages, request.context_limit_tokens) {
        return Ok(());
    }
    let end = request.messages.len();
    if host.memory.enabled() && request.retained_until < end {
        let document_id = document_id(&request.session_id, request.retained_until, end);
        on_event(AgentEvent::MemoryRetain {
            document_id: document_id.clone(),
            state: RetainState::Queued,
            kept: 0,
            dropped: 0,
            error: None,
        });
        match host
            .memory
            .retain(&document_id, &request.messages[request.retained_until..end])
        {
            Ok(receipt) => {
                request.retained_until = end;
                on_event(AgentEvent::MemoryRetain {
                    document_id,
                    state: RetainState::Done,
                    kept: receipt.kept,
                    dropped: receipt.dropped,
                    error: None,
                });
            }
            Err(err) => {
                on_event(AgentEvent::MemoryRetain {
                    document_id,
                    state: RetainState::Failed,
                    kept: 0,
                    dropped: 0,
                    error: Some(err.message.clone()),
                });
                // 提交失败就停。先压缩会把还没记住的原文丢掉。
                return Err(format!("压缩前没能记下原文。{}", err.message));
            }
        }
    }

    let cut = compaction_cut(&request.messages, COMPACT_KEEP);
    if cut == 0 {
        return Ok(());
    }
    on_event(AgentEvent::AutoCompactionStart {
        reason: "窗口快满了".to_string(),
    });
    // Pi checkpoint compaction: summarize first, then replace the old window.
    // The summary is a derived context message and never an input to retain.
    let transcript = serde_json::to_string(&request.messages[..cut]).map_err(|e| e.to_string())?;
    let previous = request.messages[..cut]
        .iter()
        .filter_map(|m| match m {
            Message::ContextSummary { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = if previous.is_empty() {
        crate::pi::compaction::SUMMARIZATION_PROMPT
    } else {
        crate::pi::compaction::UPDATE_SUMMARIZATION_PROMPT
    };
    let summary_request = [Message::user(format!(
        "{prompt}\n<previous-summary>{previous}</previous-summary>\nConversation:\n{transcript}"
    ))];
    let context = Context {
        system_prompt: crate::pi::compaction::SUMMARIZATION_SYSTEM_PROMPT,
        messages: &summary_request,
        tools: &[],
    };
    let mut summary = String::new();
    let result = host.provider.stream(&context, &host.abort, &mut |event| {
        if let StreamEvent::Text(text) = event {
            summary.push_str(&text);
        }
    });
    if let Err(err) = result {
        on_event(AgentEvent::AutoCompactionEnd {
            error: Some(err.message.clone()),
        });
        return Err(err.message);
    }
    if summary.trim().is_empty() || host.abort.load(Ordering::Relaxed) {
        let error = "上下文摘要未完成，原文已保留".to_string();
        on_event(AgentEvent::AutoCompactionEnd {
            error: Some(error.clone()),
        });
        return Err(error);
    }
    request.messages.drain(..cut);
    request
        .messages
        .insert(0, Message::ContextSummary { text: summary });
    request.retained_until = request.retained_until.saturating_sub(cut) + 1;
    on_event(AgentEvent::AutoCompactionEnd { error: None });
    Ok(())
}

fn retain_new(host: &TurnHost, request: &mut TurnRequest, on_event: &mut impl FnMut(AgentEvent)) {
    if !host.memory.enabled() {
        return;
    }
    let end = request.messages.len();
    if request.retained_until >= end {
        return;
    }
    let document_id = document_id(&request.session_id, request.retained_until, end);
    on_event(AgentEvent::MemoryRetain {
        document_id: document_id.clone(),
        state: RetainState::Queued,
        kept: 0,
        dropped: 0,
        error: None,
    });
    match host
        .memory
        .retain(&document_id, &request.messages[request.retained_until..end])
    {
        Ok(receipt) => {
            request.retained_until = end;
            on_event(AgentEvent::MemoryRetain {
                document_id,
                state: RetainState::Done,
                kept: receipt.kept,
                dropped: receipt.dropped,
                error: None,
            });
        }
        Err(err) => {
            on_event(AgentEvent::MemoryRetain {
                document_id,
                state: RetainState::Failed,
                kept: 0,
                dropped: 0,
                error: Some(err.message),
            });
            return;
        }
    }

    on_event(AgentEvent::MemoryConsolidation {
        state: ConsolidateState::Running,
        created: 0,
        updated: 0,
        error: None,
    });
    match host.memory.consolidate_pending() {
        Ok(receipt) => on_event(AgentEvent::MemoryConsolidation {
            state: ConsolidateState::Done,
            created: receipt.created,
            updated: receipt.updated,
            error: None,
        }),
        Err(err) => on_event(AgentEvent::MemoryConsolidation {
            state: ConsolidateState::Failed,
            created: 0,
            updated: 0,
            error: Some(err.message),
        }),
    }
}

fn document_id(session_id: &str, start: usize, end: usize) -> String {
    format!("{session_id}@{start}-{end}")
}

fn with_recall(system: &str, items: &[RecallItem]) -> String {
    if items.is_empty() {
        return system.to_string();
    }
    let mut out = String::with_capacity(system.len() + 32);
    out.push_str(system);
    out.push_str("\n\n");
    out.push_str(RECALL_TITLE);
    for item in items {
        out.push_str("\n- (");
        out.push_str(&item.fact_type);
        out.push_str(", proof ");
        out.push_str(&item.proof_count.to_string());
        out.push_str(") ");
        out.push_str(&item.text);
    }
    out
}

fn over_budget(messages: &[Message], limit: u32) -> bool {
    let trigger = (limit.saturating_mul(4) / 5).max(1);
    estimate_tokens(messages) >= trigger
}

fn estimate_tokens(messages: &[Message]) -> u32 {
    // CJK characters commonly cost one or more tokens; chars/4 severely undercounts Chinese.
    let bytes: usize = messages
        .iter()
        .map(|message| serde_json::to_vec(message).map(|v| v.len()).unwrap_or(0))
        .sum();
    u32::try_from(bytes.div_ceil(3)).unwrap_or(u32::MAX)
}

/// 只在用户消息的边界切开，避免把工具调用和它的结果拆开。
fn compaction_cut(messages: &[Message], keep: usize) -> usize {
    if messages.len() <= keep {
        return 0;
    }
    let mut cut = messages.len() - keep;
    while cut < messages.len() && !matches!(messages[cut], Message::User { .. }) {
        cut += 1;
    }
    if cut == 0 || cut >= messages.len() {
        0
    } else {
        cut
    }
}

#[derive(Default)]
struct AssistantDraft {
    text: String,
    thinking: String,
    calls: std::collections::BTreeMap<usize, ToolAccum>,
    usage: Usage,
    stop: crate::model::StopReason,
    metadata: Option<crate::model::ProviderMetadata>,
}

struct ToolAccum {
    id: String,
    name: String,
    arguments: String,
}

impl AssistantDraft {
    fn push_tool(
        &mut self,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    ) {
        let slot = self.calls.entry(index).or_insert_with(|| ToolAccum {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
        if let Some(id) = id {
            if !id.is_empty() {
                slot.id = id;
            }
        }
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            slot.name = name;
        }
        slot.arguments.push_str(&arguments);
    }

    fn message(&self) -> Message {
        let tool_calls = self
            .calls
            .iter()
            .filter(|(_, call)| !call.name.is_empty())
            .map(|(index, call)| ToolCall {
                id: if call.id.is_empty() {
                    format!("call-{index}")
                } else {
                    call.id.clone()
                },
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();
        let _ = self.stop;
        Message::Assistant {
            text: self.text.clone(),
            thinking: self.thinking.clone(),
            tool_calls,
            provider_metadata: self.metadata.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AgentError, Result};
    use crate::memory::{ConsolidateReceipt, NullMemory, RetainReceipt};
    use crate::model::StopReason;
    use crate::tool::{Tool, ToolRegistry};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct Scripted {
        rounds: Mutex<VecDeque<Round>>,
        systems: Mutex<Vec<String>>,
        seen: Mutex<Vec<Vec<Message>>>,
    }

    enum Round {
        Text(String),
        Tools(Vec<ToolCall>),
        Fail(String),
    }

    impl Provider for Scripted {
        fn stream(
            &self,
            context: &Context<'_>,
            abort: &AtomicBool,
            on_event: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            self.systems
                .lock()
                .unwrap()
                .push(context.system_prompt.to_string());
            self.seen.lock().unwrap().push(context.messages.to_vec());
            if abort.load(Ordering::Relaxed) {
                return Ok(());
            }
            let round = self
                .rounds
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Round::Text("没有更多了".into()));
            match round {
                Round::Text(text) => {
                    on_event(StreamEvent::Text(text));
                    on_event(StreamEvent::Usage {
                        input: 3,
                        output: 2,
                    });
                    on_event(StreamEvent::Done(StopReason::Stop));
                }
                Round::Tools(calls) => {
                    for (index, call) in calls.into_iter().enumerate() {
                        on_event(StreamEvent::ToolDelta {
                            index,
                            id: Some(call.id),
                            name: Some(call.name),
                            arguments: call.arguments,
                        });
                    }
                    on_event(StreamEvent::Done(StopReason::ToolUse));
                }
                Round::Fail(message) => return Err(AgentError::new(message)),
            }
            Ok(())
        }
    }

    struct Echo;

    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn call(&self, arguments: &str) -> std::result::Result<String, String> {
            Ok(format!("echo:{arguments}"))
        }
    }

    struct RecMem {
        enabled: bool,
        fail: bool,
        recall: Vec<RecallItem>,
        retained: Mutex<Vec<Vec<Message>>>,
    }

    impl Memory for RecMem {
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn recall(&self, _query: &str) -> Vec<RecallItem> {
            self.recall.clone()
        }
        fn retain(&self, _document_id: &str, messages: &[Message]) -> Result<RetainReceipt> {
            if self.fail {
                return Err(AgentError::new("记不住"));
            }
            self.retained.lock().unwrap().push(messages.to_vec());
            Ok(RetainReceipt {
                kept: messages.len() as u32,
                dropped: 0,
            })
        }
        fn consolidate_pending(&self) -> Result<ConsolidateReceipt> {
            Ok(ConsolidateReceipt {
                created: 1,
                updated: 0,
            })
        }
    }

    fn host(provider: Arc<Scripted>, memory: Arc<dyn Memory>, tools: ToolRegistry) -> TurnHost {
        TurnHost {
            provider,
            tools: Arc::new(tools),
            memory,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    fn turn_request(messages: Vec<Message>) -> TurnRequest {
        TurnRequest {
            session_id: "talk-1".into(),
            messages,
            retained_until: 0,
            query: "今天呢".into(),
            system_prompt: "你是 Hikari".into(),
            context_limit_tokens: DEFAULT_CONTEXT_TOKENS,
            max_tool_rounds: 4,
        }
    }

    #[test]
    fn recall_is_not_stored_as_a_message() {
        let provider = Arc::new(Scripted {
            rounds: Mutex::new(VecDeque::from([Round::Text("好".into())])),
            systems: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let memory = Arc::new(RecMem {
            enabled: true,
            fail: false,
            recall: vec![RecallItem {
                id: "1".into(),
                fact_type: "observation".into(),
                text: "喜欢短回复".into(),
                proof_count: 2,
            }],
            retained: Mutex::new(Vec::new()),
        });
        let mut events = Vec::new();
        run_turn(
            &host(provider.clone(), memory.clone(), ToolRegistry::empty()),
            turn_request(vec![Message::user("今天呢")]),
            |event| events.push(event),
        );

        let system = &provider.systems.lock().unwrap()[0];
        assert!(system.contains(RECALL_TITLE));
        assert!(system.contains("喜欢短回复"));
        let AgentEvent::AgentEnd {
            messages, error, ..
        } = events.last().unwrap()
        else {
            panic!("missing end");
        };
        assert!(error.is_none());
        assert!(messages
            .iter()
            .all(|message| message.text() != "喜欢短回复"));
        assert!(messages
            .iter()
            .all(|message| !message.text().contains(RECALL_TITLE)));
        let retained = memory.retained.lock().unwrap();
        assert_eq!(retained.len(), 1);
        assert!(retained[0]
            .iter()
            .all(|message| !message.text().contains("喜欢短回复")));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::MemoryRecall { items, .. } if items.len() == 1
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::MemoryConsolidation {
                state: ConsolidateState::Done,
                created: 1,
                ..
            }
        )));
    }

    #[test]
    fn tool_round_writes_the_result_back_into_the_session() {
        let provider = Arc::new(Scripted {
            rounds: Mutex::new(VecDeque::from([
                Round::Tools(vec![ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: "{\"text\":\"hi\"}".into(),
                }]),
                Round::Text("做完了".into()),
            ])),
            systems: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let memory = Arc::new(NullMemory);
        let mut events = Vec::new();
        run_turn(
            &host(
                provider.clone(),
                memory,
                ToolRegistry::new(vec![Arc::new(Echo)]),
            ),
            turn_request(vec![Message::user("跑一下")]),
            |event| events.push(event),
        );

        let seen = provider.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1]
            .iter()
            .any(|message| message.text().starts_with("echo:")));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
                .count(),
            1
        );
        let AgentEvent::AgentEnd { messages, .. } = events.last().unwrap() else {
            panic!("missing end");
        };
        assert_eq!(messages.len(), 4);
        assert!(messages
            .iter()
            .any(|message| matches!(message, Message::ToolResult { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolExecutionEnd {
                is_error: false,
                ..
            }
        )));
        assert!(provider
            .systems
            .lock()
            .unwrap()
            .iter()
            .all(|system| !system.contains("echo:")));
    }

    #[test]
    fn compaction_retains_before_dropping_and_stops_when_retain_fails() {
        let long = "字".repeat(400);
        let mut messages = Vec::new();
        messages.push(Message::user(long.clone()));
        messages.push(Message::assistant("改过了"));
        messages.push(Message::tool_result("c1", "echo", "ok", false));
        for index in 0..9 {
            messages.push(Message::user(format!("{long}-{index}")));
        }
        assert!(messages.len() > COMPACT_KEEP);

        let provider = Arc::new(Scripted {
            rounds: Mutex::new(VecDeque::from([Round::Text("仍在".into())])),
            systems: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let memory = Arc::new(RecMem {
            enabled: true,
            fail: false,
            recall: Vec::new(),
            retained: Mutex::new(Vec::new()),
        });
        let mut request = turn_request(messages);
        request.context_limit_tokens = 50;
        let mut events = Vec::new();
        run_turn(
            &host(provider, memory.clone(), ToolRegistry::empty()),
            request,
            |event| events.push(event),
        );
        let retained = memory.retained.lock().unwrap();
        assert!(retained[0].len() > COMPACT_KEEP);
        let AgentEvent::AgentEnd {
            messages, error, ..
        } = events.last().unwrap()
        else {
            panic!("missing end");
        };
        assert!(error.is_none());
        assert!(messages.len() <= COMPACT_KEEP + 2);
        assert!(matches!(
            messages.first(),
            Some(Message::ContextSummary { .. })
        ));
        assert!(matches!(messages.get(1), Some(Message::User { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::AutoCompactionStart { .. })));

        let provider = Arc::new(Scripted {
            rounds: Mutex::new(VecDeque::from([Round::Text("不该看到".into())])),
            systems: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let failing = Arc::new(RecMem {
            enabled: true,
            fail: true,
            recall: Vec::new(),
            retained: Mutex::new(Vec::new()),
        });
        let mut original = Vec::new();
        original.push(Message::user(long.clone()));
        original.push(Message::assistant("改过了"));
        original.push(Message::tool_result("c1", "echo", "ok", false));
        for index in 0..9 {
            original.push(Message::user(format!("{long}-{index}")));
        }
        let original_len = original.len();
        let mut request = turn_request(original);
        request.context_limit_tokens = 50;
        let mut events = Vec::new();
        run_turn(
            &host(provider.clone(), failing, ToolRegistry::empty()),
            request,
            |event| events.push(event),
        );
        assert!(provider.seen.lock().unwrap().is_empty());
        let AgentEvent::AgentEnd {
            messages, error, ..
        } = events.last().unwrap()
        else {
            panic!("missing end");
        };
        assert!(error.is_some());
        assert_eq!(messages.len(), original_len);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::MemoryRetain {
                state: RetainState::Failed,
                ..
            }
        )));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::AutoCompactionStart { .. })));
    }

    #[test]
    fn prompt_uses_the_name() {
        let prompt = companion_prompt(" 光 ", "关闭");
        assert!(prompt.contains("名字是光"));
        assert!(prompt.contains("不要展开思考过程"));
    }

    #[test]
    fn provider_failure_stays_in_the_transcript() {
        let provider = Arc::new(Scripted {
            rounds: Mutex::new(VecDeque::from([Round::Fail("401".into())])),
            systems: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let mut events = Vec::new();
        run_turn(
            &host(provider, Arc::new(NullMemory), ToolRegistry::empty()),
            turn_request(vec![Message::user("在吗")]),
            |event| events.push(event),
        );
        let AgentEvent::AgentEnd {
            messages, error, ..
        } = events.last().unwrap()
        else {
            panic!("missing end");
        };
        assert!(error.as_deref().unwrap().contains("401"));
        assert!(messages.last().unwrap().text().contains("没有连上"));
    }
}

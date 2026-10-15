use std::sync::atomic::AtomicBool;

use crate::error::Result;
use crate::model::{Message, StopReason};

/// 一次补全要看的东西。系统提示可以带上这一轮的召回，但那份文本不属于 `messages`。
pub struct Context<'a> {
    pub system_prompt: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Provider 吐出的流。记忆事件不从这里来。
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        input: u64,
        output: u64,
    },
    Done(StopReason),
    Metadata(crate::model::ProviderMetadata),
}

/// 只负责把上下文变成流。
pub trait Provider: Send + Sync {
    /// Conservative fallback; protocol providers estimate their actual request body.
    fn input_tokens(&self, context: &Context<'_>) -> u32 {
        let bytes = context.system_prompt.len()
            + serde_json::to_vec(context.messages).map_or(0, |v| v.len())
            + serde_json::to_vec(context.tools).map_or(0, |v| v.len());
        u32::try_from(bytes.div_ceil(3))
            .unwrap_or(u32::MAX)
            .saturating_add(image_reserve(context.messages))
    }
    fn output_reserve(&self) -> u32 {
        0
    }
    fn stream(
        &self,
        context: &Context<'_>,
        abort: &AtomicBool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<()>;
}

// Image billing depends on the provider/model and dimensions, not base64 length.
// Reserve 32k per image until a provider exposes its own image tokenizer. This is
// deliberately conservative for common vision models; it is still an estimate.
fn image_reserve(messages: &[Message]) -> u32 {
    let count = messages
        .iter()
        .flat_map(Message::attachments)
        .filter(|a| a.is_image())
        .count();
    u32::try_from(count)
        .unwrap_or(u32::MAX)
        .saturating_mul(32_000)
}

pub(crate) fn estimate_request(
    context: &Context<'_>,
    build: impl FnOnce(&Context<'_>) -> serde_json::Value,
) -> u32 {
    let mut messages = context.messages.to_vec();
    for message in &mut messages {
        message.set_attachments(Vec::new());
    }
    let text_context = Context {
        system_prompt: context.system_prompt,
        messages: &messages,
        tools: context.tools,
    };
    u32::try_from(build(&text_context).to_string().len().div_ceil(3))
        .unwrap_or(u32::MAX)
        .saturating_add(image_reserve(context.messages))
}

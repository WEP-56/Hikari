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
        u32::try_from(bytes.div_ceil(3)).unwrap_or(u32::MAX)
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

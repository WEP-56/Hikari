//! Hikari 的小 agent loop。
//!
//! 边界见 `docs/pi-memory-plan.md`：Provider 只把上下文变成流，记忆不进 Provider。
//! 召回只附在这一次请求的系统提示上，不写入 session。

pub mod agent;
pub mod error;
pub mod memory;
pub mod model;
pub mod openai;
pub mod provider;
pub mod tool;

pub use agent::{
    companion_prompt, run_turn, AgentEvent, TurnHost, TurnRequest, DEFAULT_CONTEXT_TOKENS,
};
pub use error::{AgentError, Result};
pub use memory::{
    ConsolidateReceipt, ConsolidateState, Memory, NullMemory, RecallItem, RetainReceipt,
    RetainState,
};
pub use model::{Message, StopReason, ToolCall, Usage};
pub use openai::OpenAiProvider;
pub use provider::{Context, Provider, StreamEvent, ToolDef};
pub use tool::ToolRegistry;

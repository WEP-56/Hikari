//! Hikari 的小 agent loop。
//!
//! 边界见 `docs/pi-memory-plan.md`：Provider 只把上下文变成流，记忆不进 Provider。
//! 召回只附在这一次请求的系统提示上，不写入 session。

pub mod agent;
pub mod attachment;
pub mod delegate;
pub use attachment::{Attachment, AttachmentStore, ToolFailure, ToolOutput};
pub mod error;
pub mod execution;
pub mod memory;
pub mod model;
pub mod openai;
pub mod permission;
pub mod pi;
mod process_job;
pub mod protocols;
pub mod provider;
pub mod tool;
pub mod tool_search;
pub mod transport;
mod fs_lock;

pub use agent::{
    companion_prompt, run_turn, run_turn_with_control, AgentEvent, TurnControl, TurnHost,
    TurnRequest, DEFAULT_CONTEXT_TOKENS,
};
pub use error::{AgentError, ProviderErrorKind, Result, TurnOutcome};
pub use memory::{
    ConsolidateReceipt, ConsolidateState, Memory, MemoryJobEvent, NullMemory, RecallItem,
    RetainReceipt, RetainState,
};
pub use model::{Message, ProviderMetadata, StopReason, ToolCall, Usage};
pub use openai::OpenAiProvider;
pub use protocols::{ApiProtocol, ProtocolProvider, ProviderConfig};
pub use provider::{Context, Provider, StreamEvent, ToolDef};
pub use tool::ToolRegistry;

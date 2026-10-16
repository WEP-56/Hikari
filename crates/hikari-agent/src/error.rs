use std::fmt;
use serde::{Deserialize, Serialize};

/// Machine-readable outcome of a model turn.  The UI may render these values,
/// while callers can branch on the stable variant instead of matching prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    Completed,
    Stopped,
    ToolRoundLimit { used: usize, cap: usize },
    PauseTurnLimit { used: usize },
    TokenBudgetExhausted { reason: String },
    ContextOverBudget,
    CompactionFailed { reason: String },
    Provider { kind: ProviderErrorKind, message: String },
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Retryable,
    RateLimited,
    ContextOverflow,
    Auth,
    StreamBroken,
    IdleTimeout,
    Unknown,
}

impl TurnOutcome {
    pub fn user_text(&self) -> String {
        match self {
            Self::Completed => String::new(),
            Self::Stopped => "已停下。".into(),
            Self::ToolRoundLimit { used, cap } => format!("工具轮数已达上限（{used}/{cap}），调用未执行。"),
            Self::PauseTurnLimit { used } => format!("模型暂停的续接次数已达上限（{used}）。"),
            Self::TokenBudgetExhausted { reason } => if reason.is_empty() { "任务 token 预算已用完。".into() } else { format!("任务 token 预算已用完：{reason}") },
            Self::ContextOverBudget => "完整请求超过上下文预算，请缩短输入、角色卡或工具定义，或提高上下文限制。".into(),
            Self::CompactionFailed { reason } => format!("压缩失败：{reason}"),
            Self::Provider { message, .. } => message.clone(),
            Self::Panic => "任务异常终止。".into(),
        }
    }
}

/// 给界面看的失败原因。不带密钥，也不带堆栈。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub message: String,
}

impl AgentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {}

pub type Result<T> = std::result::Result<T, AgentError>;

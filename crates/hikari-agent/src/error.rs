use std::fmt;

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

use serde::{Deserialize, Serialize};

/// 会话原文。retain 的输入就是这个，不是模型当轮看到的系统提示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        text: String,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        thinking: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        text: String,
        is_error: bool,
    },
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User { text: text.into() }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant {
            text: text.into(),
            thinking: String::new(),
            tool_calls: Vec::new(),
        }
    }

    pub fn tool_result(
        id: impl Into<String>,
        name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_call_id: id.into(),
            name: name.into(),
            text: text.into(),
            is_error,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Self::User { text } | Self::Assistant { text, .. } | Self::ToolResult { text, .. } => {
                text
            }
        }
    }

    pub fn chars(&self) -> usize {
        match self {
            Self::User { text } => text.chars().count(),
            Self::Assistant {
                text,
                thinking,
                tool_calls,
            } => {
                text.chars().count()
                    + thinking.chars().count()
                    + tool_calls
                        .iter()
                        .map(|call| call.name.chars().count() + call.arguments.chars().count())
                        .sum::<usize>()
            }
            Self::ToolResult { text, name, .. } => text.chars().count() + name.chars().count(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 模型给出的 JSON 文本。原样送回，避免再次序列化把转义弄乱。
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[default]
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
    }

    pub fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

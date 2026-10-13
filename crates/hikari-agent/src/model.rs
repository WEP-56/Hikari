use serde::{Deserialize, Serialize};

/// 会话原文。retain 的输入就是这个，不是模型当轮看到的系统提示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<MessageOrigin>,
    },
    Assistant {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<MessageOrigin>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        thinking: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        text: String,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<MessageOrigin>,
    },
    ContextSummary {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageOrigin {
    pub id: String,
    pub mentioned_at: String,
    #[serde(default)]
    pub recalled: bool,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            text: text.into(),
            origin: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant {
            text: text.into(),
            origin: None,
            thinking: String::new(),
            tool_calls: Vec::new(),
            provider_metadata: None,
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
            origin: None,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Self::User { text, .. }
            | Self::Assistant { text, .. }
            | Self::ToolResult { text, .. } => text,
            Self::ContextSummary { text } => text,
        }
    }

    pub fn chars(&self) -> usize {
        match self {
            Self::User { text, .. } => text.chars().count(),
            Self::Assistant {
                text,
                thinking,
                tool_calls,
                ..
            } => {
                text.chars().count()
                    + thinking.chars().count()
                    + tool_calls
                        .iter()
                        .map(|call| call.name.chars().count() + call.arguments.chars().count())
                        .sum::<usize>()
            }
            Self::ToolResult { text, name, .. } => text.chars().count() + name.chars().count(),
            Self::ContextSummary { text } => text.chars().count(),
        }
    }
    pub fn origin(&self) -> Option<&MessageOrigin> {
        match self {
            Self::User { origin, .. }
            | Self::Assistant { origin, .. }
            | Self::ToolResult { origin, .. } => origin.as_ref(),
            _ => None,
        }
    }
    pub fn stamp(&mut self, id: String, recalled: bool) {
        match self {
            Self::User { origin, .. }
            | Self::Assistant { origin, .. }
            | Self::ToolResult { origin, .. } => {
                if origin.is_none() {
                    *origin = Some(MessageOrigin {
                        id,
                        mentioned_at: chrono::Utc::now().to_rfc3339(),
                        recalled,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Protocol-native content (including signed reasoning) must survive tool rounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub protocol: String,
    pub items: Vec<serde_json::Value>,
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
    PauseTurn,
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

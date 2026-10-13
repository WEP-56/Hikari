use crate::error::Result;
use crate::model::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 回复前注入的一条记忆。没有向量分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallItem {
    pub id: String,
    pub fact_type: String,
    pub text: String,
    pub proof_count: u32,
    #[serde(default)]
    pub mentioned_at: String,
    #[serde(default)]
    pub occurred_start: Option<String>,
    #[serde(default)]
    pub occurred_end: Option<String>,
    #[serde(default)]
    pub source_memory_ids: Vec<String>,
    #[serde(default)]
    pub is_stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainState {
    Queued,
    Persisted,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidateState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainReceipt {
    pub kept: u32,
    pub dropped: u32,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryJobEvent {
    pub session_id: String,
    pub job_id: String,
    pub document_id: String,
    pub kind: String,
    pub state: String,
    pub kept: u32,
    pub created: u32,
    pub updated: u32,
    pub deleted: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsolidateReceipt {
    pub created: u32,
    pub updated: u32,
}

/// 记忆核心。loop 只认这三个方法。
///
/// `retain` 在原文提交成功后就返回，抽取可以还在后面跑。
/// `consolidate_pending` 不该挡住下一句回复；慢的实现自己把活丢到后台再返回。
pub trait Memory: Send + Sync {
    fn enabled(&self) -> bool;

    fn recall(&self, query: &str) -> Vec<RecallItem>;

    fn recall_checked(&self, query: &str) -> Result<Vec<RecallItem>> {
        Ok(self.recall(query))
    }

    fn policy(&self) -> &str {
        ""
    }

    fn retain(&self, document_id: &str, messages: &[Message]) -> Result<RetainReceipt>;

    fn consolidate_pending(&self) -> Result<ConsolidateReceipt>;
}

/// 还没接上 Memory Core 时的空实现。不开事件，不写任何东西。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullMemory;

impl Memory for NullMemory {
    fn enabled(&self) -> bool {
        false
    }

    fn recall(&self, _query: &str) -> Vec<RecallItem> {
        Vec::new()
    }

    fn retain(&self, _document_id: &str, _messages: &[Message]) -> Result<RetainReceipt> {
        Ok(RetainReceipt {
            kept: 0,
            dropped: 0,
            pending: false,
        })
    }

    fn consolidate_pending(&self) -> Result<ConsolidateReceipt> {
        Ok(ConsolidateReceipt {
            created: 0,
            updated: 0,
        })
    }
}

/// 同一段原文的指纹。重复提交时 Memory Core 用它挡住。
pub fn content_hash(messages: &[Message]) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(messages).expect("serializable messages"))
    )
}

pub fn is_memory_tool(name: &str) -> bool {
    matches!(name, "search_observations" | "recall" | "expand" | "forget")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_when_the_text_changes() {
        let left = content_hash(&[Message::user("同一句")]);
        let right = content_hash(&[Message::user("另一句")]);
        assert_ne!(left, right);
        assert_eq!(left, content_hash(&[Message::user("同一句")]));
    }
}

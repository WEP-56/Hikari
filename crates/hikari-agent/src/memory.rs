use crate::error::Result;
use crate::model::Message;

/// 回复前注入的一条记忆。没有向量分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallItem {
    pub id: String,
    pub fact_type: String,
    pub text: String,
    pub proof_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainState {
    Queued,
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
    let bytes = serde_json::to_vec(messages).unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
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

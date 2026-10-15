use crate::config::{read_json_or_default, write_json};
use hikari_agent::{Message, Result, Usage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;

/// UI transcript survives context compaction; context is the active provider window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub pinned: bool,
    pub transcript: Vec<Message>,
    pub context: Vec<Message>,
    pub retained_until: usize,
    #[serde(default)]
    pub context_base: usize,
    #[serde(default = "default_memory")]
    pub memory_enabled: bool,
    pub usage: Usage,
    #[serde(default)]
    pub execution: hikari_agent::execution::ExecutionSnapshot,
}
fn default_memory() -> bool {
    true
}
#[derive(Clone)]
pub struct SessionStore {
    path: PathBuf,
}
impl SessionStore {
    /// Only flat content-addressed host resources are removed. Unknown files and links
    /// survive cleanup, and a nonempty session directory is deliberately retained.
    pub fn remove_artifacts(&self, id: u64) -> Result<()> {
        self.remove_resource_directory(&format!("talk-{id}"))
    }
    /// Draft attachment references live only in memory and cannot survive app restart.
    /// Call once at startup, before accepting fresh draft attachments.
    pub fn remove_draft_artifacts(&self) -> Result<()> {
        self.remove_resource_directory("draft")
    }
    fn remove_resource_directory(&self, directory: &str) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| hikari_agent::AgentError::new("会话数据目录无效"))?;
        let root = parent.join("artifacts");
        let target = root.join(directory);
        if !target.exists() {
            return Ok(());
        }
        let data = parent
            .canonicalize()
            .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
        let expected = data.join("artifacts").join(directory);
        if is_link(&std::fs::symlink_metadata(&root).map_err(resource_error)?)
            || is_link(&std::fs::symlink_metadata(&target).map_err(resource_error)?)
            || root
                .canonicalize()
                .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?
                != data.join("artifacts")
            || target
                .canonicalize()
                .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?
                != expected
        {
            return Err(hikari_agent::AgentError::new(
                "artifact 目录包含链接，未自动清理",
            ));
        }
        for entry in
            std::fs::read_dir(&target).map_err(|e| hikari_agent::AgentError::new(e.to_string()))?
        {
            let entry = entry.map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(resource_error)?;
            if !metadata.is_file() || is_link(&metadata) {
                continue;
            }
            let Some((hash, ext)) = name.rsplit_once('.') else {
                continue;
            };
            if hash.len() != 64
                || !hash.bytes().all(|c| c.is_ascii_hexdigit())
                || !matches!(ext, "txt" | "png" | "jpg" | "webp" | "gif" | "bin")
            {
                continue;
            }
            let resolved = entry.path().canonicalize().map_err(resource_error)?;
            if resolved.parent() != Some(expected.as_path()) {
                continue;
            }
            let mut file = std::fs::File::open(entry.path()).map_err(resource_error)?;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = file.read(&mut buffer).map_err(resource_error)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            if format!("{:x}", digest.finalize()).eq_ignore_ascii_case(hash) {
                std::fs::remove_file(entry.path())
                    .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
            }
        }
        if std::fs::read_dir(&target)
            .map_err(resource_error)?
            .next()
            .is_none()
        {
            std::fs::remove_dir(target).map_err(resource_error)?;
        }
        Ok(())
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            path: root.into().join("sessions.json"),
        }
    }
    pub fn load(&self) -> Result<Vec<Session>> {
        let records: Vec<serde_json::Value> = read_json_or_default(&self.path)?;
        records
            .into_iter()
            .map(|record| {
                let legacy = record.get("memory_enabled").is_none();
                let mut session: Session = serde_json::from_value(record)
                    .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
                session.execution.recover();
                // Complete interrupted tool pairs without executing them again. An effect may
                // have succeeded between the last checkpoint and the process crash.
                let interrupted: Vec<_> = session.context.iter().flat_map(|m|match m {
                    Message::Assistant { tool_calls, .. } => tool_calls.as_slice(),
                    _ => &[],
                }).filter(|call|!session.context.iter().any(|m|matches!(m,Message::ToolResult { tool_call_id, .. } if tool_call_id==&call.id))).cloned().collect();
                for call in interrupted {
                    let mut result = Message::tool_result(&call.id,&call.name,"应用重启，调用结果未确认。可能已经产生副作用；未自动重跑，请先检查实际状态。",true);
                    result.stamp(format!("talk-{}:interrupted:{}",session.id,call.id),true);
                    session.context.push(result.clone());
                    if !session.transcript.iter().any(|m|matches!(m,Message::ToolResult { tool_call_id, .. } if tool_call_id==&call.id)) { session.transcript.push(result); }
                }
                if legacy {
                    // Enable future turns without silently ingesting old conversation history.
                    session.retained_until = session.context.len();
                    session.context_base = session
                        .transcript
                        .len()
                        .saturating_sub(session.context.len());
                }
                Ok(session)
            })
            .collect()
    }
    pub fn save(&self, sessions: &[Session]) -> Result<()> {
        let mut sessions = sessions.to_vec();
        for session in &mut sessions {
            sync_transcript_origins(&mut session.transcript, &session.context);
        }
        write_json(&self.path, &sessions)
    }
}

fn resource_error(error: std::io::Error) -> hikari_agent::AgentError {
    hikari_agent::AgentError::new(error.to_string())
}
fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub fn sync_transcript_origins(transcript: &mut [Message], context: &[Message]) {
    // UI events build transcript separately. Copy host provenance before persistence,
    // including follow-up input delivered while the agent is running.
    for message in context.iter().rev() {
        let Some(origin) = message.origin() else {
            continue;
        };
        if transcript
            .iter()
            .any(|m| m.origin().is_some_and(|o| o.id == origin.id))
        {
            continue;
        }
        if let Some(target) = transcript.iter_mut().rev().find(|m| {
            m.origin().is_none()
                && match (&**m, message) {
                    (Message::User { text: a, .. }, Message::User { text: b, .. }) => a == b,
                    (
                        Message::ToolResult {
                            tool_call_id: a, ..
                        },
                        Message::ToolResult {
                            tool_call_id: b, ..
                        },
                    ) => a == b,
                    _ => false,
                }
        }) {
            match target {
                Message::User { origin: slot, .. } | Message::ToolResult { origin: slot, .. } => {
                    *slot = Some(origin.clone())
                }
                _ => {}
            }
        }
    }
}

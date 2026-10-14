use crate::config::{read_json_or_default, write_json};
use hikari_agent::{Message, Result, Usage};
use serde::{Deserialize, Serialize};
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
}
fn default_memory() -> bool {
    true
}
#[derive(Clone)]
pub struct SessionStore {
    path: PathBuf,
}
impl SessionStore {
    /// Only flat, host-owned artifact files are removed; linked directories are rejected.
    pub fn remove_artifacts(&self, id: u64) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| hikari_agent::AgentError::new("会话数据目录无效"))?;
        let root = parent.join("artifacts");
        let target = root.join(format!("talk-{id}"));
        if !target.exists() {
            return Ok(());
        }
        let data = parent
            .canonicalize()
            .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
        let expected = data.join("artifacts").join(format!("talk-{id}"));
        if root
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
            if name
                .strip_suffix(".txt")
                .is_some_and(|id| id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit()))
                && entry
                    .file_type()
                    .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?
                    .is_file()
            {
                std::fs::remove_file(entry.path())
                    .map_err(|e| hikari_agent::AgentError::new(e.to_string()))?;
            }
        }
        std::fs::remove_dir(target).map_err(|e| hikari_agent::AgentError::new(e.to_string()))
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

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
        write_json(&self.path, &sessions)
    }
}

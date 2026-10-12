use crate::config::{read_json_or_default, write_json};
use hikari_agent::{Message, Result, Usage};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// UI transcript survives context compaction; context is the active provider window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub title: String,
    pub transcript: Vec<Message>,
    pub context: Vec<Message>,
    pub retained_until: usize,
    pub usage: Usage,
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
        read_json_or_default(&self.path)
    }
    pub fn save(&self, sessions: &[Session]) -> Result<()> {
        write_json(&self.path, &sessions)
    }
}

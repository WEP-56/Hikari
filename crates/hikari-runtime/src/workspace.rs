use crate::config::{read_json_or_default, write_json};
use hikari_agent::{AgentError, Result};
use std::path::{Path, PathBuf};

/// Workspace history is independent of editable provider settings.
pub struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn default_directory(&self) -> PathBuf {
        self.root.join("chat")
    }
    pub fn load(&self) -> Result<Vec<PathBuf>> {
        read_json_or_default(&self.root.join("workspaces.json"))
    }
    pub fn remember(&self, history: &[PathBuf], path: &Path) -> Result<Vec<PathBuf>> {
        let mut recent = vec![path.to_path_buf()];
        recent.extend(history.iter().filter(|p| p.as_path() != path).cloned());
        recent.truncate(20);
        write_json(&self.root.join("workspaces.json"), &recent)?;
        Ok(recent)
    }
}

pub fn resolve_workspace(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .map_err(|e| AgentError::new(format!("无法打开工作区：{e}")))?;
    if !path.is_dir() {
        return Err(AgentError::new("请选择文件夹作为工作区"));
    }
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            if local.len() < 248 {
                return Ok(PathBuf::from(local));
            }
        }
    }
    Ok(path)
}

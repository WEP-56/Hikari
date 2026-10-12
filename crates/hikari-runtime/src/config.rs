use crate::mcp::McpServerConfig;
use hikari_agent::{AgentError, ProviderConfig, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub provider: ProviderConfig,
    pub persona_name: String,
    pub dark: bool,
    pub max_tool_rounds: usize,
    pub mcp_servers: Vec<McpServerConfig>,
    pub skill_roots: Vec<PathBuf>,
    pub disabled_skills: Vec<PathBuf>,
}
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            provider: ProviderConfig::default(),
            persona_name: "光".into(),
            dark: true,
            max_tool_rounds: 50,
            mcp_servers: Vec::new(),
            skill_roots: vec![PathBuf::from(".hikari/skills")],
            disabled_skills: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    root: PathBuf,
}
impl ConfigStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn path(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn load(&self) -> Result<AppConfig> {
        let config: AppConfig = read_json_or_default(&self.path())?;
        if config.version != 1 {
            return Err(AgentError::new("配置版本不受支持"));
        }
        for server in &config.mcp_servers {
            server.validate()?;
        }
        let mut names = std::collections::HashSet::new();
        if config.mcp_servers.iter().any(|s| !names.insert(&s.name)) {
            return Err(AgentError::new("MCP 名称不能重复"));
        }
        Ok(config)
    }
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        write_json(&self.path(), config)
    }
}

pub(crate) fn read_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| AgentError::new(format!("{} 格式错误：{e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(AgentError::new(e.to_string())),
    }
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or_else(|| AgentError::new("缺少父目录"))?;
    fs::create_dir_all(parent).map_err(|e| AgentError::new(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = fs::File::create(&tmp).map_err(|e| AgentError::new(e.to_string()))?;
        file.write_all(
            &serde_json::to_vec_pretty(value).map_err(|e| AgentError::new(e.to_string()))?,
        )
        .map_err(|e| AgentError::new(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AgentError::new(e.to_string()))?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| AgentError::new(format!("保存失败：{e}")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

use crate::mcp::McpServerConfig;
use crate::preferences::{AppearanceConfig, ModelMetadata, ProviderProfile};
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
    pub providers: Vec<ProviderProfile>,
    pub active_provider: String,
    pub appearance: AppearanceConfig,
    pub persona_name: String,
    pub memory: crate::MemoryConfig,
    pub dark: bool,
    pub max_tool_rounds: usize,
    pub mcp_servers: Vec<McpServerConfig>,
    pub skill_roots: Vec<PathBuf>,
    pub disabled_skills: Vec<PathBuf>,
}
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 2,
            provider: ProviderConfig::default(),
            providers: vec![ProviderProfile::new(
                "default".into(),
                "默认提供商".into(),
                ProviderConfig::default(),
            )],
            active_provider: "default".into(),
            appearance: AppearanceConfig::default(),
            persona_name: "光".into(),
            memory: crate::MemoryConfig::default(),
            dark: true,
            max_tool_rounds: 50,
            mcp_servers: Vec::new(),
            skill_roots: vec![PathBuf::from(".hikari/skills")],
            disabled_skills: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn migrate(&mut self) -> Result<()> {
        if self.version == 1 {
            self.providers = vec![ProviderProfile::new(
                "default".into(),
                "默认提供商".into(),
                self.provider.clone(),
            )];
            self.active_provider = "default".into();
            self.version = 2;
        }
        if self.version != 2 {
            return Err(AgentError::new("配置版本不受支持"));
        }
        let own = PathBuf::from(".hikari/skills");
        self.skill_roots.retain(|root| root != &own);
        self.skill_roots.insert(0, own);
        Ok(())
    }
    pub fn sync_active(&mut self) -> Result<()> {
        let profile = self
            .providers
            .iter_mut()
            .find(|p| p.id == self.active_provider)
            .ok_or_else(|| AgentError::new("当前提供商不存在"))?;
        profile.config = self.provider.clone();
        if let Some(model) = profile
            .models
            .iter_mut()
            .find(|m| m.id == self.provider.model)
        {
            model.context_tokens = self.provider.context_tokens;
            model.max_output_tokens = self.provider.max_output_tokens;
        } else {
            profile
                .models
                .push(ModelMetadata::from_config(&self.provider));
        }
        Ok(())
    }
    pub fn activate(&mut self, id: &str) -> Result<()> {
        let profile = self
            .providers
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AgentError::new("提供商不存在"))?;
        self.provider = profile.config.clone();
        self.active_provider = id.into();
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 2 {
            return Err(AgentError::new("配置版本不受支持"));
        }
        self.provider.validate()?;
        self.appearance.validate()?;
        self.memory.validate()?;
        for reference in [&self.memory.extraction, &self.memory.consolidation] {
            if !reference.provider_id.is_empty()
                && !self.providers.iter().any(|p| {
                    p.id == reference.provider_id
                        && p.models.iter().any(|m| m.id == reference.model_id)
                })
            {
                return Err(AgentError::new("记忆引用的提供商或模型不存在"));
            }
        }
        if !(1..=1000).contains(&self.max_tool_rounds) {
            return Err(AgentError::new("最大工具轮数需要在 1–1000 内"));
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.providers {
            if profile.id.is_empty() || profile.name.trim().is_empty() || !ids.insert(&profile.id) {
                return Err(AgentError::new("提供商名称不能为空，ID 不能重复"));
            }
            profile.config.validate()?;
            let mut models = std::collections::HashSet::new();
            for model in &profile.models {
                model.validate()?;
                if !models.insert(&model.id) {
                    return Err(AgentError::new("同一提供商的模型名称不能重复"));
                }
            }
            let model = profile
                .models
                .iter()
                .find(|m| m.id == profile.config.model)
                .ok_or_else(|| AgentError::new("当前模型缺少元数据"))?;
            if let Some(effort) = &profile.config.reasoning_effort {
                if !model.supports_thinking || !model.thinking_levels.contains(effort) {
                    return Err(AgentError::new("当前模型不支持所选思考挡位"));
                }
            }
            if model.context_tokens != profile.config.context_tokens
                || model.max_output_tokens != profile.config.max_output_tokens
            {
                return Err(AgentError::new("提供商当前模型的 token 限制与元数据不一致"));
            }
        }
        if !ids.contains(&self.active_provider) {
            return Err(AgentError::new("当前提供商不存在"));
        }
        let mut names = std::collections::HashSet::new();
        for server in &self.mcp_servers {
            server.validate()?;
            if !names.insert(&server.name) {
                return Err(AgentError::new("MCP 名称不能重复"));
            }
        }
        Ok(())
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
        let mut config: AppConfig = read_json_or_default(&self.path())?;
        config.migrate()?;
        let active = config.active_provider.clone();
        config.activate(&active)?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let mut config = config.clone();
        config.migrate()?;
        config.sync_active()?;
        config.validate()?;
        if self.path().exists() {
            let backup = if self.load().is_ok() {
                self.root.join("config.json.bak")
            } else {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                self.root.join(format!("config.corrupt-{timestamp}.json"))
            };
            fs::copy(self.path(), backup)
                .map_err(|e| AgentError::new(format!("配置备份失败：{e}")))?;
        }
        fs::create_dir_all(self.root.join("skills")).map_err(|e| AgentError::new(e.to_string()))?;
        write_json(&self.path(), &config)
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

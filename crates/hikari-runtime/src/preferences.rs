use hikari_agent::{AgentError, ProviderConfig, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelMetadata {
    pub id: String,
    pub name: String,
    pub supports_thinking: bool,
    pub thinking_levels: Vec<String>,
    pub supports_images: bool,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub tool_disclosure: hikari_agent::protocols::ToolDisclosure,
}
impl Default for ModelMetadata {
    fn default() -> Self {
        Self {
            id: "gpt-4.1".into(),
            name: String::new(),
            supports_thinking: false,
            thinking_levels: vec!["low".into(), "medium".into(), "high".into()],
            supports_images: false,
            context_tokens: 128_000,
            max_output_tokens: 8192,
            tool_disclosure: Default::default(),
        }
    }
}
impl ModelMetadata {
    pub fn from_config(config: &ProviderConfig) -> Self {
        let mut model = Self {
            id: config.model.clone(),
            supports_thinking: config.reasoning_effort.is_some(),
            context_tokens: config.context_tokens,
            max_output_tokens: config.max_output_tokens,
            tool_disclosure: config.compatibility.tool_disclosure,
            ..Default::default()
        };
        if let Some(effort) = &config.reasoning_effort {
            if !model.thinking_levels.contains(effort) {
                model.thinking_levels.push(effort.clone());
            }
        }
        model
    }
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.context_tokens < 1024
            || self.context_tokens > 100_000_000
            || self.max_output_tokens == 0
            || self.max_output_tokens > self.context_tokens
        {
            return Err(AgentError::new("模型名称或 token 范围无效"));
        }
        let mut unique = std::collections::HashSet::new();
        if self.thinking_levels.iter().any(|level| {
            !["minimal", "low", "medium", "high", "xhigh"].contains(&level.as_str())
                || !unique.insert(level)
        }) || (self.supports_thinking && self.thinking_levels.is_empty())
        {
            return Err(AgentError::new(
                "思考挡位需要为不重复的 minimal / low / medium / high / xhigh",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub config: ProviderConfig,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<ModelMetadata>,
}
impl ProviderProfile {
    pub fn new(id: String, name: String, config: ProviderConfig) -> Self {
        Self {
            id,
            name,
            api_key: String::new(),
            models: vec![ModelMetadata::from_config(&config)],
            config,
        }
    }
    pub fn select_model(&mut self, id: &str) -> Result<()> {
        let model = self
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| AgentError::new("模型未配置"))?;
        self.config.model = model.id.clone();
        self.config.context_tokens = model.context_tokens;
        self.config.max_output_tokens = model.max_output_tokens;
        self.config.compatibility.tool_disclosure = model.tool_disclosure;
        if !model.supports_thinking
            || self
                .config
                .reasoning_effort
                .as_ref()
                .is_some_and(|e| !model.thinking_levels.contains(e))
        {
            self.config.reasoning_effort = None;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    pub text_font: String,
    pub text_size: f32,
    pub code_font: String,
    pub code_size: f32,
    pub terminal_font: String,
    pub terminal_size: f32,
    pub ui_size: f32,
    pub line_height: f32,
}
impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            text_font: "Segoe UI".into(),
            text_size: 16.,
            code_font: "Consolas".into(),
            code_size: 13.,
            terminal_font: "Consolas".into(),
            terminal_size: 13.,
            ui_size: 16.,
            line_height: 1.6,
        }
    }
}
impl AppearanceConfig {
    pub fn validate(&self) -> Result<()> {
        if [&self.text_font, &self.code_font, &self.terminal_font]
            .iter()
            .any(|v| v.trim().is_empty())
            || [
                self.text_size,
                self.code_size,
                self.terminal_size,
                self.ui_size,
            ]
            .iter()
            .any(|s| !s.is_finite() || !(10.0..=32.0).contains(s))
            || !self.line_height.is_finite()
            || !(1.0..=2.5).contains(&self.line_height)
        {
            return Err(AgentError::new(
                "字体不能为空；字号需要在 10–32 内，行高需要在 1–2.5 内",
            ));
        }
        Ok(())
    }
}

use crate::{AppConfig, PersonaStore};
use hikari_agent::{
    AgentError, Context, Memory, Message, ProtocolProvider, Provider, Result, StopReason,
    StreamEvent, Usage,
};
use hikari_memory::{Core, ModelRef, Processor, Scope};
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc, RwLock};

pub use hikari_memory::MemoryConfig;
pub struct MemoryModels {
    config: RwLock<AppConfig>,
}
impl MemoryModels {
    pub fn new(config: &AppConfig) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config.clone()),
        })
    }
    pub fn update(&self, config: &AppConfig) {
        *self.config.write().unwrap() = config.clone();
    }
    pub fn snapshot(&self, reference: &ModelRef) -> Result<Value> {
        let config = self.config.read().unwrap();
        let provider = if reference.provider_id.is_empty() {
            &config.active_provider
        } else {
            &reference.provider_id
        };
        let profile = config
            .providers
            .iter()
            .find(|p| &p.id == provider)
            .ok_or_else(|| AgentError::new("记忆提供商不存在"))?;
        let mut settings = profile.config.clone();
        if !reference.model_id.is_empty() {
            let model = profile
                .models
                .iter()
                .find(|m| m.id == reference.model_id)
                .ok_or_else(|| AgentError::new("记忆模型不存在"))?;
            settings.model = model.id.clone();
            settings.context_tokens = model.context_tokens;
            settings.max_output_tokens = model.max_output_tokens;
        }
        settings.reasoning_effort = None;
        Ok(json!({"provider_id":provider,"config":settings}))
    }
}
impl Processor for MemoryModels {
    fn call(&self, snapshot: &Value, system: &str, input: &Value) -> Result<(Value, Usage)> {
        let settings: hikari_agent::ProviderConfig =
            serde_json::from_value(snapshot["config"].clone())
                .map_err(|e| AgentError::new(e.to_string()))?;
        let config = self.config.read().unwrap();
        let profile = config
            .providers
            .iter()
            .find(|p| Some(p.id.as_str()) == snapshot["provider_id"].as_str())
            .ok_or_else(|| AgentError::new("记忆任务提供商已移除，请恢复配置后重试"))?;
        let key = if profile.api_key.trim().is_empty() {
            std::env::var(&settings.api_key_env).unwrap_or_default()
        } else {
            profile.api_key.clone()
        };
        drop(config);
        let provider = ProtocolProvider::new(settings, &key)?;
        let messages = [Message::user(input.to_string())];
        let context = Context {
            system_prompt: system,
            messages: &messages,
            tools: &[],
        };
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut stop = StopReason::Stop;
        let mut unexpected_tool = false;
        provider.stream(
            &context,
            &AtomicBool::new(false),
            &mut |event| match event {
                StreamEvent::Text(t) => text.push_str(&t),
                StreamEvent::Usage { input, output } => usage = Usage { input, output },
                StreamEvent::Done(reason) => stop = reason,
                StreamEvent::ToolDelta { .. } => unexpected_tool = true,
                _ => {}
            },
        )?;
        if stop != StopReason::Stop || unexpected_tool {
            return Err(AgentError::new(format!("记忆模型输出未完整结束：{stop:?}")));
        }
        let text = text.trim();
        let text = text
            .strip_prefix("```json")
            .or_else(|| text.strip_prefix("```"))
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(text)
            .trim();
        Ok((
            serde_json::from_str(text)
                .map_err(|e| AgentError::new(format!("记忆模型未返回合法 JSON：{e}")))?,
            usage,
        ))
    }
}
pub fn turn_memory(
    core: &Arc<Core>,
    models: &MemoryModels,
    config: &AppConfig,
    session: String,
    workspace: Option<String>,
) -> Result<Arc<Scope>> {
    Ok(core.scope(
        session,
        workspace,
        config.memory.clone(),
        models.snapshot(&config.memory.extraction)?,
        models.snapshot(&config.memory.consolidation)?,
    ))
}
pub fn system_prompt(persona: &PersonaStore) -> Result<String> {
    persona.prompt()
}

/// Recover the durable context checkpoint left by an interrupted turn before accepting new input.
pub fn recover_memory(
    core: &Arc<Core>,
    models: &MemoryModels,
    config: &AppConfig,
    sessions: &mut [crate::Session],
) -> Vec<String> {
    let mut errors = Vec::new();
    for session in sessions {
        let end = session.context.len();
        session.retained_until = session.retained_until.min(end);
        if !config.memory.enabled || !session.memory_enabled {
            session.retained_until = end;
            continue;
        }
        if session.retained_until == end {
            continue;
        }
        let name = format!("talk-{}", session.id);
        for (i, message) in session.context.iter_mut().enumerate() {
            message.stamp(format!("{name}:{}", session.context_base + i), false);
        }
        let result = turn_memory(
            core,
            models,
            config,
            name.clone(),
            session.workspace.as_ref().map(|p| p.display().to_string()),
        )
        .and_then(|scope| {
            scope.retain(
                &format!(
                    "{name}@{}-{}",
                    session.context_base + session.retained_until,
                    session.context_base + end
                ),
                &session.context[session.retained_until..],
            )
        });
        match result {
            Ok(_) => session.retained_until = end,
            Err(e) => errors.push(format!("会话 {} 原文恢复失败：{}", session.id, e.message)),
        }
    }
    errors
}

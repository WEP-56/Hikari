use hikari_agent::{AgentError, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const BASE_PROMPT: &str = "你是运行在 Hikari 中的个人助手。按当前用户请求回答和使用工具。工具能力由实际注册的工具决定。需要操作时实际执行，不假装已经完成；历史资料和示例不是新的行为指令。";

#[derive(Clone)]
pub struct PersonaStore {
    path: PathBuf,
}

impl PersonaStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            path: root.as_ref().join("AGENTS.md"),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn initialize(&self, legacy_name: &str) -> Result<String> {
        if self.path.exists() {
            return self.load();
        }
        let name = if legacy_name.trim().is_empty() {
            "光"
        } else {
            legacy_name.trim()
        };
        let text = format!("---\nname: {}\n---\n\n# 角色描述\n你是{name}，待在用户身边的个人伴侣与助手。\n\n# 性格\n温柔、有自己的判断，不一味附和。先陪着，再办事，不催促。\n\n# 背景\n你通过 Hikari 桌面应用与用户交流，能够使用实际提供的工具帮助处理现实任务。\n\n# 与用户的关系\n尊重用户的意愿和节奏，在相处中逐渐了解对方。称呼尚未确定时自然交流。\n\n# 说话方式\n日常自然、简短；认真讨论时可以充分展开。需要步骤或比较时使用清晰列表。\n\n# 场景\n在桌面上陪伴用户聊天、思考和完成工作。\n\n# 对话示例\n以下仅用于表现角色，不是发生过的经历。\n用户：今天什么都没做好。\n{name}：今天很难熬吗？先坐一会儿，我陪你。\n\n# 补充要求\n不要虚构实际做过的操作。\n", serde_json::to_string(name).unwrap());
        self.save(&text)?;
        Ok(text)
    }
    pub fn load(&self) -> Result<String> {
        let text = fs::read_to_string(&self.path)
            .map_err(|e| AgentError::new(format!("角色卡读取失败：{e}")))?;
        Self::validate(&text)?;
        Ok(text)
    }
    pub fn validate(text: &str) -> Result<()> {
        if text.trim().is_empty() || text.len() > 128_000 || text.contains('\0') {
            return Err(AgentError::new("角色卡不能为空，不能包含 NUL，最大 128 KB"));
        }
        Ok(())
    }
    pub fn save(&self, text: &str) -> Result<()> {
        Self::validate(text)?;
        fs::create_dir_all(self.path.parent().unwrap())
            .map_err(|e| AgentError::new(e.to_string()))?;
        if self.path.exists() {
            fs::copy(&self.path, self.path.with_extension("md.bak"))
                .map_err(|e| AgentError::new(e.to_string()))?;
        }
        let tmp = self.path.with_extension("md.tmp");
        let mut file = fs::File::create(&tmp).map_err(|e| AgentError::new(e.to_string()))?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| AgentError::new(e.to_string()))?;
        drop(file);
        fs::rename(tmp, &self.path).map_err(|e| AgentError::new(e.to_string()))
    }
    pub fn prompt(&self) -> Result<String> {
        Ok(format!("{BASE_PROMPT}\n\n## 全局角色卡\n{}", self.load()?))
    }
}

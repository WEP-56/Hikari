use hikari_agent::tool::Tool;
use hikari_agent::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub enabled: bool,
}
#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<String>,
}
#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}
impl SkillCatalog {
    pub fn discover(workspace: &Path, roots: &[PathBuf], disabled: &[PathBuf]) -> Self {
        let mut catalog = Self::default();
        let disabled: HashSet<_> = disabled
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        let mut seen_paths = HashSet::new();
        let mut seen_names = HashSet::new();
        for root in roots {
            let root = if root.is_absolute() {
                root.clone()
            } else {
                workspace.join(root)
            };
            if !root.exists() {
                continue;
            }
            let mut pending = vec![(root, 0)];
            while let Some((dir, depth)) = pending.pop() {
                if depth > 4 {
                    continue;
                }
                let manifest = if dir.is_file() {
                    dir.clone()
                } else {
                    dir.join("SKILL.md")
                };
                if manifest.is_file() {
                    let path = match manifest.canonicalize() {
                        Ok(p) => p,
                        Err(e) => {
                            catalog.diagnostics.push(e.to_string());
                            continue;
                        }
                    };
                    if !seen_paths.insert(path.clone()) {
                        continue;
                    }
                    match parse_skill(&path) {
                        Ok((name, description)) if seen_names.insert(name.clone()) => {
                            catalog.skills.push(Skill {
                                name,
                                description,
                                enabled: !disabled.contains(&path),
                                path,
                            })
                        }
                        Ok((name, _)) => catalog
                            .diagnostics
                            .push(format!("跳过重复 Skill 名称：{name}")),
                        Err(error) => catalog
                            .diagnostics
                            .push(format!("{}：{error}", path.display())),
                    }
                    continue;
                }
                if let Ok(entries) = fs::read_dir(&dir) {
                    let mut children: Vec<_> = entries
                        .flatten()
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
                        .map(|e| e.path())
                        .collect();
                    children.sort();
                    pending.extend(children.into_iter().rev().map(|p| (p, depth + 1)));
                }
            }
        }
        catalog.skills.sort_by(|a, b| a.name.cmp(&b.name));
        catalog
    }
    pub fn prompt(&self) -> String {
        let skills: Vec<_> = self
            .skills
            .iter()
            .filter(|s| s.enabled)
            .map(|s| json!({"name":s.name,"description":s.description}))
            .collect();
        if skills.is_empty() {
            return String::new();
        }
        format!("\n\nAvailable skills (metadata only):\n{}\nWhen a task matches a skill, call load_skill with its name, then follow its instructions. Skill-relative resources resolve from the returned base directory.",serde_json::to_string(&skills).unwrap())
    }
    pub fn register(&self, tools: &mut ToolRegistry) -> std::result::Result<(), String> {
        if self.skills.iter().any(|s| s.enabled) {
            tools.register(Arc::new(LoadSkill {
                skills: self.skills.iter().filter(|s| s.enabled).cloned().collect(),
            }))?;
        }
        Ok(())
    }
}
fn parse_skill(path: &Path) -> std::result::Result<(String, String), String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let rest = normalized
        .strip_prefix("---\n")
        .ok_or("SKILL.md 需要 YAML frontmatter")?;
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(str::len))
        .ok_or("缺少 frontmatter 结束标记")?;
    let meta: Frontmatter = serde_yaml::from_str(&rest[..end]).map_err(|e| e.to_string())?;
    if meta.name.trim().is_empty() || meta.description.trim().is_empty() {
        return Err("name 和 description 不能为空".into());
    }
    Ok((meta.name, meta.description))
}
struct LoadSkill {
    skills: Vec<Skill>,
}
impl Tool for LoadSkill {
    fn target_path(&self, arguments: &str) -> std::result::Result<Option<PathBuf>, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let skill = self
            .skills
            .iter()
            .find(|s| Some(s.name.as_str()) == args["name"].as_str())
            .ok_or("Skill 不存在或未启用")?;
        let base = skill.path.parent().ok_or("Skill 目录不存在")?;
        let path = args["resource"].as_str().map_or_else(
            || Ok(skill.path.clone()),
            |resource| {
                base.join(resource)
                    .canonicalize()
                    .map_err(|e| e.to_string())
            },
        )?;
        if !path.starts_with(base) {
            return Err("资源路径超出 Skill 目录".into());
        }
        Ok(Some(path))
    }
    fn call_authorized(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        target: Option<&std::path::Path>,
    ) -> std::result::Result<String, String> {
        if abort.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消".into());
        }
        if let Some(target) = target {
            if self.target_path(arguments)?.as_deref() != Some(target) {
                return Err("Skill 资源目标已改变，需要重新审批".into());
            }
        }
        self.call(arguments)
    }
    fn output_limit(&self) -> usize {
        128 * 1024
    }
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "按名称加载已启用 Skill 的完整指令。resource 可读取该 Skill 目录内的文本资源。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"name":{"type":"string"},"resource":{"type":"string"}},"required":["name"],"additionalProperties":false})
    }
    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let skill = self
            .skills
            .iter()
            .find(|s| Some(s.name.as_str()) == args["name"].as_str())
            .ok_or("Skill 不存在或未启用")?;
        let base = skill.path.parent().ok_or("Skill 目录不存在")?;
        let path = if let Some(resource) = args["resource"].as_str() {
            let path = base
                .join(resource)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !path.starts_with(base) {
                return Err("资源路径超出 Skill 目录".into());
            }
            path
        } else {
            skill.path.clone()
        };
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 128 * 1024 {
            return Err("Skill 资源过大".into());
        }
        let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(format!(
            "Skill: {}\nBase directory: {}\n\n{}",
            skill.name,
            base.display(),
            content
        ))
    }
}

use hikari_agent::tool::Tool;
use hikari_agent::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
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
    #[serde(skip)]
    resources: Arc<BTreeMap<String, Vec<u8>>>,
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
        // A managed directory swap and the complete resource read form one version
        // boundary. New tasks must not observe a missing or mixed Skill revision.
        let _guard = match MANAGEMENT_LOCK.lock() {
            Ok(guard) => guard,
            Err(_) => {
                catalog.diagnostics.push("Skill 管理锁失败".into());
                return catalog;
            }
        };
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
                    match parse_skill(&path).and_then(|(name, description)| {
                        let resources = read_resources(path.parent().ok_or("Skill 目录不存在")?)?;
                        Ok((name, description, resources))
                    }) {
                        Ok((name, description, resources)) if seen_names.insert(name.clone()) => {
                            catalog.skills.push(Skill {
                                name,
                                description,
                                enabled: !disabled.contains(&path),
                                path,
                                resources: Arc::new(resources),
                            })
                        }
                        Ok((name, _, _)) => catalog
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
                        .filter(|e| {
                            !e.file_name().to_string_lossy().starts_with(".stage-")
                                && !e.file_name().to_string_lossy().starts_with(".backup-")
                        })
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
        // Conservative UTF-8 byte ceiling also bounds Chinese metadata and JSON overhead.
        let mut remaining = 3000usize.saturating_sub(160);
        let mut skills = Vec::new();
        let mut omitted = 0;
        for skill in self.skills.iter().filter(|s| s.enabled) {
            let item = json!({"name":skill.name,"description":skill.description.chars().take(320).collect::<String>()});
            let cost = item.to_string().len();
            if cost > remaining {
                omitted += 1;
                continue;
            }
            remaining -= cost;
            skills.push(item);
        }
        if skills.is_empty() {
            return String::new();
        }
        format!("\n\nAvailable skills (metadata only):\n{}\nWhen a task matches a skill, call load_skill with its name, then follow its instructions. Skill-relative resources resolve from the returned frozen base directory. {omitted} entries omitted by the index budget; known names can still be loaded.",serde_json::to_string(&skills).unwrap())
    }
    pub fn register(&self, tools: &mut ToolRegistry) -> std::result::Result<(), String> {
        if self.skills.iter().any(|s| s.enabled) {
            tools.register(Arc::new(LoadSkill {
                skills: self.skills.iter().filter(|s| s.enabled).cloned().collect(),
                snapshot: FrozenDirectory::new(self.skills.iter().filter(|s| s.enabled))?,
            }))?;
        }
        Ok(())
    }
    /// Management changes become visible when the host discovers the next task's catalog.
    pub fn register_management(
        &self,
        tools: &mut ToolRegistry,
        workspace: &Path,
    ) -> Result<(), String> {
        tools.register(Arc::new(SkillManage {
            workspace: workspace.canonicalize().map_err(|e| e.to_string())?,
        }))
    }
}
fn parse_skill(path: &Path) -> std::result::Result<(String, String), String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 128 * 1024 {
        return Err("SKILL.md 超出 128 KiB 限制".into());
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_manifest(&text)
}
fn parse_manifest(text: &str) -> Result<(String, String), String> {
    if text.len() > 128 * 1024 {
        return Err("SKILL.md 超出 128 KiB 限制".into());
    }
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
    validate_name(&meta.name)?;
    Ok((meta.name, meta.description))
}

const MAX_SKILL_BYTES: usize = 16 * 1024 * 1024;
const MAX_SKILL_FILES: usize = 512;
static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MANAGEMENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn unique_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Skill 名称只允许字母、数字、连字符和下划线，最多 128 字节".into());
    }
    Ok(())
}
fn validate_resource(resource: &str) -> Result<(), String> {
    if resource.is_empty()
        || resource.contains(['\\', ':'])
        || resource.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
        })
        || Path::new(resource).is_absolute()
    {
        return Err("资源路径必须是 Skill 内的普通相对路径".into());
    }
    Ok(())
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn read_resources(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut resources = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    let mut bytes = 0usize;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if is_link(&metadata) {
            return Err("Skill 资源不允许符号链接或 junction".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
                pending.push(entry.map_err(|e| e.to_string())?.path());
                if pending.len() > MAX_SKILL_FILES * 4 {
                    return Err("Skill 目录过多".into());
                }
            }
        } else if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len() as usize);
            if bytes > MAX_SKILL_BYTES || resources.len() >= MAX_SKILL_FILES {
                return Err("Skill 超出 512 文件 / 16 MiB 限制".into());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("资源路径不是 UTF-8")?
                .replace('\\', "/");
            validate_resource(&relative)?;
            resources.insert(relative, fs::read(&path).map_err(|e| e.to_string())?);
        } else {
            return Err("Skill 含非常规资源文件".into());
        }
    }
    Ok(resources)
}
fn write_resources(root: &Path, resources: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    fs::create_dir(root).map_err(|e| e.to_string())?;
    for (relative, bytes) in resources {
        validate_resource(relative)?;
        let path = root.join(relative);
        fs::create_dir_all(path.parent().ok_or("资源目录无效")?).map_err(|e| e.to_string())?;
        fs::write(path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}
struct FrozenDirectory(PathBuf);
impl FrozenDirectory {
    fn skill_path(&self, name: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        // Names remain case-sensitive API identities even on a case-insensitive host.
        self.0
            .join(".hikari")
            .join(format!("{:x}", Sha256::digest(name.as_bytes())))
    }
    fn new<'a>(skills: impl Iterator<Item = &'a Skill>) -> Result<Self, String> {
        let frozen = Self(std::env::temp_dir().join(format!("hikari-skills-{}", unique_id())));
        fs::create_dir(&frozen.0).map_err(|e| e.to_string())?;
        // Preserve sensitive-resource checks when callers read or execute a frozen
        // resource directly, instead of returning to load_skill for each resource.
        fs::create_dir(frozen.0.join(".hikari")).map_err(|e| e.to_string())?;
        for skill in skills {
            write_resources(&frozen.skill_path(&skill.name), &skill.resources)?;
        }
        Ok(frozen)
    }
}
impl Drop for FrozenDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct SkillManage {
    workspace: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageArgs {
    action: String,
    name: String,
    #[serde(default)]
    files: Vec<ResourceInput>,
    #[serde(default)]
    remove: Vec<String>,
    revision: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    path: String,
    text: Option<String>,
    base64: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Revision {
    id: String,
    files: BTreeMap<String, String>,
}
impl SkillManage {
    fn args(&self, arguments: &str) -> Result<ManageArgs, String> {
        let args: ManageArgs = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        validate_name(&args.name)?;
        if !matches!(
            args.action.as_str(),
            "create" | "install" | "update" | "delete" | "rollback" | "history"
        ) {
            return Err("未知 Skill 管理操作".into());
        }
        Ok(args)
    }
    fn managed_path(&self, suffix: &str) -> Result<PathBuf, String> {
        let path = self.workspace.join(suffix);
        let mut ancestor = path.as_path();
        while ancestor != self.workspace {
            if let Ok(meta) = fs::symlink_metadata(ancestor) {
                if is_link(&meta) {
                    return Err("Skill 管理路径不允许符号链接或 junction".into());
                }
            }
            ancestor = ancestor.parent().ok_or("管理路径超出工作区")?;
        }
        hikari_agent::permission::resolve_path(&self.workspace, suffix)
    }
    fn archive(
        &self,
        root: &Path,
        resources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<String, String> {
        use base64::Engine;
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let id = unique_id();
        let revision = Revision {
            id: id.clone(),
            files: resources
                .iter()
                .map(|(p, b)| {
                    (
                        p.clone(),
                        base64::engine::general_purpose::STANDARD.encode(b),
                    )
                })
                .collect(),
        };
        let path = root.join(format!("{id}.json"));
        fs::write(
            path,
            serde_json::to_vec(&revision).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(id)
    }
    fn execute(&self, args: ManageArgs) -> Result<String, String> {
        use base64::Engine;
        let _guard = MANAGEMENT_LOCK.lock().map_err(|_| "Skill 管理锁失败")?;
        let target = self.managed_path(&format!(".hikari/skills/{}", args.name))?;
        let history = self.managed_path(&format!(".hikari/skill-versions/{}", args.name))?;
        if args.action == "history" {
            let mut revisions = Vec::new();
            if history.exists() {
                for entry in fs::read_dir(&history).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if entry.file_type().map_err(|e| e.to_string())?.is_file()
                        && entry.path().extension().is_some_and(|ext| ext == "json")
                    {
                        revisions.push(
                            entry
                                .path()
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
            }
            revisions.sort();
            return Ok(json!({"name":args.name,"revisions":revisions}).to_string());
        }
        let exists = target.exists();
        if matches!(args.action.as_str(), "create" | "install") && exists {
            return Err("Skill 已存在；请使用 update".into());
        }
        if matches!(args.action.as_str(), "update" | "delete") && !exists {
            return Err("Skill 不存在".into());
        }
        let before = if exists {
            read_resources(&target)?
        } else {
            BTreeMap::new()
        };
        let mut resources = if args.action == "update" {
            before.clone()
        } else {
            BTreeMap::new()
        };
        if args.action == "rollback" {
            let revision = args.revision.as_deref().ok_or("rollback 缺少 revision")?;
            if revision.is_empty() || !revision.bytes().all(|c| c.is_ascii_digit() || c == b'-') {
                return Err("revision 无效".into());
            }
            let path = history.join(format!("{revision}.json"));
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if is_link(&metadata) || metadata.len() > (MAX_SKILL_BYTES * 2) as u64 {
                return Err("版本资源无效".into());
            }
            let stored: Revision =
                serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            for (path, content) in stored.files {
                validate_resource(&path)?;
                resources.insert(
                    path,
                    base64::engine::general_purpose::STANDARD
                        .decode(content)
                        .map_err(|e| e.to_string())?,
                );
            }
        } else if args.action != "delete" {
            for resource in args.files {
                validate_resource(&resource.path)?;
                let bytes = match (resource.text, resource.base64) {
                    (Some(text), None) => text.into_bytes(),
                    (None, Some(content)) => base64::engine::general_purpose::STANDARD
                        .decode(content)
                        .map_err(|e| e.to_string())?,
                    _ => return Err("每个文件必须且只能提供 text 或 base64".into()),
                };
                resources.insert(resource.path, bytes);
            }
            for path in args.remove {
                validate_resource(&path)?;
                resources.remove(&path);
            }
        }
        if args.action != "delete" {
            let mut normalized = HashSet::new();
            for path in resources.keys() {
                if !normalized.insert(path.to_lowercase()) {
                    return Err("资源路径存在大小写冲突".into());
                }
            }
            if resources.len() > MAX_SKILL_FILES
                || resources.values().map(Vec::len).sum::<usize>() > MAX_SKILL_BYTES
            {
                return Err("Skill 超出 512 文件 / 16 MiB 限制".into());
            }
            let manifest = resources
                .get("SKILL.md")
                .ok_or("完整 Skill 必须包含 SKILL.md")?;
            let (name, _) =
                parse_manifest(std::str::from_utf8(manifest).map_err(|e| e.to_string())?)?;
            if name != args.name {
                return Err("SKILL.md name 必须与目标名称一致".into());
            }
        }
        let previous_revision = if exists {
            Some(self.archive(&history, &before)?)
        } else {
            None
        };
        // Persist all recovery data before the visible directory swap.
        let revision = if args.action != "delete" {
            Some(self.archive(&history, &resources)?)
        } else {
            None
        };
        fs::create_dir_all(target.parent().ok_or("管理目录无效")?).map_err(|e| e.to_string())?;
        let stage = target.with_file_name(format!(".stage-{}", unique_id()));
        if args.action != "delete" {
            if let Err(error) = write_resources(&stage, &resources) {
                let _ = fs::remove_dir_all(&stage);
                return Err(error);
            }
        }
        let backup = target.with_file_name(format!(".backup-{}", unique_id()));
        if exists {
            fs::rename(&target, &backup).map_err(|e| e.to_string())?;
        }
        if args.action != "delete" {
            if let Err(error) = fs::rename(&stage, &target) {
                if exists {
                    let _ = fs::rename(&backup, &target);
                }
                let _ = fs::remove_dir_all(&stage);
                return Err(error.to_string());
            }
        }
        let cleanup_warning = if exists {
            fs::remove_dir_all(&backup).err().map(|e| e.to_string())
        } else {
            None
        };
        Ok(json!({"action":args.action,"name":args.name,"path":target,"revision":revision,"previous_revision":previous_revision,"cleanup_warning":cleanup_warning,"refresh":"next_task","running_tasks":"retain_frozen_skill_version"}).to_string())
    }
}
impl Tool for SkillManage {
    fn name(&self) -> &str {
        "skill_manage"
    }
    fn description(&self) -> &str {
        "管理应用内置技能目录 .hikari/skills（相对应用根目录，与设置里的资源目录相同；不要装到会话工作目录或 .hikari/chat 下面）。create/install 使用完整 files（含 SKILL.md 和所有资源）；update 合并 files 并删除 remove；delete 保留可回滚历史；history/rollback 查询及恢复 revision。支持文本或 base64 二进制资源。返回的 path 就是实际安装位置。管理后下一任务刷新，正在运行的任务保留冻结版本。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["create","install","update","delete","history","rollback"]},"name":{"type":"string"},"files":{"type":"array","maxItems":512,"items":{"type":"object","properties":{"path":{"type":"string"},"text":{"type":"string"},"base64":{"type":"string"}},"required":["path"],"additionalProperties":false}},"remove":{"type":"array","items":{"type":"string"}},"revision":{"type":"string"}},"required":["action","name"],"additionalProperties":false})
    }
    fn target_path(&self, arguments: &str) -> Result<Option<PathBuf>, String> {
        let args = self.args(arguments)?;
        self.managed_path(&format!(".hikari/skills/{}", args.name))
            .map(Some)
    }
    fn call(&self, arguments: &str) -> Result<String, String> {
        self.execute(self.args(arguments)?)
    }
    fn call_authorized(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        target: Option<&Path>,
    ) -> Result<String, String> {
        if abort.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("已取消 Skill 管理".into());
        }
        if target.is_some() && self.target_path(arguments)?.as_deref() != target {
            return Err("Skill 管理目标已改变，需要重新审批".into());
        }
        self.call(arguments)
    }
}
struct LoadSkill {
    skills: Vec<Skill>,
    snapshot: FrozenDirectory,
}
impl Tool for LoadSkill {
    fn target_path(&self, arguments: &str) -> std::result::Result<Option<PathBuf>, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let skill = self
            .skills
            .iter()
            .find(|s| Some(s.name.as_str()) == args["name"].as_str())
            .ok_or("Skill 不存在或未启用")?;
        let resource = args["resource"].as_str().unwrap_or("SKILL.md");
        validate_resource(resource)?;
        if !skill.resources.contains_key(resource) {
            return Err("Skill 资源不存在".into());
        }
        // The original resource identity determines authorization even if the live version is deleted.
        Ok(Some(hikari_agent::permission::resolve_path(
            skill.path.parent().ok_or("Skill 目录不存在")?,
            resource,
        )?))
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
        let resource = args["resource"].as_str().unwrap_or("SKILL.md");
        validate_resource(resource)?;
        let bytes = skill.resources.get(resource).ok_or("Skill 资源不存在")?;
        if bytes.len() > 128 * 1024 {
            return Err("Skill 资源过大".into());
        }
        let content =
            std::str::from_utf8(bytes).map_err(|_| "资源是二进制文件；请使用返回的冻结目录")?;
        let base = self.snapshot.skill_path(&skill.name);
        Ok(format!(
            "Skill: {}\nBase directory: {}\n\n{}",
            skill.name,
            base.display(),
            content
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_waits_for_a_complete_managed_publication() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join(".hikari/skills/sample");
        fs::create_dir_all(&target).unwrap();
        fs::write(
            target.join("SKILL.md"),
            "---\nname: sample\ndescription: old\n---\nold",
        )
        .unwrap();
        let guard = MANAGEMENT_LOCK.lock().unwrap();
        // Hold the same publication lock while the live directory is absent.
        fs::rename(&target, target.with_file_name(".backup-test")).unwrap();
        let workspace = temp.path().to_owned();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(SkillCatalog::discover(
                    &workspace,
                    &[".hikari/skills".into()],
                    &[],
                ))
                .unwrap();
        });
        started_rx.recv().unwrap();
        assert!(result_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        fs::create_dir(&target).unwrap();
        fs::write(
            target.join("SKILL.md"),
            "---\nname: sample\ndescription: new\n---\nnew",
        )
        .unwrap();
        fs::write(target.join("resource.txt"), "new resource").unwrap();
        drop(guard);
        let catalog = result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        reader.join().unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].description, "new");
        assert_eq!(catalog.skills[0].resources["resource.txt"], b"new resource");
    }
}

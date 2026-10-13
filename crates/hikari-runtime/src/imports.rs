//! Read-only discovery of other agents. Scanning never starts MCP processes.
use crate::{McpServerConfig, McpTransport, SkillCatalog};
use hikari_agent::{AgentError, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct SkillSource {
    pub label: String,
    pub path: PathBuf,
}
pub fn skill_sources(home: &Path, workspace: &Path) -> Vec<SkillSource> {
    let mut roots = vec![
        ("Codex", home.join(".codex/skills")),
        ("Agents", home.join(".agents/skills")),
        ("Claude", home.join(".claude/skills")),
        ("Hermes", home.join(".hermes/skills")),
        ("Codex · 项目", workspace.join(".codex/skills")),
        ("Agents · 项目", workspace.join(".agents/skills")),
        ("Claude · 项目", workspace.join(".claude/skills")),
        ("Hermes · 项目", workspace.join(".hermes/skills")),
    ];
    for (env, label) in [
        ("CODEX_HOME", "Codex · 自定义"),
        ("HERMES_HOME", "Hermes · 自定义"),
    ] {
        if let Some(path) = std::env::var_os(env) {
            roots.push((label, PathBuf::from(path).join("skills")));
        }
    }
    roots
        .into_iter()
        .map(|(label, path)| SkillSource {
            label: label.into(),
            path,
        })
        .collect()
}

/// Copy all resources, refusing links and existing destinations. Sources remain untouched.
pub fn import_skill(manifest: &Path, own_root: &Path) -> Result<PathBuf> {
    let catalog = SkillCatalog::discover(Path::new("."), &[manifest.to_path_buf()], &[]);
    if catalog.skills.len() != 1 {
        return Err(AgentError::new("无法解析 Skill"));
    }
    let skill = &catalog.skills[0];
    let name = &skill.name;
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AgentError::new("Skill 名称不能包含路径或特殊字符"));
    }
    fs::create_dir_all(own_root).map_err(io_error)?;
    let own_root = own_root.canonicalize().map_err(io_error)?;
    let source = skill
        .path
        .parent()
        .ok_or_else(|| AgentError::new("Skill 缺少目录"))?;
    if own_root.starts_with(source) {
        return Err(AgentError::new("导入目标不能位于来源 Skill 内"));
    }
    let target = own_root.join(name);
    if target.exists() {
        return Err(AgentError::new(format!("Skill 已存在：{name}")));
    }
    let stage = own_root.join(format!(".{name}.importing"));
    fs::create_dir(&stage).map_err(io_error)?;
    let result =
        copy_tree(source, &stage, 0).and_then(|_| fs::rename(&stage, &target).map_err(io_error));
    if result.is_err() && stage.starts_with(&own_root) {
        let _ = fs::remove_dir_all(&stage);
    }
    result.map(|_| target.join("SKILL.md"))
}
fn copy_tree(source: &Path, target: &Path, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err(AgentError::new("Skill 资源目录过深"));
    }
    for entry in fs::read_dir(source).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let kind = entry.file_type().map_err(io_error)?;
        if kind.is_symlink() {
            return Err(AgentError::new("Skill 导入不复制符号链接"));
        }
        let dest = target.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&dest).map_err(io_error)?;
            copy_tree(&entry.path(), &dest, depth + 1)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), dest).map_err(io_error)?;
        }
    }
    Ok(())
}
fn io_error(e: std::io::Error) -> AgentError {
    AgentError::new(e.to_string())
}

#[derive(Debug, Clone)]
pub struct McpCandidate {
    pub source: String,
    pub path: PathBuf,
    pub server: McpServerConfig,
}
#[derive(Debug, Default)]
pub struct McpScan {
    pub candidates: Vec<McpCandidate>,
    pub diagnostics: Vec<String>,
}
pub fn mcp_sources(home: &Path, workspace: &Path) -> Vec<(String, PathBuf)> {
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let mut sources = vec![
        ("Claude Desktop / CLI".into(), home.join(".claude.json")),
        (
            "Claude MCP 合集".into(),
            home.join(".claude/mcp-configs/mcp-servers.json"),
        ),
        ("Codex".into(), codex.join("config.toml")),
        ("Claude · 项目".into(), workspace.join(".mcp.json")),
        ("Codex · 项目".into(), workspace.join(".codex/config.toml")),
        ("Hermes".into(), home.join(".hermes/config.yaml")),
    ];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        sources.push((
            "Claude Desktop".into(),
            PathBuf::from(appdata).join("Claude/claude_desktop_config.json"),
        ));
    }
    sources
}
pub fn scan_mcp(sources: &[(String, PathBuf)]) -> McpScan {
    let mut scan = McpScan::default();
    for (source, path) in sources {
        if !path.is_file() {
            scan.diagnostics
                .push(format!("{source} · {} · 未找到", path.display()));
            continue;
        }
        match parse_mcp_file(path) {
            Ok((servers, errors)) => {
                scan.diagnostics.push(format!(
                    "{source} · {} · {} 个",
                    path.display(),
                    servers.len()
                ));
                scan.diagnostics
                    .extend(errors.into_iter().map(|e| format!("{source} · {e}")));
                scan.candidates
                    .extend(servers.into_iter().map(|server| McpCandidate {
                        source: source.clone(),
                        path: path.clone(),
                        server,
                    }));
            }
            Err(_) => scan
                .diagnostics
                .push(format!("{source} · {} · 格式错误，已跳过", path.display())),
        }
    }
    scan
}
pub fn parse_mcp_file(path: &Path) -> Result<(Vec<McpServerConfig>, Vec<String>)> {
    let text = fs::read_to_string(path).map_err(io_error)?;
    let value: Value = match path.extension().and_then(|s| s.to_str()) {
        Some("toml") => serde_json::to_value(
            toml::from_str::<toml::Value>(&text).map_err(|_| AgentError::new("无效 TOML"))?,
        )
        .unwrap(),
        Some("yaml" | "yml") => {
            serde_yaml::from_str(&text).map_err(|_| AgentError::new("无效 YAML"))?
        }
        _ => serde_json::from_str(&text).map_err(|_| AgentError::new("无效 JSON"))?,
    };
    let mut tables = Vec::new();
    for key in ["mcpServers", "mcp_servers"] {
        if let Some(table) = value[key].as_object() {
            tables.push(table);
        }
    }
    // Claude CLI also stores project-local MCP definitions in ~/.claude.json.
    if let Some(projects) = value["projects"].as_object() {
        for project in projects.values() {
            if let Some(table) = project["mcpServers"].as_object() {
                tables.push(table);
            }
        }
    }
    let mut servers = Vec::new();
    let mut errors = Vec::new();
    for table in tables {
        for (name, value) in table {
            match parse_server(name, value) {
                Ok(server) => servers.push(server),
                Err(e) => errors.push(format!("{name} · {}", e.message)),
            }
        }
    }
    Ok((servers, errors))
}
fn string_map(value: &Value) -> Result<BTreeMap<String, String>> {
    if value.is_null() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_value(value.clone())
        .map_err(|_| AgentError::new("环境变量和请求头必须为字符串映射"))
}
fn parse_server(original_name: &str, value: &Value) -> Result<McpServerConfig> {
    let mut name: String = original_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(24)
        .collect();
    if name.is_empty() {
        name = "imported".into();
    }
    let transport = if let Some(command) = value["command"].as_str() {
        let args = if value["args"].is_null() {
            Vec::new()
        } else {
            serde_json::from_value(value["args"].clone())
                .map_err(|_| AgentError::new("启动参数必须为字符串数组"))?
        };
        McpTransport::Stdio {
            command: command.into(),
            args,
            env: string_map(&value["env"])?,
        }
    } else if let Some(url) = value["url"].as_str() {
        if value["type"] == "sse" {
            return Err(AgentError::new("旧版 SSE 传输暂不支持"));
        }
        let bearer_token_env = value["bearer_token_env_var"]
            .as_str()
            .or_else(|| value["bearer_token_env"].as_str())
            .map(str::to_owned);
        let mut headers = string_map(&value["http_headers"])?;
        headers.extend(string_map(&value["headers"])?);
        for (name, env) in string_map(&value["env_http_headers"])? {
            headers.insert(name, format!("${{{env}}}"));
        }
        hikari_agent::protocols::validate_headers(&headers)?;
        McpTransport::StreamableHttp {
            url: url.into(),
            bearer_token_env,
            headers,
        }
    } else {
        return Err(AgentError::new("缺少 command 或 url"));
    };
    let server = McpServerConfig {
        name,
        transport,
        enabled: false,
        timeout_seconds: value["timeout_seconds"].as_u64().unwrap_or(30),
    };
    server.validate()?;
    Ok(server)
}

pub fn merge_mcp(
    existing: &[McpServerConfig],
    candidates: &[McpCandidate],
    selected: &[bool],
    overwrite: bool,
) -> Result<Vec<McpServerConfig>> {
    let mut result = existing.to_vec();
    let mut imported = std::collections::HashSet::new();
    for (candidate, picked) in candidates.iter().zip(selected) {
        if !picked {
            continue;
        }
        candidate.server.validate()?;
        if !imported.insert(&candidate.server.name) {
            return Err(AgentError::new(format!(
                "所选来源有重名服务：{}，请只选择一个",
                candidate.server.name
            )));
        }
        if let Some(index) = result.iter().position(|s| s.name == candidate.server.name) {
            if overwrite {
                result[index] = candidate.server.clone();
            }
        } else {
            result.push(candidate.server.clone());
        }
    }
    Ok(result)
}

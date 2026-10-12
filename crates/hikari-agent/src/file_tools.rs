// Hikari adaptation of Pi's read/write/edit/ls/find/grep tools.
// Exact editing uses Pi's original BOM and line-ending helpers.
use super::*;
use crate::pi::edit::{detect_line_ending, normalize_to_lf, strip_bom};

pub(super) struct FileTool {
    pub root: PathBuf,
    pub kind: &'static str,
}
impl Tool for FileTool {
    fn name(&self) -> &str {
        self.kind
    }
    fn description(&self) -> &str {
        match self.kind {
            "read" => "读取工作区文本文件。offset 从 1 开始，limit 指定行数。",
            "write" => "创建或覆盖工作区文件，自动创建父目录。",
            "edit" => {
                "精确替换文件中的 old_text 为 new_text。旧文本必须只出现一次。保留 BOM 与换行风格。"
            }
            "ls" => "列出工作区目录内容。",
            "find" => "在工作区查找文件路径，pattern 是路径中的文本。",
            _ => "在工作区搜索文本，pattern 为字面文本，返回路径和行号。",
        }
    }
    fn parameters(&self) -> Value {
        let mut properties = json!({"path":{"type":"string"}});
        let mut required = vec!["path"];
        match self.kind {
            "read" => {
                properties["offset"] = json!({"type":"integer","minimum":1});
                properties["limit"] = json!({"type":"integer","minimum":1});
            }
            "write" => {
                properties["content"] = json!({"type":"string"});
                required.push("content");
            }
            "edit" => {
                properties["old_text"] = json!({"type":"string"});
                properties["new_text"] = json!({"type":"string"});
                required.extend(["old_text", "new_text"]);
            }
            "grep" | "find" => {
                properties["pattern"] = json!({"type":"string"});
                required.push("pattern");
            }
            _ => {}
        }
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        self.call_with_abort(arguments, &AtomicBool::new(false))
    }
    fn call_with_abort(
        &self,
        arguments: &str,
        abort: &AtomicBool,
    ) -> std::result::Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let text = |key: &str| args[key].as_str().ok_or_else(|| format!("缺少 {key}"));
        let path = resolve_inside(&self.root, text("path")?)?;
        if abort.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        match self.kind {
            "read" => {
                let data = read_workspace_file(&self.root, text("path")?)?;
                let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
                Ok(data
                    .lines()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, s)| format!("{}: {s}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "write" => {
                let content = text("content")?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::write(&path, content).map_err(|e| e.to_string())?;
                Ok(format!(
                    "已写入 {}（{} bytes）",
                    path.display(),
                    content.len()
                ))
            }
            "edit" => {
                let original = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let (body, bom) = strip_bom(&original);
                let ending = detect_line_ending(body);
                let normalized = normalize_to_lf(body);
                let old = normalize_to_lf(text("old_text")?);
                if old.is_empty() {
                    return Err("old_text 不能为空".into());
                }
                let count = normalized.matches(&old).count();
                if count != 1 {
                    return Err(format!("old_text 匹配 {count} 次，需要精确匹配一次"));
                }
                let mut updated = normalized
                    .replacen(&old, &normalize_to_lf(text("new_text")?), 1)
                    .replace('\n', ending);
                if bom {
                    updated.insert(0, '\u{FEFF}');
                }
                if fs::read_to_string(&path).map_err(|e| e.to_string())? != original {
                    return Err("文件已被其他程序修改，请重新读取".into());
                }
                fs::write(&path, updated).map_err(|e| e.to_string())?;
                Ok(format!("已修改 {}", path.display()))
            }
            "ls" => {
                let mut entries = fs::read_dir(&path)
                    .map_err(|e| e.to_string())?
                    .collect::<std::io::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                entries.sort_by_key(|e| e.file_name());
                Ok(entries
                    .iter()
                    .take(500)
                    .map(|e| {
                        format!(
                            "{}{}",
                            e.file_name().to_string_lossy(),
                            if e.path().is_dir() { "/" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            _ => {
                let pattern = text("pattern")?;
                if pattern.is_empty() {
                    return Err("pattern 不能为空".into());
                }
                let mut stack = vec![path];
                let mut matches = Vec::new();
                let mut visited = 0;
                while let Some(path) = stack.pop() {
                    if abort.load(Ordering::Relaxed) {
                        return Err("搜索已取消".into());
                    }
                    visited += 1;
                    if visited > 20_000 || matches.len() >= 200 {
                        break;
                    }
                    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    if meta.is_dir() {
                        if matches!(
                            path.file_name().and_then(|s| s.to_str()),
                            Some(".git" | "target" | "node_modules" | ".hikari")
                        ) {
                            continue;
                        }
                        for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
                            stack.push(entry.map_err(|e| e.to_string())?.path());
                        }
                    } else {
                        let shown = path
                            .strip_prefix(&self.root)
                            .unwrap_or(&path)
                            .display()
                            .to_string();
                        if self.kind == "find" {
                            if shown.contains(pattern) {
                                matches.push(shown);
                            }
                        } else if meta.len() < 1_000_000 {
                            if let Ok(content) = fs::read_to_string(&path) {
                                for (i, line) in content.lines().enumerate() {
                                    if line.contains(pattern) {
                                        matches.push(format!(
                                            "{shown}:{}: {}",
                                            i + 1,
                                            cap_chars(line, 300)
                                        ));
                                        if matches.len() >= 200 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(if matches.is_empty() {
                    "没有匹配".into()
                } else {
                    matches.join("\n")
                })
            }
        }
    }
}

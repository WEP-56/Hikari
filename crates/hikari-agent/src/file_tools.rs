// Hikari adaptation of Pi's read/write/edit/ls/find/grep tools.
// Exact editing uses Pi's original BOM and line-ending helpers.
use super::*;
use crate::pi::edit::{detect_line_ending, normalize_to_lf, strip_bom};
use std::sync::atomic::AtomicU64;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) struct FileTool {
    pub root: PathBuf,
    pub kind: &'static str,
    pub locks: Arc<crate::fs_lock::PathLocks>,
}

fn run_ripgrep(path: &Path, args: &Value, root: &Path, abort: &AtomicBool) -> Result<Option<String>, String> {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else { return Ok(None); };
    let Some(program) = resolve_rg() else { return Ok(None); };
    let mut command = Command::new(program);
    hide_console_window(&mut command);
    command.args(["-n", "--no-heading", "--color", "never", "--max-count", "200", "--hidden", "--no-ignore"]);
    for excluded in ["!.git", "!target", "!node_modules", "!.hikari"] {
        command.args(["-g", excluded]);
    }
    if args.get("case_sensitive").and_then(Value::as_bool) == Some(false) {
        command.arg("-i");
    }
    if args.get("multiline").and_then(Value::as_bool) == Some(true) {
        command.arg("-U");
    }
    if let Some(context) = args.get("context").and_then(Value::as_u64) {
        command.args(["-C", &context.min(20).to_string()]);
    }
    if let Some(glob) = args.get("glob").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        command.args(["-g", glob]);
    }
    command.arg(pattern).arg(path);
    let mut child = command
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("ripgrep 启动失败：{error}"))?;
    let stdout = child.stdout.take().map(|mut reader| thread::spawn(move || { let mut bytes = Vec::new(); let _ = reader.read_to_end(&mut bytes); bytes }));
    let stderr = child.stderr.take().map(|mut reader| thread::spawn(move || { let mut bytes = Vec::new(); let _ = reader.read_to_end(&mut bytes); bytes }));
    let started = Instant::now();
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout.and_then(|handle| handle.join().ok());
            let _ = stderr.and_then(|handle| handle.join().ok());
            return Err("搜索已取消".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.code() == Some(2) {
                    let _ = stdout.and_then(|handle| handle.join().ok());
                    let error = stderr.and_then(|handle| handle.join().ok()).unwrap_or_default();
                    return Err(format!("正则表达式无效：{}", String::from_utf8_lossy(&error).trim()));
                }
                if !status.success() && status.code() != Some(1) {
                    let _ = stdout.and_then(|handle| handle.join().ok());
                    let _ = stderr.and_then(|handle| handle.join().ok());
                    return Err(format!("ripgrep 失败（退出码 {:?}）", status.code()));
                }
                break;
            }
            Ok(None) if started.elapsed() >= Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout.and_then(|handle| handle.join().ok());
                let _ = stderr.and_then(|handle| handle.join().ok());
                return Err("搜索超过 30 秒，已停止".into());
            }
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout.and_then(|handle| handle.join().ok());
                let _ = stderr.and_then(|handle| handle.join().ok());
                return Err(format!("搜索进程中断：{error}"));
            }
        }
    }
    let _ = child.wait();
    let output = stdout.and_then(|handle| handle.join().ok()).unwrap_or_default();
    let text = String::from_utf8_lossy(&output);
    let mut lines = text.lines().take(200).map(|line| {
        let (file, rest) = split_rg_line(line);
        let relative = Path::new(file).strip_prefix(root).unwrap_or(Path::new(file));
        format!("{}:{rest}", relative.display())
    }).collect::<Vec<_>>();
    if lines.is_empty() { lines.push("没有匹配".into()); }
    Ok(Some(lines.join("\n")))
}

fn resolve_rg() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut candidates = Vec::new();
    let binary = if cfg!(windows) { "rg.exe" } else { "rg" };
    candidates.push(exe.parent()?.join("vendor/ripgrep").join(binary));
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest).join("vendor/ripgrep").join(binary));
    }
    candidates.into_iter().find(|path| path.is_file()).or_else(|| {
        let probe = if cfg!(windows) { "where" } else { "which" };
        let mut command = Command::new(probe);
        hide_console_window(&mut command);
        let output = command.arg("rg").output().ok()?;
        if !output.status.success() { return None; }
        String::from_utf8(output.stdout).ok()?.lines().next().map(PathBuf::from)
    })
}

// 后台调用控制台程序（rg / where.exe）时不创建控制台窗口，否则 release 下会闪一下黑框。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

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
            _ => "在工作区搜索文本；系统有 ripgrep 时 pattern 支持正则，否则使用字面文本。返回路径和行号。",
        }
    }
    fn parameters(&self) -> Value {
        let mut properties = json!({"path":{"type":"string"}});
        let mut required = vec!["path"];
        match self.kind {
            "read" => {
                properties["offset"] = json!({"type":"integer","minimum":1});
                properties["limit"] = json!({"type":"integer","minimum":1,"maximum":1000});
            }
            "write" => {
                properties["content"] = json!({"type":"string"});
                required.push("content");
            }
            "edit" => {
                properties["old_text"] = json!({"type":"string"});
                properties["new_text"] = json!({"type":"string"});
                properties["replace_all"] = json!({"type":"boolean","default":false});
                required.extend(["old_text", "new_text"]);
            }
            "grep" | "find" => {
                properties["pattern"] = json!({"type":"string"});
                if self.kind == "grep" {
                    properties["glob"] = json!({"type":"string"});
                    properties["case_sensitive"] = json!({"type":"boolean","default":true});
                    properties["multiline"] = json!({"type":"boolean","default":false});
                    properties["context"] = json!({"type":"integer","minimum":0,"maximum":20});
                }
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
        self.call_authorized(arguments, abort, None)
    }
    fn call_authorized(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        target: Option<&Path>,
    ) -> std::result::Result<String, String> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        let text = |key: &str| args[key].as_str().ok_or_else(|| format!("缺少 {key}"));
        let path = authorized_path(&self.root, text("path")?, target)?;
        if abort.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        match self.kind {
            "read" => {
                let data = read_text_file(&path)?;
                let total = data.lines().count();
                let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
                if args["__hikari_legacy_raw"] == true {
                    return Ok(data.lines().skip(offset - 1).collect::<Vec<_>>().join("\n"));
                }
                let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
                if offset > total && total > 0 {
                    return Err(format!("偏移超过文件末尾（共 {total} 行）"));
                }
                let end = offset.saturating_add(limit).saturating_sub(1).min(total);
                let mut output = data
                    .lines()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, s)| format!("{}: {s}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                if end < total {
                    output.push_str(&format!("\n（文件共 {total} 行，已显示第 {offset}-{end} 行）"));
                }
                Ok(output)
            }
            "write" => {
                let lock = self.locks.acquire(&path);
                let started = Instant::now();
                let _guard = loop {
                    if let Ok(guard) = lock.try_lock() { break guard; }
                    if started.elapsed() >= Duration::from_secs(10) { return Err("该文件正被另一个任务修改，请稍后重试".into()); }
                    std::thread::sleep(Duration::from_millis(20));
                };
                let content = text("content")?;
                if content.len() > 16 * 1024 * 1024 {
                    return Err("写入内容超过 16 MiB 上限".into());
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let tmp = temporary_path(&path);
                fs::write(&tmp, content).map_err(|e| e.to_string())?;
                atomic_replace(&tmp, &path)?;
                Ok(format!(
                    "已写入 {}（{} bytes）",
                    path.display(),
                    content.len()
                ))
            }
            "edit" => {
                let lock = self.locks.acquire(&path);
                let started = Instant::now();
                let _guard = loop {
                    if let Ok(guard) = lock.try_lock() { break guard; }
                    if started.elapsed() >= Duration::from_secs(10) { return Err("该文件正被另一个任务修改，请稍后重试".into()); }
                    std::thread::sleep(Duration::from_millis(20));
                };
                let original = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let (body, bom) = strip_bom(&original);
                let ending = detect_line_ending(body);
                let normalized = normalize_to_lf(body);
                let old = normalize_to_lf(text("old_text")?);
                if old.is_empty() {
                    return Err("old_text 不能为空".into());
                }
                let count = normalized.matches(&old).count();
                let replace_all = args["replace_all"].as_bool().unwrap_or(false);
                if count == 0 || (count != 1 && !replace_all) {
                    return Err(format!("old_text 匹配 {count} 次，需要精确匹配一次"));
                }
                let mut updated = normalized
                    .replacen(&old, &normalize_to_lf(text("new_text")?), if replace_all { usize::MAX } else { 1 })
                    .replace('\n', ending);
                if bom {
                    updated.insert(0, '\u{FEFF}');
                }
                if fs::read_to_string(&path).map_err(|e| e.to_string())? != original {
                    return Err("文件已被其他程序修改，请重新读取".into());
                }
                let tmp = temporary_path(&path);
                fs::write(&tmp, updated).map_err(|e| e.to_string())?;
                atomic_replace(&tmp, &path)?;
                Ok(format!("已修改 {}", path.display()))
            }
            "ls" => {
                let mut entries = fs::read_dir(&path)
                    .map_err(|e| e.to_string())?
                    .collect::<std::io::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                entries.sort_by_key(|e| e.file_name());
                let truncated = entries.len() > 500;
                let mut output = entries
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
                    .join("\n");
                if truncated { output.push_str("\n…列表已截断，仅显示前 500 项"); }
                Ok(output)
            }
            _ => {
                let pattern = text("pattern")?;
                if pattern.is_empty() {
                    return Err("pattern 不能为空".into());
                }
                let mut fallback_note = false;
                if self.kind == "grep" {
                    match run_ripgrep(&path, &args, &self.root, abort)? {
                        Some(output) => return Ok(output),
                        None => fallback_note = true,
                    }
                }
                let mut stack = vec![path];
                let mut matches = Vec::new();
                let mut visited = 0;
                let mut skipped = 0;
                let mut truncated = false;
                while let Some(path) = stack.pop() {
                    if abort.load(Ordering::Relaxed) {
                        return Err("搜索已取消".into());
                    }
                    // A junction discovered during traversal must not widen the approved root.
                    if !crate::permission::resolve_path(&self.root, &path.to_string_lossy())?
                        .starts_with(
                            target
                                .unwrap_or(&self.root)
                                .canonicalize()
                                .unwrap_or_else(|_| target.unwrap_or(&self.root).to_owned()),
                        )
                    {
                        continue;
                    }
                    visited += 1;
                    if visited > 20_000 || matches.len() >= 200 {
                        truncated = true;
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
                            } else { skipped += 1; }
                        } else { skipped += 1;
                        }
                    }
                }
                let mut output = if matches.is_empty() { "没有匹配".into() } else { matches.join("\n") };
                if truncated { output.push_str(&format!("\n…结果被截断，共扫描 {visited} 个文件，仅显示前 200 条匹配")); }
                if skipped > 0 { output.push_str(&format!("\n另有 {skipped} 个文件因过大/二进制/编码问题被跳过")); }
                if fallback_note { output.push_str("\n未找到 ripgrep，已使用内置字面量搜索（不支持正则、glob、大小写/多行/上下文参数）"); }
                Ok(output)
            }
        }
    }
}

fn atomic_replace(tmp: &Path, target: &Path) -> Result<(), String> {
    fs::rename(tmp, target).map_err(|error| {
        let _ = fs::remove_file(tmp);
        error.to_string()
    })
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file");
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.hikari-tmp.{}.{}", std::process::id(), seq))
}

fn split_rg_line(line: &str) -> (&str, &str) {
    // rg emits path:line:content. Find the first colon followed by digits,
    // which handles Windows drive letters without making the path absolute.
    let bytes = line.as_bytes();
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index] == b':' && bytes[index + 1..].iter().take_while(|byte| byte.is_ascii_digit()).count() > 0 {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() { end += 1; }
            if end < bytes.len() && bytes[end] == b':' {
                return (&line[..index], &line[end + 1..]);
            }
        }
    }
    (line, "")
}

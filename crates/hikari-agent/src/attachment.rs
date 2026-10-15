//! Host-owned, content-addressed resources. Sessions persist references, never base64 blobs.
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf, sync::Mutex};

static IMPORT_LOCK: Mutex<()> = Mutex::new(());

pub const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub path: PathBuf,
    pub size: u64,
}
impl Attachment {
    pub fn data_url(&self) -> Result<String, String> {
        if self.size > MAX_ATTACHMENT_BYTES as u64 {
            return Err("附件过大".into());
        }
        let bytes = read_bounded(&self.path)?;
        if bytes.len() as u64 != self.size || format!("{:x}", Sha256::digest(&bytes)) != self.id {
            return Err("附件内容已改变".into());
        }
        Ok(format!(
            "data:{};base64,{}",
            self.mime_type,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }
    pub fn is_image(&self) -> bool {
        matches!(
            self.mime_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        )
    }
}
#[derive(Clone)]
pub struct AttachmentStore {
    root: PathBuf,
}
impl AttachmentStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn import(&self, name: &str, mime: &str, bytes: &[u8]) -> Result<Attachment, String> {
        if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err("附件为空或超过 20 MiB".into());
        }
        let mime = mime.to_ascii_lowercase();
        let ext = match mime.as_str() {
            "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "png",
            "image/jpeg" if bytes.starts_with(b"\xff\xd8\xff") => "jpg",
            "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
                "webp"
            }
            "image/gif" if bytes.starts_with(b"GIF8") => "gif",
            m if !m.starts_with("image/") && !m.contains(['\r', '\n', ';']) => "bin",
            _ => return Err("不支持的图片格式或文件内容不匹配".into()),
        };
        // Serialize imports across cloned stores: a concurrent reader must never
        // mistake a partially-written identical resource for a hash collision.
        let _guard = IMPORT_LOCK.lock().map_err(|_| "附件存储锁不可用")?;
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let id = format!("{:x}", Sha256::digest(bytes));
        let path = self.root.join(format!("{id}.{ext}"));
        if path.exists() && fs::read(&path).map_err(|e| e.to_string())? != bytes {
            return Err("附件存储冲突".into());
        }
        if !path.exists() {
            fs::write(&path, bytes).map_err(|e| e.to_string())?;
        }
        Ok(Attachment {
            id,
            name: name.chars().take(200).collect(),
            mime_type: mime,
            path,
            size: bytes.len() as u64,
        })
    }
    pub fn import_file(&self, path: &std::path::Path) -> Result<Attachment, String> {
        if fs::metadata(path).map_err(|e| e.to_string())?.len() > MAX_ATTACHMENT_BYTES as u64 {
            return Err("附件超过 20 MiB".into());
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => return Err("请选择 PNG、JPEG、WebP 或 GIF 图片".into()),
        };
        self.import(
            &path.file_name().unwrap_or_default().to_string_lossy(),
            mime,
            &read_bounded(path)?,
        )
    }
}

fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("附件不可用：{e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err("附件超过 20 MiB".into());
    }
    Ok(bytes)
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolFailure {
    pub kind: String,
    pub message: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolOutput {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ToolFailure>,
    pub truncated: bool,
    pub retryable: bool,
}
impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
    pub fn error(message: String) -> Self {
        Self {
            text: message.clone(),
            failure: Some(ToolFailure {
                kind: "execution".into(),
                message,
            }),
            ..Self::default()
        }
    }
}

impl AttachmentStore {
    /// Resolve only an image already owned by this store; model-supplied paths are never accepted.
    pub fn get_image(&self, id: &str) -> Result<Attachment, String> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("图片附件 id 无效".into());
        }
        let root = self.root.canonicalize().map_err(|_| "附件存储不可用")?;
        for (ext, mime) in [
            ("png", "image/png"),
            ("jpg", "image/jpeg"),
            ("webp", "image/webp"),
            ("gif", "image/gif"),
        ] {
            let candidate = root.join(format!("{id}.{ext}"));
            if !candidate.exists() {
                continue;
            }
            let path = candidate.canonicalize().map_err(|e| e.to_string())?;
            if path.parent() != Some(root.as_path()) {
                return Err("图片附件不在所属存储中".into());
            }
            let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            let attachment = Attachment {
                id: id.into(),
                name: format!("{id}.{ext}"),
                mime_type: mime.into(),
                path,
                size,
            };
            attachment.data_url()?;
            return Ok(attachment);
        }
        Err("本会话附件存储中没有该图片".into())
    }
}

/// Protocol-native image blocks; failures stay visible to the model.
pub fn content_parts(text: &str, attachments: &[Attachment], protocol: &str) -> Value {
    let mut parts = Vec::new();
    if !text.is_empty() || attachments.is_empty() {
        parts.push(serde_json::json!({"type":if protocol=="responses" {"input_text"} else {"text"},"text":if text.is_empty() { "（空消息）" } else { text }}));
    }
    for attachment in attachments {
        // Stable host reference lets the model delegate an existing image by id.
        parts.push(serde_json::json!({"type":if protocol=="responses" {"input_text"} else {"text"},"text":format!("[附件 {}，id: {}]", attachment.name, attachment.id)}));
        let block = if attachment.is_image() {
            match attachment.data_url() {
                Ok(url) => match protocol {
                    "responses" => serde_json::json!({"type":"input_image","image_url":url}),
                    "anthropic" => {
                        serde_json::json!({"type":"image","source":{"type":"base64","media_type":attachment.mime_type,"data":url.split_once(',').unwrap().1}})
                    }
                    _ => serde_json::json!({"type":"image_url","image_url":{"url":url}}),
                },
                Err(error) => {
                    serde_json::json!({"type":if protocol=="responses" {"input_text"} else {"text"},"text":format!("[{}: {error}]",attachment.name)})
                }
            }
        } else {
            serde_json::json!({"type":if protocol=="responses" {"input_text"} else {"text"},"text":format!("[文件 {}: {}，{} bytes]",attachment.name,attachment.id,attachment.size)})
        };
        parts.push(block);
    }
    Value::Array(parts)
}

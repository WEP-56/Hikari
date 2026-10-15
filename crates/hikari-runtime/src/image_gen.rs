//! OpenAI-compatible image generation. A generation POST is issued exactly once.
use base64::{engine::general_purpose::STANDARD, Engine};
use hikari_agent::{
    attachment::{Attachment, AttachmentStore, ToolOutput},
    tool::Tool,
    ToolRegistry,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const MAX_IMAGE_BYTES: usize = hikari_agent::attachment::MAX_ATTACHMENT_BYTES;
const MAX_RESPONSE_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageGenConfig {
    pub enabled: bool,
    /// Full OpenAI-compatible generations endpoint, including /images/generations.
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub api_key_env: String,
    pub timeout_seconds: u64,
}
impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "https://api.openai.com/v1/images/generations".into(),
            model: "gpt-image-1".into(),
            api_key: String::new(),
            api_key_env: "OPENAI_API_KEY".into(),
            timeout_seconds: 180,
        }
    }
}
impl ImageGenConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_url(&self.endpoint)?;
        if self.model.trim().is_empty() {
            return Err("图片生成模型不能为空".into());
        }
        if !(1..=600).contains(&self.timeout_seconds) {
            return Err("图片生成超时需要在 1–600 秒内".into());
        }
        if self.api_key.contains(['\r', '\n']) {
            return Err("图片生成密钥不能包含换行".into());
        }
        Ok(())
    }
}

fn validate_url(url: &str) -> Result<(), String> {
    let authority = url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?']).next().unwrap_or_default())
        .unwrap_or_default();
    if !(url.starts_with("http://") || url.starts_with("https://"))
        || authority.is_empty()
        || authority.contains('@')
        || url.contains(['\r', '\n', '#'])
    {
        return Err("图片地址需要不含凭据的 HTTP(S) URL".into());
    }
    Ok(())
}

pub fn register(
    config: ImageGenConfig,
    store: AttachmentStore,
    registry: &mut ToolRegistry,
) -> Result<(), String> {
    config.validate()?;
    if config.enabled {
        registry.register(Arc::new(ImageGenTool { config, store }))?;
    }
    Ok(())
}

struct ImageGenTool {
    config: ImageGenConfig,
    store: AttachmentStore,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateArgs {
    prompt: String,
    size: Option<String>,
    quality: Option<String>,
    output_format: Option<String>,
    background: Option<String>,
}

impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_gen"
    }
    fn description(&self) -> &str {
        "使用已配置的 OpenAI 兼容图片提供商生成一张图片并保存为附件。生成可能收费，不会自动重试失败或超时的请求。返回附件引用，图片由聊天界面展示。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "prompt":{"type":"string","description":"完整图片描述"},
            "size":{"type":"string","description":"提供商支持的图片尺寸，例如 auto、1024x1024、1536x1024、1024x1536"},
            "quality":{"type":"string","description":"提供商支持的质量，例如 auto、low、medium、high"},
            "output_format":{"type":"string","enum":["png","jpeg","webp"]},
            "background":{"type":"string","enum":["auto","opaque","transparent"]}
        },"required":["prompt"],"additionalProperties":false})
    }
    fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_with_abort(arguments, &AtomicBool::new(false))
    }
    fn call_with_abort(&self, arguments: &str, abort: &AtomicBool) -> Result<String, String> {
        Ok(self.generate(arguments, abort)?.text)
    }
    fn call_output(
        &self,
        arguments: &str,
        abort: &AtomicBool,
        _target: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        self.generate(arguments, abort)
    }
}

impl ImageGenTool {
    fn generate(&self, arguments: &str, abort: &AtomicBool) -> Result<ToolOutput, String> {
        self.config.validate()?;
        let args: GenerateArgs =
            serde_json::from_str(arguments).map_err(|e| format!("图片参数错误：{e}"))?;
        if args.prompt.trim().is_empty() || args.prompt.chars().count() > 32_000 {
            return Err("图片描述需要在 1–32000 字符内".into());
        }
        if args
            .output_format
            .as_deref()
            .is_some_and(|value| !["png", "jpeg", "webp"].contains(&value))
        {
            return Err("图片格式需要是 png、jpeg 或 webp".into());
        }
        if args
            .background
            .as_deref()
            .is_some_and(|value| !["auto", "opaque", "transparent"].contains(&value))
        {
            return Err("图片背景需要是 auto、opaque 或 transparent".into());
        }
        check_abort(abort)?;
        let key = if self.config.api_key.trim().is_empty() {
            std::env::var(&self.config.api_key_env).unwrap_or_default()
        } else {
            self.config.api_key.clone()
        };
        if key.trim().is_empty() || key.contains(['\r', '\n']) {
            return Err("请在其它提供商设置中配置图片生成 API Key 或密钥环境变量".into());
        }
        let mut body = json!({"model": self.config.model, "prompt":args.prompt, "n":1});
        for (name, value) in [
            ("size", args.size),
            ("quality", args.quality),
            ("output_format", args.output_format.clone()),
            ("background", args.background),
        ] {
            if let Some(value) = value {
                body[name] = Value::String(value);
            }
        }
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(self.config.timeout_seconds))
            // Do not resend a paid POST or forward its credentials through redirects.
            .redirects(0)
            .build();
        let response = http
            .post(&self.config.endpoint)
            .set("Authorization", &format!("Bearer {}", key.trim()))
            .send_json(body)
            .map_err(generation_error)?;
        let bytes = read_bounded(response, MAX_RESPONSE_BYTES, abort)?;
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "图片 API 返回无效 JSON；请求未自动重试".to_string())?;
        check_abort(abort)?;
        let images = response
            .get("data")
            .and_then(Value::as_array)
            .filter(|data| !data.is_empty())
            .ok_or("图片 API 没有返回图片；请求未自动重试")?;
        if images.len() != 1 {
            return Err("图片 API 返回数量与请求不符；请求未自动重试".into());
        }
        let item = &images[0];
        let bytes = if let Some(encoded) = item.get("b64_json").and_then(Value::as_str) {
            if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
                return Err("图片超过 20 MiB 限制".into());
            }
            STANDARD
                .decode(encoded)
                .map_err(|_| "图片 API 返回无效 base64".to_string())?
        } else if let Some(url) = item.get("url").and_then(Value::as_str) {
            validate_url(url)?;
            check_abort(abort)?;
            let response = http.get(url).call().map_err(|error| match error {
                ureq::Error::Status(status, _) => {
                    format!("图片下载失败（HTTP {status}）；生成请求未自动重试")
                }
                _ => "图片下载失败；生成请求未自动重试".into(),
            })?;
            read_bounded(response, MAX_IMAGE_BYTES, abort)?
        } else {
            return Err("图片 API 缺少 b64_json 或 url".into());
        };
        check_abort(abort)?;
        let (mime, extension) = image_type(&bytes)?;
        let attachment: Attachment =
            self.store
                .import(&format!("generated.{extension}"), mime, &bytes)?;
        let structured = json!({"status":"completed","model": self.config.model,"attachments":[attachment],"revised_prompt":item.get("revised_prompt")});
        Ok(ToolOutput {
            text: serde_json::to_string(&structured).map_err(|e| e.to_string())?,
            structured: Some(structured),
            attachments: vec![attachment],
            retryable: false,
            ..Default::default()
        })
    }
}

fn check_abort(abort: &AtomicBool) -> Result<(), String> {
    if abort.load(Ordering::Relaxed) {
        Err("图片生成已取消；已发送的请求可能仍计费，请勿自动重试".into())
    } else {
        Ok(())
    }
}
fn generation_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, _) => format!("图片生成失败（HTTP {status}）；请求未自动重试"),
        _ => "图片生成连接失败或超时，结果未知；请求未自动重试".into(),
    }
}
fn read_bounded(
    response: ureq::Response,
    max: usize,
    abort: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 32 * 1024];
    loop {
        check_abort(abort)?;
        let read = reader
            .read(&mut chunk)
            .map_err(|_| "读取图片响应失败；请求未自动重试".to_string())?;
        if read == 0 {
            break;
        }
        if bytes.len() + read > max {
            return Err("图片响应超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}
fn image_type(bytes: &[u8]) -> Result<(&'static str, &'static str), String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("图片为空或超过 20 MiB 限制".into());
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Ok(("image/webp", "webp"))
    } else {
        Err("图片 API 返回不支持的图片格式（需要 PNG、JPEG 或 WebP）".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    fn fixture(
        status: u16,
        body: Vec<u8>,
    ) -> (
        String,
        std::thread::JoinHandle<(String, BTreeMap<String, String>, Vec<u8>)>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/images/generations",
            listener.local_addr().unwrap()
        );
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut headers = BTreeMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
            }
            let size = headers
                .get("content-length")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).unwrap();
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
            (request, headers, bytes)
        });
        (url, task)
    }

    fn tool(config: ImageGenConfig, root: &Path) -> ImageGenTool {
        ImageGenTool {
            config,
            store: AttachmentStore::new(root.to_path_buf()),
        }
    }

    #[test]
    fn generations_post_imports_a_recoverable_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\nfixture-image";
        let (endpoint, server) = fixture(
            200,
            serde_json::to_vec(
                &json!({"data":[{"b64_json":STANDARD.encode(png),"revised_prompt":"refined"}]}),
            )
            .unwrap(),
        );
        let tool = tool(
            ImageGenConfig {
                enabled: true,
                endpoint,
                api_key: "fixture-secret".into(),
                ..Default::default()
            },
            dir.path(),
        );
        let output = tool
            .generate(
                r#"{"prompt":"a blue cat","size":"1024x1024","quality":"high"}"#,
                &AtomicBool::new(false),
            )
            .unwrap();
        let (request, headers, body) = server.join().unwrap();
        assert!(request.starts_with("POST /v1/images/generations "));
        assert_eq!(headers["authorization"], "Bearer fixture-secret");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            json!({"model":"gpt-image-1","n":1,"prompt":"a blue cat","size":"1024x1024","quality":"high"})
        );
        assert_eq!(output.attachments.len(), 1);
        assert_eq!(std::fs::read(&output.attachments[0].path).unwrap(), png);
        assert!(output.attachments[0]
            .data_url()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(output.structured.unwrap()["revised_prompt"], "refined");
        assert!(!output.retryable);
        assert!(!output.text.contains("fixture-secret"));
        assert!(!output.text.contains("b64_json"));
    }

    #[test]
    fn downloads_url_results_without_forwarding_api_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = b"\xff\xd8\xfffixture";
        let (image_url, image_server) = fixture(200, jpeg.to_vec());
        let (endpoint, server) = fixture(
            200,
            serde_json::to_vec(&json!({"data":[{"url":image_url}]})).unwrap(),
        );
        let tool = tool(
            ImageGenConfig {
                endpoint,
                api_key: "secret".into(),
                ..Default::default()
            },
            dir.path(),
        );
        let output = tool
            .generate(r#"{"prompt":"cat"}"#, &AtomicBool::new(false))
            .unwrap();
        server.join().unwrap();
        let (request, headers, _) = image_server.join().unwrap();
        assert!(request.starts_with("GET "));
        assert!(!headers.contains_key("authorization"));
        assert_eq!(output.attachments[0].mime_type, "image/jpeg");
        assert_eq!(std::fs::read(&output.attachments[0].path).unwrap(), jpeg);
    }

    #[test]
    fn failed_paid_request_is_not_retried_or_echoed() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, server) =
            fixture(500, br#"{"error":"sensitive provider message"}"#.to_vec());
        let tool = tool(
            ImageGenConfig {
                endpoint,
                api_key: "secret".into(),
                ..Default::default()
            },
            dir.path(),
        );
        let error = tool
            .generate(r#"{"prompt":"cat"}"#, &AtomicBool::new(false))
            .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("HTTP 500"));
        assert!(error.contains("未自动重试"));
        assert!(!error.contains("sensitive"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn abort_and_malformed_responses_produce_no_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(
            ImageGenConfig {
                endpoint: "http://127.0.0.1:1/never".into(),
                api_key: "secret".into(),
                ..Default::default()
            },
            dir.path(),
        );
        assert!(tool
            .generate(r#"{"prompt":"cat"}"#, &AtomicBool::new(true))
            .unwrap_err()
            .contains("已取消"));
        let (endpoint, server) = fixture(
            200,
            serde_json::to_vec(
                &json!({"data":[{"b64_json":STANDARD.encode(b"<svg>not a bitmap</svg>")}]}),
            )
            .unwrap(),
        );
        let tool = ImageGenTool {
            config: ImageGenConfig {
                endpoint,
                ..tool.config
            },
            store: tool.store,
        };
        assert!(tool
            .generate(r#"{"prompt":"cat"}"#, &AtomicBool::new(false))
            .unwrap_err()
            .contains("不支持的图片格式"));
        server.join().unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn config_defaults_roundtrip_and_reject_invalid_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::ConfigStore::new(dir.path());
        let mut config = crate::AppConfig::default();
        config.image_gen.model = "compatible-model".into();
        config.image_gen.api_key_env = "CUSTOM_IMAGE_KEY".into();
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.image_gen.model, "compatible-model");
        assert_eq!(loaded.image_gen.api_key_env, "CUSTOM_IMAGE_KEY");
        assert!(!loaded.image_gen.enabled);
        for endpoint in [
            "file:///x",
            "https://secret@example.com/x",
            "https://",
            "http://host/x\r\nheader:value",
        ] {
            config.image_gen.endpoint = endpoint.into();
            assert!(config.validate().is_err());
        }
        let default: ImageGenConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(default.timeout_seconds, 180);
        assert_eq!(default.model, "gpt-image-1");
    }
}

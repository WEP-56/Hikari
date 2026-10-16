use hikari_agent::protocols::{request_body, Decoder};
use hikari_agent::{
    ApiProtocol, Context, Message, ProtocolProvider, Provider, ProviderConfig, ProviderMetadata,
    StopReason, StreamEvent,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;

fn image_attachment(root: &std::path::Path) -> hikari_agent::Attachment {
    hikari_agent::attachment::AttachmentStore::new(root.to_path_buf())
        .import(
            "preview.png",
            "image/png",
            b"\x89PNG\r\n\x1a\nfixture-image",
        )
        .unwrap()
}

fn wire_body(protocol: ApiProtocol, messages: &[Message]) -> Value {
    request_body(
        &ProviderConfig {
            protocol,
            ..Default::default()
        },
        &Context {
            system_prompt: "base",
            messages,
            tools: &[],
        },
    )
}

fn block_of_type<'a>(blocks: &'a Value, kind: &str) -> &'a Value {
    blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == kind)
        .unwrap()
}

#[test]
fn image_only_user_input_uses_native_blocks_in_all_protocols() {
    let root = tempfile::tempdir().unwrap();
    let attachment = image_attachment(root.path());
    let mut message = Message::user("");
    message.set_attachments(vec![attachment.clone()]);
    let data_url = attachment.data_url().unwrap();
    let chat = wire_body(ApiProtocol::ChatCompletions, &[message.clone()]);
    assert_eq!(
        block_of_type(&chat["messages"][1]["content"], "image_url"),
        &json!({"type":"image_url","image_url":{"url":data_url}})
    );
    let responses = wire_body(ApiProtocol::Responses, &[message.clone()]);
    assert_eq!(
        block_of_type(&responses["input"][0]["content"], "input_image"),
        &json!({"type":"input_image","image_url":data_url})
    );
    let anthropic = wire_body(ApiProtocol::Anthropic, &[message]);
    assert_eq!(
        block_of_type(&anthropic["messages"][0]["content"], "image"),
        &json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":data_url.split_once(',').unwrap().1}})
    );
    for content in [
        &chat["messages"][1]["content"],
        &responses["input"][0]["content"],
        &anthropic["messages"][0]["content"],
    ] {
        assert!(content
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block["text"].as_str())
            .any(|text| text.contains(&attachment.id)));
        assert!(!content
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["text"] == ""));
    }
}

#[test]
fn chat_tool_images_follow_every_result_of_the_assistant_call_group() {
    let root = tempfile::tempdir().unwrap();
    let image = image_attachment(root.path());
    let mut assistant = Message::assistant("");
    if let Message::Assistant { tool_calls, .. } = &mut assistant {
        *tool_calls = vec![
            hikari_agent::ToolCall {
                id: "image-call".into(),
                name: "image_gen".into(),
                arguments: "{}".into(),
            },
            hikari_agent::ToolCall {
                id: "read-call".into(),
                name: "read".into(),
                arguments: "{}".into(),
            },
        ];
    }
    let image_result = Message::with_output(
        "image-call",
        "image_gen",
        hikari_agent::ToolOutput {
            text: "generated".into(),
            attachments: vec![image],
            ..Default::default()
        },
    );
    let messages = [
        Message::user("go"),
        assistant,
        image_result,
        Message::ToolCatalog { tools: vec![] },
        Message::tool_result("read-call", "read", "read complete", false),
        Message::assistant("finished"),
    ];
    let chat = wire_body(ApiProtocol::ChatCompletions, &messages);
    let wire = chat["messages"].as_array().unwrap();
    let roles: Vec<_> = wire
        .iter()
        .map(|item| item["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "system",
            "user",
            "assistant",
            "tool",
            "tool",
            "user",
            "assistant"
        ]
    );
    assert_eq!(wire[3]["tool_call_id"], "image-call");
    assert_eq!(wire[4]["tool_call_id"], "read-call");
    assert!(wire[5]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("image-call"));
    assert_eq!(
        block_of_type(&wire[5]["content"], "image_url")["type"],
        "image_url"
    );
    // The same ordering must hold while awaiting the assistant's next response.
    let pending = wire_body(ApiProtocol::ChatCompletions, &messages[..5]);
    assert_eq!(pending["messages"][4]["role"], "tool");
    assert_eq!(pending["messages"][5]["role"], "user");
}

#[test]
fn rich_tool_result_preserves_failure_structured_data_and_native_images() {
    let root = tempfile::tempdir().unwrap();
    let image = image_attachment(root.path());
    let result = Message::with_output(
        "call-1",
        "preview",
        hikari_agent::ToolOutput {
            text: "partial preview".into(),
            structured: Some(json!({"verified":false,"pages":2})),
            attachments: vec![image],
            failure: Some(hikari_agent::ToolFailure {
                kind: "provider_error".into(),
                message: "partial response".into(),
            }),
            truncated: true,
            retryable: false,
        },
    );
    let restored: Message = serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
    assert_eq!(restored, result);
    let chat = wire_body(ApiProtocol::ChatCompletions, &[restored.clone()]);
    let responses = wire_body(ApiProtocol::Responses, &[restored.clone()]);
    let anthropic = wire_body(ApiProtocol::Anthropic, &[restored]);
    for text in [
        chat["messages"][1]["content"].as_str().unwrap(),
        responses["input"][0]["output"][0]["text"].as_str().unwrap(),
        anthropic["messages"][0]["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap(),
    ] {
        let metadata: Value = serde_json::from_str(text).unwrap();
        assert_eq!(metadata["text"], "partial preview");
        assert_eq!(metadata["structured"], json!({"verified":false,"pages":2}));
        assert_eq!(metadata["failure"]["kind"], "provider_error");
        assert_eq!(metadata["is_error"], true);
        assert_eq!(metadata["truncated"], true);
        assert_eq!(metadata["retryable"], false);
    }
    assert_eq!(
        block_of_type(&responses["input"][0]["output"], "input_image")["type"],
        "input_image"
    );
    let anthropic_result = &anthropic["messages"][0]["content"][0];
    assert_eq!(anthropic_result["is_error"], true);
    assert_eq!(
        block_of_type(&anthropic_result["content"], "image")["type"],
        "image"
    );
}

#[test]
fn missing_and_changed_images_become_visible_text_without_sending_corrupt_bytes() {
    let root = tempfile::tempdir().unwrap();
    let image = image_attachment(root.path());
    let mut message = Message::user("inspect");
    message.set_attachments(vec![image.clone()]);
    std::fs::write(&image.path, b"replaced").unwrap();
    for protocol in [
        ApiProtocol::ChatCompletions,
        ApiProtocol::Responses,
        ApiProtocol::Anthropic,
    ] {
        let body = wire_body(protocol, &[message.clone()]).to_string();
        assert!(body.contains("附件内容已改变"));
        assert!(!body.contains("base64"));
    }
    std::fs::remove_file(image.path).unwrap();
    assert!(wire_body(ApiProtocol::Responses, &[message])
        .to_string()
        .contains("附件不可用"));
}

#[test]
fn responses_fragmented_call_keeps_native_reasoning_and_replays_result() {
    let mut decoder = Decoder::new(ApiProtocol::Responses);
    let frames = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]}}),
        json!({"type":"response.output_item.added","output_index":25,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":"}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"a.txt\"}"}),
        json!({"type":"response.output_item.done","output_index":25,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":"{\"path\":\"a.txt\"}"}}),
        json!({"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":7}}}),
    ];
    let events: Vec<_> = frames
        .into_iter()
        .flat_map(|frame| decoder.decode(frame).unwrap())
        .collect();
    let arguments: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolDelta { arguments, .. } => Some(arguments.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).unwrap(),
        json!({"path":"a.txt"})
    );
    assert!(events.contains(&StreamEvent::Done(StopReason::ToolUse)));
    let metadata = events
        .into_iter()
        .find_map(|e| match e {
            StreamEvent::Metadata(meta) => Some(meta),
            _ => None,
        })
        .unwrap();
    let messages = [
        Message::User {
            attachments: Vec::new(),
            origin: None,
            text: "读取".into(),
        },
        Message::Assistant {
            origin: None,
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            provider_metadata: Some(metadata),
            usage: None,
        },
        Message::tool_result("call_1", "read", "result", false),
    ];
    let body = request_body(
        &ProviderConfig {
            protocol: ApiProtocol::Responses,
            ..Default::default()
        },
        &Context {
            system_prompt: "base",
            messages: &messages,
            tools: &[],
        },
    );
    assert_eq!(body["input"][1]["encrypted_content"], "opaque");
    assert_eq!(body["input"][2]["id"], "fc_1");
    assert_eq!(
        body["input"][3],
        json!({"type":"function_call_output","call_id":"call_1","output":"result"})
    );
}

#[test]
fn anthropic_signed_thinking_and_multiple_tool_results_roundtrip() {
    let mut decoder = Decoder::new(ApiProtocol::Anthropic);
    let frames = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想一想"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"a","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ];
    let events: Vec<_> = frames
        .into_iter()
        .flat_map(|frame| decoder.decode(frame).unwrap())
        .collect();
    assert!(events.contains(&StreamEvent::Usage {
        input: 9,
        output: 5
    }));
    let metadata = events
        .into_iter()
        .find_map(|e| match e {
            StreamEvent::Metadata(meta) => Some(meta),
            _ => None,
        })
        .unwrap();
    assert_eq!(metadata.items[0]["signature"], "signed");
    assert_eq!(metadata.items[1]["input"], json!({"path":"a"}));
    let messages = [
        Message::user("go"),
        Message::Assistant {
            origin: None,
            text: String::new(),
            thinking: "想一想".into(),
            tool_calls: Vec::new(),
            provider_metadata: Some(metadata),
            usage: None,
        },
        Message::tool_result("a", "read", "done", false),
        Message::tool_result("b", "read", "failed", true),
    ];
    let config = ProviderConfig {
        protocol: ApiProtocol::Anthropic,
        ..Default::default()
    };
    let body = request_body(
        &config,
        &Context {
            system_prompt: "base",
            messages: &messages,
            tools: &[],
        },
    );
    assert_eq!(body["messages"][1]["content"][0]["signature"], "signed");
    assert_eq!(body["messages"][2]["content"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][2]["content"][1]["is_error"], true);
    let cross = request_body(
        &ProviderConfig {
            protocol: ApiProtocol::Responses,
            ..Default::default()
        },
        &Context {
            system_prompt: "base",
            messages: &messages,
            tools: &[],
        },
    );
    assert!(!cross.to_string().contains("signed"));
}

#[test]
fn chat_completions_reads_usage_after_finish_reason_over_http() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut size = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                size = n.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; size];
        reader.read_exact(&mut body).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["stream_options"]["include_usage"], true);
        let body="data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":31,\"completion_tokens\":7}}\n\ndata: [DONE]\n\n";
        let mut stream = stream;
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    let provider = ProtocolProvider::new(
        ProviderConfig {
            base_url: format!("http://{address}/v1"),
            ..Default::default()
        },
        "",
    )
    .unwrap();
    let messages = [Message::user("hi")];
    let mut events = Vec::new();
    provider
        .stream(
            &Context {
                system_prompt: "base",
                messages: &messages,
                tools: &[],
            },
            &AtomicBool::new(false),
            &mut |e| events.push(e),
        )
        .unwrap();
    assert!(events.contains(&StreamEvent::Text("你好".into())));
    assert!(events.contains(&StreamEvent::Usage {
        input: 31,
        output: 7
    }));
    server.join().unwrap();
}

#[test]
fn protocol_metadata_survives_session_serialization() {
    let message = Message::Assistant {
        origin: None,
        text: "answer".into(),
        thinking: "reason".into(),
        tool_calls: Vec::new(),
        provider_metadata: Some(ProviderMetadata {
            protocol: "responses".into(),
            items: vec![json!({"type":"reasoning","encrypted_content":"opaque"})],
        }),
        usage: None,
    };
    assert_eq!(
        message,
        serde_json::from_str::<Message>(&serde_json::to_string(&message).unwrap()).unwrap()
    );
}

#[test]
fn gateway_compatibility_changes_wire_fields_and_reasoning() {
    let context = Context {
        system_prompt: "base",
        messages: &[],
        tools: &[],
    };
    let mut config = ProviderConfig::default();
    config.reasoning_effort = Some("high".into());
    config.compatibility.max_tokens_field = "max_tokens".into();
    config.compatibility.system_role = "developer".into();
    config.compatibility.stream_usage = false;
    config.compatibility.reasoning = false;
    let body = request_body(&config, &context);
    assert_eq!(body["max_tokens"], 8192);
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("stream_options").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["messages"][0]["role"], "developer");
    config.protocol = ApiProtocol::Responses;
    config.compatibility.responses_store = true;
    let body = request_body(&config, &context);
    assert_eq!(body["store"], true);
    assert!(body.get("include").is_none());
    config.compatibility.reasoning = true;
    assert_eq!(
        request_body(&config, &context)["reasoning"]["effort"],
        "high"
    );
    config.protocol = ApiProtocol::Anthropic;
    config.max_output_tokens = 2048;
    assert!(config.validate().is_err());
    config.max_output_tokens = 8192;
    assert_eq!(
        request_body(&config, &context)["thinking"]["budget_tokens"],
        4096
    );
}

#[test]
fn initial_inline_tools_request_already_sends_merged_beta_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = Vec::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            headers.push(line.to_lowercase());
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "system"));
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["defer_loading"] == true));
        assert!(headers
            .iter()
            .any(|h| h == "anthropic-beta: existing-beta,inline-tools-2026-09-15\r\n"));
        let data = "data: {\"type\":\"message_stop\"}\n\n";
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
    });
    let mut config = ProviderConfig {
        protocol: ApiProtocol::Anthropic,
        base_url: format!("http://{address}/v1"),
        ..Default::default()
    };
    config
        .headers
        .insert("anthropic-beta".into(), "existing-beta".into());
    config.compatibility.tool_disclosure =
        hikari_agent::protocols::ToolDisclosure::AnthropicInlineTools;
    let provider = ProtocolProvider::new(config, "").unwrap();
    let tools = vec![hikari_agent::ToolDef {
        name: "tool_search".into(),
        description: "search".into(),
        parameters: json!({"type":"object"}),
    }];
    let messages = vec![
        Message::ToolCatalog {
            tools: tools.clone(),
        },
        Message::user("hi"),
    ];
    provider
        .stream(
            &Context {
                system_prompt: "base",
                messages: &messages,
                tools: &tools,
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
    server.join().unwrap();
}

#[test]
fn model_discovery_uses_endpoint_headers_auth_and_anthropic_pagination() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for page in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                lines.push(line.to_ascii_lowercase());
            }
            assert!(lines[0].starts_with("get /v1/models?limit=100"));
            assert!(lines.iter().any(|line| line == "x-client: hikari\r\n"));
            assert!(lines
                .iter()
                .any(|line| line == "x-api-key: test-credential\r\n"));
            if page == 1 {
                assert!(lines[0].contains("after_id=model-a"));
            }
            let body = if page == 0 {
                json!({"data":[{"id":"model-a"}],"has_more":true,"last_id":"model-a"})
            } else {
                json!({"data":[{"id":"model-b"},{"id":"model-a"}],"has_more":false})
            }
            .to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let config = ProviderConfig {
        protocol: ApiProtocol::Anthropic,
        base_url: format!("http://{address}/v1/messages"),
        headers: std::collections::BTreeMap::from([("X-Client".into(), "hikari".into())]),
        ..Default::default()
    };
    assert_eq!(
        hikari_agent::protocols::fetch_models(&config, "test-credential").unwrap(),
        vec!["model-a", "model-b"]
    );
    server.join().unwrap();
}

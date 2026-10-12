use hikari_agent::ToolRegistry;
use hikari_runtime::{McpManager, McpServerConfig, McpTransport};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[test]
fn streamable_http_session_pagination_tool_error_and_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut headers = BTreeMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
            }
            let len = headers
                .get("content-length")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let mut bytes = vec![0; len];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = if len > 0 {
                serde_json::from_slice(&bytes).unwrap()
            } else {
                json!({})
            };
            captured
                .lock()
                .unwrap()
                .push(body["method"].as_str().unwrap_or("DELETE").to_string());
            let initial = body["method"] == "initialize";
            if !initial {
                assert_eq!(headers.get("mcp-session-id").unwrap(), "session-1");
            }
            let result = match body["method"].as_str() {
                Some("initialize") => {
                    json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                Some("tools/list") if body["params"].get("cursor").is_none() => {
                    json!({"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}],"nextCursor":"page-2"})
                }
                Some("tools/list") => {
                    assert_eq!(body["params"]["cursor"], "page-2");
                    json!({"tools":[{"name":"fail","inputSchema":{"type":"object"}}]})
                }
                Some("tools/call") => {
                    if body["params"]["name"] == "echo" {
                        json!({"content":[{"type":"text","text":body["params"]["arguments"]["text"]}],"structuredContent":{"ok":true}})
                    } else {
                        json!({"isError":true,"content":[{"type":"text","text":"tool failure"}]})
                    }
                }
                _ => json!({}),
            };
            if body["method"] == "notifications/initialized" || first.starts_with("DELETE") {
                write!(
                    stream,
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                continue;
            }
            let response = json!({"jsonrpc":"2.0","id":body["id"],"result":result});
            let data = if body["method"] == "tools/call" {
                format!("event: message\ndata: {response}\n\n")
            } else {
                response.to_string()
            };
            let content_type = if body["method"] == "tools/call" {
                "text/event-stream"
            } else {
                "application/json"
            };
            let session = if initial {
                "Mcp-Session-Id: session-1\r\n"
            } else {
                ""
            };
            write!(stream,"HTTP/1.1 200 OK\r\n{session}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
        }
    });
    let manager = McpManager::default();
    let config = McpServerConfig {
        name: "fixture".into(),
        enabled: true,
        transport: McpTransport::StreamableHttp {
            url: format!("http://{address}/mcp"),
            bearer_token_env: None,
        },
        timeout_seconds: 5,
    };
    manager
        .connect(&config, std::env::temp_dir().as_path())
        .unwrap();
    assert_eq!(manager.statuses()[0].tools.len(), 2);
    let mut tools = ToolRegistry::empty();
    manager.register(&mut tools).unwrap();
    assert!(tools
        .call("mcp_fixture_echo", r#"{"text":"你好"}"#)
        .unwrap()
        .contains("你好"));
    assert!(tools
        .call("mcp_fixture_fail", "{}")
        .unwrap_err()
        .contains("tool failure"));
    manager.disconnect("fixture");
    assert!(tools.call("mcp_fixture_echo", "{}").is_err());
    server.join().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list",
            "tools/call",
            "tools/call",
            "DELETE"
        ]
    );
}

#[cfg(windows)]
#[test]
fn stdio_initializes_and_uses_tools_over_newline_jsonrpc() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("mcp.ps1");
    std::fs::write(&script,r#"
$ErrorActionPreference = 'Stop'
while ($null -ne ($line = [Console]::ReadLine())) {
  $request = $line | ConvertFrom-Json
  if ($null -eq $request.id) { continue }
  $result = switch ($request.method) {
    'initialize' { @{ protocolVersion = '2025-06-18'; capabilities = @{ tools = @{} }; serverInfo = @{ name = 'fixture'; version = '1' } } }
    'tools/list' { @{ tools = @(@{ name = 'echo'; description = 'echo'; inputSchema = @{ type = 'object'; properties = @{ text = @{ type = 'string' } }; required = @('text') } }) } }
    'tools/call' { @{ content = @(@{ type = 'text'; text = $request.params.arguments.text }) } }
  }
  [Console]::WriteLine((@{ jsonrpc = '2.0'; id = $request.id; result = $result } | ConvertTo-Json -Depth 20 -Compress))
}
"#).unwrap();
    let config = McpServerConfig {
        name: "local".into(),
        enabled: true,
        transport: McpTransport::Stdio {
            command: "powershell.exe".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                script.display().to_string(),
            ],
            env: BTreeMap::new(),
        },
        timeout_seconds: 10,
    };
    let manager = McpManager::default();
    manager.connect(&config, temp.path()).unwrap();
    let mut tools = ToolRegistry::empty();
    manager.register(&mut tools).unwrap();
    assert_eq!(
        tools
            .call("mcp_local_echo", r#"{"text":"hikari fixture"}"#)
            .unwrap(),
        "hikari fixture"
    );
    manager.disconnect("local");
    assert_eq!(manager.statuses()[0].state, "disconnected");
}

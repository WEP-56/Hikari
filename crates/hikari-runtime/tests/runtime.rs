use hikari_agent::protocols::{validate_headers, CompatibilityConfig};
use hikari_agent::{Message, ToolRegistry, Usage};
use hikari_runtime::imports::{merge_mcp, parse_mcp_file, McpCandidate};
use hikari_runtime::{AppConfig, ConfigStore, Session, SessionStore, SkillCatalog};
use hikari_runtime::{McpServerConfig, McpTransport};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[test]
fn configuration_and_session_replace_previous_files_without_losing_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let config_store = ConfigStore::new(temp.path());
    let mut config = AppConfig::default();
    config_store.save(&config).unwrap();
    config.persona_name = "Hikari".into();
    config_store.save(&config).unwrap();
    assert_eq!(config_store.load().unwrap().persona_name, "Hikari");
    let store = SessionStore::new(temp.path());
    let transcript = vec![
        Message::user("original"),
        Message::assistant("done"),
        Message::tool_result("a", "read", "evidence", false),
    ];
    let context = vec![
        Message::ContextSummary {
            text: "summary".into(),
        },
        Message::user("current"),
    ];
    let session = Session {
        id: 1,
        title: "title".into(),
        workspace: Some(temp.path().join("workspace")),
        pinned: true,
        transcript: transcript.clone(),
        context: context.clone(),
        retained_until: 1,
        context_base: 0,
        memory_enabled: true,
        usage: Usage {
            input: 10,
            output: 5,
        },
        execution: Default::default(),
    };
    store.save(&[session.clone()]).unwrap();
    store.save(&[session]).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].transcript, transcript);
    assert_eq!(loaded[0].context, context);
    assert_eq!(loaded[0].usage.total(), 15);
    assert_eq!(loaded[0].workspace, Some(temp.path().join("workspace")));
    assert!(loaded[0].pinned);
    fs::write(config_store.path(), b"invalid json").unwrap();
    assert!(config_store.load().is_err());
}

#[test]
fn restart_preserves_todos_and_completes_unknown_tool_results_without_replay() {
    use hikari_agent::{
        execution::{Execution, ExecutionSnapshot, TaskStatus, TodoItem, TodoStatus},
        ToolCall,
    };
    let temp = tempfile::tempdir().unwrap();
    let execution = Execution::new(
        "talk-7".into(),
        ExecutionSnapshot {
            todos: vec![TodoItem {
                id: "one".into(),
                text: "持久待办".into(),
                status: TodoStatus::InProgress,
            }],
            ..Default::default()
        },
    );
    execution
        .begin(temp.path().to_owned(), "fixture".into())
        .unwrap();
    let mut assistant = Message::assistant("");
    if let Message::Assistant { tool_calls, .. } = &mut assistant {
        tool_calls.push(ToolCall {
            id: "unknown".into(),
            name: "shell".into(),
            arguments: r#"{"command":"echo effect"}"#.into(),
        });
    }
    let session = Session {
        id: 7,
        title: "crash".into(),
        workspace: Some(temp.path().to_owned()),
        pinned: false,
        transcript: vec![assistant.clone()],
        context: vec![assistant],
        retained_until: 0,
        context_base: 0,
        memory_enabled: false,
        usage: Usage::default(),
        execution: execution.snapshot(),
    };
    let store = SessionStore::new(temp.path());
    store.save(&[session]).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].execution.todos[0].text, "持久待办");
    assert_eq!(loaded[0].execution.tasks[0].status, TaskStatus::Failed);
    assert!(
        matches!(loaded[0].context.last(),Some(Message::ToolResult {tool_call_id,is_error:true,text,..}) if tool_call_id=="unknown" && text.contains("未自动重跑"))
    );
    store.save(&loaded).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].context.len(), 2);
    assert_eq!(loaded[0].execution.results.len(), 1);
}

#[test]
fn skill_discovery_uses_yaml_and_loads_only_enabled_skills_and_contained_resources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("skills");
    let path = root.join("sample");
    fs::create_dir_all(path.join("references")).unwrap();
    fs::write(path.join("SKILL.md"),"---\nname: sample\ndescription: >-\n  This is a multiline\n  description.\n---\n\nSkill instructions.").unwrap();
    fs::write(path.join("references/helper.md"), "helper data").unwrap();
    fs::write(root.join("outside.md"), "outside").unwrap();
    let roots = vec![PathBuf::from("skills")];
    let catalog = SkillCatalog::discover(temp.path(), &roots, &[]);
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(
        catalog.skills[0].description,
        "This is a multiline description."
    );
    assert!(!catalog.prompt().contains("Skill instructions"));
    let mut tools = ToolRegistry::empty();
    catalog.register(&mut tools).unwrap();
    assert!(tools
        .call("load_skill", r#"{"name":"sample"}"#)
        .unwrap()
        .contains("Skill instructions"));
    assert!(tools
        .call(
            "load_skill",
            r#"{"name":"sample","resource":"references/helper.md"}"#
        )
        .unwrap()
        .contains("helper data"));
    assert!(tools
        .call(
            "load_skill",
            r#"{"name":"sample","resource":"../outside.md"}"#
        )
        .is_err());
    let disabled = SkillCatalog::discover(temp.path(), &roots, &[catalog.skills[0].path.clone()]);
    let mut registry = ToolRegistry::empty();
    disabled.register(&mut registry).unwrap();
    assert!(registry.defs().is_empty());
    assert!(disabled.prompt().is_empty());
}

#[test]
fn config_migrates_v1_and_rejects_plaintext_auth_headers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.json");
    let mut old = serde_json::to_value(AppConfig::default()).unwrap();
    old["version"] = 1.into();
    old.as_object_mut().unwrap().remove("providers");
    old.as_object_mut().unwrap().remove("active_provider");
    old.as_object_mut().unwrap().remove("appearance");
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let config = ConfigStore::new(temp.path()).load().unwrap();
    assert_eq!(config.version, 2);
    assert_eq!(config.providers.len(), 1);
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".into(), "Bearer plaintext-secret".into());
    assert!(validate_headers(&headers).is_err());
    headers.insert("Authorization".into(), "Bearer ${API_KEY}".into());
    assert!(validate_headers(&headers).is_ok());
    assert_eq!(
        CompatibilityConfig::default().max_tokens_field,
        "max_completion_tokens"
    );
}

#[test]
fn mcp_import_parses_known_formats_and_merges_without_starting_services() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("claude.json");
    fs::write(&path, r#"{"mcpServers":{"browser":{"command":"node","args":["server.js"],"env":{"MODE":"test"}},"remote":{"url":"https://example.test/mcp","headers":{"Authorization":"Bearer ${MCP_TOKEN}"}}}}"#).unwrap();
    let (servers, errors) = parse_mcp_file(&path).unwrap();
    assert!(errors.is_empty());
    assert_eq!(servers.len(), 2);
    assert!(matches!(servers[0].transport, McpTransport::Stdio { .. }));
    let existing = vec![McpServerConfig {
        name: "browser".into(),
        enabled: true,
        transport: McpTransport::Stdio {
            command: "old".into(),
            args: vec![],
            env: BTreeMap::new(),
        },
        timeout_seconds: 30,
    }];
    let candidates = servers
        .into_iter()
        .map(|server| McpCandidate {
            source: "test".into(),
            path: path.clone(),
            server,
        })
        .collect::<Vec<_>>();
    let merged = merge_mcp(&existing, &candidates, &[true, true], false).unwrap();
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].transport, existing[0].transport);
    assert!(!merged.iter().all(|server| server.enabled));
}

#[test]
fn release_tags_are_checked_without_following_external_urls() {
    let value = serde_json::json!({"tag_name":"v0.2.0","html_url":"https://github.com/WEP-56/Hikari/releases/tag/v0.2.0"});
    let release = hikari_runtime::updates::release_info(&value, "0.1.0").unwrap();
    assert!(release.newer);
    assert!(hikari_runtime::updates::release_info(
        &serde_json::json!({"tag_name":"v0.2.0","html_url":"https://evil.test"}),
        "0.1.0"
    )
    .is_err());
}

#[test]
fn rejected_save_preserves_config_and_backup_and_corrupt_file_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temp.path());
    let mut config = AppConfig::default();
    store.save(&config).unwrap();
    config.persona_name = "saved".into();
    config.appearance.terminal_font = "Cascadia Mono".into();
    store.save(&config).unwrap();
    let saved = fs::read(store.path()).unwrap();
    let backup = fs::read(temp.path().join("config.json.bak")).unwrap();
    config.provider.max_output_tokens = config.provider.context_tokens + 1;
    assert!(store.save(&config).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), saved);
    assert_eq!(
        fs::read(temp.path().join("config.json.bak")).unwrap(),
        backup
    );
    assert_eq!(
        store.load().unwrap().appearance.terminal_font,
        "Cascadia Mono"
    );
    fs::write(store.path(), "broken json").unwrap();
    store.save(&AppConfig::default()).unwrap();
    let corrupt = fs::read_dir(temp.path())
        .unwrap()
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("config.corrupt-")
        })
        .unwrap();
    assert_eq!(fs::read_to_string(corrupt.path()).unwrap(), "broken json");
    assert_eq!(
        fs::read(temp.path().join("config.json.bak")).unwrap(),
        backup
    );
}

#[test]
fn models_and_provider_switch_restore_independent_limits_and_thinking() {
    use hikari_runtime::{ModelMetadata, ProviderProfile};
    let temp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(temp.path());
    let mut config = AppConfig::default();
    let mut profile = ProviderProfile::new("local".into(), "Local".into(), Default::default());
    profile.models.push(ModelMetadata {
        id: "reasoner".into(),
        supports_thinking: true,
        thinking_levels: vec!["high".into()],
        context_tokens: 64_000,
        max_output_tokens: 4096,
        tool_disclosure: hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch,
        ..Default::default()
    });
    profile.select_model("reasoner").unwrap();
    profile.config.reasoning_effort = Some("high".into());
    config.providers.push(profile);
    config.activate("local").unwrap();
    store.save(&config).unwrap();
    let mut config = store.load().unwrap();
    assert_eq!(config.provider.context_tokens, 64_000);
    assert_eq!(
        config.provider.compatibility.tool_disclosure,
        hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch
    );
    assert_eq!(config.provider.reasoning_effort.as_deref(), Some("high"));
    config.providers[1].select_model("gpt-4.1").unwrap();
    config.activate("local").unwrap();
    assert_eq!(config.provider.context_tokens, 128_000);
    assert_eq!(
        config.provider.compatibility.tool_disclosure,
        hikari_agent::protocols::ToolDisclosure::Compatible
    );
    assert_eq!(config.provider.reasoning_effort, None);
    config.activate("default").unwrap();
    assert_eq!(config.provider.max_output_tokens, 8192);
}

#[test]
fn codex_toml_and_hermes_yaml_import_and_duplicate_selection_are_validated() {
    let temp = tempfile::tempdir().unwrap();
    let codex = temp.path().join("config.toml");
    fs::write(&codex, "[mcp_servers.chrome-devtools]\ncommand = 'cmd'\nargs = ['/c', 'npx', 'chrome-devtools-mcp@latest']\n\n[mcp_servers.remote]\nurl = 'https://example.test/mcp'\nbearer_token_env_var = 'MCP_TOKEN'\n[mcp_servers.remote.http_headers]\nX-Client = 'hikari'\n[mcp_servers.remote.env_http_headers]\nX-Api-Key = 'MCP_KEY'\n").unwrap();
    let (servers, errors) = parse_mcp_file(&codex).unwrap();
    assert!(errors.is_empty());
    assert_eq!(servers[0].name, "chrome_devtools");
    match &servers[1].transport {
        McpTransport::StreamableHttp {
            headers,
            bearer_token_env,
            ..
        } => {
            assert_eq!(headers["X-Api-Key"], "${MCP_KEY}");
            assert_eq!(headers["X-Client"], "hikari");
            assert_eq!(bearer_token_env.as_deref(), Some("MCP_TOKEN"));
        }
        _ => panic!("expected HTTP"),
    }
    let hermes = temp.path().join("config.yaml");
    fs::write(&hermes, "mcp_servers:\n  chrome-devtools:\n    command: cmd\n    args: [other]\n  old:\n    type: sse\n    url: https://example.test/sse\n  invalid:\n    command: cmd\n    args: [5]\n").unwrap();
    let (other, errors) = parse_mcp_file(&hermes).unwrap();
    assert_eq!(errors.len(), 2);
    let candidates = vec![
        McpCandidate {
            source: "Codex".into(),
            path: codex,
            server: servers[0].clone(),
        },
        McpCandidate {
            source: "Hermes".into(),
            path: hermes,
            server: other[0].clone(),
        },
    ];
    assert!(merge_mcp(&[], &candidates, &[true, true], true).is_err());
    let merged = merge_mcp(&servers[..1], &candidates[1..], &[true], true).unwrap();
    assert_eq!(merged[0], other[0]);
    assert!(!merged[0].enabled);
}

#[test]
fn skill_copy_preserves_resources_and_does_not_overwrite_existing_skill() {
    use hikari_runtime::imports::import_skill;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("external/sample");
    fs::create_dir_all(source.join("scripts")).unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\nname: sample\ndescription: example\n---\nHello",
    )
    .unwrap();
    fs::write(source.join("scripts/helper.py"), "print('hello')").unwrap();
    let target = temp.path().join(".hikari/skills");
    let manifest = import_skill(&source.join("SKILL.md"), &target).unwrap();
    assert!(manifest.is_file());
    assert_eq!(
        fs::read(source.join("scripts/helper.py")).unwrap(),
        fs::read(target.join("sample/scripts/helper.py")).unwrap()
    );
    fs::write(target.join("sample/scripts/helper.py"), "local changes").unwrap();
    assert!(import_skill(&source.join("SKILL.md"), &target).is_err());
    assert_eq!(
        fs::read_to_string(target.join("sample/scripts/helper.py")).unwrap(),
        "local changes"
    );
    assert!(import_skill(&source.join("SKILL.md"), &source.join("nested")).is_err());
}

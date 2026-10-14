use hikari_agent::{Message, ToolRegistry, Usage};
use hikari_runtime::{
    history,
    web::{self, SearchConfig, SearchProvider, WebConfig},
    AppConfig, ConfigStore, Session, SessionStore,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::atomic::AtomicBool,
};

fn fixture(
    status: u16,
    mime: &str,
    body: String,
) -> (
    String,
    std::thread::JoinHandle<(BTreeMap<String, String>, Value)>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let mime = mime.to_owned();
    let task = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            headers.insert(key.to_lowercase(), value.trim().to_string());
        }
        let size = headers
            .get("content-length")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut data = vec![0; size];
        reader.read_exact(&mut data).unwrap();
        let request = if data.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&data).unwrap()
        };
        let _ = write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        (headers, request)
    });
    (url, task)
}

#[test]
fn search_profiles_persist_independently() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path());
    let mut config = AppConfig::default();
    config.web.enabled = true;
    for provider in SearchProvider::ALL {
        *config.web.profile_mut(provider) = SearchConfig {
            api_key: format!("fixture-{provider:?}"),
            api_key_env: format!("FIXTURE_{provider:?}"),
            endpoint: format!("https://example.com/{provider:?}"),
        };
    }
    for selected in SearchProvider::ALL {
        config.web.provider = selected;
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.web.provider, selected);
        for provider in SearchProvider::ALL {
            let actual = loaded.web.profile(provider);
            let expected = config.web.profile(provider);
            assert_eq!(actual.api_key, expected.api_key);
            assert_eq!(actual.api_key_env, expected.api_key_env);
            assert_eq!(actual.endpoint, expected.endpoint);
        }
        let serialized = serde_json::to_value(&loaded.web).unwrap();
        for legacy in ["api_key", "api_key_env", "endpoint"] {
            assert!(serialized.get(legacy).is_none());
        }
    }
    config.web.exa.api_key.clear();
    config.web.serper.api_key_env.clear();
    store.save(&config).unwrap();
    let loaded = store.load().unwrap();
    assert!(loaded.web.exa.api_key.is_empty());
    assert!(loaded.web.serper.api_key_env.is_empty());
    assert_eq!(loaded.web.tavily.api_key, "fixture-Tavily");
}

#[test]
fn search_profiles_migrate_legacy_and_default_missing_fields() {
    let defaults = WebConfig::default();
    for provider in SearchProvider::ALL {
        let legacy = json!({
            "enabled": true, "provider": provider,
            "api_key": "legacy-key", "api_key_env": "LEGACY_ENV",
            "endpoint": "https://example.com/legacy", "timeout_seconds": 45,
        });
        let migrated: WebConfig = serde_json::from_value(legacy.clone()).unwrap();
        assert!(migrated.enabled);
        assert_eq!(migrated.provider, provider);
        assert_eq!(migrated.timeout_seconds, 45);
        for candidate in SearchProvider::ALL {
            let profile = migrated.profile(candidate);
            if candidate == provider {
                assert_eq!(profile.api_key, "legacy-key");
                assert_eq!(profile.api_key_env, "LEGACY_ENV");
                assert_eq!(profile.endpoint, "https://example.com/legacy");
            } else {
                assert!(profile.api_key.is_empty() && profile.endpoint.is_empty());
                assert_eq!(profile.api_key_env, defaults.profile(candidate).api_key_env);
            }
        }
        let mut mixed = legacy;
        let name = serde_json::to_value(provider).unwrap();
        mixed[name.as_str().unwrap()] = json!({"api_key": "new-key"});
        let current: WebConfig = serde_json::from_value(mixed).unwrap();
        assert_eq!(current.profile(provider).api_key, "new-key");
        assert_eq!(
            current.profile(provider).api_key_env,
            defaults.profile(provider).api_key_env
        );
        assert!(current.profile(provider).endpoint.is_empty());
    }
    let partial: WebConfig = serde_json::from_value(json!({
        "tavily": {}, "exa": {"api_key_env": ""}, "serper": {}
    }))
    .unwrap();
    assert_eq!(partial.tavily.api_key_env, "TAVILY_API_KEY");
    assert!(partial.exa.api_key_env.is_empty());
    assert_eq!(partial.serper.api_key_env, "SERPER_API_KEY");
    assert_eq!(partial.timeout_seconds, 30);
}

#[test]
fn search_provider_auth_and_result_limits() {
    for provider in [
        SearchProvider::Tavily,
        SearchProvider::Exa,
        SearchProvider::Serper,
    ] {
        let item = json!({"title":"文档","url":"https://example.com","link":"https://example.com","content":"正文","text":"正文","snippet":"正文"});
        let (url, task) = fixture(
            200,
            "application/json",
            json!({"results":[item.clone(),item.clone()],"organic":[item.clone(),item]})
                .to_string(),
        );
        let mut tools = ToolRegistry::empty();
        let mut config = WebConfig {
            enabled: true,
            provider,
            ..Default::default()
        };
        for other in SearchProvider::ALL {
            *config.profile_mut(other) = SearchConfig {
                api_key: format!("unused-{other:?}"),
                api_key_env: String::new(),
                endpoint: "http://127.0.0.1:1/unused".into(),
            };
        }
        config.profile_mut(provider).api_key = "fixture-secret".into();
        config.profile_mut(provider).endpoint = url;
        web::register(config, &mut tools).unwrap();
        let output: Value = serde_json::from_str(
            &tools
                .call("web_search", r#"{"query":"测试","limit":1}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(output["results"].as_array().unwrap().len(), 1);
        assert_eq!(output["results"][0]["text"], "正文");
        let (headers, body) = task.join().unwrap();
        match provider {
            SearchProvider::Tavily => {
                assert_eq!(body["api_key"], "fixture-secret");
                assert_eq!(body["max_results"], 1);
                assert_eq!(body["query"], "测试");
            }
            SearchProvider::Exa => {
                assert_eq!(headers["x-api-key"], "fixture-secret");
                assert_eq!(body["numResults"], 1);
                assert_eq!(body["query"], "测试");
            }
            SearchProvider::Serper => {
                assert_eq!(headers["x-api-key"], "fixture-secret");
                assert_eq!(body["num"], 1);
                assert_eq!(body["q"], "测试");
            }
        }
    }
}

#[test]
fn fetch_extracts_html_rejects_large_body_and_hides_error_secrets() {
    let mut tools = ToolRegistry::empty();
    web::register(WebConfig::default(), &mut tools).unwrap();
    let (url, task) = fixture(200, "text/html", "<html><body><h1>标题</h1><p>正文 &amp; 内容</p><script>hidden_code()</script></body></html>".into());
    let output = tools
        .call("web_fetch", &json!({"url":url}).to_string())
        .unwrap();
    assert!(output.contains(&url) && output.contains("正文 & 内容"));
    assert!(!output.contains("hidden_code"));
    task.join().unwrap();
    let (url, task) = fixture(200, "text/plain", "x".repeat(2 * 1024 * 1024 + 1));
    assert!(tools
        .call("web_fetch", &json!({"url":url}).to_string())
        .unwrap_err()
        .contains("2 MB"));
    task.join().unwrap();
    assert!(tools
        .call_with_abort("web_fetch", "{}", &AtomicBool::new(true))
        .unwrap_err()
        .contains("取消"));
    assert!(tools
        .call("web_fetch", r#"{"url":"http://secret@example.com"}"#)
        .is_err());
    let (url, task) = fixture(401, "text/plain", "fixture-secret".into());
    let mut tools = ToolRegistry::empty();
    web::register(
        WebConfig {
            enabled: true,
            tavily: SearchConfig {
                api_key: "fixture-secret".into(),
                endpoint: url,
                ..Default::default()
            },
            ..Default::default()
        },
        &mut tools,
    )
    .unwrap();
    let error = tools.call("web_search", r#"{"query":"test"}"#).unwrap_err();
    assert!(error.contains("401") && !error.contains("fixture-secret"));
    task.join().unwrap();
}

fn session(id: u64, memory_enabled: bool, transcript: Vec<Message>) -> Session {
    Session {
        id,
        title: format!("会话{id}"),
        workspace: None,
        pinned: false,
        transcript: transcript.clone(),
        context: transcript,
        retained_until: 0,
        context_base: 0,
        memory_enabled,
        usage: Usage::default(),
        execution: Default::default(),
    }
}

#[test]
fn history_paging_provenance_disabled_sessions_and_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let mut user = Message::user("笔记原文");
    user.stamp("1:0".into(), false);
    let mut recalled = Message::assistant("笔记召回内容");
    recalled.stamp("1:1".into(), true);
    let records = vec![
        session(
            1,
            true,
            vec![
                user,
                recalled,
                Message::tool_result("x", "shell", "笔记工具输出", false),
                Message::assistant("笔记末尾"),
            ],
        ),
        session(2, false, vec![Message::user("笔记隐私")]),
    ];
    store.save(&records).unwrap();
    let mut tools = ToolRegistry::empty();
    let (core, _) = hikari_runtime::MemoryCore::open(&dir.path().join("memory.db"), true).unwrap();
    history::register(store.clone(), Some(core.clone()), 1, &mut tools).unwrap();
    let search: Value =
        serde_json::from_str(&tools.call("session_search", r#"{"query":"笔记"}"#).unwrap())
            .unwrap();
    assert_eq!(search["total"], 1);
    assert_eq!(search["results"][0]["matches"][0]["source_id"], "1:0");
    assert!(!search.to_string().contains("召回内容"));
    let read: Value = serde_json::from_str(
        &tools
            .call("session_read", r#"{"session_id":1,"limit":1}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["next_offset"], 1);
    assert_eq!(read["has_more"], true);
    let read: Value = serde_json::from_str(
        &tools
            .call("session_read", r#"{"session_id":1,"offset":1}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["messages"][0]["message_index"], 3);
    assert!(tools.call("session_read", r#"{"session_id":2}"#).is_err());
    let mut private_tools = ToolRegistry::empty();
    history::register(store.clone(), Some(core), 2, &mut private_tools).unwrap();
    assert!(private_tools
        .call("session_read", r#"{"session_id":1}"#)
        .is_err());
    assert!(private_tools
        .call("session_read", r#"{"session_id":2}"#)
        .unwrap()
        .contains("隐私"));
    let mut unavailable = ToolRegistry::empty();
    history::register(store.clone(), None, 1, &mut unavailable).unwrap();
    assert!(unavailable
        .call("session_search", r#"{"query":"笔记"}"#)
        .unwrap_err()
        .contains("无法核对"));
    assert!(unavailable
        .call("session_read", r#"{"session_id":1}"#)
        .is_err());
    store.save(&[]).unwrap();
    assert!(tools
        .call("session_read", r#"{"session_id":1}"#)
        .unwrap_err()
        .contains("已删除"));
}

#[test]
fn checkpoint_copies_followup_and_tool_provenance_to_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let mut record = session(
        1,
        true,
        vec![
            Message::user("重复"),
            Message::user("重复"),
            Message::tool_result("x", "session_read", "history", false),
        ],
    );
    for (i, m) in record.context.iter_mut().enumerate() {
        m.stamp(format!("1:{i}"), i == 2);
    }
    store.save(&[record]).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded[0].transcript[0].origin().unwrap().id, "1:0");
    assert_eq!(loaded[0].transcript[1].origin().unwrap().id, "1:1");
    assert!(loaded[0].transcript[2].origin().unwrap().recalled);
}

#[test]
fn artifact_cleanup_is_limited_to_the_deleted_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    for id in [1, 2] {
        let root = dir.path().join("artifacts").join(format!("talk-{id}"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(format!("{}.txt", "a".repeat(64))), "artifact").unwrap();
    }
    store.remove_artifacts(1).unwrap();
    assert!(!dir.path().join("artifacts/talk-1").exists());
    assert!(dir.path().join("artifacts/talk-2").exists());
}

#[test]
fn forgetting_filters_original_message_and_derived_title() {
    use hikari_agent::Memory;
    use hikari_memory::{Core, MemoryConfig, Processor};
    use std::sync::Arc;
    struct Extract;
    impl Processor for Extract {
        fn call(
            &self,
            _: &Value,
            system: &str,
            input: &Value,
        ) -> hikari_agent::Result<(Value, Usage)> {
            let output = if system.contains("事实抽取器") {
                json!({"facts":[{"text":"偏好清淡","fact_type":"world","source_message_ids":[input["messages"][0]["id"]],"entities":[],"occurred_start":null,"occurred_end":null}]})
            } else {
                json!({"creates":[],"updates":[],"deletes":[]})
            };
            Ok((output, Usage::default()))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&dir.path().join("memory.db"), true).unwrap();
    core.set_processor(Arc::new(Extract));
    let scope = core.scope(
        "1".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let mut user = Message::user("偏好清淡");
    user.stamp("1:0".into(), false);
    scope.retain("1@0-1", &[user.clone()]).unwrap();
    loop {
        let event = events
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(event.error.is_none());
        if event.kind == "consolidate" && event.state == "done" {
            break;
        }
    }
    let store = SessionStore::new(dir.path());
    let mut record = session(1, true, vec![user]);
    record.title = "偏好清淡".into();
    store.save(&[record]).unwrap();
    let mut tools = ToolRegistry::empty();
    history::register(store, Some(core.clone()), 99, &mut tools).unwrap();
    assert!(tools
        .call("session_search", r#"{"query":"清淡"}"#)
        .unwrap()
        .contains("1:0"));
    let facts = scope
        .query("清淡", &["world"], 5, 2000, None, None)
        .unwrap();
    let id = facts["items"][0]["id"].as_str().unwrap().to_string();
    scope.forget("请忘记清淡偏好", &[id]).unwrap();
    assert!(core.suppressed_sources().unwrap().contains("1:0"));
    let search: Value =
        serde_json::from_str(&tools.call("session_search", r#"{"query":"清淡"}"#).unwrap())
            .unwrap();
    assert_eq!(search["total"], 0);
    let read = tools.call("session_read", r#"{"session_id":1}"#).unwrap();
    assert!(!read.contains("清淡"));
    core.set_enabled(false);
    assert!(tools.call("session_read", r#"{"session_id":1}"#).is_err());
}

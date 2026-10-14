use hikari_agent::{
    protocols::{request_body, ToolDisclosure},
    tool::Tool,
    *,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

struct Notes;
impl Tool for Notes {
    fn name(&self) -> &str {
        "mcp_notes_search"
    }
    fn description(&self) -> &str {
        "搜索个人笔记和文档"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string","description":"笔记关键词"}}})
    }
    fn call(&self, _: &str) -> std::result::Result<String, String> {
        Ok("笔记内容".into())
    }
}
fn registry() -> ToolRegistry {
    let mut tools = ToolRegistry::empty();
    tools
        .register_deferred(Arc::new(Notes), "个人知识库".into())
        .unwrap();
    tools
}

#[test]
fn discovery_changes_next_request_and_restores_in_a_new_registry() {
    struct Script {
        round: AtomicUsize,
        seen: Mutex<Vec<Vec<ToolDef>>>,
    }
    impl Provider for Script {
        fn stream(
            &self,
            context: &Context<'_>,
            _: &AtomicBool,
            emit: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            self.seen.lock().unwrap().push(context.tools.to_vec());
            match self.round.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    emit(StreamEvent::ToolDelta {
                        index: 0,
                        id: Some("search".into()),
                        name: Some("tool_search".into()),
                        arguments: json!({"query":"搜索笔记"}).to_string(),
                    });
                    emit(StreamEvent::Done(StopReason::ToolUse));
                }
                1 => {
                    emit(StreamEvent::ToolDelta {
                        index: 0,
                        id: Some("notes".into()),
                        name: Some("mcp_notes_search".into()),
                        arguments: "{}".into(),
                    });
                    emit(StreamEvent::Done(StopReason::ToolUse));
                }
                _ => {
                    emit(StreamEvent::Text("完成".into()));
                    emit(StreamEvent::Done(StopReason::Stop));
                }
            }
            Ok(())
        }
    }
    let provider = Arc::new(Script {
        round: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    });
    let tools = registry();
    assert!(tools.call("mcp_notes_search", "{}").is_err());
    let host = TurnHost {
        provider: provider.clone(),
        tools: Arc::new(tools),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "test".into(),
        messages: vec![Message::user("查笔记")],
        query: "查笔记".into(),
        system_prompt: "test".into(),
        retained_until: 0,
        context_base: 0,
        context_limit_tokens: 128000,
        max_tool_rounds: 4,
    };
    let mut final_messages = Vec::new();
    run_turn(&host, request, |event| {
        if let AgentEvent::AgentEnd {
            messages, error, ..
        } = event
        {
            assert!(error.is_none());
            final_messages = messages;
        }
    });
    let seen = provider.seen.lock().unwrap();
    assert!(!seen[0].iter().any(|t| t.name == "mcp_notes_search"));
    assert!(seen[1].iter().any(|t| t.name == "mcp_notes_search"));
    assert!(final_messages
        .iter()
        .any(|m| matches!(m,Message::ToolResult{text,..} if text=="笔记内容")));
    let restored: Vec<Message> =
        serde_json::from_str(&serde_json::to_string(&final_messages).unwrap()).unwrap();
    let Message::ToolCatalog { tools } = restored
        .iter()
        .rev()
        .find(|m| matches!(m, Message::ToolCatalog { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    let next = registry();
    next.restore_loaded(tools);
    assert_eq!(next.call("mcp_notes_search", "{}").unwrap(), "笔记内容");
    assert!(next
        .call("tool_search", r#"{"query":"笔记"}"#)
        .unwrap()
        .contains("no_match"));
}

#[test]
fn native_disclosure_keeps_initial_list_and_fallback_uses_current_tools() {
    let base = crate_tool("tool_search");
    let late = crate_tool("mcp_notes_search");
    let tools = vec![base.clone(), late.clone()];
    let messages = vec![
        Message::ToolCatalog {
            tools: vec![base.clone()],
        },
        Message::user("find"),
        Message::assistant(""),
        Message::tool_result("load", "tool_search", "loaded", false),
        Message::ToolCatalog {
            tools: tools.clone(),
        },
    ];
    let context = Context {
        system_prompt: "base",
        messages: &messages,
        tools: &tools,
    };
    for (protocol, mode, kind) in [
        (
            ApiProtocol::Responses,
            ToolDisclosure::ResponsesAdditionalTools,
            "additional_tools",
        ),
        (
            ApiProtocol::Responses,
            ToolDisclosure::ResponsesToolSearch,
            "tool_search_output",
        ),
        (
            ApiProtocol::Anthropic,
            ToolDisclosure::AnthropicInlineTools,
            "tool_addition",
        ),
    ] {
        let mut config = ProviderConfig {
            protocol,
            ..Default::default()
        };
        config.compatibility.tool_disclosure = mode;
        let body = request_body(&config, &context);
        assert_eq!(body["tools"][0]["name"], "tool_search");
        assert!(!body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "mcp_notes_search"));
        assert!(body.to_string().contains(kind));
        config.compatibility.tool_disclosure = ToolDisclosure::Compatible;
        let body = request_body(&config, &context);
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "mcp_notes_search"));
        assert!(!body.to_string().contains(kind));
    }
}
fn crate_tool(name: &str) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type":"object"}),
    }
}

#[test]
fn long_output_is_saved_and_read_without_losing_unicode() {
    struct Long;
    impl Tool for Long {
        fn name(&self) -> &str {
            "long"
        }
        fn description(&self) -> &str {
            "long"
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        fn call(&self, _: &str) -> std::result::Result<String, String> {
            Ok("笔记🙂".repeat(9000))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut tools = ToolRegistry::new(vec![Arc::new(Long)]);
    tools.enable_artifacts(dir.path().into()).unwrap();
    let preview = tools.call("long", "{}").unwrap();
    assert!(preview.contains("artifact_read"));
    let id = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let page: Value = serde_json::from_str(
        &tools
            .call(
                "artifact_read",
                &json!({"id":id,"offset":16000,"limit":12000}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["total_chars"], 27000);
    assert_eq!(page["next_offset"], 27000);
    assert_eq!(page["has_more"], false);
    assert!(tools
        .call("artifact_read", r#"{"id":"../secret"}"#)
        .is_err());
}

#[test]
fn oversized_system_and_schema_stop_before_provider_request() {
    struct NoCall;
    impl Provider for NoCall {
        fn stream(
            &self,
            _: &Context<'_>,
            _: &AtomicBool,
            _: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            panic!("oversized request sent")
        }
    }
    let host = TurnHost {
        provider: Arc::new(NoCall),
        tools: Arc::new(registry()),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "budget".into(),
        messages: vec![Message::user("hi")],
        query: "hi".into(),
        system_prompt: "字".repeat(2000),
        retained_until: 0,
        context_base: 0,
        context_limit_tokens: 1024,
        max_tool_rounds: 1,
    };
    let mut failed = false;
    run_turn(&host, request, |event| {
        if let AgentEvent::AgentEnd { error, .. } = event {
            failed = error.is_some();
        }
    });
    assert!(failed);
}

#[test]
fn removals_and_redefinitions_use_safe_responses_fallback() {
    let base = crate_tool("tool_search");
    let old = crate_tool("mcp_notes_search");
    let mut replacement = old.clone();
    replacement.description = "changed".into();
    for current in [vec![base.clone()], vec![base.clone(), replacement]] {
        let messages = vec![
            Message::ToolCatalog {
                tools: vec![base.clone()],
            },
            Message::user("find"),
            Message::ToolCatalog {
                tools: vec![base.clone(), old.clone()],
            },
            Message::ToolCatalog {
                tools: current.clone(),
            },
        ];
        for mode in [
            ToolDisclosure::ResponsesAdditionalTools,
            ToolDisclosure::ResponsesToolSearch,
        ] {
            let mut config = ProviderConfig {
                protocol: ApiProtocol::Responses,
                ..Default::default()
            };
            config.compatibility.tool_disclosure = mode;
            let body = request_body(
                &config,
                &Context {
                    system_prompt: "",
                    messages: &messages,
                    tools: &current,
                },
            );
            assert_eq!(body["tools"].as_array().unwrap().len(), current.len());
            assert!(!body.to_string().contains("additional_tools"));
            assert!(!body.to_string().contains("tool_search_output"));
        }
    }
}

#[test]
fn inline_disclosure_keeps_parallel_tool_results_together() {
    let base = crate_tool("tool_search");
    let late = crate_tool("mcp_notes_search");
    let mut assistant = Message::assistant("");
    if let Message::Assistant { tool_calls, .. } = &mut assistant {
        *tool_calls = vec![
            ToolCall {
                id: "a".into(),
                name: "tool_search".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "b".into(),
                name: "tool_search".into(),
                arguments: "{}".into(),
            },
        ];
    }
    let tools = vec![base.clone(), late.clone()];
    let messages = vec![
        Message::ToolCatalog { tools: vec![base] },
        Message::user("find"),
        assistant,
        Message::tool_result("a", "tool_search", "loaded", false),
        Message::ToolCatalog {
            tools: tools.clone(),
        },
        Message::tool_result("b", "tool_search", "done", false),
        Message::user("continue"),
    ];
    let mut config = ProviderConfig {
        protocol: ApiProtocol::Anthropic,
        ..Default::default()
    };
    config.compatibility.tool_disclosure = ToolDisclosure::AnthropicInlineTools;
    let body = request_body(
        &config,
        &Context {
            system_prompt: "",
            messages: &messages,
            tools: &tools,
        },
    );
    let wire = body["messages"].as_array().unwrap();
    let assistant_index = wire.iter().position(|m| m["role"] == "assistant").unwrap();
    let results = wire[assistant_index + 1]["content"].as_array().unwrap();
    assert_eq!(
        results
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .count(),
        2
    );
    assert_eq!(results[0]["tool_use_id"], "a");
    assert_eq!(results[1]["tool_use_id"], "b");
    assert_eq!(wire[assistant_index + 2]["role"], "system");
    assert_eq!(
        wire[assistant_index + 2]["content"][0]["tool"]["definition"]["name"],
        "mcp_notes_search"
    );
}

#[test]
fn oversized_compaction_request_is_not_sent() {
    struct NoCall;
    impl Provider for NoCall {
        fn output_reserve(&self) -> u32 {
            200
        }
        fn stream(
            &self,
            _: &Context<'_>,
            _: &AtomicBool,
            _: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            panic!("oversized summary sent")
        }
    }
    let host = TurnHost {
        provider: Arc::new(NoCall),
        tools: Arc::new(ToolRegistry::empty()),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "summary-budget".into(),
        messages: (0..12).map(|_| Message::user("字".repeat(2000))).collect(),
        query: "hi".into(),
        system_prompt: "".into(),
        retained_until: 0,
        context_base: 0,
        context_limit_tokens: 1024,
        max_tool_rounds: 1,
    };
    let mut end = None;
    run_turn(&host, request, |e| {
        if let AgentEvent::AgentEnd {
            messages, error, ..
        } = e
        {
            assert_eq!(messages.len(), 12);
            end = error;
        }
    });
    assert!(end.unwrap().contains("摘要请求超过"));
}

#[test]
fn compaction_preserves_loaded_catalog_and_callable_tools() {
    struct Compact;
    impl Provider for Compact {
        fn stream(
            &self,
            context: &Context<'_>,
            _: &AtomicBool,
            emit: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            if context.tools.is_empty() {
                emit(StreamEvent::Text("早期对话摘要".into()));
            } else {
                assert!(context.tools.iter().any(|t| t.name == "mcp_notes_search"));
                emit(StreamEvent::Text("完成".into()));
            }
            emit(StreamEvent::Done(StopReason::Stop));
            Ok(())
        }
    }
    let tools = registry();
    tools.call("tool_search", r#"{"query":"笔记"}"#).unwrap();
    let mut messages = vec![Message::ToolCatalog {
        tools: tools.visible_defs(),
    }];
    messages.extend((0..12).map(|_| Message::user("字".repeat(400))));
    let host = TurnHost {
        provider: Arc::new(Compact),
        tools: Arc::new(tools),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "compact".into(),
        messages,
        query: "继续".into(),
        system_prompt: "base".into(),
        retained_until: 0,
        context_base: 0,
        context_limit_tokens: 6000,
        max_tool_rounds: 1,
    };
    let mut saved = Vec::new();
    run_turn(&host, request, |e| {
        if let AgentEvent::AgentEnd {
            messages, error, ..
        } = e
        {
            assert!(error.is_none());
            saved = messages;
        }
    });
    assert!(matches!(saved[0], Message::ContextSummary { .. }));
    let Message::ToolCatalog { tools } = saved
        .iter()
        .find(|m| matches!(m, Message::ToolCatalog { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    let restored = registry();
    restored.restore_loaded(tools);
    assert!(restored.call("mcp_notes_search", "{}").is_ok());
}

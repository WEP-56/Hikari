use hikari_agent::{Memory, Message, ToolRegistry, Usage};
use hikari_memory::{BrowseView, Core, MemoryConfig, Processor, Unit};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn wait_done(events: &std::sync::mpsc::Receiver<hikari_agent::MemoryJobEvent>, kind: &str) {
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(event.error.is_none(), "{event:?}");
        if event.kind == kind && event.state == "done" {
            break;
        }
    }
}

#[test]
fn memory_browser_groups_evidence_before_search_and_pagination() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("m.db");
    let (core, _) = Core::open(&path, false).unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    let insert = |id: String, kind: &str, evidence: Vec<String>, consolidated: bool| {
        let unit = Unit {
            id: id.clone(),
            text: format!("profile {id}"),
            fact_type: kind.into(),
            mentioned_at: "2026-10-13T09:00:00Z".into(),
            occurred_start: None,
            occurred_end: None,
            created_at: "2026-10-13T09:00:00Z".into(),
            updated_at: "2026-10-13T09:00:00Z".into(),
            entities: vec![],
            source_message_ids: vec![],
            source_memory_ids: evidence,
            document_id: None,
            proof_count: 1,
            metadata: json!({}),
            consolidated,
        };
        db.execute(
            "INSERT INTO units(id,kind,data,consolidated) VALUES(?,?,?,?)",
            rusqlite::params![
                id,
                kind,
                serde_json::to_string(&unit).unwrap(),
                consolidated
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO unit_search(id,terms) VALUES(?,'profile')",
            [&unit.id],
        )
        .unwrap();
    };
    for i in 0..30 {
        insert(format!("fact-{i}"), "world", vec![], true);
        insert(
            format!("obs-{i}"),
            "observation",
            vec![format!("fact-{i}")],
            true,
        );
    }
    // A pending fact and a consolidated fact with no remaining observation stay visible.
    insert("pending".into(), "world", vec![], false);
    insert("orphan".into(), "experience", vec![], true);
    for query in ["", "profile"] {
        let first = core.browse(query, 0).unwrap();
        let next = core.browse(query, 25).unwrap();
        assert_eq!(first.len(), 25);
        assert_eq!(next.len(), 7);
        assert!(first
            .iter()
            .chain(&next)
            .all(|u| !u.id.starts_with("fact-")));
        assert!(first.iter().chain(&next).any(|u| u.id == "pending"));
        assert!(first.iter().chain(&next).any(|u| u.id == "orphan"));
        let facts = core.browse_view(query, 0, BrowseView::Facts).unwrap();
        assert_eq!(facts.len(), 25);
        assert!(facts.iter().all(|u| u.fact_type != "observation"));
    }
    db.execute("DELETE FROM units WHERE id='obs-0'", [])
        .unwrap();
    let mut after_delete = core.browse("profile", 0).unwrap();
    after_delete.extend(core.browse("profile", 25).unwrap());
    assert!(after_delete.iter().any(|u| u.id == "fact-0"));
}

#[test]
fn consolidation_uses_actual_dates_and_current_transaction_for_duplicate_creates() {
    struct Replace;
    impl Processor for Replace {
        fn call(
            &self,
            _: &Value,
            system: &str,
            input: &Value,
        ) -> hikari_agent::Result<(Value, Usage)> {
            let value = if system.contains("事实抽取器") {
                json!({"facts": input["messages"].as_array().unwrap().iter().map(|m|
                    json!({"text":"饮食偏好清淡","fact_type":"world","source_message_ids":[m["id"]],"entities":[],"occurred_start":"2020-01-01T00:00:00Z","occurred_end":"2020-01-01T00:00:00.1Z"})
                ).collect::<Vec<_>>()})
            } else {
                let deletes = input["EXISTING OBSERVATIONS"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| json!({"observation_id":o["id"],"reason":"replace"}))
                    .collect::<Vec<_>>();
                let creates = input["NEW FACTS"].as_array().unwrap().iter()
                    .map(|f| json!({"text":"饮食偏好清淡","source_fact_ids":[f["id"]],"reason":"new"})).collect::<Vec<_>>();
                json!({"creates":creates,"updates":[],"deletes":deletes})
            };
            Ok((value, Usage::default()))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("m.db"), true).unwrap();
    core.set_processor(Arc::new(Replace));
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let message = |id: &str, date: &str| {
        serde_json::from_value::<Message>(json!({
        "role":"user","text":"饮食偏好清淡","origin":{"id":id,"mentioned_at":date,"recalled":false}
    })).unwrap()
    };
    scope
        .retain("s@0-1", &[message("old", "2020-01-02T03:00:00+08:00")])
        .unwrap();
    wait_done(&events, "consolidate");
    let old_id = scope
        .query("饮食", &["observation"], 5, 2000, None, None)
        .unwrap()["items"][0]["id"]
        .clone();
    scope
        .retain(
            "s@1-3",
            &[
                message("new-a", "2020-01-01T21:00:00Z"),
                message("new-b", "2020-01-02T04:00:00+08:00"),
            ],
        )
        .unwrap();
    wait_done(&events, "consolidate");
    let observations = scope
        .query("饮食", &["observation"], 5, 2000, None, None)
        .unwrap();
    let items = observations["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_ne!(items[0]["id"], old_id);
    assert_eq!(items[0]["proof_count"], 2);
    assert_eq!(items[0]["mentioned_at"], "2020-01-01T21:00:00Z");
    let recalled = scope.recall("饮食");
    assert_eq!(
        recalled[0].occurred_start.as_deref(),
        Some("2020-01-01T00:00:00+00:00")
    );
    assert!(recalled[0].occurred_end.is_some());
}

#[test]
fn failed_extraction_retries_from_original_without_duplicate_evidence() {
    struct Once(std::sync::atomic::AtomicBool);
    impl Processor for Once {
        fn call(&self, m: &Value, s: &str, i: &Value) -> hikari_agent::Result<(Value, Usage)> {
            if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(hikari_agent::AgentError::new("fixture offline"));
            }
            Fixture.call(m, s, i)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("m.db"), true).unwrap();
    core.set_processor(Arc::new(Once(std::sync::atomic::AtomicBool::new(true))));
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    scope.retain("s@0-1", &[Message::user("饮食")]).unwrap();
    loop {
        let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if e.state == "failed" {
            break;
        }
    }
    assert_eq!(core.status().unwrap()["documents"], 1);
    assert_eq!(core.status().unwrap()["facts"], 0);
    core.retry_failed().unwrap();
    wait_done(&events, "consolidate");
    assert_eq!(scope.recall("饮食")[0].proof_count, 1);
    assert_eq!(core.status().unwrap()["facts"], 1);
}

#[test]
fn forgetting_while_consolidation_runs_cannot_restore_deleted_sources() {
    struct Block {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Processor for Block {
        fn call(&self, m: &Value, s: &str, i: &Value) -> hikari_agent::Result<(Value, Usage)> {
            if s.contains("沉淀器") {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Fixture.call(m, s, i)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("m.db"), true).unwrap();
    let (entered, rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    core.set_processor(Arc::new(Block {
        entered,
        release: std::sync::Mutex::new(release_rx),
    }));
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let messages = [Message::user("饮食")];
    scope.retain("s@0-1", &messages).unwrap();
    loop {
        let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if e.kind == "extract" && e.state == "done" {
            break;
        }
    }
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let fact = scope
        .query("饮食", &["world"], 5, 3000, None, None)
        .unwrap()["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    scope.forget("忘掉饮食", &[fact]).unwrap();
    release.send(()).unwrap();
    loop {
        let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if e.state == "failed" {
            break;
        }
    }
    core.retry_failed().unwrap();
    wait_done(&events, "consolidate");
    assert_eq!(core.status().unwrap()["facts"], 0);
    assert_eq!(core.status().unwrap()["observations"], 0);
    let next = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    assert!(!next.retain("s@0-2", &messages).unwrap().pending);
    assert!(next.recall("饮食").is_empty());
    assert!(core.browse("", 0).unwrap().is_empty());
}

#[test]
fn closing_and_reopening_invalidates_old_turn_permission() {
    let temp = tempfile::tempdir().unwrap();
    let (core, _) = Core::open(&temp.path().join("m.db"), true).unwrap();
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    core.set_enabled(false);
    core.set_enabled(true);
    assert!(!scope.enabled());
    assert!(scope
        .retain("s@0-1", &[Message::user("关闭期间的输入")])
        .is_err());
    let next = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    assert!(next.enabled());
}

#[test]
fn recalled_assistant_is_not_a_fact_source_and_original_time_is_preserved() {
    struct EchoExtract;
    impl Processor for EchoExtract {
        fn call(&self, _: &Value, s: &str, i: &Value) -> hikari_agent::Result<(Value, Usage)> {
            if s.contains("事实抽取器") {
                let source = &i["messages"][0];
                Ok((
                    json!({"facts":[{"text":"用户喜欢清淡饮食","fact_type":"world","source_message_ids":[source["id"]],"entities":[]},{"text":"助手已执行操作","fact_type":"experience","source_message_ids":[source["id"]],"entities":[]}]}),
                    Usage::default(),
                ))
            } else {
                Fixture.call(&json!({}), s, i)
            }
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("m.db"), true).unwrap();
    core.set_processor(Arc::new(EchoExtract));
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let mut message = Message::assistant("我记得你喜欢清淡饮食");
    message.stamp("original".into(), true);
    scope.retain("s@0-1", &[message]).unwrap();
    wait_done(&events, "extract");
    assert_eq!(core.status().unwrap()["facts"], 0);
    let message:Message=serde_json::from_value(json!({"role":"user","text":"饮食偏好","origin":{"id":"original-user","mentioned_at":"2020-01-02T03:04:05+08:00","recalled":false}})).unwrap();
    scope.retain("s@1-2", &[message]).unwrap();
    wait_done(&events, "consolidate");
    let facts = scope
        .query("清淡", &["world"], 5, 3000, None, None)
        .unwrap();
    assert_eq!(
        facts["items"][0]["mentioned_at"],
        "2020-01-02T03:04:05+08:00"
    );
    assert_eq!(facts["items"][0]["source_message_ids"][0], "original-user");
    assert!(scope
        .query(
            "清淡",
            &["world"],
            5,
            3000,
            Some("2020-01-01T20:00:00Z"),
            None
        )
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

struct Fixture;
impl Processor for Fixture {
    fn call(&self, _: &Value, system: &str, input: &Value) -> hikari_agent::Result<(Value, Usage)> {
        let output = if system.contains("事实抽取器") {
            let source = &input["messages"][0];
            json!({"facts":[{"text":"用户不喜欢辣味，喜欢清淡饮食。","fact_type":"world","source_message_ids":[source["id"]],"entities":["user","饮食"],"occurred_start":null,"occurred_end":null}]})
        } else {
            json!({"creates":[{"text":"用户不喜欢辣味，喜欢清淡饮食。","source_fact_ids":[input["NEW FACTS"][0]["id"]],"reason":"新的饮食偏好"}],"updates":[],"deletes":[]})
        };
        Ok((
            output,
            Usage {
                input: 10,
                output: 5,
            },
        ))
    }
}

#[test]
fn durable_jobs_cross_session_recall_expand_forget_and_disable() {
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("memory.db"), true).unwrap();
    core.set_processor(Arc::new(Fixture));
    let scope = core.scope(
        "talk-1".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let messages = vec![
        Message::user("我不喜欢辣，喜欢清淡饮食"),
        Message::assistant("好的"),
    ];
    assert!(scope.retain("talk-1@0-2", &messages).unwrap().pending);
    scope.retain("talk-1@0-2", &messages).unwrap();
    let mut done = 0;
    while done < 2 {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(event.error.is_none(), "{event:?}");
        if event.state == "done" {
            done += 1;
        }
    }
    assert_eq!(core.status().unwrap()["documents"], 1);
    let next = core.scope(
        "talk-2".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let recalled = next.recall_checked("辣味 饮食").unwrap();
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].fact_type, "observation");
    assert_eq!(recalled[0].proof_count, 1);
    assert!(next.recall_checked("编译器错误").unwrap().is_empty());
    let facts = next.query("饮食", &["world"], 5, 3000, None, None).unwrap();
    let fact = facts["items"][0]["id"].as_str().unwrap().to_string();
    let expanded = next.expand(&[fact], "document", 5000).unwrap();
    assert!(expanded["items"][0]["text"]
        .as_str()
        .unwrap()
        .contains("我不喜欢辣"));
    next.forget("忘掉饮食偏好", &[recalled[0].id.clone()])
        .unwrap();
    assert!(next.recall_checked("饮食").unwrap().is_empty());
    assert_eq!(core.status().unwrap()["facts"], 0);
    let mut registry = ToolRegistry::empty();
    scope.register(&mut registry).unwrap();
    assert_eq!(registry.defs().len(), 4);
    core.set_enabled(false);
    assert!(scope
        .retain("talk-1@2-3", &[Message::user("新偏好")])
        .is_err());
    assert!(registry.call("recall", r#"{"query":"饮食"}"#).is_err());
    let empty = core.scope(
        "talk-3".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    let mut registry = ToolRegistry::empty();
    empty.register(&mut registry).unwrap();
    assert!(registry.defs().is_empty());
}

#[test]
fn queued_documents_survive_reopen_and_output_is_bounded_valid_json() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    let (core, _) = Core::open(&path, true).unwrap();
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    scope.retain("s@0-1", &[Message::user("饮食偏好")]).unwrap();
    core.set_enabled(false);
    drop(scope);
    drop(core);
    let (core, events) = Core::open(&path, true).unwrap();
    core.set_processor(Arc::new(Fixture));
    loop {
        let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(e.error.is_none(), "{e:?}");
        if e.kind == "consolidate" && e.state == "done" {
            break;
        }
    }
    let out = hikari_memory::bounded(
        json!({"items":[{"id":"x","text":"很长的中文原文".repeat(5000)}]}),
        "items",
        1000,
    );
    assert!(hikari_memory::estimate_tokens(&out.to_string()) <= 1000);
    assert_eq!(out["truncated"], true);
}

#[test]
fn closing_core_does_not_cancel_running_job() {
    struct Blocking {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Processor for Blocking {
        fn call(&self, m: &Value, s: &str, i: &Value) -> hikari_agent::Result<(Value, Usage)> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Fixture.call(m, s, i)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (core, events) = Core::open(&temp.path().join("m.db"), true).unwrap();
    let (entered, rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    core.set_processor(Arc::new(Blocking {
        entered,
        release: std::sync::Mutex::new(release_rx),
    }));
    let scope = core.scope(
        "s".into(),
        None,
        MemoryConfig::default(),
        json!({}),
        json!({}),
    );
    scope.retain("s@0-1", &[Message::user("饮食")]).unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    core.set_enabled(false);
    release.send(()).unwrap();
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(event.error.is_none());
        if event.state == "done" {
            break;
        }
    }
    assert_eq!(core.status().unwrap()["facts"], 1);
    assert_eq!(core.status().unwrap()["queued"], 1);
}

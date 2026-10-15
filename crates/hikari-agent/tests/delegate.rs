use hikari_agent::{
    execution::{Execution, ExecutionBudget, ExecutionSnapshot, TaskStatus},
    permission::PermissionMode,
    provider::{Context, StreamEvent},
    tool::Tool,
    Message, Provider, ToolRegistry, TurnControl, Usage,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

fn wait(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "condition timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[derive(Default)]
struct Fixture {
    calls: AtomicUsize,
    release: AtomicBool,
    contexts: Mutex<Vec<(String, Vec<Message>, Vec<String>)>>,
}
impl Provider for Fixture {
    fn input_tokens(&self, _: &Context<'_>) -> u32 {
        10
    }
    fn output_reserve(&self) -> u32 {
        20
    }
    fn stream(
        &self,
        context: &Context<'_>,
        abort: &AtomicBool,
        emit: &mut dyn FnMut(StreamEvent),
    ) -> hikari_agent::Result<()> {
        self.contexts.lock().unwrap().push((
            context.system_prompt.into(),
            context.messages.to_owned(),
            context.tools.iter().map(|t| t.name.clone()).collect(),
        ));
        self.calls.fetch_add(1, Ordering::Release);
        let started = Instant::now();
        while !self.release.load(Ordering::Acquire) && !abort.load(Ordering::Relaxed) {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        if !abort.load(Ordering::Relaxed) {
            emit(StreamEvent::Text("{\"verified\":true}".into()));
            emit(StreamEvent::Usage {
                input: 10,
                output: 5,
            });
        }
        emit(StreamEvent::Done(hikari_agent::StopReason::Stop));
        Ok(())
    }
}
struct Dummy(&'static str);
impl Tool for Dummy {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "fixture"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object"})
    }
    fn call(&self, _: &str) -> Result<String, String> {
        Ok("fixture".into())
    }
}
fn setup(
    root: &std::path::Path,
    fixture: Arc<Fixture>,
    snapshot: ExecutionSnapshot,
) -> (Arc<Execution>, ToolRegistry) {
    let execution = Execution::new("delegation".into(), snapshot);
    execution.begin(root.into(), "fixture".into()).unwrap();
    let mut registry = ToolRegistry::workspace(root);
    registry.enable_execution(execution.clone()).unwrap();
    registry
        .enable_delegation(fixture, "base rules".into(), 128000, None)
        .unwrap();
    (execution, registry)
}
fn spawn(registry: &ToolRegistry, tasks: Value) -> Value {
    serde_json::from_str(
        &registry
            .call(
                "delegate_task",
                &json!({"action":"spawn","tasks":tasks}).to_string(),
            )
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn asynchronous_child_has_isolated_context_visibility_and_persistent_steering_channel() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    let execution = Execution::new("isolation".into(), ExecutionSnapshot::default());
    execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    let mut registry = ToolRegistry::workspace(root.path());
    registry.enable_execution(execution.clone()).unwrap();
    registry
        .register_deferred(Arc::new(Dummy("mcp_secret")), "fixture".into())
        .unwrap();
    registry.register(Arc::new(Dummy("session_read"))).unwrap();
    registry.register(Arc::new(Dummy("recall"))).unwrap();
    registry
        .call("tool_search", r#"{"query":"mcp_secret"}"#)
        .unwrap();
    assert!(registry
        .visible_defs()
        .iter()
        .any(|d| d.name == "mcp_secret"));
    registry
        .enable_delegation(fixture.clone(), "base rules".into(), 128000, None)
        .unwrap();
    let result = spawn(
        &registry,
        json!([{"goal":"inspect assigned file only","context":"explicit context","group":"review","output_schema":{"type":"object","properties":{"verified":{"type":"boolean"}},"required":["verified"],"additionalProperties":false}}]),
    );
    let id = result["results"][0]["task"]["task_id"].as_str().unwrap();
    wait(|| fixture.calls.load(Ordering::Acquire) == 1);
    let contexts = fixture.contexts.lock().unwrap();
    assert!(contexts[0]
        .1
        .iter()
        .any(|m| m.text().contains("explicit context")));
    for tool in [
        "mcp_secret",
        "session_read",
        "recall",
        "delegate_task",
        "todo_list",
    ] {
        assert!(!contexts[0].2.iter().any(|t| t == tool), "{tool}");
    }
    assert!(contexts[0].2.iter().any(|t| t == "tool_search"));
    drop(contexts);
    assert!(execution.take_results().is_empty());
    // The parent may finish and a new registry may be built before the child completes.
    execution.finish(None, false, Usage::default());
    let mut next = ToolRegistry::workspace(root.path());
    next.enable_execution(execution.clone()).unwrap();
    next.enable_delegation(fixture.clone(), "base rules".into(), 128000, None)
        .unwrap();
    next.call(
        "delegate_task",
        &json!({"action":"steer","task_id":id,"message":"also report verified evidence"})
            .to_string(),
    )
    .unwrap();
    fixture.release.store(true, Ordering::Release);
    wait(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .result
            .is_some()
    });
    let snapshot = execution.snapshot();
    let child = snapshot.tasks.iter().find(|t| t.task_id == id).unwrap();
    assert_eq!(child.status, TaskStatus::Completed);
    assert_eq!(child.permission, PermissionMode::Careful);
    assert_eq!(child.group.as_deref(), Some("review"));
    assert!(fixture.calls.load(Ordering::Acquire) >= 2);
    assert!(fixture
        .contexts
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .iter()
        .any(|m| m.text().contains("also report verified evidence")));
    let results = execution.take_results();
    assert_eq!(results.len(), 1);
    assert!(results[0].text.contains("verified"));
    assert!(execution.take_results().is_empty());
}

#[test]
fn stop_cancels_child_provider_and_parent_failure_propagates() {
    for parent_cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Arc::new(Fixture::default());
        let (execution, registry) =
            setup(root.path(), fixture.clone(), ExecutionSnapshot::default());
        let result = spawn(&registry, json!([{"goal":"wait for cancellation"}]));
        let id = result["results"][0]["task"]["task_id"].as_str().unwrap();
        wait(|| fixture.calls.load(Ordering::Acquire) == 1);
        if parent_cancel {
            execution.finish(None, true, Usage::default());
        } else {
            registry
                .call(
                    "delegate_task",
                    &json!({"action":"stop","task_id":id}).to_string(),
                )
                .unwrap();
        }
        wait(|| {
            execution
                .snapshot()
                .tasks
                .iter()
                .find(|t| t.task_id == id)
                .unwrap()
                .status
                .terminal()
        });
        assert_eq!(
            execution
                .snapshot()
                .tasks
                .iter()
                .find(|t| t.task_id == id)
                .unwrap()
                .status,
            TaskStatus::Cancelled
        );
    }
}

#[test]
fn limits_schema_validation_and_attachment_path_rejection_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    fixture.release.store(true, Ordering::Release);
    let snapshot = ExecutionSnapshot {
        budget: ExecutionBudget {
            child_tasks: 1,
            ..ExecutionBudget::default()
        },
        ..ExecutionSnapshot::default()
    };
    let (execution, registry) = setup(root.path(), fixture, snapshot);
    let result = spawn(
        &registry,
        json!([
            {"goal":"invalid image","images":["C:\\secrets.png"]},
            {"goal":"invalid schema","output_schema":{"pattern":".*"}},
            {"goal":"invalid result","output_schema":{"type":"object","required":["missing"]}},
            {"goal":"over child budget"}
        ]),
    );
    assert!(result["results"][0]["error"].is_string());
    assert!(result["results"][1]["error"].is_string());
    assert!(result["results"][3]["error"]
        .as_str()
        .unwrap()
        .contains("子任务预算"));
    let id = result["results"][2]["task"]["task_id"].as_str().unwrap();
    wait(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .result
            .is_some()
    });
    let child = execution
        .snapshot()
        .tasks
        .into_iter()
        .find(|t| t.task_id == id)
        .unwrap();
    assert_eq!(child.status, TaskStatus::Failed);
    assert!(child.result.unwrap().contains("output_schema"));
}

#[test]
fn task_scopes_share_token_reservations_and_preserve_parent_identity() {
    let root = tempfile::tempdir().unwrap();
    let snapshot = ExecutionSnapshot {
        budget: ExecutionBudget {
            tokens: 100,
            ..ExecutionBudget::default()
        },
        ..ExecutionSnapshot::default()
    };
    let execution = Execution::new("budget".into(), snapshot);
    let parent = execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    let child = execution
        .spawn_agent(
            "child",
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(TurnControl::default()),
        )
        .unwrap();
    execution.reserve_request(60).unwrap();
    assert!(child.reserve_request(50).is_err());
    child.reserve_request(30).unwrap();
    assert!(execution.reserve_request(80).is_err());
    child
        .check_budget(Usage {
            input: 20,
            output: 10,
        })
        .unwrap();
    child.finish(
        None,
        false,
        Usage {
            input: 20,
            output: 10,
        },
    );
    assert_eq!(execution.active().as_deref(), Some(parent.as_str()));
    assert_eq!(execution.available_tokens(), 70);
    assert!(execution
        .check_budget(Usage {
            input: 60,
            output: 10
        })
        .is_err());
}

#[test]
fn stored_images_resolve_by_id_and_reject_paths_or_tampering() {
    let root = tempfile::tempdir().unwrap();
    let store = hikari_agent::AttachmentStore::new(root.path().to_owned());
    let image = store
        .import("fixture.png", "image/png", b"\x89PNG\r\n\x1a\nfixture")
        .unwrap();
    assert_eq!(store.get_image(&image.id).unwrap().id, image.id);
    assert!(store.get_image("../fixture.png").is_err());
    std::fs::write(&image.path, b"changed").unwrap();
    assert!(store.get_image(&image.id).is_err());
}

#[test]
fn delegated_images_reach_child_and_child_approval_keeps_frozen_parent_permissions() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    let execution = Execution::new("images".into(), ExecutionSnapshot::default());
    execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    let store = hikari_agent::AttachmentStore::new(root.path().join("attachments"));
    let image = store
        .import("fixture.png", "image/png", b"\x89PNG\r\n\x1a\nfixture")
        .unwrap();
    let mut registry = ToolRegistry::workspace(root.path());
    registry.enable_execution(execution.clone()).unwrap();
    registry
        .enable_delegation(fixture.clone(), "base rules".into(), 128000, Some(store))
        .unwrap();
    let result = spawn(
        &registry,
        json!([{"goal":"inspect attached image","images":[image.id]}]),
    );
    let id = result["results"][0]["task"]["task_id"].as_str().unwrap();
    wait(|| fixture.calls.load(Ordering::Acquire) == 1);
    assert_eq!(
        fixture.contexts.lock().unwrap()[0].1[0].attachments()[0].id,
        image.id
    );
    execution.set_mode(PermissionMode::Autonomous);
    assert_eq!(
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .permission,
        PermissionMode::Careful
    );
    fixture.release.store(true, Ordering::Release);
    wait(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status
            .terminal()
    });

    let child = execution
        .spawn_agent(
            "permission probe",
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(TurnControl::default()),
        )
        .unwrap();
    assert_eq!(child.active_mode(), PermissionMode::Careful);
    let task_id = child.active().unwrap();
    let worker = child.clone();
    let pending = std::thread::spawn(move || {
        worker.approve(
            root.path().into(),
            "fixture",
            "{}",
            None,
            "test approval".into(),
            &AtomicBool::new(false),
        )
    });
    wait(|| execution.pending().is_some());
    let question = execution.pending().unwrap();
    assert_eq!(question.task_id, task_id);
    execution.answer(question.id, hikari_agent::execution::Answer::Reject);
    assert!(pending.join().unwrap().unwrap_err().contains("用户拒绝"));
    child.finish(None, true, Usage::default());
}

#[test]
fn session_cancel_reaches_children_after_parent_completion_and_prevents_spawn_races() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Arc::new(Fixture::default());
    let (execution, registry) = setup(root.path(), fixture.clone(), ExecutionSnapshot::default());
    let result = spawn(&registry, json!([{"goal":"wait past parent completion"}]));
    let id = result["results"][0]["task"]["task_id"].as_str().unwrap();
    wait(|| fixture.calls.load(Ordering::Acquire) == 1);
    execution.finish(None, false, Usage::default());
    assert!(execution.active().is_none());
    assert_eq!(
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status,
        TaskStatus::Running
    );
    execution.cancel_all();
    wait(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status
            .terminal()
    });
    assert_eq!(
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    wait(|| !execution.snapshot().results.is_empty());
    assert_eq!(execution.take_results()[0].task_id, id);

    execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    execution.cancel_all();
    assert!(execution
        .spawn_agent(
            "late spawn",
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(TurnControl::default())
        )
        .is_err());
    assert!(execution
        .spawn_process(
            root.path().into(),
            "echo should-not-run".into(),
            &AtomicBool::new(false)
        )
        .is_err());
    execution.finish(None, true, Usage::default());
    execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    assert!(
        execution.check_deadline().is_ok(),
        "cancellation must not poison future turns"
    );
}

#[test]
fn restart_reports_delegate_completed_before_its_final_handoff() {
    let root = tempfile::tempdir().unwrap();
    let execution = Execution::new("handoff".into(), ExecutionSnapshot::default());
    execution
        .begin(root.path().into(), "fixture".into())
        .unwrap();
    let child = execution
        .spawn_agent(
            "task",
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(TurnControl::default()),
        )
        .unwrap();
    let id = child.active().unwrap();
    child.finish(None, false, Usage::default());
    let restored = Execution::new("handoff".into(), execution.snapshot());
    assert_eq!(
        restored
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    assert!(restored
        .take_results()
        .iter()
        .any(|r| r.task_id == id && r.text.contains("未重跑")));
}

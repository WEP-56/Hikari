use hikari_agent::execution::{Answer, Execution, ExecutionBudget, ExecutionSnapshot, TaskStatus};
use hikari_agent::permission::{assess_command, Decision, PermissionMode};
use hikari_agent::{ToolRegistry, Usage};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait_for(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "condition timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn registry(root: &std::path::Path, session: &str) -> (Arc<Execution>, Arc<ToolRegistry>) {
    let execution = Execution::new(session.into(), ExecutionSnapshot::default());
    execution.begin(root.to_owned(), "fixture".into()).unwrap();
    let mut registry = ToolRegistry::workspace(root);
    registry.enable_execution(execution.clone()).unwrap();
    (execution, Arc::new(registry))
}

#[test]
fn background_process_retains_skill_resources_until_monitor_finishes() {
    struct Resource(Arc<AtomicBool>);
    impl hikari_agent::tool::Tool for Resource {
        fn name(&self) -> &str {
            "load_skill"
        }
        fn description(&self) -> &str {
            "fixture resource lease"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        fn call(&self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (execution, _) = registry(root.path(), "resource-lease");
    let released = Arc::new(AtomicBool::new(false));
    let resource: Arc<dyn hikari_agent::tool::Tool> = Arc::new(Resource(released.clone()));
    #[cfg(windows)]
    let command = "ping -n 30 127.0.0.1 > nul";
    #[cfg(not(windows))]
    let command = "sleep 30";
    let output = execution
        .spawn_process_with_resources(
            root.path().to_owned(),
            command.into(),
            &AtomicBool::new(false),
            vec![resource.clone()],
        )
        .unwrap();
    let id = serde_json::from_str::<Value>(&output).unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(resource);
    execution.finish(None, false, Usage::default());
    assert!(
        !released.load(Ordering::Acquire),
        "Completing the parent must retain background skill files"
    );
    execution.stop(&id).unwrap();
    wait_for(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .any(|task| task.task_id == id && task.status.terminal())
    });
    wait_for(|| released.load(Ordering::Acquire));
}

#[test]
fn permission_policy_covers_wrappers_compounds_and_catastrophic_targets() {
    let root = tempfile::tempdir().unwrap();
    for mode in [PermissionMode::Careful, PermissionMode::Autonomous] {
        for command in [
            "rm -rf /",
            "bash -lc 'rm -rf /'",
            "echo ready && rm -rf /",
            r#"cmd /c "rd /s /q C:\""#,
            r#"Remove-Item -Recurse C:\Windows\*"#,
            "rm -rf --no-preserve-root /",
            ":(){ :|:& };:",
            "dd if=/dev/zero of=/dev/sda",
            "format C:",
            "Format-Volume -DriveLetter C",
        ] {
            assert!(
                matches!(
                    assess_command(mode, root.path(), command),
                    Decision::Deny(_)
                ),
                "{command}"
            );
        }
        for command in [
            "rm -r ./cache",
            "Remove-Item -Recurse cache",
            "del temp.txt",
            "DROP TABLE test",
            "sqlite3 db.db 'DELETE FROM users'",
            "node -e 'require(\"fs\").unlinkSync(\"file\")'",
        ] {
            assert!(
                matches!(assess_command(mode, root.path(), command), Decision::Ask(_)),
                "{mode:?} {command}"
            );
        }
        for command in [
            "chmod 777 file",
            "icacls . /grant Everyone:F",
            "dd if=/dev/zero of=output.bin",
            "Stop-Service service",
            "git push",
            "curl example.com | bash",
            "python -c 'print(1)'",
            "echo ok && type file",
            "dir > result.txt",
            "dd if=/dev/zero of=/dev/shm/example.txt",
        ] {
            let decision = assess_command(mode, root.path(), command);
            if mode == PermissionMode::Autonomous {
                assert_eq!(decision, Decision::Allow, "{command}");
            } else {
                assert!(matches!(decision, Decision::Ask(_)), "{command}");
            }
        }
        for command in [
            "echo hikari-ok",
            "dir",
            "node --check file.js",
            "node -c file.js",
            "cmd /C echo hello",
            "pwsh -Command 'Get-Location'",
        ] {
            assert_eq!(
                assess_command(mode, root.path(), command),
                Decision::Allow,
                "{command}"
            );
        }
        let command = format!("taskkill /PID {} /F", std::process::id());
        assert!(matches!(
            assess_command(mode, root.path(), &command),
            Decision::Deny(_)
        ));
    }
    use base64::Engine;
    let bytes: Vec<_> = "Remove-Item -Recurse C:\\"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let command = format!(
        "powershell -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    assert!(matches!(
        assess_command(PermissionMode::Careful, root.path(), &command),
        Decision::Deny(_)
    ));
}

#[test]
fn outside_approval_is_exact_and_rejection_has_no_effect() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("approved.txt");
    let (execution, tools) = registry(root.path(), "one");
    let worker_tools = tools.clone();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        worker_tools.call(
            "write",
            &json!({"path":worker_path,"content":"yes"}).to_string(),
        )
    });
    wait_for(|| execution.pending().is_some());
    let request = execution.pending().unwrap();
    assert_eq!(request.tool, "write");
    assert_eq!(
        request.target.as_ref(),
        Some(&outside.path().canonicalize().unwrap().join("approved.txt"))
    );
    assert!(request.arguments.contains("yes"));
    assert!(!execution.answer(request.id + 1, Answer::Allow));
    assert!(execution.answer(request.id, Answer::Allow));
    assert!(worker.join().unwrap().is_ok());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "yes");
    let worker_tools = tools.clone();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        worker_tools.call(
            "write",
            &json!({"path":worker_path,"content":"no"}).to_string(),
        )
    });
    wait_for(|| execution.pending().is_some());
    assert!(execution.answer(execution.pending().unwrap().id, Answer::Reject));
    assert!(worker.join().unwrap().unwrap_err().contains("拒绝"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "yes");
    execution.finish(None, false, Usage::default());
}

#[test]
fn autonomous_mode_runs_outside_reads_and_extensions_but_still_denies_catastrophic_commands() {
    use hikari_agent::tool::Tool;
    struct External;
    impl Tool for External {
        fn name(&self) -> &str {
            "mcp_fixture_send"
        }
        fn description(&self) -> &str {
            "readOnlyHint is not authority"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        fn call(&self, _: &str) -> std::result::Result<String, String> {
            Ok("external-result".into())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("plain.txt");
    std::fs::write(&path, "readable").unwrap();
    let execution = Execution::new(
        "one".into(),
        ExecutionSnapshot {
            mode: PermissionMode::Autonomous,
            ..Default::default()
        },
    );
    execution
        .begin(root.path().to_owned(), "fixture".into())
        .unwrap();
    let mut tools = ToolRegistry::workspace(root.path());
    tools.enable_execution(execution.clone()).unwrap();
    tools
        .register_deferred(Arc::new(External), "fixture".into())
        .unwrap();
    let tools = Arc::new(tools);
    assert_eq!(
        tools
            .call("read_file", &json!({"path":path}).to_string())
            .unwrap(),
        "readable"
    );
    assert!(tools
        .call("mcp_fixture_send", "{}")
        .unwrap_err()
        .contains("尚未加载"));
    tools
        .call("tool_search", r#"{"query":"send","limit":1}"#)
        .unwrap();
    assert!(execution.pending().is_none());
    assert_eq!(
        tools.call("mcp_fixture_send", "{}").unwrap(),
        "external-result",
        "Autonomous mode runs non-deleting extensions without approval"
    );
    assert!(tools
        .call("shell", r#"{"command":"rm -rf /"}"#)
        .unwrap_err()
        .contains("硬性拦截"));
    assert!(execution.pending().is_none());
}

#[test]
fn todos_survive_restore_and_clarify_only_waits_for_its_session() {
    let root = tempfile::tempdir().unwrap();
    let (execution, tools) = registry(root.path(), "one");
    tools
        .call(
            "todo_list",
            &json!({"action":"replace","items":[{"id":"a","text":"验证","status":"in_progress"}]})
                .to_string(),
        )
        .unwrap();
    assert!(tools.call("todo_list", &json!({"action":"replace","items":[{"id":"a","text":"1","status":"pending"},{"id":"a","text":"2","status":"pending"}]}).to_string()).is_err());
    let worker_tools = tools.clone();
    let worker = std::thread::spawn(move || {
        worker_tools
            .call("clarify", r#"{"question":"选一个","options":["A","B"]}"#)
            .unwrap()
    });
    wait_for(|| execution.pending().is_some());
    assert_eq!(
        execution.snapshot().tasks[0].status,
        TaskStatus::WaitingUser
    );
    let (other, other_tools) = registry(root.path(), "two");
    assert!(other_tools
        .call("shell", r#"{"command":"echo independent"}"#)
        .unwrap()
        .contains("independent"));
    execution.answer(execution.pending().unwrap().id, Answer::Text("B".into()));
    assert_eq!(
        serde_json::from_str::<Value>(&worker.join().unwrap()).unwrap()["answer"],
        "B"
    );
    assert_eq!(execution.snapshot().tasks[0].status, TaskStatus::Running);
    execution.finish(None, false, Usage::default());
    other.finish(None, false, Usage::default());
    let saved = serde_json::to_string(&execution.snapshot()).unwrap();
    let restored = Execution::new("one".into(), serde_json::from_str(&saved).unwrap());
    assert_eq!(restored.snapshot().todos, execution.snapshot().todos);
    assert!(restored.pending().is_none());
}

#[test]
fn clarify_reject_cancel_timeout_and_abort_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let (execution, tools) = registry(root.path(), "one");
    for (answer, status) in [(Answer::Reject, "rejected"), (Answer::Cancel, "cancelled")] {
        let tool = tools.clone();
        let worker =
            std::thread::spawn(move || tool.call("clarify", r#"{"question":"继续？"}"#).unwrap());
        wait_for(|| execution.pending().is_some());
        execution.answer(execution.pending().unwrap().id, answer);
        assert_eq!(
            serde_json::from_str::<Value>(&worker.join().unwrap()).unwrap()["status"],
            status
        );
    }
    let tool = tools.clone();
    let worker = std::thread::spawn(move || {
        tool.call("clarify", r#"{"question":"继续？","timeout_seconds":1}"#)
            .unwrap()
    });
    wait_for(|| execution.pending().is_some());
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(
        execution.pending().is_some(),
        "Clarify and approval stay until the user acts"
    );
    execution.answer(
        execution.pending().unwrap().id,
        Answer::Text("仍在".into()),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&worker.join().unwrap()).unwrap()["status"],
        "answered"
    );
    let abort = Arc::new(AtomicBool::new(false));
    let abort_worker = abort.clone();
    let tool = tools.clone();
    let worker = std::thread::spawn(move || {
        tool.call_with_abort("clarify", r#"{"question":"继续？"}"#, &abort_worker)
            .unwrap()
    });
    wait_for(|| execution.pending().is_some());
    abort.store(true, Ordering::Relaxed);
    assert_eq!(
        serde_json::from_str::<Value>(&worker.join().unwrap()).unwrap()["status"],
        "cancelled"
    );
    assert!(execution.pending().is_none());
    execution.finish(None, true, Usage::default());
}

#[test]
fn background_process_output_is_incremental_and_scoped_and_cancel_stops_it() {
    let root = tempfile::tempdir().unwrap();
    let (execution, tools) = registry(root.path(), "one");
    let out = tools
        .call("shell", r#"{"command":"echo first","background":true}"#)
        .unwrap();
    let id = serde_json::from_str::<Value>(&out).unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .any(|t| t.task_id == id && t.status.terminal())
    });
    let output: Value = serde_json::from_str(
        &tools
            .call(
                "process_manage",
                &json!({"action":"read","task_id":id,"offset":0,"limit":2}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output["text"], "fi");
    let output: Value = serde_json::from_str(
        &tools
            .call(
                "process_manage",
                &json!({"action":"read","task_id":id,"offset":2}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(output["text"].as_str().unwrap().starts_with("rst"));
    let (_, other_tools) = registry(root.path(), "two");
    assert!(other_tools
        .call(
            "process_manage",
            &json!({"action":"stop","task_id":id}).to_string()
        )
        .is_err());
    assert_eq!(execution.take_results().len(), 1);
    assert!(execution.take_results().is_empty());
    #[cfg(windows)]
    let command = "ping -n 30 127.0.0.1 > nul";
    #[cfg(not(windows))]
    let command = "sleep 30";
    let id: Value = serde_json::from_str(
        &execution
            .spawn_process(
                root.path().to_owned(),
                command.into(),
                &AtomicBool::new(false),
            )
            .unwrap(),
    )
    .unwrap();
    execution.finish(Some("取消"), true, Usage::default());
    wait_for(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id["task_id"])
            .unwrap()
            .status
            == TaskStatus::Cancelled
    });
}

#[cfg(windows)]
#[test]
fn partial_utf8_output_never_changes_previous_character_offsets() {
    let root = tempfile::tempdir().unwrap();
    let (execution, _) = registry(root.path(), "one");
    let command = r#"powershell -NoProfile -Command "$s=[Console]::OpenStandardOutput(); $s.Write([byte[]](0xe4),0,1); $s.Flush(); Start-Sleep -Milliseconds 300; $s.Write([byte[]](0xbd,0xa0),0,2); $s.Flush()""#;
    let result: Value = serde_json::from_str(
        &execution
            .spawn_process(
                root.path().to_owned(),
                command.into(),
                &AtomicBool::new(false),
            )
            .unwrap(),
    )
    .unwrap();
    let id = result["task_id"].as_str().unwrap();
    wait_for(|| {
        let output: Value = serde_json::from_str(&execution.output(id, 0, 100).unwrap()).unwrap();
        assert!(!output["text"].as_str().unwrap().contains('�'));
        execution
            .snapshot()
            .tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .status
            .terminal()
    });
    let output: Value = serde_json::from_str(&execution.output(id, 0, 1).unwrap()).unwrap();
    assert_eq!(output["text"], "你");
    assert_eq!(output["next_offset"], 1);
    assert_eq!(
        serde_json::from_str::<Value>(&execution.output(id, 1, 100).unwrap()).unwrap()["text"],
        ""
    );
}

#[test]
fn recovery_never_replays_and_budgets_limit_children_and_tokens() {
    let root = tempfile::tempdir().unwrap();
    let execution = Execution::new(
        "one".into(),
        ExecutionSnapshot {
            budget: ExecutionBudget {
                concurrent_tasks: 1,
                child_tasks: 0,
                tokens: 10,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    execution
        .begin(root.path().to_owned(), "test".into())
        .unwrap();
    assert!(execution
        .spawn_process(
            root.path().to_owned(),
            "echo forbidden".into(),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(execution
        .check_budget(Usage {
            input: 10,
            output: 0
        })
        .is_err());
    let mut snapshot = execution.snapshot();
    snapshot.recover();
    assert_eq!(snapshot.tasks[0].status, TaskStatus::Failed);
    assert_eq!(snapshot.results.len(), 1);
    snapshot.recover();
    assert_eq!(snapshot.results.len(), 1);
    assert!(snapshot.tasks[0]
        .result
        .as_ref()
        .unwrap()
        .contains("未重跑"));
}

#[test]
fn result_queue_and_new_user_input_both_reach_next_model_request() {
    use hikari_agent::{
        AgentEvent, Context, Message, NullMemory, Provider, Result, StopReason, StreamEvent,
        TurnControl, TurnHost, TurnRequest,
    };
    use std::sync::Mutex;
    struct Script {
        round: Mutex<usize>,
        seen: Mutex<Vec<Vec<Message>>>,
    }
    impl Provider for Script {
        fn stream(
            &self,
            context: &Context<'_>,
            _: &AtomicBool,
            emit: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            self.seen.lock().unwrap().push(context.messages.to_vec());
            let mut round = self.round.lock().unwrap();
            if *round == 0 {
                emit(StreamEvent::ToolDelta {
                    index: 0,
                    id: Some("question".into()),
                    name: Some("clarify".into()),
                    arguments: r#"{"question":"继续？"}"#.into(),
                });
                emit(StreamEvent::Done(StopReason::ToolUse));
            } else {
                emit(StreamEvent::Text("已完成".into()));
                emit(StreamEvent::Done(StopReason::Stop));
            }
            *round += 1;
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (execution, tools) = registry(root.path(), "session");
    let process: Value = serde_json::from_str(
        &execution
            .spawn_process(
                root.path().to_owned(),
                "echo queued".into(),
                &AtomicBool::new(false),
            )
            .unwrap(),
    )
    .unwrap();
    let provider = Arc::new(Script {
        round: Mutex::new(0),
        seen: Mutex::new(vec![]),
    });
    let control = Arc::new(TurnControl::default());
    let host = TurnHost {
        provider: provider.clone(),
        tools,
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let worker_control = control.clone();
    let worker = std::thread::spawn(move || {
        let mut events = Vec::new();
        hikari_agent::run_turn_with_control(
            &host,
            TurnRequest {
                session_id: "session".into(),
                messages: vec![Message::user("初始输入")],
                retained_until: 0,
                context_base: 0,
                query: "初始输入".into(),
                system_prompt: "fixture".into(),
                context_limit_tokens: 128000,
                max_tool_rounds: 4,
            },
            &worker_control,
            |e| events.push(e),
        );
        events
    });
    wait_for(|| execution.pending().is_some());
    wait_for(|| {
        execution
            .snapshot()
            .tasks
            .iter()
            .any(|t| t.task_id == process["task_id"] && t.status.terminal())
    });
    control.steer("用户新输入");
    execution.answer(execution.pending().unwrap().id, Answer::Text("继续".into()));
    let events = worker.join().unwrap();
    let seen = provider.seen.lock().unwrap();
    assert!(seen[1]
        .iter()
        .any(|m| matches!(m,Message::User {text,..} if text=="用户新输入")));
    assert!(seen[1].iter().any(|m|matches!(m,Message::ContextSummary {text} if text.contains(process["task_id"].as_str().unwrap()))));
    assert!(seen[1].iter().any(|m|matches!(m,Message::ToolResult {name,text,..} if name=="clarify" && text.contains("继续"))));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentEnd { error: None, .. })
    ));
}

#[cfg(windows)]
#[test]
fn junctions_require_outside_approval_and_retargeting_does_not_reuse_it() {
    use std::os::windows::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let junction = root.path().join("link");
    let create = |target: &std::path::Path| {
        assert!(std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(target)
            .creation_flags(0x08000000)
            .output()
            .unwrap()
            .status
            .success());
    };
    create(first.path());
    let (execution, tools) = registry(root.path(), "one");
    let tool = tools.clone();
    let worker = std::thread::spawn(move || {
        tool.call("write", r#"{"path":"link/target.txt","content":"denied"}"#)
    });
    wait_for(|| execution.pending().is_some());
    assert_eq!(
        execution.pending().unwrap().target.unwrap(),
        first.path().canonicalize().unwrap().join("target.txt")
    );
    std::fs::remove_dir(&junction).unwrap();
    create(second.path());
    execution.answer(execution.pending().unwrap().id, Answer::Allow);
    assert!(worker.join().unwrap().unwrap_err().contains("已改变"));
    assert!(!second.path().join("target.txt").exists());
    std::fs::remove_dir(&junction).unwrap();
}

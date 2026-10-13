use hikari_agent::tool::Tool;
use hikari_agent::{
    run_turn_with_control, AgentEvent, Context, Message, NullMemory, Provider, Result, StopReason,
    StreamEvent, ToolRegistry, TurnControl, TurnHost, TurnRequest,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Script {
    turns: Mutex<usize>,
    seen: Mutex<Vec<Vec<Message>>>,
    control: Arc<TurnControl>,
}
impl Provider for Script {
    fn stream(
        &self,
        context: &Context<'_>,
        _: &AtomicBool,
        emit: &mut dyn FnMut(StreamEvent),
    ) -> Result<()> {
        self.seen.lock().unwrap().push(context.messages.to_vec());
        let mut index = self.turns.lock().unwrap();
        match *index {
            0 => {
                self.control.steer("change the current task");
                emit(StreamEvent::ToolDelta {
                    index: 25,
                    id: Some("a".into()),
                    name: Some("echo".into()),
                    arguments: "{}".into(),
                });
                emit(StreamEvent::Done(StopReason::ToolUse));
            }
            1 => {
                self.control.follow_up("next request");
                emit(StreamEvent::Text("first reply".into()));
                emit(StreamEvent::Done(StopReason::Stop));
            }
            _ => {
                emit(StreamEvent::Text("second reply".into()));
                emit(StreamEvent::Done(StopReason::Stop));
            }
        }
        *index += 1;
        Ok(())
    }
}
struct Echo;
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object"})
    }
    fn call(&self, _: &str) -> std::result::Result<String, String> {
        Ok("tool evidence".into())
    }
}

#[test]
fn recall_is_reused_during_tool_rounds_and_refreshes_for_identical_followup() {
    struct Counting(std::sync::atomic::AtomicUsize);
    impl hikari_agent::Memory for Counting {
        fn enabled(&self) -> bool {
            true
        }
        fn recall(&self, _: &str) -> Vec<hikari_agent::RecallItem> {
            self.0.fetch_add(1, Ordering::SeqCst);
            vec![]
        }
        fn retain(&self, _: &str, _: &[Message]) -> Result<hikari_agent::RetainReceipt> {
            Ok(hikari_agent::RetainReceipt {
                kept: 0,
                dropped: 0,
                pending: true,
            })
        }
        fn consolidate_pending(&self) -> Result<hikari_agent::ConsolidateReceipt> {
            unreachable!()
        }
    }
    struct Rounds {
        round: Mutex<usize>,
        control: Arc<TurnControl>,
    }
    impl Provider for Rounds {
        fn stream(
            &self,
            _: &Context<'_>,
            _: &AtomicBool,
            emit: &mut dyn FnMut(StreamEvent),
        ) -> Result<()> {
            let mut round = self.round.lock().unwrap();
            if *round < 2 {
                emit(StreamEvent::ToolDelta {
                    index: 0,
                    id: Some(format!("tool-{round}")),
                    name: Some("echo".into()),
                    arguments: "{}".into(),
                });
                emit(StreamEvent::Done(StopReason::ToolUse));
            } else {
                if *round == 2 {
                    self.control.follow_up("same");
                }
                emit(StreamEvent::Text("reply".into()));
                emit(StreamEvent::Done(StopReason::Stop));
            }
            *round += 1;
            Ok(())
        }
    }
    let control = Arc::new(TurnControl::default());
    let memory = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
    let host = TurnHost {
        provider: Arc::new(Rounds {
            round: Mutex::new(0),
            control: control.clone(),
        }),
        tools: Arc::new(ToolRegistry::new(vec![Arc::new(Echo)])),
        memory: memory.clone(),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "s".into(),
        messages: vec![Message::user("same")],
        retained_until: 0,
        context_base: 0,
        query: "same".into(),
        system_prompt: "base".into(),
        context_limit_tokens: 128_000,
        max_tool_rounds: 50,
    };
    run_turn_with_control(&host, request, &control, |_| {});
    assert_eq!(memory.0.load(Ordering::SeqCst), 2);
}
#[test]
fn steering_waits_for_tools_followup_waits_for_reply_and_sparse_tool_indexes_work() {
    let control = Arc::new(TurnControl::default());
    let provider = Arc::new(Script {
        turns: Mutex::new(0),
        seen: Mutex::new(Vec::new()),
        control: control.clone(),
    });
    let host = TurnHost {
        provider: provider.clone(),
        tools: Arc::new(ToolRegistry::new(vec![Arc::new(Echo)])),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "session".into(),
        messages: vec![Message::user("initial")],
        retained_until: 0,
        context_base: 0,
        query: "initial".into(),
        system_prompt: "base".into(),
        context_limit_tokens: 128_000,
        max_tool_rounds: 50,
    };
    let mut events = Vec::new();
    run_turn_with_control(&host, request, &control, |e| events.push(e));
    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(matches!(seen[1].get(2),Some(Message::ToolResult {text,..}) if text=="tool evidence"));
    assert_eq!(seen[1][3].text(), "change the current task");
    assert_eq!(seen[2].last().unwrap().text(), "next request");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::InputDelivered { .. }))
            .count(),
        2
    );
    assert_eq!(control.pending(), 0);
}

struct CancellingProvider;
impl Provider for CancellingProvider {
    fn stream(
        &self,
        _: &Context<'_>,
        abort: &AtomicBool,
        emit: &mut dyn FnMut(StreamEvent),
    ) -> Result<()> {
        for index in 0..2 {
            emit(StreamEvent::ToolDelta {
                index,
                id: Some(format!("call-{index}")),
                name: Some("echo".into()),
                arguments: "{}".into(),
            });
        }
        emit(StreamEvent::Done(StopReason::ToolUse));
        abort.store(true, Ordering::Relaxed);
        Ok(())
    }
}
#[test]
fn cancellation_keeps_results_paired_with_every_unexecuted_tool_call() {
    let host = TurnHost {
        provider: Arc::new(CancellingProvider),
        tools: Arc::new(ToolRegistry::new(vec![Arc::new(Echo)])),
        memory: Arc::new(NullMemory),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let request = TurnRequest {
        session_id: "session".into(),
        messages: vec![Message::user("initial")],
        retained_until: 0,
        context_base: 0,
        query: "initial".into(),
        system_prompt: "base".into(),
        context_limit_tokens: 128_000,
        max_tool_rounds: 50,
    };
    let mut messages = Vec::new();
    run_turn_with_control(&host, request, &TurnControl::default(), |event| {
        if let AgentEvent::AgentEnd {
            messages: context,
            error,
            ..
        } = event
        {
            assert!(error.is_some());
            messages = context;
        }
    });
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Message::ToolResult { is_error: true, .. }))
            .count(),
        2
    );
}

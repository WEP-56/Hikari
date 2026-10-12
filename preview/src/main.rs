use gpui_kit::component::{
    bubble::{Bubble, BubbleVariant},
    button::{Button, ButtonVariants as _},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    message::{Message, MessageAlignment, MessageContent},
    switch::Switch,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, Theme, ThemeColor, ThemeMode, TitleBar,
};
use gpui_kit::prelude::{FluentBuilder as _, StatefulInteractiveElement as _};
use gpui_kit::*;
use hikari_agent::{
    companion_prompt, run_turn_with_control, AgentEvent, ApiProtocol, ConsolidateState,
    Message as AgentMessage, NullMemory, ProtocolProvider, RetainState, ToolRegistry, TurnControl,
    TurnHost, TurnRequest, Usage, DEFAULT_CONTEXT_TOKENS,
};
use hikari_runtime::{
    AppConfig, ConfigStore, McpManager, McpServerConfig, McpStatus, McpTransport, Session,
    SessionStore, SkillCatalog,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

mod shell;

enum LineBody {
    User(SharedString),
    Assistant {
        text: SharedString,
        thinking: SharedString,
        live: bool,
    },
    Tool {
        id: String,
        name: SharedString,
        detail: SharedString,
        running: bool,
        error: bool,
    },
    Note(SharedString),
}

struct Line {
    body: LineBody,
}

impl Line {
    fn user(text: impl Into<SharedString>) -> Self {
        Self {
            body: LineBody::User(text.into()),
        }
    }

    fn assistant(text: impl Into<SharedString>) -> Self {
        Self {
            body: LineBody::Assistant {
                text: text.into(),
                thinking: SharedString::new(""),
                live: false,
            },
        }
    }

    fn pending() -> Self {
        Self {
            body: LineBody::Assistant {
                text: "…".into(),
                thinking: SharedString::new(""),
                live: true,
            },
        }
    }

    fn note(text: impl Into<SharedString>) -> Self {
        Self {
            body: LineBody::Note(text.into()),
        }
    }

    fn is_live(&self) -> bool {
        matches!(self.body, LineBody::Assistant { live: true, .. })
    }
}

struct Talk {
    id: u64,
    title: SharedString,
    preview: SharedString,
    when: SharedString,
    lines: Vec<Line>,
    messages: Vec<AgentMessage>,
    retained_until: usize,
    transcript: Vec<AgentMessage>,
    usage: Usage,
}

struct Incoming {
    talk_id: u64,
    gen: u64,
    event: AgentEvent,
}

struct RuntimeUpdate {
    statuses: Vec<McpStatus>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Provider,
    Appearance,
    Memory,
    Persona,
    Skill,
    Mcp,
    Channels,
    About,
}

impl Section {
    fn all() -> [Section; 8] {
        [
            Section::Provider,
            Section::Appearance,
            Section::Memory,
            Section::Persona,
            Section::Skill,
            Section::Mcp,
            Section::Channels,
            Section::About,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Section::Provider => "提供商",
            Section::Appearance => "界面",
            Section::Memory => "记忆",
            Section::Persona => "人格",
            Section::Skill => "Skill",
            Section::Mcp => "MCP",
            Section::Channels => "外部通讯",
            Section::About => "关于",
        }
    }
}

struct Preview {
    talks: Vec<Talk>,
    active: Option<usize>,
    sidebar: bool,
    sidebar_width: f32,
    resizing: bool,
    resize_origin: f32,
    resize_start: f32,
    thinking: usize,
    next_talk_id: u64,
    turn_gen: u64,
    busy: bool,
    turn_control: Arc<TurnControl>,
    running_talk: Option<u64>,
    abort: Option<Arc<AtomicBool>>,
    event_tx: Sender<Incoming>,
    event_rx: Receiver<Incoming>,
    turn_pump: Option<Task<()>>,
    usage: Usage,
    searching: bool,
    settings: bool,
    section: Section,
    dark: bool,
    memory_on: bool,
    terminal_open: bool,
    terminal_text: String,
    terminal_error: Option<String>,
    shell: Option<shell::Shell>,
    terminal_focus: FocusHandle,
    terminal_pump: Option<Task<()>>,
    draft: Entity<TextareaState>,
    search: Entity<InputState>,
    provider: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
    key_env: Entity<InputState>,
    call_name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    config: AppConfig,
    config_store: ConfigStore,
    session_store: SessionStore,
    skills: SkillCatalog,
    skill_path: Entity<InputState>,
    mcp_config: Entity<TextareaState>,
    mcp: Arc<McpManager>,
    mcp_statuses: Vec<McpStatus>,
    runtime_tx: Sender<RuntimeUpdate>,
    runtime_rx: Receiver<RuntimeUpdate>,
    management_notice: String,
    mcp_name: Entity<InputState>,
    mcp_target: Entity<InputState>,
    mcp_args: Entity<InputState>,
    mcp_http: bool,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config_store = ConfigStore::new(workspace_dir().join(".hikari"));
        let session_store = SessionStore::new(workspace_dir().join(".hikari"));
        let mut management_notice = String::new();
        let config = config_store.load().unwrap_or_else(|e| {
            management_notice = e.message;
            AppConfig::default()
        });
        let skills = SkillCatalog::discover(
            &workspace_dir(),
            &config.skill_roots,
            &config.disabled_skills,
        );
        let draft = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .placeholder("想说的话，放在这里")
                .submit_on_enter(true);
            state.set_auto_grow(1, 6, cx);
            state
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("找一段过去的对话"));
        let provider = cx
            .new(|cx| InputState::new(window, cx).default_value(config.provider.base_url.clone()));
        let model =
            cx.new(|cx| InputState::new(window, cx).default_value(config.provider.model.clone()));
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("本次运行使用的密钥")
                .masked(true)
        });
        let key_env = cx.new(|cx| {
            InputState::new(window, cx).default_value(config.provider.api_key_env.clone())
        });
        let call_name =
            cx.new(|cx| InputState::new(window, cx).default_value(config.persona_name.clone()));
        let skill_path = cx
            .new(|cx| InputState::new(window, cx).placeholder("Skill 目录或包含多个 Skill 的目录"));
        let mcp_config = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(
                serde_json::to_string_pretty(&config.mcp_servers).unwrap_or_default(),
                window,
                cx,
            );
            state
        });
        let mcp_name = cx.new(|cx| InputState::new(window, cx).placeholder("服务名称"));
        let mcp_target = cx.new(|cx| InputState::new(window, cx).placeholder("启动命令或服务 URL"));
        let mcp_args = cx.new(|cx| InputState::new(window, cx).default_value("[]"));

        let search_sub = cx.subscribe_in(&search, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let draft_sub = cx.subscribe_in(&draft, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.send(window, cx);
            }
        });

        let (event_tx, event_rx) = mpsc::channel();
        let sessions = session_store.load().unwrap_or_else(|e| {
            management_notice = e.message;
            Vec::new()
        });
        let next_talk_id = sessions.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        let talks = sessions.into_iter().map(talk_from_session).collect();
        let (runtime_tx, runtime_rx) = mpsc::channel();
        let mcp = Arc::new(McpManager::default());
        Theme::change(
            if config.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );

        Self {
            talks,
            active: None,
            sidebar: true,
            sidebar_width: 292.,
            resizing: false,
            resize_origin: 0.,
            resize_start: 292.,
            thinking: 3,
            next_talk_id,
            turn_gen: 0,
            busy: false,
            turn_control: Arc::new(TurnControl::default()),
            running_talk: None,
            abort: None,
            event_tx,
            event_rx,
            turn_pump: None,
            usage: Usage::default(),
            searching: false,
            settings: false,
            section: Section::Provider,
            dark: config.dark,
            memory_on: false,
            terminal_open: false,
            terminal_text: String::new(),
            terminal_error: None,
            shell: None,
            terminal_focus: cx.focus_handle(),
            terminal_pump: None,
            draft,
            search,
            provider,
            model,
            key,
            key_env,
            call_name,
            _subscriptions: vec![search_sub, draft_sub],
            config,
            config_store,
            session_store,
            skills,
            skill_path,
            mcp_config,
            mcp,
            mcp_statuses: Vec::new(),
            runtime_tx,
            runtime_rx,
            management_notice,
            mcp_name,
            mcp_target,
            mcp_args,
            mcp_http: false,
        }
    }

    fn set_dark(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = dark;
        Theme::change(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.queue_input(false, window, cx);
            return;
        }
        let text = self.draft.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let idx = match self.active {
            Some(idx) => idx,
            None => {
                let id = self.next_talk_id;
                self.next_talk_id += 1;
                self.talks.insert(
                    0,
                    Talk {
                        id,
                        title: title_from(&text),
                        preview: text.clone().into(),
                        when: "现在".into(),
                        lines: Vec::new(),
                        messages: Vec::new(),
                        retained_until: 0,
                        transcript: Vec::new(),
                        usage: Usage::default(),
                    },
                );
                self.active = Some(0);
                0
            }
        };
        let talk_id = self.talks[idx].id;
        self.talks[idx].preview = text.clone().into();
        if self.talks[idx].lines.is_empty() {
            self.talks[idx].title = title_from(&text);
        }
        self.talks[idx].lines.push(Line::user(&text));
        self.talks[idx].lines.push(Line::pending());
        self.talks[idx].messages.push(AgentMessage::user(&text));
        self.talks[idx].transcript.push(AgentMessage::user(&text));
        self.save_sessions();
        self.draft.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });

        self.sync_provider_config(cx);
        let provider_config = self.config.provider.clone();
        let key = self.key.read(cx).value().to_string();
        let key = if key.trim().is_empty() {
            std::env::var(&provider_config.api_key_env).unwrap_or_default()
        } else {
            key
        };
        let name = self.call_name.read(cx).value().to_string();
        let thinking = THINKING[self.thinking];
        let messages = self.talks[idx].messages.clone();
        let retained_until = self.talks[idx].retained_until;
        // 记忆核心还没接上。开关先留着，loop 里的 recall / retain 缝已经在。
        let _memory_on = self.memory_on;

        let provider = match ProtocolProvider::new(provider_config.clone(), key.trim()) {
            Ok(provider) => provider,
            Err(err) => {
                self.finish_pending(idx, &err.message);
                cx.notify();
                return;
            }
        };

        self.turn_gen += 1;
        let gen = self.turn_gen;
        let abort = Arc::new(AtomicBool::new(false));
        self.abort = Some(abort.clone());
        self.busy = true;
        self.running_talk = Some(talk_id);
        self.turn_control = Arc::new(TurnControl::default());
        let turn_control = self.turn_control.clone();
        self.start_pump(cx);
        let tx = self.event_tx.clone();
        let cwd = workspace_dir();
        let skills = self.skills.clone();
        let mcp = self.mcp.clone();
        let mcp_configs = self.config.mcp_servers.clone();
        let runtime_tx = self.runtime_tx.clone();
        let max_tool_rounds = self.config.max_tool_rounds;
        thread::spawn(move || {
            mcp.reconcile(&mcp_configs, &cwd);
            let _ = runtime_tx.send(RuntimeUpdate {
                statuses: mcp.statuses(),
            });
            let mut tools = ToolRegistry::workspace(cwd);
            let setup_error = skills
                .register(&mut tools)
                .and_then(|_| mcp.register(&mut tools))
                .err();
            let host = TurnHost {
                provider: Arc::new(provider),
                tools: Arc::new(tools),
                memory: Arc::new(NullMemory),
                abort,
            };
            let request = TurnRequest {
                session_id: format!("talk-{talk_id}"),
                messages,
                retained_until,
                query: text.clone(),
                system_prompt: format!("{}{}", companion_prompt(&name, thinking), skills.prompt()),
                context_limit_tokens: provider_config.context_tokens,
                max_tool_rounds,
            };
            let emit = |event| {
                let _ = tx.send(Incoming {
                    talk_id,
                    gen,
                    event,
                });
            };
            if let Some(error) = setup_error {
                emit(AgentEvent::AgentEnd {
                    messages: request.messages,
                    error: Some(error),
                    usage: Usage::default(),
                    retained_until,
                });
                return;
            }
            if catch_unwind(AssertUnwindSafe(|| {
                run_turn_with_control(&host, request, &turn_control, emit)
            }))
            .is_err()
            {
                let _ = tx.send(Incoming {
                    talk_id,
                    gen,
                    event: AgentEvent::AgentEnd {
                        messages: Vec::new(),
                        error: Some("内部出错了。".into()),
                        usage: Usage::default(),
                        retained_until,
                    },
                });
            }
        });
        cx.notify();
    }

    fn start_pump(&mut self, cx: &mut Context<Self>) {
        if self.turn_pump.is_some() {
            return;
        }
        self.turn_pump = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            if this.update(cx, |this, cx| this.drain_events(cx)).is_err() {
                break;
            }
        }));
    }

    fn drain_events(&mut self, cx: &mut Context<Self>) {
        while let Ok(update) = self.runtime_rx.try_recv() {
            self.mcp_statuses = update.statuses;
            cx.notify();
        }
        loop {
            match self.event_rx.try_recv() {
                Ok(incoming) => self.apply_event(incoming, cx),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply_event(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        let Incoming {
            talk_id,
            gen,
            event,
        } = incoming;
        let Some(idx) = self.talks.iter().position(|talk| talk.id == talk_id) else {
            return;
        };
        match event {
            AgentEvent::InputDelivered { text } => {
                self.talks[idx].lines.push(Line::user(text.clone()));
                self.talks[idx].transcript.push(AgentMessage::user(text));
            }
            AgentEvent::MessageStart { .. } => self.ensure_live(idx),
            AgentEvent::MessageUpdate { text, thinking } => {
                self.ensure_live(idx);
                if let Some(line) = self.talks[idx]
                    .lines
                    .iter_mut()
                    .rev()
                    .find(|line| line.is_live())
                {
                    if let LineBody::Assistant {
                        text: slot,
                        thinking: think,
                        ..
                    } = &mut line.body
                    {
                        *slot = text.into();
                        *think = thinking.into();
                    }
                }
            }
            AgentEvent::MessageEnd { message } => {
                self.talks[idx].transcript.push(message.clone());
                if let Some(pos) = self.talks[idx].lines.iter().rposition(Line::is_live) {
                    let empty = message.text().is_empty();
                    if empty {
                        self.talks[idx].lines.remove(pos);
                    } else if let LineBody::Assistant { live, text, .. } =
                        &mut self.talks[idx].lines[pos].body
                    {
                        *live = false;
                        if text.is_empty() || text.as_ref() == "…" {
                            *text = message.text().to_string().into();
                        }
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.close_empty_live(idx);
                self.talks[idx].lines.push(Line {
                    body: LineBody::Tool {
                        id: tool_call_id,
                        name: tool_name.into(),
                        detail: args.into(),
                        running: true,
                        error: false,
                    },
                });
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                text,
                is_error,
            } => {
                self.talks[idx].transcript.push(AgentMessage::tool_result(
                    &tool_call_id,
                    &tool_name,
                    &text,
                    is_error,
                ));
                if let Some(line) = self.talks[idx].lines.iter_mut().rev().find(
                    |line| matches!(&line.body, LineBody::Tool { id, .. } if id == &tool_call_id),
                ) {
                    if let LineBody::Tool {
                        detail,
                        running,
                        error,
                        ..
                    } = &mut line.body
                    {
                        *detail = text.into();
                        *running = false;
                        *error = is_error;
                    }
                }
            }
            AgentEvent::MemoryRecall { items, .. } => {
                if !items.is_empty() {
                    let text = items
                        .iter()
                        .map(|item| item.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    self.insert_before_live(idx, Line::note(format!("想起了 · {text}")));
                }
            }
            AgentEvent::MemoryRetain {
                state, error, kept, ..
            } => {
                let note = match state {
                    RetainState::Failed => Some(format!("没记下。{}", error.unwrap_or_default())),
                    RetainState::Done if kept > 0 => Some(format!("记下了 {kept} 件")),
                    _ => None,
                };
                if let Some(note) = note {
                    self.insert_before_live(idx, Line::note(note));
                }
            }
            AgentEvent::MemoryConsolidation { state, error, .. } => {
                if state == ConsolidateState::Failed {
                    self.insert_before_live(
                        idx,
                        Line::note(format!("沉淀没完成。{}", error.unwrap_or_default())),
                    );
                }
            }
            AgentEvent::AutoCompactionStart { .. } => {
                self.insert_before_live(idx, Line::note("在把更早的对话收起来"));
            }
            AgentEvent::AutoCompactionEnd { error } => {
                if let Some(error) = error {
                    self.insert_before_live(idx, Line::note(error));
                }
            }
            AgentEvent::TurnEnd { .. } => {}
            AgentEvent::AgentEnd {
                messages,
                error,
                usage,
                retained_until,
            } => {
                if !messages.is_empty() {
                    self.talks[idx].messages = messages;
                }
                self.usage = usage;
                self.talks[idx].usage = usage;
                self.talks[idx].retained_until = retained_until;
                self.finish_pending(idx, error.as_deref().unwrap_or(""));
                if gen == self.turn_gen {
                    self.busy = false;
                    self.running_talk = None;
                    self.abort = None;
                    let pending = self.turn_control.drain_pending();
                    if !pending.is_empty() {
                        self.management_notice =
                            format!("{} 条排队输入未执行，已保留在会话旁注", pending.len());
                        // No window is available in the event pump; preserve the text as a note.
                        self.talks[idx].lines.push(Line::note(format!(
                            "尚未执行的输入：{}",
                            pending.join("\n")
                        )));
                    }
                }
                self.save_sessions();
            }
        }
        cx.notify();
    }

    fn ensure_live(&mut self, idx: usize) {
        if self.talks[idx].lines.iter().any(Line::is_live) {
            return;
        }
        self.talks[idx].lines.push(Line::pending());
    }

    fn close_empty_live(&mut self, idx: usize) {
        if let Some(pos) = self.talks[idx].lines.iter().rposition(Line::is_live) {
            let empty = match &self.talks[idx].lines[pos].body {
                LineBody::Assistant { text, thinking, .. } => {
                    (text.is_empty() || text.as_ref() == "…") && thinking.is_empty()
                }
                _ => false,
            };
            if empty {
                self.talks[idx].lines.remove(pos);
            } else if let LineBody::Assistant { live, .. } = &mut self.talks[idx].lines[pos].body {
                *live = false;
            }
        }
    }

    fn finish_pending(&mut self, idx: usize, error: &str) {
        if let Some(pos) = self.talks[idx].lines.iter().rposition(Line::is_live) {
            if let LineBody::Assistant { text, live, .. } = &mut self.talks[idx].lines[pos].body {
                *live = false;
                if text.is_empty() || text.as_ref() == "…" {
                    *text = if error.is_empty() {
                        "没有收到回复。".into()
                    } else {
                        error.to_string().into()
                    };
                }
            }
        } else if !error.is_empty()
            && !self.talks[idx]
                .lines
                .iter()
                .rev()
                .take(4)
                .any(|line| match &line.body {
                    LineBody::Assistant { text, .. } | LineBody::Note(text) => text.contains(error),
                    _ => false,
                })
        {
            self.talks[idx].lines.push(Line::note(error.to_string()));
        }
    }

    fn insert_before_live(&mut self, idx: usize, line: Line) {
        if let Some(pos) = self.talks[idx].lines.iter().rposition(Line::is_live) {
            self.talks[idx].lines.insert(pos, line);
        } else {
            self.talks[idx].lines.push(line);
        }
    }
}

fn talk_from_session(session: Session) -> Talk {
    let lines = session
        .transcript
        .iter()
        .filter_map(|message| match message {
            AgentMessage::User { text } => Some(Line::user(text.clone())),
            AgentMessage::Assistant { text, thinking, .. } => Some(Line {
                body: LineBody::Assistant {
                    text: text.clone().into(),
                    thinking: thinking.clone().into(),
                    live: false,
                },
            }),
            AgentMessage::ToolResult {
                tool_call_id,
                name,
                text,
                is_error,
            } => Some(Line {
                body: LineBody::Tool {
                    id: tool_call_id.clone(),
                    name: name.clone().into(),
                    detail: text.clone().into(),
                    running: false,
                    error: *is_error,
                },
            }),
            AgentMessage::ContextSummary { .. } => None,
        })
        .collect();
    let preview = session
        .transcript
        .iter()
        .rev()
        .find(|m| !m.text().is_empty())
        .map(|m| m.text().chars().take(60).collect::<String>())
        .unwrap_or_default();
    Talk {
        id: session.id,
        title: session.title.into(),
        preview: preview.into(),
        when: "已保存".into(),
        lines,
        messages: session.context,
        transcript: session.transcript,
        retained_until: session.retained_until,
        usage: session.usage,
    }
}
fn title_from(text: &str) -> SharedString {
    let trimmed: String = text.chars().take(18).collect();
    if text.chars().count() > 18 {
        format!("{trimmed}…").into()
    } else {
        trimmed.into()
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = ink(self.dark);
        let body: AnyElement = if self.settings {
            v_flex()
                .size_full()
                .child(self.caption(window, cx))
                .child(div().flex_1().min_h_0().child(self.settings_page(cx)))
                .into_any_element()
        } else {
            self.chat_column(window, cx).into_any_element()
        };

        div()
            .id("hikari-root")
            .relative()
            .size_full()
            .flex()
            .bg(ink.bg)
            .text_color(ink.fg)
            .font_family("Segoe UI")
            .when(self.sidebar && !self.settings, |this| {
                this.child(self.sidebar(cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(body),
            )
            .when(self.resizing, |this| {
                this.child(
                    div()
                        .id("resize-capture")
                        .absolute()
                        .size_full()
                        .cursor(CursorStyle::ResizeColumn)
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            let next = this.resize_start
                                + (event.position.x.as_f32() - this.resize_origin);
                            if next < 232. {
                                this.sidebar = false;
                                this.resizing = false;
                            } else {
                                this.sidebar_width = next.clamp(240., 480.);
                            }
                            cx.notify();
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.resizing = false;
                                cx.notify();
                            }),
                        ),
                )
            })
    }
}

impl Preview {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = ink(self.dark);
        let query = self.search.read(cx).value().to_lowercase();
        let rows: Vec<_> = self
            .talks
            .iter()
            .enumerate()
            .filter(|(_, talk)| {
                query.is_empty()
                    || talk.title.to_lowercase().contains(&query)
                    || talk.preview.to_lowercase().contains(&query)
            })
            .map(|(idx, talk)| side_row(idx, talk, self.active == Some(idx), ink, cx))
            .collect();

        let width = self.sidebar_width;
        let resizing = self.resizing;
        div()
            .relative()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .child(
                v_flex()
                    .absolute()
                    .top(px(8.))
                    .left(px(8.))
                    .right(px(4.))
                    .bottom(px(8.))
                    .rounded_xl()
                    .border_1()
                    .border_color(ink.line)
                    .bg(ink.side)
                    .overflow_hidden()
                    .shadow_sm()
                    .child(
                        h_flex()
                            .h(px(40.))
                            .px_3()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Hikari"),
                            )
                            .child(
                                div()
                                    .id("side-drag")
                                    .flex_1()
                                    .h_full()
                                    .window_control_area(WindowControlArea::Drag),
                            )
                            .child(icon_hit(
                                "side-search",
                                IconName::Search,
                                cx.listener(|this, _, window, cx| {
                                    this.searching = !this.searching;
                                    if this.searching {
                                        window.focus(&this.search.focus_handle(cx), cx);
                                    }
                                    cx.notify();
                                }),
                            ))
                            .child(icon_hit(
                                "side-collapse",
                                IconName::PanelLeft,
                                cx.listener(|this, _, _, cx| {
                                    this.sidebar = false;
                                    cx.notify();
                                }),
                            )),
                    )
                    .when(self.searching, |this| {
                        this.child(div().px_2().pb_1().child(Input::new(&self.search).small()))
                    })
                    .child(side_action(
                        "new-chat",
                        IconName::Plus,
                        "新建会话",
                        cx.listener(|this, _, _, cx| {
                            this.active = None;
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .px_3()
                            .pt_3()
                            .pb_1()
                            .text_xs()
                            .text_color(ink.muted)
                            .child("本地"),
                    )
                    .child(
                        div()
                            .id("talk-list")
                            .flex_1()
                            .min_h_0()
                            .px_2()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .children(rows),
                    )
                    .child(
                        h_flex()
                            .h(px(44.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .border_t_1()
                            .border_color(ink.line)
                            .child(icon_hit(
                                "side-settings",
                                IconName::Settings,
                                cx.listener(|this, _, _, cx| {
                                    this.settings = true;
                                    cx.notify();
                                }),
                            ))
                            .child(icon_hit(
                                "side-theme",
                                if self.dark {
                                    IconName::Sun
                                } else {
                                    IconName::Moon
                                },
                                cx.listener(|this, _, window, cx| {
                                    this.set_dark(!this.dark, window, cx);
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .id("side-resize")
                    .absolute()
                    .top(px(16.))
                    .bottom(px(16.))
                    .right(px(-2.))
                    .w(px(12.))
                    .cursor(CursorStyle::ResizeColumn)
                    .flex()
                    .justify_end()
                    .child(div().w(px(2.)).h_full().rounded_full().bg(if resizing {
                        hsla(27.1 / 360., 1., 0.8, 0.7)
                    } else {
                        hsla(0., 0., 0., 0.)
                    }))
                    .hover(|this| this.bg(hsla(27.1 / 360., 1., 0.8, 0.35)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.resizing = true;
                            this.resize_origin = event.position.x.as_f32();
                            this.resize_start = this.sidebar_width;
                            cx.notify();
                        }),
                    ),
            )
    }

    fn chat_column(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = ink(self.dark);
        v_flex().size_full().child(self.caption(window, cx)).child(
            v_flex()
                .flex_1()
                .min_h_0()
                .child(self.stage(cx))
                .child(self.composer(ink, cx))
                .when(self.terminal_open, |this| {
                    this.child(self.terminal_panel(ink, cx))
                }),
        )
    }

    fn ensure_shell(&mut self, cx: &mut Context<Self>) {
        if self.shell.is_some() {
            return;
        }
        match shell::Shell::spawn(&workspace_dir()) {
            Ok(shell) => {
                self.shell = Some(shell);
                self.terminal_error = None;
                self.pump_terminal(cx);
            }
            Err(err) => self.terminal_error = Some(err),
        }
    }

    fn pump_terminal(&mut self, cx: &mut Context<Self>) {
        self.terminal_pump = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let keep = this.update(cx, |this, cx| {
                let Some(shell) = this.shell.as_ref() else {
                    return false;
                };
                let text = shell.screen();
                if text != this.terminal_text {
                    this.terminal_text = text;
                    cx.notify();
                }
                true
            });
            if !matches!(keep, Ok(true)) {
                break;
            }
        }));
    }

    fn terminal_panel(&self, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        let text = if let Some(err) = &self.terminal_error {
            format!("终端没有打开。\n{err}")
        } else if self.terminal_text.is_empty() {
            "正在打开命令行…".to_string()
        } else {
            self.terminal_text.clone()
        };
        v_flex()
            .h(px(240.))
            .w_full()
            .flex_shrink_0()
            .border_t_1()
            .border_color(ink.line)
            .bg(ink.bg)
            .child(
                h_flex()
                    .h(px(34.))
                    .px_2()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(ink.line)
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(ink.line)
                            .text_xs()
                            .child("本地 1"),
                    )
                    .child(icon_hit(
                        "term-hide",
                        IconName::Close,
                        cx.listener(|this, _, _, cx| {
                            this.terminal_open = false;
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .id("terminal-body")
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .overflow_y_scroll()
                    .font_family("Consolas")
                    .text_xs()
                    .track_focus(&self.terminal_focus)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            window.focus(&this.terminal_focus, cx);
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                        if let Some(shell) = this.shell.as_ref() {
                            if let Some(bytes) = shell_bytes(event) {
                                shell.write(&bytes);
                            }
                        }
                    }))
                    .child(text),
            )
    }

    fn open_menu(&self) -> impl IntoElement {
        Button::new("open-external")
            .ghost()
            .small()
            .icon(IconName::FolderOpen)
            .dropdown_menu_with_anchor(Anchor::TopRight, |menu, _, _| {
                menu.item(
                    PopupMenuItem::new("文件资源管理器（默认）")
                        .icon(IconName::Folder)
                        .on_click(|_, _, _| {
                            let _ = open_workspace(OpenTarget::Explorer);
                        }),
                )
                .item(PopupMenuItem::new("VS Code").on_click(|_, _, _| {
                    let _ = open_workspace(OpenTarget::Code);
                }))
                .item(PopupMenuItem::new("Git Bash").on_click(|_, _, _| {
                    let _ = open_workspace(OpenTarget::Bash);
                }))
            })
    }

    fn caption(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximized = window.is_maximized();
        let lead: AnyElement = if self.settings {
            h_flex()
                .id("back")
                .h(px(28.))
                .px_2()
                .gap_1()
                .rounded_md()
                .items_center()
                .cursor_pointer()
                .hover(|this| this.bg(hsla(0., 0., 0.5, 0.08)))
                .child(Icon::new(IconName::ArrowLeft).small())
                .child(div().text_sm().child("返回"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.settings = false;
                        cx.notify();
                    }),
                )
                .into_any_element()
        } else if !self.sidebar {
            icon_hit(
                "expand",
                IconName::PanelLeft,
                cx.listener(|this, _, _, cx| {
                    this.sidebar = true;
                    cx.notify();
                }),
            )
            .into_any_element()
        } else {
            div().into_any_element()
        };

        h_flex()
            .h(px(40.))
            .w_full()
            .items_center()
            .pl_2()
            .child(lead)
            .child(
                div()
                    .id("main-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(caption_tool(
                "term",
                IconName::SquareTerminal,
                self.terminal_open,
                cx.listener(|this, _, window, cx| {
                    this.terminal_open = !this.terminal_open;
                    if this.terminal_open {
                        this.ensure_shell(cx);
                        window.focus(&this.terminal_focus, cx);
                    }
                    cx.notify();
                }),
            ))
            .child(self.open_menu())
            .child(caption_hit(
                "win-min",
                IconName::WindowMinimize,
                false,
                |_, window, _| {
                    window.minimize_window();
                },
            ))
            .child(caption_hit(
                "win-max",
                if maximized {
                    IconName::WindowRestore
                } else {
                    IconName::WindowMaximize
                },
                false,
                |_, window, _| window.zoom_window(),
            ))
            .child(caption_hit(
                "win-close",
                IconName::WindowClose,
                true,
                |_, window, _| {
                    window.remove_window();
                },
            ))
    }

    fn stage(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let ink = ink(self.dark);
        let body = if let Some(idx) = self.active {
            div()
                .id("stage")
                .size_full()
                .overflow_y_scroll()
                .py_8()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(720.))
                        .mx_auto()
                        .px_6()
                        .gap_3()
                        .children(self.talks[idx].lines.iter().map(|line| message(line, ink))),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(portrait(px(112.)))
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("今天要做点什么？"),
                )
                .into_any_element()
        };
        div().flex_1().min_h_0().child(body)
    }

    fn composer(&self, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        div().w_full().px_4().pb(px(20.)).flex_shrink_0().child(
            v_flex()
                .w_full()
                .max_w(px(680.))
                .mx_auto()
                .rounded_xl()
                .border_1()
                .border_color(ink.line)
                .bg(ink.input)
                .px_2()
                .py_1()
                .gap_1()
                .child(Textarea::new(&self.draft).appearance(false).h(px(72.)))
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(icon_hit(
                                    "attach",
                                    IconName::Plus,
                                    cx.listener(|this, _, _, cx| {
                                        this.active = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(self.run_config(cx)),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(self.context_usage(ink))
                                .when(self.busy, |this| {
                                    this.child(
                                        Button::new("queue-steering")
                                            .ghost()
                                            .small()
                                            .label("插入当前任务")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.queue_input(true, window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("queue-follow-up")
                                            .ghost()
                                            .small()
                                            .label(format!(
                                                "排队 · {}",
                                                self.turn_control.pending()
                                            ))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.queue_input(false, window, cx)
                                            })),
                                    )
                                })
                                .child(
                                    div()
                                        .id("send")
                                        .size(px(28.))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(ink.fg)
                                        .text_color(ink.bg)
                                        .cursor_pointer()
                                        .child(if self.busy {
                                            Icon::from(gpui_kit::assets::IconName::SquareStop)
                                                .small()
                                        } else {
                                            Icon::new(IconName::ArrowUp).small()
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, window, cx| {
                                                if this.busy {
                                                    if let Some(abort) = &this.abort {
                                                        abort.store(true, Ordering::Relaxed);
                                                    }
                                                } else {
                                                    this.send(window, cx);
                                                }
                                            }),
                                        ),
                                ),
                        ),
                ),
        )
    }

    fn run_config(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let model_name = self.model.read(cx).value().to_string();
        let thinking = self.thinking;
        let shown = if model_name.is_empty() {
            "未设置"
        } else {
            model_name.as_str()
        };
        let label = format!("{shown} · {}", THINKING[thinking]);
        Button::new("run-config")
            .ghost()
            .small()
            .label(label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, menu_cx| {
                let mut menu = menu.item(PopupMenuItem::label("模型"));
                for name in MODELS {
                    let view = view.clone();
                    let picked = name.to_string();
                    menu = menu.item(
                        PopupMenuItem::new(name)
                            .checked(name == model_name)
                            .on_click(move |_, window, cx| {
                                let picked = picked.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.model.update(cx, |state, cx| {
                                        state.set_value(picked.clone(), window, cx);
                                    });
                                    cx.notify();
                                });
                            }),
                    );
                }
                let view = view.clone();
                menu.separator()
                    .submenu("思考", window, menu_cx, move |menu, _, _| {
                        let mut menu = menu;
                        for (index, name) in THINKING.into_iter().enumerate() {
                            let view = view.clone();
                            menu = menu.item(
                                PopupMenuItem::new(name)
                                    .checked(index == thinking)
                                    .on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.thinking = index;
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    })
            })
    }
}

const MODELS: [&str; 3] = ["gpt-4.1", "gpt-4o", "claude-sonnet"];
const THINKING: [&str; 5] = ["关闭", "低", "中", "高", "最高"];

#[derive(Clone, Copy)]
struct Ink {
    bg: Hsla,
    side: Hsla,
    fg: Hsla,
    muted: Hsla,
    line: Hsla,
    hover: Hsla,
    input: Hsla,
}

fn ink(dark: bool) -> Ink {
    if dark {
        Ink {
            bg: hsla(0., 0., 0.063, 1.),
            side: hsla(0., 0., 0.086, 1.),
            fg: hsla(0., 0., 1., 1.),
            muted: hsla(0., 0., 0.627, 1.),
            line: hsla(0., 0., 0.157, 1.),
            hover: hsla(0., 0., 0.157, 1.),
            input: hsla(0., 0., 0.11, 1.),
        }
    } else {
        Ink {
            bg: hsla(0., 0., 1., 1.),
            side: hsla(220. / 360., 0.231, 0.975, 1.),
            fg: hsla(225. / 360., 0.071, 0.11, 1.),
            muted: hsla(220. / 360., 0.089, 0.438, 1.),
            line: hsla(222.9 / 360., 0.163, 0.916, 1.),
            hover: hsla(225. / 360., 0.154, 0.949, 1.),
            input: hsla(0., 0., 1., 1.),
        }
    }
}

impl Preview {
    fn context_usage(&self, ink: Ink) -> impl IntoElement {
        let used = self.usage.total();
        let ratio = if used == 0 {
            0.0
        } else {
            (used as f32 / DEFAULT_CONTEXT_TOKENS as f32).clamp(0.0, 1.0)
        };
        let percent = (ratio * 100.0).round() as u32;
        let input = self.usage.input;
        let output = self.usage.output;
        HoverCard::new("context-usage")
            .anchor(Anchor::BottomCenter)
            .trigger(
                div()
                    .id("usage-ring")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(
                        usage_ring(ratio)
                            .w(px(16.))
                            .h(px(16.))
                            .text_color(ink.muted),
                    ),
            )
            .content(move |_, _, _| {
                v_flex()
                    .w(px(220.))
                    .gap_2()
                    .text_sm()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(format!("上下文 {percent}%"))
                            .child(
                                div()
                                    .text_color(ink.muted)
                                    .child(format!("{used} / {DEFAULT_CONTEXT_TOKENS}")),
                            ),
                    )
                    .child(usage_line("输入", input.to_string(), ink))
                    .child(usage_line("输出", output.to_string(), ink))
            })
    }
}

fn usage_line(label: &'static str, value: impl Into<SharedString>, ink: Ink) -> impl IntoElement {
    h_flex()
        .justify_between()
        .text_xs()
        .child(label)
        .child(div().text_color(ink.muted).child(value.into()))
}

fn usage_ring(used_ratio: f32) -> Icon {
    let radius = 10.0_f32;
    let length = std::f32::consts::PI * 2.0 * radius;
    let offset = length * (1.0 - used_ratio.clamp(0.0, 1.0));
    Icon::default().data(
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
            <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-opacity="0.25" stroke-width="2"/>
            <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="{length:.2} {length:.2}" stroke-dashoffset="{offset:.2}" transform="rotate(-90 12 12)"/>
            </svg>"#
        )
        .as_bytes(),
    )
}

#[derive(Clone, Copy)]
enum OpenTarget {
    Explorer,
    Code,
    Bash,
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn open_workspace(target: OpenTarget) -> std::io::Result<()> {
    let dir = workspace_dir();
    match target {
        OpenTarget::Explorer => {
            Command::new("explorer").arg(&dir).spawn()?;
        }
        OpenTarget::Code => {
            Command::new("code").arg(&dir).spawn()?;
        }
        OpenTarget::Bash => {
            let bash = [
                r"C:\Program Files\Git\git-bash.exe",
                r"C:\Program Files (x86)\Git\git-bash.exe",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.exists());
            let Some(bash) = bash else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "没有找到 Git Bash",
                ));
            };
            Command::new(bash)
                .arg(format!("--cd={}", dir.display()))
                .spawn()?;
        }
    }
    Ok(())
}

fn shell_bytes(event: &KeyDownEvent) -> Option<Vec<u8>> {
    let key = event.keystroke.key.as_ref();
    if event.keystroke.modifiers.control {
        if let Some(text) = event.keystroke.key_char.as_deref() {
            if let Some(ch) = text.chars().next() {
                let lower = ch.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    return Some(vec![lower as u8 - b'a' + 1]);
                }
            }
        }
    }
    match key {
        "enter" => Some(vec![b'\r']),
        "backspace" => Some(vec![0x08]),
        "tab" => Some(vec![b'\t']),
        "escape" => Some(vec![0x1b]),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        _ => event
            .keystroke
            .key_char
            .as_ref()
            .map(|text| text.as_bytes().to_vec()),
    }
}

fn portrait(size: Pixels) -> impl IntoElement {
    img(portrait_image())
        .w(size)
        .h(size)
        .object_fit(ObjectFit::Contain)
}

fn portrait_image() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../iconin.png").to_vec(),
            ))
        })
        .clone()
}

fn icon_hit(
    id: &'static str,
    icon: IconName,
    handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(28.))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|this| this.bg(hsla(0., 0., 0.5, 0.08)))
        .child(Icon::new(icon).small())
        .on_mouse_down(MouseButton::Left, handler)
}

fn caption_tool(
    id: &'static str,
    icon: IconName,
    active: bool,
    handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(36.))
        .h(px(28.))
        .mx_1()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(active, |this| {
            this.text_color(hsla(27.1 / 360., 1., 0.8, 1.))
        })
        .hover(|this| this.bg(hsla(0., 0., 0.5, 0.08)))
        .child(Icon::new(icon).small())
        .on_mouse_down(MouseButton::Left, handler)
}

fn caption_hit(
    id: &'static str,
    icon: IconName,
    danger: bool,
    handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(36.))
        .h(px(28.))
        .mx_1()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |this| {
            if danger {
                this.bg(hsla(0., 0.8, 0.55, 1.))
                    .text_color(hsla(0., 0., 1., 1.))
            } else {
                this.bg(hsla(0., 0., 0.5, 0.08))
            }
        })
        .child(Icon::new(icon).small())
        .on_mouse_down(MouseButton::Left, handler)
}

fn side_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .mx_2()
        .px_2()
        .h(px(32.))
        .gap_2()
        .rounded_md()
        .items_center()
        .cursor_pointer()
        .hover(|this| this.bg(hsla(0., 0., 0.5, 0.08)))
        .child(Icon::new(icon).small())
        .child(div().text_sm().child(label))
        .on_mouse_down(MouseButton::Left, handler)
}

fn side_row(
    idx: usize,
    talk: &Talk,
    selected: bool,
    ink: Ink,
    cx: &mut Context<Preview>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("talk-{idx}")))
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |this| this.bg(ink.hover))
        .hover(|this| this.bg(ink.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.active = Some(idx);
                cx.notify();
            }),
        )
        .child(
            v_flex()
                .gap(px(2.))
                .child(div().text_sm().child(talk.title.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(ink.muted)
                        .child(talk.preview.clone()),
                ),
        )
}

impl Preview {
    fn sync_provider_config(&mut self, cx: &mut Context<Self>) {
        self.config.provider.base_url = self.provider.read(cx).value().trim().to_string();
        self.config.provider.model = self.model.read(cx).value().trim().to_string();
        self.config.provider.api_key_env = self.key_env.read(cx).value().trim().to_string();
        self.config.persona_name = self.call_name.read(cx).value().to_string();
        self.config.dark = self.dark;
    }

    fn queue_input(&mut self, steering: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.active.map(|i| self.talks[i].id) != self.running_talk {
            self.management_notice = "请回到正在进行的会话，或等它完成后再发送".into();
            cx.notify();
            return;
        }
        if steering {
            self.turn_control.steer(text);
        } else {
            self.turn_control.follow_up(text);
        }
        self.draft
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }
    fn save_config(&mut self, cx: &mut Context<Self>) {
        self.sync_provider_config(cx);
        self.management_notice = match self.config_store.save(&self.config) {
            Ok(()) => "设置已保存。输入的密钥仅用于本次运行；重启后从环境变量读取。".into(),
            Err(e) => e.message,
        };
        cx.notify();
    }
    fn save_sessions(&mut self) {
        let sessions: Vec<_> = self
            .talks
            .iter()
            .map(|talk| Session {
                id: talk.id,
                title: talk.title.to_string(),
                transcript: talk.transcript.clone(),
                context: talk.messages.clone(),
                retained_until: talk.retained_until,
                usage: talk.usage,
            })
            .collect();
        if let Err(e) = self.session_store.save(&sessions) {
            self.management_notice = e.message;
        }
    }
    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.skills = SkillCatalog::discover(
            &workspace_dir(),
            &self.config.skill_roots,
            &self.config.disabled_skills,
        );
        self.management_notice = if self.skills.diagnostics.is_empty() {
            format!("发现 {} 个 Skill", self.skills.skills.len())
        } else {
            self.skills.diagnostics.join("\n")
        };
        cx.notify();
    }
    fn add_skill_root(&mut self, cx: &mut Context<Self>) {
        let value = self.skill_path.read(cx).value().trim().to_string();
        if value.is_empty() {
            self.management_notice = "请填写 Skill 目录".into();
            cx.notify();
            return;
        }
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            workspace_dir().join(path)
        };
        match path.canonicalize() {
            Ok(path) => {
                if !self.config.skill_roots.contains(&path) {
                    self.config.skill_roots.push(path);
                }
                self.save_config(cx);
                self.refresh_skills(cx);
            }
            Err(e) => {
                self.management_notice = format!("目录无法打开：{e}");
                cx.notify();
            }
        }
    }
    fn toggle_skill(&mut self, path: PathBuf, enabled: bool, cx: &mut Context<Self>) {
        self.config.disabled_skills.retain(|p| p != &path);
        if !enabled {
            self.config.disabled_skills.push(path);
        }
        self.save_config(cx);
        self.refresh_skills(cx);
    }
    fn sync_mcp_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = serde_json::to_string_pretty(&self.config.mcp_servers).unwrap_or_default();
        self.mcp_config
            .update(cx, |state, cx| state.set_value(value, window, cx));
    }
    fn reconnect_mcp(&mut self, cx: &mut Context<Self>) {
        let mcp = self.mcp.clone();
        let configs = self.config.mcp_servers.clone();
        let tx = self.runtime_tx.clone();
        self.start_pump(cx);
        self.management_notice = "正在连接 MCP 服务".into();
        thread::spawn(move || {
            mcp.reconcile(&configs, &workspace_dir());
            let _ = tx.send(RuntimeUpdate {
                statuses: mcp.statuses(),
            });
        });
        cx.notify();
    }
    fn apply_mcp_config(&mut self, cx: &mut Context<Self>) {
        let parsed =
            serde_json::from_str::<Vec<McpServerConfig>>(&self.mcp_config.read(cx).value());
        let result = parsed.map_err(|e| e.to_string()).and_then(|servers| {
            let mut names = std::collections::HashSet::new();
            for server in &servers {
                server.validate().map_err(|e| e.message)?;
                if !names.insert(server.name.clone()) {
                    return Err("MCP 名称不能重复".into());
                }
            }
            Ok(servers)
        });
        match result {
            Ok(servers) => {
                self.config.mcp_servers = servers;
                self.save_config(cx);
                self.reconnect_mcp(cx);
            }
            Err(error) => {
                self.management_notice = error;
                cx.notify();
            }
        }
    }
    fn add_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.mcp_name.read(cx).value().trim().to_string();
        let target = self.mcp_target.read(cx).value().trim().to_string();
        let transport = if self.mcp_http {
            McpTransport::StreamableHttp {
                url: target,
                bearer_token_env: None,
            }
        } else {
            let args = match serde_json::from_str::<Vec<String>>(&self.mcp_args.read(cx).value()) {
                Ok(args) => args,
                Err(e) => {
                    self.management_notice = format!("参数需要字符串数组：{e}");
                    cx.notify();
                    return;
                }
            };
            McpTransport::Stdio {
                command: target,
                args,
                env: Default::default(),
            }
        };
        let config = McpServerConfig {
            name,
            enabled: true,
            transport,
            timeout_seconds: 30,
        };
        if let Err(e) = config.validate() {
            self.management_notice = e.message;
            cx.notify();
            return;
        }
        if self
            .config
            .mcp_servers
            .iter()
            .any(|s| s.name == config.name)
        {
            self.management_notice = "这个 MCP 名称已存在".into();
            cx.notify();
            return;
        }
        self.config.mcp_servers.push(config);
        self.sync_mcp_editor(window, cx);
        self.save_config(cx);
        self.reconnect_mcp(cx);
    }

    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .min_h_0()
            .child(self.settings_nav(cx))
            .child(self.settings_body(cx))
    }

    fn settings_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let current = self.section;
        v_flex()
            .w(px(220.))
            .h_full()
            .px_4()
            .py_6()
            .gap_1()
            .flex_shrink_0()
            .children(Section::all().into_iter().map(|section| {
                let selected = section == current;
                div()
                    .id(SharedString::from(format!("section-{}", section.label())))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(theme.secondary).font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |this| this.text_color(theme.muted_foreground))
                    .hover(|this| this.bg(theme.muted))
                    .child(section.label())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.section = section;
                            cx.notify();
                        }),
                    )
            }))
    }

    fn settings_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let body = match self.section {
            Section::Provider => div().child(self.provider_page(cx)),
            Section::Appearance => div().child(self.appearance_page(cx)),
            Section::Memory => div().child(self.memory_page(cx)),
            Section::Persona => div().child(self.persona_page(cx)),
            Section::Skill => div().child(self.skill_page(cx)),
            Section::Mcp => div().child(self.mcp_page(cx)),
            Section::Channels => div().child(note(
                "外部通讯",
                "Telegram、Discord 以后可以接进来。今天先只在这里说话。",
                theme.clone(),
            )),
            Section::About => div().child(v_flex().gap_4().child(portrait(px(96.))).child(note(
                "关于",
                "Hikari 是一个待在身边的助手。能说话，也能看这台电脑上的文件。密钥留在本机。",
                theme.clone(),
            ))),
        };

        div()
            .id("settings-body")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .px(px(48.))
            .py(px(36.))
            .child(
                div()
                    .max_w(px(560.))
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(body)
                    .child(
                        Button::new("save-settings")
                            .label("保存设置")
                            .on_click(cx.listener(|this, _, _, cx| this.save_config(cx))),
                    )
                    .when(!self.management_notice.is_empty(), |this| {
                        this.child(div().text_xs().child(self.management_notice.clone()))
                    }),
            )
    }

    fn provider_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_5()
            .child(page_title("提供商", "连上一个模型就够了。", theme.clone()))
            .child(
                v_flex().gap_2().children(
                    [
                        ApiProtocol::ChatCompletions,
                        ApiProtocol::Responses,
                        ApiProtocol::Anthropic,
                    ]
                    .into_iter()
                    .map(|protocol| {
                        Button::new(SharedString::from(format!("protocol-{protocol:?}")))
                            .label(protocol.label())
                            .when(self.config.provider.protocol == protocol, |this| {
                                this.primary()
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.config.provider.protocol = protocol;
                                let (base, model, key_env) = if protocol == ApiProtocol::Anthropic {
                                    (
                                        "https://api.anthropic.com/v1",
                                        "claude-sonnet-4-6",
                                        "ANTHROPIC_API_KEY",
                                    )
                                } else {
                                    ("https://api.openai.com/v1", "gpt-4.1", "OPENAI_API_KEY")
                                };
                                this.provider
                                    .update(cx, |state, cx| state.set_value(base, window, cx));
                                this.model
                                    .update(cx, |state, cx| state.set_value(model, window, cx));
                                this.key_env
                                    .update(cx, |state, cx| state.set_value(key_env, window, cx));
                                cx.notify();
                            }))
                    }),
                ),
            )
            .child(field("地址", Input::new(&self.provider), theme.clone()))
            .child(field("模型", Input::new(&self.model), theme.clone()))
            .child(field(
                "密钥（本次运行）",
                Input::new(&self.key),
                theme.clone(),
            ))
            .child(field(
                "密钥环境变量（重启后使用）",
                Input::new(&self.key_env),
                theme,
            ))
    }

    fn appearance_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_4()
            .child(page_title(
                "界面",
                "亮一点或暗一点，都只是换一层光。",
                theme,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("light")
                            .when(!self.dark, |this| this.primary())
                            .when(self.dark, |this| this.ghost())
                            .label("浅色")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_dark(false, window, cx);
                            })),
                    )
                    .child(
                        Button::new("dark")
                            .when(self.dark, |this| this.primary())
                            .when(!self.dark, |this| this.ghost())
                            .label("深色")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_dark(true, window, cx);
                            })),
                    ),
            )
    }

    fn memory_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_4()
            .child(page_title(
                "记忆",
                "记得住的事会自己收好。用不到的，不会拿来打扰你。",
                theme,
            ))
            .child(
                Switch::new("memory")
                    .label("让 Hikari 记住")
                    .checked(self.memory_on)
                    .on_change(cx.listener(|this, value, _, cx| {
                        this.memory_on = *value;
                        cx.notify();
                    })),
            )
    }

    fn persona_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_4()
            .child(page_title(
                "人格",
                "它怎么称呼自己，说话轻不轻。",
                theme.clone(),
            ))
            .child(field("名字", Input::new(&self.call_name), theme))
            .child(
                div()
                    .text_sm()
                    .child("先陪着，再办事。不催，不列清单，除非你要。"),
            )
    }

    fn skill_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_4()
            .child(page_title(
                "Skill",
                "添加本机的 Skill，让 Hikari 在需要时读取它的指令。",
                theme.clone(),
            ))
            .child(Input::new(&self.skill_path))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("add-skill-root")
                            .label("添加目录")
                            .on_click(cx.listener(|this, _, _, cx| this.add_skill_root(cx))),
                    )
                    .child(
                        Button::new("refresh-skills")
                            .ghost()
                            .label("重新扫描")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_skills(cx))),
                    ),
            )
            .children(
                self.config
                    .skill_roots
                    .iter()
                    .enumerate()
                    .map(|(index, path)| {
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().child(path.display().to_string()))
                            .child(
                                Button::new(SharedString::from(format!(
                                    "remove-skill-root-{index}"
                                )))
                                .ghost()
                                .small()
                                .label("移除目录")
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.config.skill_roots.remove(index);
                                        this.save_config(cx);
                                        this.refresh_skills(cx);
                                    },
                                )),
                            )
                    }),
            )
            .children(self.skills.skills.iter().enumerate().map(|(index, skill)| {
                let path = skill.path.clone();
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        Switch::new(SharedString::from(format!("skill-{index}")))
                            .label(skill.name.clone())
                            .checked(skill.enabled)
                            .on_change(cx.listener(move |this, value, _, cx| {
                                this.toggle_skill(path.clone(), *value, cx)
                            })),
                    )
                    .child(div().text_sm().child(skill.description.clone()))
                    .child(div().text_xs().child(skill.path.display().to_string()))
            }))
            .when(self.skills.skills.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .child("还没有 Skill。目录里需要包含 SKILL.md。"),
                )
            })
    }

    fn mcp_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        v_flex()
            .gap_4()
            .child(page_title(
                "MCP",
                "连接本机或远程工具服务。连接成功后，Hikari 就可以调用它们。",
                theme.clone(),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("mcp-stdio")
                            .label("本机 stdio")
                            .when(!self.mcp_http, |this| this.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_http = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("mcp-http")
                            .label("Streamable HTTP")
                            .when(self.mcp_http, |this| this.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_http = true;
                                cx.notify();
                            })),
                    ),
            )
            .child(field("服务名称", Input::new(&self.mcp_name), theme.clone()))
            .child(field(
                if self.mcp_http {
                    "服务 URL"
                } else {
                    "启动命令"
                },
                Input::new(&self.mcp_target),
                theme.clone(),
            ))
            .when(!self.mcp_http, |this| {
                this.child(field(
                    "启动参数（字符串数组）",
                    Input::new(&self.mcp_args),
                    theme.clone(),
                ))
            })
            .child(
                Button::new("add-mcp")
                    .label("添加并连接")
                    .on_click(cx.listener(|this, _, window, cx| this.add_mcp(window, cx))),
            )
            .children(
                self.config
                    .mcp_servers
                    .iter()
                    .enumerate()
                    .map(|(index, server)| {
                        let name = server.name.clone();
                        let retry_name = name.clone();
                        let disconnect_name = name.clone();
                        let status = self.mcp_statuses.iter().find(|s| s.name == name);
                        let label = match status.map(|s| s.state.as_str()) {
                            Some("connected") => "已连接",
                            Some("failed") => "连接失败",
                            Some("connecting") => "连接中",
                            _ if !server.enabled => "已停用",
                            _ => "未连接",
                        };
                        let detail = status.and_then(|s| s.error.clone()).unwrap_or_else(|| {
                            status
                                .map(|s| {
                                    format!("{} 个工具\n{}", s.tools.len(), s.tools.join(" · "))
                                })
                                .unwrap_or_default()
                        });
                        v_flex()
                            .gap_2()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                Switch::new(SharedString::from(format!("mcp-enabled-{index}")))
                                    .label(format!("{name} · {label}"))
                                    .checked(server.enabled)
                                    .on_change(cx.listener(move |this, value, window, cx| {
                                        this.config.mcp_servers[index].enabled = *value;
                                        this.sync_mcp_editor(window, cx);
                                        this.save_config(cx);
                                        this.reconnect_mcp(cx);
                                    })),
                            )
                            .child(div().text_xs().child(detail))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "mcp-retry-{index}"
                                        )))
                                        .ghost()
                                        .small()
                                        .label("重连")
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let Some(config) = this
                                                    .config
                                                    .mcp_servers
                                                    .iter()
                                                    .find(|s| s.name == retry_name)
                                                    .cloned()
                                                else {
                                                    return;
                                                };
                                                let mcp = this.mcp.clone();
                                                let tx = this.runtime_tx.clone();
                                                this.start_pump(cx);
                                                thread::spawn(move || {
                                                    let _ = mcp.connect(&config, &workspace_dir());
                                                    let _ = tx.send(RuntimeUpdate {
                                                        statuses: mcp.statuses(),
                                                    });
                                                });
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "mcp-disconnect-{index}"
                                        )))
                                        .ghost()
                                        .small()
                                        .label("断开")
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.config.mcp_servers[index].enabled = false;
                                                this.sync_mcp_editor(window, cx);
                                                this.save_config(cx);
                                                let mcp = this.mcp.clone();
                                                let tx = this.runtime_tx.clone();
                                                let name = disconnect_name.clone();
                                                this.start_pump(cx);
                                                thread::spawn(move || {
                                                    mcp.disconnect(&name);
                                                    let _ = tx.send(RuntimeUpdate {
                                                        statuses: mcp.statuses(),
                                                    });
                                                });
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "mcp-remove-{index}"
                                        )))
                                        .ghost()
                                        .small()
                                        .label("移除")
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.config.mcp_servers.remove(index);
                                                this.sync_mcp_editor(window, cx);
                                                this.save_config(cx);
                                                this.reconnect_mcp(cx);
                                            }),
                                        ),
                                    ),
                            )
                    }),
            )
            .child(div().text_sm().child("高级配置：修改环境变量、认证或超时"))
            .child(Textarea::new(&self.mcp_config).h(px(220.)))
            .child(
                Button::new("apply-mcp-json")
                    .label("应用配置并连接")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_mcp_config(cx))),
            )
    }
}

fn message(line: &Line, ink: Ink) -> AnyElement {
    match &line.body {
        LineBody::User(text) => Message::new()
            .alignment(MessageAlignment::End)
            .content(
                MessageContent::new().bubble(
                    Bubble::new()
                        .with_variant(BubbleVariant::Secondary)
                        .child(text.clone()),
                ),
            )
            .into_any_element(),
        LineBody::Assistant { text, thinking, .. } => v_flex()
            .gap_1()
            .when(!thinking.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(ink.muted)
                        .child(thinking.clone()),
                )
            })
            .child(
                Message::new()
                    .alignment(MessageAlignment::Start)
                    .content(MessageContent::new().bubble(Bubble::new().child(text.clone()))),
            )
            .into_any_element(),
        LineBody::Tool {
            name,
            detail,
            running,
            error,
            ..
        } => v_flex()
            .w_full()
            .max_w(px(560.))
            .px_3()
            .py_2()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(ink.line)
            .bg(ink.input)
            .child(
                div()
                    .text_xs()
                    .text_color(if *error {
                        hsla(0., 0.7, 0.65, 1.)
                    } else {
                        ink.muted
                    })
                    .child(if *running {
                        format!("{name} · 正在做")
                    } else {
                        name.to_string()
                    }),
            )
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .child(detail.clone()),
            )
            .into_any_element(),
        LineBody::Note(text) => div()
            .text_xs()
            .text_color(ink.muted)
            .child(text.clone())
            .into_any_element(),
    }
}

fn talk_row(
    idx: usize,
    talk: &Talk,
    selected: bool,
    cx: &mut Context<Preview>,
) -> impl IntoElement {
    let theme = cx.theme().colors.clone();
    div()
        .id(SharedString::from(format!("talk-{idx}")))
        .px_3()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .when(selected, |this| this.bg(theme.secondary))
        .hover(|this| this.bg(theme.muted))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.active = Some(idx);

                cx.notify();
            }),
        )
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .child(div().text_sm().child(talk.title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(talk.when.clone()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(talk.preview.clone()),
                ),
        )
}

fn page_title(title: &'static str, hint: &'static str, theme: ThemeColor) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_xl().font_weight(FontWeight::MEDIUM).child(title))
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint),
        )
}

fn field(label: &'static str, input: impl IntoElement, theme: ThemeColor) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(input)
}

fn note(title: &'static str, hint: &'static str, theme: ThemeColor) -> impl IntoElement {
    page_title(title, hint, theme)
}

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let bounds = Bounds::centered(None, size(px(1080.), px(720.)), cx);
        open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..TitleBar::window_options()
            },
            cx,
            |window, cx| cx.new(|cx| Preview::new(window, cx)),
        )
        .expect("打开预览窗口失败");
    });
}

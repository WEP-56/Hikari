// Windows 下按 GUI 子系统构建，避免启动时由系统额外分配一个控制台窗口。
// 从终端（cargo run / cmd / PowerShell）启动时标准输出仍由该终端接收。
#![cfg_attr(windows, windows_subsystem = "windows")]

use gpui_kit::base::GlobalState as BaseGlobalState;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    Theme, ThemeColor, ThemeMode, TitleBar,
};
use gpui_kit::prelude::{FluentBuilder as _, StatefulInteractiveElement as _};
use gpui_kit::*;
use hikari_agent::{
    run_turn_with_control, AgentEvent, Message as AgentMessage, NullMemory, ProtocolProvider,
    ToolRegistry, TurnControl, TurnHost, TurnRequest, Usage,
};
use hikari_runtime::{
    AppConfig, ConfigStore, McpManager, McpServerConfig, McpStatus, McpTransport, MemoryCore,
    MemoryModels, PersonaStore, Session, SessionStore, SkillCatalog,
};
use settings_ui::{settings_card, settings_heading, settings_label, settings_row};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

mod attachments_ui;
mod chat;
mod execution_ui;
mod memory_ui;
mod settings;
mod settings_ui;
mod shell;
mod workspace;

struct SessionTurn {
    gen: u64,
    abort: Arc<AtomicBool>,
    control: Arc<TurnControl>,
}

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
        args: SharedString,
        detail: SharedString,
        running: bool,
        error: bool,
    },
    Note(SharedString),
}

struct Line {
    attachments: Vec<hikari_agent::Attachment>,
    body: LineBody,
}

impl Line {
    fn user(text: impl Into<SharedString>) -> Self {
        Self {
            attachments: Vec::new(),
            body: LineBody::User(text.into()),
        }
    }

    #[cfg(test)]
    fn assistant(text: impl Into<SharedString>) -> Self {
        Self {
            attachments: Vec::new(),
            body: LineBody::Assistant {
                text: text.into(),
                thinking: SharedString::new(""),
                live: false,
            },
        }
    }

    fn pending() -> Self {
        Self {
            attachments: Vec::new(),
            body: LineBody::Assistant {
                text: "".into(),
                thinking: SharedString::new(""),
                live: true,
            },
        }
    }

    fn note(text: impl Into<SharedString>) -> Self {
        Self {
            attachments: Vec::new(),
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
    workspace: Option<PathBuf>,
    pinned: bool,
    preview: SharedString,
    when: SharedString,
    lines: Vec<Line>,
    messages: Vec<AgentMessage>,
    retained_until: usize,
    context_base: usize,
    memory_enabled: bool,
    transcript: Vec<AgentMessage>,
    usage: Usage,
    view: chat::ChatState,
    execution: Arc<hikari_agent::execution::Execution>,
    execution_revision: u64,
    turn: Option<SessionTurn>,
    mcp: Arc<McpManager>,
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
    OtherProviders,
    Appearance,
    Memory,
    Persona,
    Skill,
    Mcp,
    Channels,
    About,
}

impl Section {
    fn all() -> [Section; 9] {
        [
            Section::Provider,
            Section::OtherProviders,
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
            Section::OtherProviders => "其它提供商",
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
    new_workspace: Option<PathBuf>,
    workspace_store: hikari_runtime::workspace::WorkspaceStore,
    recent_workspaces: Vec<PathBuf>,
    workspace_notice: String,
    notice_generation: u64,
    workspace_picker_generation: u64,
    sidebar: bool,
    sidebar_width: f32,
    titlebar_drag_pending: bool,
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
    settings_open: std::collections::HashSet<String>,
    dark: bool,
    follow_system: bool,
    new_memory: bool,
    memory_core: Option<Arc<MemoryCore>>,
    memory_models: Arc<MemoryModels>,
    memory_events: Option<Receiver<hikari_agent::MemoryJobEvent>>,
    memory_status: String,
    memory_path: Entity<InputState>,
    memory_tokens: Entity<InputState>,
    memory_items: Entity<InputState>,
    memory_query: Entity<InputState>,
    memory_units: Vec<hikari_runtime::MemoryUnit>,
    memory_offset: usize,
    memory_source: String,
    memory_source_id: Option<String>,
    memory_tab: usize,
    memory_facts: bool,
    persona_store: PersonaStore,
    persona_editor: Entity<TextareaState>,
    persona_preview: bool,
    terminal_open: bool,
    terminal_text: String,
    terminal_error: Option<String>,
    shell: Option<shell::Shell>,
    terminal_focus: FocusHandle,
    terminal_scroll: ScrollHandle,
    terminal_pump: Option<Task<()>>,
    draft: Entity<TextareaState>,
    clarification: Entity<InputState>,
    new_permission: hikari_agent::permission::PermissionMode,
    conversation_drafts: std::collections::HashMap<Option<u64>, String>,
    attachment_drafts: std::collections::HashMap<Option<u64>, Vec<hikari_agent::Attachment>>,
    search: Entity<InputState>,
    provider: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
    key_env: Entity<InputState>,
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
    settings_draft: settings::SettingsDraft,
    saved_config: AppConfig,
    settings_dirty: bool,
    settings_tx: Sender<settings::SettingsResult>,
    settings_rx: Receiver<settings::SettingsResult>,
    model_fetching: bool,
    update_checking: bool,
    update_notice: String,
    release_url: Option<String>,
    external_mcp: hikari_runtime::imports::McpScan,
    mcp_selected: Vec<bool>,
    import_overwrite: bool,
    external_skills: SkillCatalog,
    skill_selected: Vec<bool>,
    provider_focus: Option<String>,
    model_focus: Option<usize>,
    advanced_open: bool,
    picker: settings::PickerView,
    effort_drag: Option<usize>,
    model_query: Entity<InputState>,
}

impl Preview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_in(workspace_dir().join(".hikari"), window, cx)
    }

    fn new_in(data_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config_store = ConfigStore::new(&data_dir);
        let session_store = SessionStore::new(&data_dir);
        let workspace_store = hikari_runtime::workspace::WorkspaceStore::new(&data_dir);
        let recent_workspaces = workspace_store.load().unwrap_or_default();
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
        if let Err(e) = std::fs::create_dir_all(data_dir.join("skills")) {
            management_notice = format!("无法创建 Hikari Skills 目录：{e}");
        }
        let settings_draft = settings::SettingsDraft::new(&config, window, cx);
        let persona_store = PersonaStore::new(&data_dir);
        let card = persona_store
            .initialize(&config.persona_name)
            .unwrap_or_else(|e| {
                management_notice = e.message;
                String::new()
            });
        let persona_editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(card, window, cx);
            state
        });
        let memory_path =
            cx.new(|cx| InputState::new(window, cx).default_value(config.memory.database.clone()));
        let memory_tokens = cx.new(|cx| {
            InputState::new(window, cx).default_value(config.memory.auto_tokens.to_string())
        });
        let memory_items = cx.new(|cx| {
            InputState::new(window, cx).default_value(config.memory.auto_items.to_string())
        });
        let memory_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索观察或事实，留空查看全部"));
        let memory_models = MemoryModels::new(&config);
        let db_path = data_dir.join(&config.memory.database);
        let (memory_core, memory_events, memory_status) =
            match MemoryCore::open(&db_path, config.memory.enabled) {
                Ok((core, events)) => {
                    core.set_processor(memory_models.clone());
                    (Some(core), Some(events), "记忆库已初始化".to_string())
                }
                Err(err) => (None, None, format!("记忆初始化失败：{}", err.message)),
            };
        let saved_config = config.clone();
        let (settings_tx, settings_rx) = mpsc::channel();
        let draft = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .placeholder("想说的话，放在这里")
                .submit_on_enter(true);
            state.set_auto_grow(1, 6, cx);
            state
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("找一段过去的对话"));
        let clarification = cx.new(|cx| InputState::new(window, cx).placeholder("输入你的回答"));
        let model_query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型"));
        let provider = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(config.provider.base_url.clone())
                .placeholder("https://api.openai.com/v1")
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(config.provider.model.clone())
                .placeholder("例如 gpt-4.1")
        });
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空时使用环境变量")
                .default_value(
                    config
                        .providers
                        .iter()
                        .find(|p| p.id == config.active_provider)
                        .map(|p| p.api_key.clone())
                        .unwrap_or_default(),
                )
                .masked(true)
        });
        let key_env = cx.new(|cx| {
            InputState::new(window, cx).default_value(config.provider.api_key_env.clone())
        });
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
            if matches!(event, InputEvent::Change) {
                this.dismiss_composer_notice(cx);
            }
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.send(window, cx);
            }
        });
        let model_query_sub = cx.subscribe_in(&model_query, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });

        let (event_tx, event_rx) = mpsc::channel();
        // Unsent image drafts are intentionally transient; persisted messages
        // have already copied their resources into the owning session store.
        if let Err(error) = session_store.remove_draft_artifacts() {
            management_notice = error.message;
        }
        let mut sessions = session_store.load().unwrap_or_else(|e| {
            management_notice = e.message;
            Vec::new()
        });
        if let Some(core) = &memory_core {
            let errors = hikari_runtime::memory::recover_memory(
                core,
                &memory_models,
                &config,
                &mut sessions,
            );
            if !errors.is_empty() {
                management_notice = errors.join("\n");
            }
            if let Err(e) = session_store.save(&sessions) {
                management_notice = e.message;
            }
        }
        let next_talk_id = sessions.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        let talks = sessions.into_iter().map(talk_from_session).collect();
        let (runtime_tx, runtime_rx) = mpsc::channel();
        let mcp = Arc::new(McpManager::default());
        let follow_system = config.follow_system;
        let dark = if follow_system {
            ThemeMode::from(cx.window_appearance()).is_dark()
        } else {
            config.dark
        };
        cx.set_window_appearance(if follow_system {
            None
        } else {
            Some(if dark {
                WindowAppearance::Dark
            } else {
                WindowAppearance::Light
            })
        });
        Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);

        let mut this = Self {
            talks,
            active: None,
            new_workspace: None,
            workspace_store,
            recent_workspaces,
            workspace_notice: String::new(),
            notice_generation: 0,
            workspace_picker_generation: 0,
            sidebar: true,
            sidebar_width: 292.,
            titlebar_drag_pending: false,
            resizing: false,
            resize_origin: 0.,
            resize_start: 292.,
            thinking: settings::effort_index(config.provider.reasoning_effort.as_deref()),
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
            settings_open: Default::default(),
            dark,
            follow_system,
            new_memory: true,
            memory_core,
            memory_models,
            memory_events,
            memory_status,
            memory_path,
            memory_tokens,
            memory_items,
            memory_query,
            memory_units: Vec::new(),
            memory_offset: 0,
            memory_source: String::new(),
            memory_source_id: None,
            memory_tab: 0,
            memory_facts: false,
            persona_store,
            persona_editor,
            persona_preview: false,
            terminal_open: false,
            terminal_text: String::new(),
            terminal_error: None,
            shell: None,
            terminal_focus: cx.focus_handle(),
            terminal_scroll: ScrollHandle::new(),
            terminal_pump: None,
            draft,
            clarification,
            new_permission: Default::default(),
            conversation_drafts: Default::default(),
            attachment_drafts: Default::default(),
            search,
            provider,
            model,
            key,
            key_env,
            _subscriptions: vec![search_sub, draft_sub, model_query_sub],
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
            settings_draft,
            saved_config,
            settings_dirty: false,
            settings_tx,
            settings_rx,
            model_fetching: false,
            update_checking: false,
            update_notice: String::new(),
            release_url: None,
            external_mcp: Default::default(),
            mcp_selected: Vec::new(),
            import_overwrite: false,
            external_skills: Default::default(),
            skill_selected: Vec::new(),
            provider_focus: None,
            model_focus: None,
            advanced_open: false,
            picker: settings::PickerView::Closed,
            effort_drag: None,
            model_query,
        };
        this.apply_typography(cx);
        this.observe_settings(window, cx);
        let view = cx.entity().downgrade();
        this._subscriptions.push(window.observe_window_appearance(move |window, cx| {
            let _ = view.update(cx, |this, cx| {
                if this.follow_system {
                    this.dark = ThemeMode::from(window.appearance()).is_dark();
                    this.apply_theme(window, cx);
                    cx.notify();
                }
            });
        }));
        this
    }

    fn set_dark(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let was_dirty = self.settings_dirty;
        self.dark = dark;
        self.follow_system = false;
        self.apply_theme(window, cx);
        if self.persist_theme() {
            self.settings_dirty = was_dirty;
        } else {
            self.settings_dirty = true;
        }
        cx.notify();
    }

    fn set_follow_system(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_dirty = self.settings_dirty;
        self.follow_system = true;
        cx.set_window_appearance(None);
        self.dark = ThemeMode::from(cx.window_appearance()).is_dark();
        self.apply_theme(window, cx);
        if self.persist_theme() {
            self.settings_dirty = was_dirty;
        } else {
            self.settings_dirty = true;
        }
        cx.notify();
    }

    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.follow_system {
            cx.set_window_appearance(None);
        } else {
            cx.set_window_appearance(Some(if self.dark {
                WindowAppearance::Dark
            } else {
                WindowAppearance::Light
            }));
        }
        Theme::change(if self.dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
        self.apply_typography(cx);
    }

    fn persist_theme(&mut self) -> bool {
        let mut next = self.saved_config.clone();
        next.dark = self.dark;
        next.follow_system = self.follow_system;
        match self.config_store.save(&next) {
            Ok(()) => {
                self.saved_config.dark = next.dark;
                self.saved_config.follow_system = next.follow_system;
                self.config.dark = next.dark;
                self.config.follow_system = next.follow_system;
                true
            }
            Err(error) => {
                self.management_notice = format!("主题未保存：{}", error.message);
                false
            }
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_busy() {
            self.queue_input(false, window, cx);
            return;
        }
        let text = self.draft.read(cx).value().trim().to_string();
        if text.is_empty() && self.current_attachments().is_empty() {
            return;
        }
        let text = if text.is_empty() {
            "请查看图片。".to_string()
        } else {
            text
        };
        let attachments = self.current_attachments().to_vec();
        let attachment_key = self.active.map(|idx| self.talks[idx].id);
        let cwd = self.agent_directory();
        if let Err(error) = self.prepare_agent_directory() {
            self.workspace_notice = error;
            cx.notify();
            return;
        }
        let idx = match self.active {
            Some(idx) => idx,
            None => {
                self.conversation_drafts.remove(&None);
                let id = self.next_talk_id;
                self.next_talk_id += 1;
                self.talks.insert(
                    0,
                    Talk {
                        id,
                        title: title_from(&text),
                        workspace: self.new_workspace.clone(),
                        pinned: false,
                        preview: text.clone().into(),
                        when: "现在".into(),
                        lines: Vec::new(),
                        messages: Vec::new(),
                        retained_until: 0,
                        context_base: 0,
                        memory_enabled: self.new_memory,
                        transcript: Vec::new(),
                        usage: Usage::default(),
                        view: chat::ChatState::default(),
                        execution: hikari_agent::execution::Execution::new(
                            format!("talk-{id}"),
                            hikari_agent::execution::ExecutionSnapshot {
                                mode: self.new_permission,
                                ..Default::default()
                            },
                        ),
                        execution_revision: 0,
                        turn: None,
                        mcp: Arc::new(McpManager::default()),
                    },
                );
                self.active = Some(0);
                0
            }
        };
        let talk_id = self.talks[idx].id;
        self.conversation_drafts.remove(&Some(talk_id));
        self.talks[idx].preview = text.clone().into();
        if self.talks[idx].lines.is_empty() {
            self.talks[idx].title = title_from(&text);
        }
        let attachments = match self.save_input_attachments(talk_id, &attachments) {
            Ok(attachments) => attachments,
            Err(error) => {
                self.workspace_notice = error;
                cx.notify();
                return;
            }
        };
        self.attachment_drafts.remove(&attachment_key);
        let mut user_line = Line::user(&text);
        user_line.attachments = attachments.clone();
        self.talks[idx].lines.push(user_line);
        let start = self.talks[idx].lines.len() - 1;
        self.talks[idx].view.start(start);
        self.talks[idx].lines.push(Line::pending());
        if !self.talks[idx].memory_enabled || !self.saved_config.memory.enabled {
            self.talks[idx].retained_until = self.talks[idx].messages.len();
        }
        let mut user_message = AgentMessage::user(&text);
        user_message.set_attachments(attachments);
        user_message.stamp(
            format!(
                "talk-{talk_id}:{}",
                self.talks[idx].context_base + self.talks[idx].messages.len()
            ),
            false,
        );
        self.talks[idx].messages.push(user_message.clone());
        self.talks[idx].transcript.push(user_message);
        if !self.talks[idx].memory_enabled || !self.saved_config.memory.enabled {
            self.talks[idx].retained_until = self.talks[idx].messages.len();
        }
        self.save_sessions();
        self.draft.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });

        let provider_config = self.saved_config.provider.clone();
        let key = self.saved_provider_key(cx);
        let key = if key.trim().is_empty() {
            std::env::var(&provider_config.api_key_env).unwrap_or_default()
        } else {
            key
        };
        let persona_prompt = match self.persona_store.prompt() {
            Ok(prompt) => prompt,
            Err(err) => {
                self.finish_pending(idx, &err.message);
                cx.notify();
                return;
            }
        };
        let messages = self.talks[idx].messages.clone();
        let retained_until = self.talks[idx].retained_until;
        let context_base = self.talks[idx].context_base;
        let memory_scope = if self.talks[idx].memory_enabled && self.saved_config.memory.enabled {
            match &self.memory_core {
                Some(core) => match hikari_runtime::memory::turn_memory(
                    core,
                    &self.memory_models,
                    &self.saved_config,
                    format!("talk-{talk_id}"),
                    self.talks[idx]
                        .workspace
                        .as_ref()
                        .map(|p| p.display().to_string()),
                ) {
                    Ok(scope) => Some(scope),
                    Err(err) => {
                        self.finish_pending(idx, &err.message);
                        cx.notify();
                        return;
                    }
                },
                None => {
                    self.finish_pending(idx, &self.memory_status.clone());
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };

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
        self.workspace_picker_generation += 1;
        self.running_talk = Some(talk_id);
        self.turn_control = Arc::new(TurnControl::default());
        let turn_control = self.turn_control.clone();
        self.talks[idx].turn = Some(SessionTurn {
            gen,
            abort: abort.clone(),
            control: turn_control.clone(),
        });
        let execution = self.talks[idx].execution.clone();
        if let Err(error) = execution.begin(cwd.clone(), provider_config.model.clone()) {
            self.talks[idx].turn = None;
            self.finish_pending(idx, &error);
            self.busy = self.talks.iter().any(|t| t.turn.is_some());
            cx.notify();
            return;
        }
        self.save_sessions();
        self.start_pump(cx);
        let tx = self.event_tx.clone();
        // Skill roots follow the settings page (application root), not the
        // default chat scratch directory under .hikari/chat.
        let skill_roots = skill_search_roots(
            &self.saved_config.skill_roots,
            self.talks[idx].workspace.as_deref(),
        );
        let skills = SkillCatalog::discover(
            Path::new("."),
            &skill_roots,
            &self.saved_config.disabled_skills,
        );
        // Connections are bound to this turn's workspace, never retargeted by another session.
        let mcp = self.talks[idx].mcp.clone();
        let mcp_configs = self.saved_config.mcp_servers.clone();
        let runtime_tx = self.runtime_tx.clone();
        let max_tool_rounds = self.saved_config.max_tool_rounds;
        let web_config = self.saved_config.web.clone();
        let image_config = self.saved_config.image_gen.clone();
        let history_store = self.session_store.clone();
        let history_core = self.memory_core.clone();
        let artifact_root = self
            .session_store
            .path()
            .parent()
            .unwrap()
            .join("artifacts")
            .join(format!("talk-{talk_id}"));
        let directory_prompt = format!(
            "\n当前会话的工作目录：{}。相对文件路径与命令都以此目录为基准。",
            cwd.display()
        );
        thread::spawn(move || {
            mcp.reconcile(&mcp_configs, &cwd);
            let _ = runtime_tx.send(RuntimeUpdate {
                statuses: mcp.statuses(),
            });
            let attachments = hikari_agent::AttachmentStore::new(artifact_root.clone());
            let mut tools = ToolRegistry::workspace(cwd.clone());
            let setup_error = tools
                .enable_artifacts(artifact_root)
                .and_then(|_| tools.enable_execution(execution.clone()))
                .and_then(|_| hikari_runtime::web::register(web_config, &mut tools))
                .and_then(|_| {
                    hikari_runtime::image_gen::register(
                        image_config,
                        attachments.clone(),
                        &mut tools,
                    )
                })
                .and_then(|_| {
                    hikari_runtime::history::register(
                        history_store,
                        history_core,
                        talk_id,
                        &mut tools,
                    )
                })
                .and_then(|_| skills.register(&mut tools))
                .and_then(|_| skills.register_management(&mut tools, &workspace_dir()))
                .and_then(|_| mcp.register_with_attachments(attachments.clone(), &mut tools))
                .and_then(|_| {
                    memory_scope
                        .as_ref()
                        .map(|scope| scope.register(&mut tools).map_err(|e| e.message))
                        .unwrap_or(Ok(()))
                })
                .err();
            let provider: Arc<dyn hikari_agent::Provider> = Arc::new(provider);
            let system_prompt = format!(
                "{}{}{}{}",
                persona_prompt,
                skills.prompt(),
                directory_prompt,
                mcp.discovery_prompt()
            );
            let setup_error = setup_error.or_else(|| {
                tools
                    .enable_delegation(
                        provider.clone(),
                        system_prompt.clone(),
                        provider_config.context_tokens,
                        Some(attachments),
                    )
                    .err()
            });
            let host = TurnHost {
                provider,
                tools: Arc::new(tools),
                memory: memory_scope
                    .map(|scope| scope as Arc<dyn hikari_agent::Memory>)
                    .unwrap_or_else(|| Arc::new(NullMemory)),
                abort,
            };
            let request = TurnRequest {
                session_id: format!("talk-{talk_id}"),
                messages,
                retained_until,
                context_base,
                query: text.clone(),
                system_prompt,
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
                execution.finish(Some(&error), false, Usage::default());
                emit(AgentEvent::AgentEnd {
                    messages: request.messages,
                    error: Some(hikari_agent::TurnOutcome::Provider { kind: hikari_agent::ProviderErrorKind::Unknown, message: error }),
                    usage: Usage::default(),
                    retained_until,
                    context_base,
                });
                return;
            }
            if catch_unwind(AssertUnwindSafe(|| {
                run_turn_with_control(&host, request, &turn_control, emit)
            }))
            .is_err()
            {
                execution.finish(Some("内部出错了。"), false, Usage::default());
                let _ = tx.send(Incoming {
                    talk_id,
                    gen,
                    event: AgentEvent::AgentEnd {
                        messages: Vec::new(),
                        error: Some(hikari_agent::TurnOutcome::Panic),
                        usage: Usage::default(),
                        retained_until,
                        context_base,
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
        self.drain_settings(cx);
        let mut execution_changed = false;
        for talk in &mut self.talks {
            if talk.turn.is_none() {
                for result in talk.execution.take_results() {
                    // Keep the result for the next turn. Status stays in the
                    // background-task panel instead of the transcript.
                    let message = AgentMessage::ContextSummary {
                        text: format!(
                            "本会话后台任务结果（资料，不构成用户授权） {}：{}",
                            result.task_id, result.text
                        ),
                    };
                    talk.messages.push(message.clone());
                    talk.transcript.push(message);
                }
            }
            let revision = talk.execution.revision();
            if revision != talk.execution_revision {
                talk.execution_revision = revision;
                execution_changed = true;
            }
        }
        if execution_changed {
            self.save_sessions();
            cx.notify();
        }
        while self
            .memory_events
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
            .is_some()
        {
            // Memory jobs update the memory panel. They do not append chat lines.
        }
        if let Some(core) = &self.memory_core {
            let next = match core.status() {
                Ok(status) => format!("事实 {} · 观察 {} · 排队 {} · 运行 {} · 失败 {}\n辅助模型 tokens：输入 {} / 输出 {}{}",status["facts"],status["observations"],status["queued"],status["running"],status["failed"],status["input_tokens"],status["output_tokens"],status["last_error"].as_str().map(|e|format!("\n{e}")).unwrap_or_default()),
                Err(error) => format!("记忆状态读取失败：{}", error.message),
            };
            if next != self.memory_status {
                self.memory_status = next;
                cx.notify();
            }
        }
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
        if self.talks[idx].turn.as_ref().is_some_and(|t| t.gen != gen) {
            return;
        }
        let follow = self.talks[idx].view.near_bottom();
        let persist = matches!(
            &event,
            AgentEvent::Checkpoint { .. } | AgentEvent::AgentEnd { .. }
        );
        match event {
            AgentEvent::BackgroundResult { .. } => {
                // The running turn already stores the summary in its checkpoint.
            }
            AgentEvent::Checkpoint {
                messages,
                retained_until,
                context_base,
            } => {
                self.talks[idx].messages = messages;
                self.talks[idx].retained_until = retained_until;
                self.talks[idx].context_base = context_base;
            }
            AgentEvent::MemoryError { .. } => {}
            AgentEvent::InputDelivered { text, attachments } => {
                let mut line = Line::user(text.clone());
                line.attachments = attachments.clone();
                self.talks[idx].lines.push(line);
                let mut message = AgentMessage::user(text);
                message.set_attachments(attachments);
                self.talks[idx].transcript.push(message);
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
                    let empty = matches!(&message, AgentMessage::Assistant { text, thinking, .. }
                        if text.is_empty() && thinking.is_empty());
                    if empty {
                        self.talks[idx].lines.remove(pos);
                    } else if let LineBody::Assistant {
                        live,
                        text,
                        thinking,
                    } = &mut self.talks[idx].lines[pos].body
                    {
                        *live = false;
                        if let AgentMessage::Assistant {
                            text: final_text,
                            thinking: final_thinking,
                            ..
                        } = &message
                        {
                            *text = final_text.clone().into();
                            *thinking = final_thinking.clone().into();
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
                    attachments: Vec::new(),
                    body: LineBody::Tool {
                        id: tool_call_id,
                        name: tool_name.into(),
                        args: args.into(),
                        detail: "".into(),
                        running: true,
                        error: false,
                    },
                });
            }
            AgentEvent::ToolExecutionEnd {
                output,
                tool_call_id,
                tool_name,
                text,
                is_error,
            } => {
                self.talks[idx].transcript.push(AgentMessage::with_output(
                    &tool_call_id,
                    &tool_name,
                    output.clone(),
                ));
                if let Some(line) = self.talks[idx].lines.iter_mut().rev().find(
                    |line| matches!(&line.body, LineBody::Tool { id, .. } if id == &tool_call_id),
                ) {
                    line.attachments = output.attachments;
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
            AgentEvent::ToolProgress { tool_call_id, text } => {
                if let Some(line) = self.talks[idx].lines.iter_mut().rev().find(
                    |line| matches!(&line.body, LineBody::Tool { id, .. } if id == &tool_call_id),
                ) {
                    if let LineBody::Tool { detail, .. } = &mut line.body {
                        *detail = text.into();
                    }
                }
            }
            AgentEvent::MemoryRecall { .. }
            | AgentEvent::MemoryRetain { .. }
            | AgentEvent::MemoryConsolidation { .. } => {}
            AgentEvent::AutoCompactionStart { .. } => {
                self.insert_before_live(idx, Line::note("在把更早的对话收起来"));
            }
            AgentEvent::AutoCompactionEnd { error } => {
                if let Some(error) = error {
                    self.insert_before_live(idx, Line::note(error.user_text()));
                }
            }
            AgentEvent::TurnEnd { .. } => {}
            AgentEvent::AgentEnd {
                messages,
                error,
                usage,
                retained_until,
                context_base,
            } => {
                if !messages.is_empty() {
                    self.talks[idx].messages = messages;
                }
                if self.active == Some(idx) {
                    self.usage = usage;
                }
                self.talks[idx].usage = usage;
                self.talks[idx].retained_until = retained_until;
                self.talks[idx].context_base = context_base;
                let error_text = error.as_ref().map_or_else(String::new, hikari_agent::TurnOutcome::user_text);
                self.finish_pending(idx, &error_text);
                if self.talks[idx].turn.as_ref().is_some_and(|t| t.gen == gen)
                    || gen == self.turn_gen
                {
                    let control = self.talks[idx]
                        .turn
                        .take()
                        .map(|t| t.control)
                        .unwrap_or_else(|| self.turn_control.clone());
                    self.busy = self.talks.iter().any(|t| t.turn.is_some());
                    self.running_talk = self.talks.iter().find(|t| t.turn.is_some()).map(|t| t.id);
                    self.abort = self
                        .talks
                        .iter()
                        .find_map(|t| t.turn.as_ref().map(|t| t.abort.clone()));
                    let pending = control.drain_pending_messages();
                    if !pending.is_empty() {
                        self.management_notice =
                            format!("{} 条排队输入未执行，已保留在会话旁注", pending.len());
                        // Preserve unsent images in this conversation's composer.
                        let drafts = self
                            .attachment_drafts
                            .entry(Some(self.talks[idx].id))
                            .or_default();
                        for message in &pending {
                            for attachment in message.attachments() {
                                if !drafts.iter().any(|a| a.id == attachment.id) {
                                    drafts.push(attachment.clone());
                                }
                            }
                        }
                        // No window is available in the event pump; preserve the text as a note.
                        self.talks[idx].lines.push(Line::note(format!(
                            "尚未执行的输入：{}",
                            pending
                                .iter()
                                .map(|m| m.text())
                                .collect::<Vec<_>>()
                                .join("\n")
                        )));
                    }
                }
                for line in &mut self.talks[idx].lines {
                    if let LineBody::Tool {
                        running,
                        error: failed,
                        detail,
                        ..
                    } = &mut line.body
                    {
                        if *running {
                            *running = false;
                            *failed = true;
                            if detail.is_empty() {
                                *detail = error.as_ref().map_or_else(
                                    || "任务结束，未收到工具结果。".to_string(),
                                    hikari_agent::TurnOutcome::user_text,
                                ).into();
                            }
                        }
                    }
                }
            }
        }
        if persist {
            let talk = &mut self.talks[idx];
            hikari_runtime::session::sync_transcript_origins(&mut talk.transcript, &talk.messages);
            self.save_sessions();
        }
        if follow {
            self.talks[idx].view.scroll.scroll_to_bottom();
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
        self.talks[idx].view.finish();
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
    let arguments: std::collections::HashMap<_, _> = session
        .transcript
        .iter()
        .flat_map(|message| match message {
            AgentMessage::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .map(|call| (call.id.as_str(), call.arguments.as_str()))
        .collect();
    let lines = session
        .transcript
        .iter()
        .filter_map(|message| match message {
            AgentMessage::User {
                text, attachments, ..
            } => {
                let mut line = Line::user(text.clone());
                line.attachments = attachments.clone();
                Some(line)
            }
            AgentMessage::Assistant { text, thinking, .. }
                if !text.is_empty() || !thinking.is_empty() =>
            {
                Some(Line {
                    attachments: Vec::new(),
                    body: LineBody::Assistant {
                        text: text.clone().into(),
                        thinking: thinking.clone().into(),
                        live: false,
                    },
                })
            }
            AgentMessage::ToolResult {
                tool_call_id,
                name,
                text,
                is_error,
                ..
            } => Some(Line {
                attachments: message.attachments().to_vec(),
                body: LineBody::Tool {
                    id: tool_call_id.clone(),
                    name: name.clone().into(),
                    args: arguments
                        .get(tool_call_id.as_str())
                        .copied()
                        .unwrap_or_default()
                        .to_string()
                        .into(),
                    detail: text.clone().into(),
                    running: false,
                    error: *is_error,
                },
            }),
            AgentMessage::ContextSummary { text } if !quiet_status(text) => {
                Some(Line::note(text.clone()))
            }
            AgentMessage::ContextSummary { .. } => None,
            AgentMessage::ToolCatalog { .. } | AgentMessage::Assistant { .. } => None,
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
        workspace: session.workspace,
        pinned: session.pinned,
        preview: preview.into(),
        when: "已保存".into(),
        lines,
        messages: session.context,
        transcript: session.transcript,
        retained_until: session.retained_until,
        context_base: session.context_base,
        memory_enabled: session.memory_enabled,
        usage: session.usage,
        view: chat::ChatState::default(),
        execution: hikari_agent::execution::Execution::new(
            format!("talk-{}", session.id),
            session.execution,
        ),
        execution_revision: 0,
        turn: None,
        mcp: Arc::new(McpManager::default()),
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
            .font_family(self.config.appearance.text_font.clone())
            .text_size(px(self.config.appearance.ui_size))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, _| {
                terminal_debug(format!(
                    "root_key key={:?} char={:?} term_focused={} terminal_open={}",
                    event.keystroke.key,
                    event.keystroke.key_char,
                    this.terminal_focus.is_focused(window),
                    this.terminal_open,
                ));
            }))
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
        let mut indices: Vec<_> = self
            .talks
            .iter()
            .enumerate()
            .filter(|(_, talk)| {
                query.is_empty()
                    || talk.title.to_lowercase().contains(&query)
                    || talk.preview.to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect();
        indices.sort_by_key(|&i| !self.talks[i].pinned);
        let rows: Vec<_> = indices
            .into_iter()
            .map(|idx| {
                side_row(
                    &self.talks[idx],
                    self.active == Some(idx),
                    self.talks[idx].turn.is_some(),
                    ink,
                    cx,
                )
            })
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
                            .id("side-head")
                            .h(px(40.))
                            .px_3()
                            .items_center()
                            .gap_1()
                            // 与顶栏一致：这里按下拖动整窗时不启动文字选择。
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                BaseGlobalState::suppress_text_selection(cx);
                            })
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
                        cx.listener(|this, _, window, cx| this.new_conversation(window, cx)),
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
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
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
        v_flex()
            .size_full()
            .min_w_0()
            .child(self.caption(window, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.stage(cx))
                            .child(
                                div()
                                    .id("process-menu-layer")
                                    .absolute()
                                    .top(px(8.))
                                    .right(px(24.))
                                    .occlude()
                                    .child(self.process_menu(cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .relative()
                            .w_full()
                            .flex_shrink_0()
                            .child(self.execution_panel(ink, cx))
                            .child(self.composer(ink, cx))
                            .child(self.todo_overlay(ink, cx)),
                    )
                    .when(self.terminal_open, |this| {
                        this.child(self.terminal_panel(ink, cx))
                    }),
            )
    }

    fn ensure_shell(&mut self, cx: &mut Context<Self>) {
        if self.shell.is_some() {
            return;
        }
        if !self.workspace_available() {
            return;
        }
        match shell::Shell::spawn(&self.agent_directory()) {
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
                let io_error = shell.io_error();
                if io_error != this.terminal_error {
                    this.terminal_error = io_error;
                    cx.notify();
                }
                if text != this.terminal_text {
                    terminal_debug(format!(
                        "pump text_len={} tail={:?}",
                        text.len(),
                        text.chars().rev().take(80).collect::<String>().chars().rev().collect::<String>()
                    ));
                    this.terminal_text = text;
                    this.terminal_scroll.scroll_to_bottom();
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
            format!("{}\n\n终端错误：{err}", self.terminal_text)
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
                            .child(
                                self.selected_workspace()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default(),
                            ),
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
                    .track_scroll(&self.terminal_scroll)
                    .font_family(self.config.appearance.terminal_font.clone())
                    .text_size(px(self.config.appearance.terminal_size))
                    .track_focus(&self.terminal_focus)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            window.focus(&this.terminal_focus, cx);
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        terminal_debug(format!(
                            "key_down key={:?} char={:?} mods={:?} focused={} pending={}",
                            event.keystroke.key,
                            event.keystroke.key_char,
                            event.keystroke.modifiers,
                            this.terminal_focus.is_focused(window),
                            window.has_pending_keystrokes(),
                        ));
                        if let Some(shell) = this.shell.as_ref() {
                            if let Some(bytes) = shell_bytes(event) {
                                if let Err(err) = shell.write(&bytes) {
                                    this.terminal_error = Some(err);
                                    cx.notify();
                                }
                            } else {
                                terminal_debug("key_down unmapped: no bytes sent to PTY");
                            }
                        }
                        cx.stop_propagation();
                    }))
                    .child(text),
            )
    }

    fn open_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let disabled = !self.workspace_available();
        Button::new("open-external")
            .ghost()
            .small()
            .icon(IconName::FolderOpen)
            .disabled(disabled)
            .tooltip(if disabled {
                "选择工作区后可外部打开"
            } else {
                "外部打开工作区"
            })
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let explorer = view.clone();
                let code = view.clone();
                let bash = view.clone();
                menu.item(
                    PopupMenuItem::new("文件资源管理器")
                        .icon(IconName::Folder)
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let _ = explorer.update(cx, |this, cx| {
                                this.open_external(OpenTarget::Explorer, cx)
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new("VS Code")
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let _ = code
                                .update(cx, |this, cx| this.open_external(OpenTarget::Code, cx));
                        }),
                )
                .item(PopupMenuItem::new("Git Bash").disabled(disabled).on_click(
                    move |_, _, cx| {
                        let _ =
                            bash.update(cx, |this, cx| this.open_external(OpenTarget::Bash, cx));
                    },
                ))
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
                        this.stash_visible_key(cx);
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
            .id("caption")
            .h(px(40.))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .pl_2()
            // 顶栏按下不参与窗口级文字选择：原生拖动期间收不到 mouse up，
            // 选择层会残留选中状态并跟随后续鼠标移动。
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                BaseGlobalState::suppress_text_selection(cx);
            })
            .child(lead)
            .when(self.selected_workspace().is_some(), |this| {
                this.child(
                    div()
                        .max_w(px(240.))
                        .truncate()
                        .text_xs()
                        .text_color(ink(self.dark).muted)
                        .child(self.selected_workspace().unwrap().display().to_string()),
                )
            })
            .child(
                div()
                    .id("main-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| {
                            this.titlebar_drag_pending = true;
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.titlebar_drag_pending = false;
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.titlebar_drag_pending = false;
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, _, window, _| {
                        if this.titlebar_drag_pending {
                            this.titlebar_drag_pending = false;
                            window.start_window_move();
                        }
                    })),
            )
            .child(
                Button::new("term")
                    .ghost()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .selected(self.terminal_open)
                    .disabled(!self.workspace_available())
                    .tooltip(if self.workspace_available() {
                        "打开工作区终端"
                    } else {
                        "选择工作区后可打开终端"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx))),
            )
            .child(self.open_menu(cx))
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

    fn stage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = ink(self.dark);
        let body = if let Some(idx) = self.active {
            div()
                .id(SharedString::from(format!("stage-{}", self.talks[idx].id)))
                .size_full()
                .relative()
                .overflow_y_scroll()
                .track_scroll(&self.talks[idx].view.scroll)
                .vertical_scrollbar(&self.talks[idx].view.scroll)
                .py_8()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(800.))
                        .mx_auto()
                        .px_6()
                        .gap_8()
                        .font_family(self.config.appearance.text_font.clone())
                        .text_size(px(self.config.appearance.text_size))
                        .line_height(relative(self.config.appearance.line_height))
                        .children(self.conversation(
                            idx,
                            ink,
                            cx,
                        )),
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
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(self.workspace_selector(cx))
                        .child(self.workspace_browse(cx)),
                )
                .child(self.session_memory_switch(cx))
                .into_any_element()
        };
        let stage = div().relative().flex_1().min_h_0().child(body);
        #[cfg(test)]
        let stage = stage.child(
            canvas(
                |_, _, _| (),
                |bounds, _, _, _| {
                    crate::chat::tests::record_bounds("chat-stage".into(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        stage
    }

    fn composer(&self, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .px_4()
            .pb(px(20.))
            .flex_shrink_0()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(752.))
                    .mx_auto()
                    .rounded_xl()
                    .border_1()
                    .border_color(ink.line)
                    .bg(ink.input)
                    .px_2()
                    .py_1()
                    .gap_1()
                    // TextareaState controls the height: it grows from one row to
                    // six rows, then keeps the composer size stable and scrolls
                    // the editor internally.
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        this.attach_paths(&paths.0, cx)
                    }))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::Paste, _, cx| {
                            if this.paste_images(cx) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .when(!self.current_attachments().is_empty(), |this| {
                        this.child(self.attachment_draft_view(cx))
                    })
                    .child(Textarea::new(&self.draft).appearance(false))
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
                                        cx.listener(|this, _, window, cx| {
                                            let _ = window;
                                            this.choose_images(cx)
                                        }),
                                    ))
                                    .child(self.run_config(cx)),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_1()
                                    .child(self.context_usage(ink))
                                    .when(self.active_busy(), |this| {
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
                                                    self.active_control()
                                                        .map_or(0, |c| c.pending())
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
                                            .child(if self.active_busy() {
                                                Icon::from(gpui_kit::assets::IconName::SquareStop)
                                                    .small()
                                            } else {
                                                Icon::new(IconName::ArrowUp).small()
                                            })
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    if this.active_busy() {
                                                        this.cancel_active();
                                                    } else {
                                                        this.send(window, cx);
                                                    }
                                                }),
                                            ),
                                    ),
                            ),
                    ),
            )
            .when(!self.workspace_notice.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(ink.muted)
                        .pt_1()
                        .child(self.workspace_notice.clone()),
                )
            })
    }

    fn run_config(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .child(self.model_menu(cx))
            .child(self.permission_switch(cx))
            .when(self.active.is_some(), |row| {
                row.child(self.session_memory_switch(cx))
            })
    }
}

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
        let context_tokens = self.saved_config.provider.context_tokens;
        let used = self.usage.total();
        let ratio = if used == 0 {
            0.0
        } else {
            (used as f32 / self.saved_config.provider.context_tokens as f32).clamp(0.0, 1.0)
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
                                    .child(format!("{used} / {context_tokens}")),
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

/// Background results, sub-agent callbacks, and memory bookkeeping stay out of
/// the transcript. The task panel and memory page already show them.
fn quiet_status(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("后台任务")
        || text.contains("后台任务结果")
        || text.starts_with("记下了")
        || text.starts_with("想起了")
        || text.starts_with("记忆任务")
        || text.starts_with("没记下")
        || text.starts_with("沉淀没完成")
        || text.starts_with("检索失败")
}

/// Relative skill roots resolve from the application root, matching Settings.
/// An explicitly selected project workspace is searched as well. The default
/// chat directory is not a workspace and must not nest another `.hikari/skills`.
fn skill_search_roots(configured: &[PathBuf], workspace: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in configured {
        if root.is_absolute() {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
            continue;
        }
        let app = workspace_dir().join(root);
        if !roots.contains(&app) {
            roots.push(app);
        }
        if let Some(workspace) = workspace {
            let project = workspace.join(root);
            if !roots.contains(&project) {
                roots.push(project);
            }
        }
    }
    roots
}

fn skill_root_label(path: &Path) -> String {
    if path.is_absolute() {
        path.display().to_string()
    } else {
        workspace_dir().join(path).display().to_string()
    }
}

/// 应用根目录：源码构建是仓库根目录，安装包运行时可执行文件所在目录。
/// `CARGO_MANIFEST_DIR` 是编译期路径，安装到用户机器后并不存在，因此用它判断是否为源码构建。
fn workspace_dir() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(PathBuf::from));
        if let Some(root) =
            manifest_root.filter(|root| root.join("crates/hikari/Cargo.toml").is_file())
        {
            return root;
        }
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    })
    .clone()
}

fn open_workspace(target: OpenTarget, dir: &std::path::Path) -> std::io::Result<()> {
    match target {
        OpenTarget::Explorer => {
            Command::new("explorer").arg(&dir).spawn()?;
        }
        OpenTarget::Code => {
            let code = [
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .map(|p| p.join("Programs/Microsoft VS Code/Code.exe")),
                std::env::var_os("ProgramFiles")
                    .map(PathBuf::from)
                    .map(|p| p.join("Microsoft VS Code/Code.exe")),
            ]
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
            .or_else(|| {
                std::env::var_os("PATH").and_then(|paths| {
                    std::env::split_paths(&paths)
                        .flat_map(|p| [p.join("Code.exe"), p.join("../Code.exe")])
                        .find(|p| p.is_file())
                })
            })
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "没有找到 VS Code"))?;
            Command::new(code).arg(dir).spawn()?;
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

/// 临时诊断：只有设置了 HIKARI_TERMINAL_DEBUG 才写日志。
pub(crate) fn terminal_debug(message: impl AsRef<str>) {
    if std::env::var_os("HIKARI_TERMINAL_DEBUG").is_none() {
        return;
    }
    use std::io::Write as _;
    let path = std::env::temp_dir().join("hikari-terminal.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", message.as_ref());
    }
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
                include_bytes!("../resources/branding/portrait.png").to_vec(),
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
    talk: &Talk,
    selected: bool,
    running: bool,
    ink: Ink,
    cx: &mut Context<Preview>,
) -> impl IntoElement {
    let id = talk.id;
    let pinned = talk.pinned;
    let view = cx.entity().downgrade();
    div()
        .id(SharedString::from(format!("talk-{id}")))
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |this| this.bg(ink.hover))
        .hover(|this| this.bg(ink.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| this.select_conversation(id, window, cx)),
        )
        .child(
            v_flex()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap_1()
                        .min_w_0()
                        .when(pinned, |this| {
                            this.child(div().text_xs().text_color(ink.muted).child("↑"))
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(talk.title.clone()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(ink.muted)
                        .truncate()
                        .child(talk.preview.clone()),
                ),
        )
        .context_menu(move |menu, _, _| {
            let rename = view.clone();
            let pin = view.clone();
            let delete = view.clone();
            menu.item(PopupMenuItem::new("重命名").on_click(move |_, window, cx| {
                let _ = rename.update(cx, |this, cx| this.rename_dialog(id, window, cx));
            }))
            .item(
                PopupMenuItem::new(if pinned { "取消置顶" } else { "置顶" }).on_click(
                    move |_, _, cx| {
                        let _ = pin.update(cx, |this, cx| this.toggle_pin(id, cx));
                    },
                ),
            )
            .separator()
            .item(
                PopupMenuItem::new(if running {
                    "删除（会话运行中）"
                } else {
                    "删除会话"
                })
                .disabled(running)
                .on_click(move |_, window, cx| {
                    let _ = delete.update(cx, |this, cx| this.delete_conversation(id, window, cx));
                }),
            )
        })
}

impl Preview {
    fn queue_input(&mut self, steering: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.read(cx).value().trim().to_string();
        if text.is_empty() && self.current_attachments().is_empty() {
            return;
        }
        let Some(control) = self.active_control() else {
            self.workspace_notice = "请回到正在进行的会话，或等它完成后再发送".into();
            cx.notify();
            return;
        };
        let key = self.active.map(|i| self.talks[i].id);
        let Some(id) = key else {
            return;
        };
        let attachments = match self.save_input_attachments(id, self.current_attachments()) {
            Ok(attachments) => attachments,
            Err(error) => {
                self.workspace_notice = error;
                cx.notify();
                return;
            }
        };
        let mut message = AgentMessage::user(if text.is_empty() {
            "请查看图片。".into()
        } else {
            text
        });
        message.set_attachments(attachments);
        if steering {
            control.steer_message(message);
        } else {
            control.follow_up_message(message);
        }
        self.attachment_drafts.remove(&key);
        self.draft
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.conversation_drafts
            .remove(&self.active.map(|i| self.talks[i].id));
        cx.notify();
    }
    fn save_config(&mut self, cx: &mut Context<Self>) {
        self.stash_visible_key(cx);
        match self.read_settings(cx).and_then(|config| {
            self.config_store.save(&config)?;
            Ok(config)
        }) {
            Ok(config) => {
                if self.saved_config.memory.enabled != config.memory.enabled {
                    for talk in &mut self.talks {
                        talk.retained_until = talk.messages.len();
                    }
                    self.save_sessions();
                }
                self.config = config.clone();
                self.saved_config = config;
                self.memory_models.update(&self.saved_config);
                if let Some(core) = &self.memory_core {
                    core.set_enabled(self.saved_config.memory.enabled);
                }
                self.settings_dirty = false;
                self.apply_typography(cx);
                self.management_notice = "设置已保存，下一轮请求使用已保存配置。".into();
            }
            Err(e) => self.management_notice = format!("未保存：{}", e.message),
        }
        cx.notify();
    }
    fn save_sessions(&mut self) {
        let sessions: Vec<_> = self
            .talks
            .iter()
            .map(|talk| Session {
                id: talk.id,
                title: talk.title.to_string(),
                workspace: talk.workspace.clone(),
                pinned: talk.pinned,
                transcript: talk.transcript.clone(),
                context: talk.messages.clone(),
                retained_until: talk.retained_until,
                context_base: talk.context_base,
                memory_enabled: talk.memory_enabled,
                usage: talk.usage,
                execution: talk.execution.snapshot(),
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
                if self.persist_resources(cx) {
                    self.refresh_skills(cx);
                }
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
        if self.persist_resources(cx) {
            self.refresh_skills(cx);
        }
    }
    fn sync_mcp_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = serde_json::to_string_pretty(&self.config.mcp_servers).unwrap_or_default();
        self.mcp_config
            .update(cx, |state, cx| state.set_value(value, window, cx));
    }
    fn reconnect_mcp(&mut self, cx: &mut Context<Self>) {
        let mcp = self.mcp.clone();
        let configs = self.saved_config.mcp_servers.clone();
        let tx = self.runtime_tx.clone();
        let cwd = self.mcp_directory();
        if let Err(e) = std::fs::create_dir_all(&cwd) {
            self.management_notice = format!("无法访问 MCP 工作目录：{e}");
            cx.notify();
            return;
        }
        self.start_pump(cx);
        self.management_notice = "正在连接 MCP 服务".into();
        thread::spawn(move || {
            mcp.reconcile(&configs, &cwd);
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
                if self.persist_resources(cx) {
                    self.reconnect_mcp(cx);
                }
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
                headers: Default::default(),
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
        if self.persist_resources(cx) {
            self.reconnect_mcp(cx);
        }
        self.sync_mcp_editor(window, cx);
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
                            this.management_notice.clear();
                            if section == Section::Memory {
                                this.refresh_memory(cx);
                            }
                            cx.notify();
                        }),
                    )
            }))
    }

    fn settings_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let body = match self.section {
            Section::Provider => div().child(self.provider_page(cx)),
            Section::OtherProviders => div().child(self.other_providers_page(cx)),
            Section::Appearance => div().child(self.appearance_page(cx)),
            Section::Memory => div().child(self.memory_page(cx)),
            Section::Persona => div().child(self.persona_page(cx)),
            Section::Skill => div().child(self.skill_page(cx)),
            Section::Mcp => div().child(self.mcp_page(cx)),
            Section::Channels => div().child(
                v_flex()
                    .gap_4()
                    .child(settings_heading("外部通讯"))
                    .child(settings_card(settings_label("暂未开放", cx), cx)),
            ),
            Section::About => div().child(self.about_page(cx)),
        };

        let configurable = matches!(
            self.section,
            Section::Provider | Section::OtherProviders | Section::Appearance
        ) || (self.section == Section::Memory && self.memory_tab == 1);
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(32.))
                    .py(px(28.))
                    .child(div().w_full().max_w(px(760.)).child(body)),
            )
            .when(
                configurable || self.settings_dirty || !self.management_notice.is_empty(),
                |page| {
                    page.child(
                        v_flex()
                            .gap_2()
                            .px(px(32.))
                            .py_3()
                            .border_t_1()
                            .border_color(theme.border)
                            .when(configurable || self.settings_dirty, |footer| {
                                footer.child(
                                    h_flex()
                                        .max_w(px(760.))
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(settings_label(
                                            if self.settings_dirty {
                                                "未保存"
                                            } else {
                                                "已保存"
                                            },
                                            cx,
                                        ))
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    Button::new("discard-settings")
                                                        .ghost()
                                                        .label("恢复")
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.discard_settings(window, cx)
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new("save-settings")
                                                        .primary()
                                                        .label("保存设置")
                                                        .disabled(!self.settings_dirty)
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.save_config(cx)
                                                        })),
                                                ),
                                        ),
                                )
                            })
                            .when(!self.management_notice.is_empty(), |footer| {
                                footer.child(
                                    div()
                                        .id("settings-notice")
                                        .max_h(px(80.))
                                        .overflow_y_scroll()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(self.management_notice.clone()),
                                )
                            }),
                    )
                },
            )
    }

    fn provider_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.provider_settings(cx)
    }

    fn appearance_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let themes = h_flex()
            .gap_2()
            .child(
                Button::new("system")
                    .label("跟随设备")
                    .when(self.follow_system, |b| b.primary())
                    .when(!self.follow_system, |b| b.ghost())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_follow_system(window, cx)
                    })),
            )
            .child(
                Button::new("light")
                    .label("浅色")
                    .when(!self.follow_system && !self.dark, |b| b.primary())
                    .when(self.follow_system || self.dark, |b| b.ghost())
                    .on_click(cx.listener(|this, _, window, cx| this.set_dark(false, window, cx))),
            )
            .child(
                Button::new("dark")
                    .label("深色")
                    .when(!self.follow_system && self.dark, |b| b.primary())
                    .when(self.follow_system || !self.dark, |b| b.ghost())
                    .on_click(cx.listener(|this, _, window, cx| this.set_dark(true, window, cx))),
            );
        v_flex()
            .gap_5()
            .child(settings_heading("界面"))
            .child(settings_card(settings_row("主题", themes, cx), cx))
            .child(self.typography_page(cx))
    }

    fn skill_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let directories =
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&self.skill_path)))
                        .child(
                            Button::new("add-skill-root")
                                .label("添加")
                                .on_click(cx.listener(|this, _, _, cx| this.add_skill_root(cx))),
                        ),
                )
                .children(
                    self.config
                        .skill_roots
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            h_flex()
                                .gap_3()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .truncate()
                                        .child(skill_root_label(path)),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "remove-skill-root-{index}"
                                    )))
                                    .ghost()
                                    .small()
                                    .label(if index == 0 { "内置" } else { "移除" })
                                    .disabled(index == 0)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if index == 0 {
                                            return;
                                        }
                                        this.config.skill_roots.remove(index);
                                        if this.persist_resources(cx) {
                                            this.refresh_skills(cx);
                                        }
                                    })),
                                )
                        }),
                );
        let mut page = v_flex()
            .gap_4()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(settings_heading("Skills"))
                    .child(
                        Button::new("refresh-skills")
                            .ghost()
                            .small()
                            .label("刷新")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_skills(cx))),
                    ),
            )
            .child(settings_label(
                format!("{} 个 Skill", self.skills.skills.len()),
                cx,
            ));
        for (index, skill) in self.skills.skills.iter().enumerate() {
            let path = skill.path.clone();
            let details = v_flex()
                .gap_2()
                .child(div().text_sm().child(skill.description.clone()))
                .child(settings_label(skill.path.display().to_string(), cx));
            page = page.child(settings_card(
                v_flex()
                    .gap_3()
                    .child(
                        Switch::new(SharedString::from(format!("skill-{index}")))
                            .label(skill.name.clone())
                            .checked(skill.enabled)
                            .on_change(cx.listener(move |this, value, _, cx| {
                                this.toggle_skill(path.clone(), *value, cx)
                            })),
                    )
                    .child(self.settings_fold(
                        format!("skill-detail-{index}"),
                        "详情",
                        details,
                        cx,
                    )),
                cx,
            ));
        }
        page.when(self.skills.skills.is_empty(), |page| {
            page.child(settings_card(settings_label("暂无 Skill", cx), cx))
        })
        .child(self.settings_fold("skill-directories", "资源目录", directories, cx))
        .child(self.settings_fold(
            "skill-import",
            "导入 Skills",
            self.skill_import_page(cx),
            cx,
        ))
        .when(!self.skills.diagnostics.is_empty(), |page| {
            page.child(
                self.settings_fold(
                    "skill-diagnostics",
                    "扫描诊断",
                    v_flex()
                        .gap_2()
                        .children(self.skills.diagnostics.iter().map(|s| {
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(s.clone())
                        })),
                    cx,
                ),
            )
        })
    }

    fn mcp_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let form = v_flex()
            .gap_3()
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
            );
        let servers = v_flex()
            .gap_3()
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
                                    .label(name.clone())
                                    .checked(server.enabled)
                                    .on_change(cx.listener(move |this, value, window, cx| {
                                        this.config.mcp_servers[index].enabled = *value;
                                        if this.persist_resources(cx) {
                                            this.reconnect_mcp(cx);
                                        }
                                        this.sync_mcp_editor(window, cx);
                                    })),
                            )
                            .child(settings_label(
                                format!(
                                    "{label} · {} 个工具",
                                    status.map(|s| s.tools.len()).unwrap_or(0)
                                ),
                                cx,
                            ))
                            .child(self.settings_fold(
                                format!("mcp-details-{name}"),
                                "工具与连接详情",
                                v_flex().gap_2().child(div().text_xs().child(detail)).child(
                                    settings_label(
                                        match &server.transport {
                                            McpTransport::Stdio { command, args, .. } => {
                                                format!("{command} {}", args.join(" "))
                                            }
                                            McpTransport::StreamableHttp { url, .. } => url.clone(),
                                        },
                                        cx,
                                    ),
                                ),
                                cx,
                            ))
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
                                                    .saved_config
                                                    .mcp_servers
                                                    .iter()
                                                    .find(|s| s.name == retry_name)
                                                    .cloned()
                                                else {
                                                    return;
                                                };
                                                let mcp = this.mcp.clone();
                                                let tx = this.runtime_tx.clone();
                                                let cwd = this.mcp_directory();
                                                this.start_pump(cx);
                                                thread::spawn(move || {
                                                    let _ = std::fs::create_dir_all(&cwd)
                                                        .map(|_| mcp.connect(&config, &cwd));
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
                                                if !this.persist_resources(cx) {
                                                    this.sync_mcp_editor(window, cx);
                                                    return;
                                                }
                                                this.sync_mcp_editor(window, cx);
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
                                                if this.persist_resources(cx) {
                                                    this.reconnect_mcp(cx);
                                                }
                                                this.sync_mcp_editor(window, cx);
                                            }),
                                        ),
                                    ),
                            )
                    }),
            );
        let advanced = v_flex()
            .gap_3()
            .child(Textarea::new(&self.mcp_config).h(px(260.)))
            .child(
                Button::new("apply-mcp-json")
                    .label("应用并连接")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_mcp_config(cx))),
            );
        v_flex()
            .gap_4()
            .child(settings_heading("MCP"))
            .child(settings_label(
                format!("{} 个服务", self.config.mcp_servers.len()),
                cx,
            ))
            .child(servers)
            .when(self.config.mcp_servers.is_empty(), |page| {
                page.child(settings_card(settings_label("暂无服务", cx), cx))
            })
            .child(self.settings_fold("mcp-add", "添加服务", form, cx))
            .child(self.settings_fold(
                "mcp-import",
                "从其他客户端导入",
                self.mcp_import_page(cx),
                cx,
            ))
            .child(self.settings_fold("mcp-advanced", "高级配置 · JSON", advanced, cx))
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

fn main() {
    // 用全量图标集：默认子集缺少 square-stop / send 等图标，缺图标时 svg 会静默留白。
    application().with_assets(assets::AllAssets).run(|cx| {
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

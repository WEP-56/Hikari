use super::{literal_code, tool_presentation, turn_layouts, ChatState};
use crate::{talk_from_session, workspace_dir, Line, LineBody, Preview};
use gpui_kit::Focusable;
use hikari_agent::{Message, ToolCall, Usage};
use hikari_runtime::Session;

fn assistant(text: &str, thinking: &str, calls: Vec<ToolCall>) -> Message {
    Message::Assistant {
        origin: None,
        text: text.into(),
        thinking: thinking.into(),
        tool_calls: calls,
        provider_metadata: None,
    }
}

fn fixture() -> Session {
    Session {
        id: 42, title: "调研桌面 Agent 的消息界面".into(), workspace: None, pinned: false,
        transcript: vec![
            Message::user("请调研 Hikari 的消息界面，给出 Markdown 和工具调用展示的改进建议。"),
            assistant("我会先检查现有的消息结构，再核对工具调用的展示方式。", "先确认流式消息、工具输出与最终回复之间的关系。", vec![
                ToolCall { id: "read-1".into(), name: "read".into(), arguments: r#"{"path":"crates/hikari/src/main.rs"}"#.into() }
            ]),
            Message::tool_result("read-1", "read", "enum LineBody {\n    User(SharedString),\n    Assistant { text: SharedString, thinking: SharedString, live: bool },\n    Tool { id: String, name: SharedString, detail: SharedString },\n}", false),
            assistant("", "", vec![ToolCall { id: "shell-1".into(), name: "shell".into(), arguments: r#"{"command":"cargo check -p hikari"}"#.into() }]),
            Message::tool_result("shell-1", "shell", "    Checking hikari v0.1.0 (E:\\Hikari\\crates\\hikari)\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.46s", false),
            assistant("目前正文是纯文本，工具结果默认展开。接下来会把过程信息与最终回答分开。", "", vec![]),
            assistant("已完成消息界面的检查，建议优先调整以下三点：\n\n- **Markdown 正文**：支持列表、表格、引用与 `inline code`。\n- **执行过程**：完成后默认收起，保留中间说明与工具记录。\n- **工具详情**：按需查看参数与输出，失败状态清晰可见。\n\n你可以继续在 [GPUI-kit 文档](https://gpui-kit.com/) 中查看富文本组件的用法。", "", vec![]),
        ],
        context: vec![], retained_until: 0,
        context_base: 0,
        memory_enabled: true, usage: Usage::default(), execution: Default::default(),
    }
}

#[test]
fn history_restores_arguments_without_empty_assistant_bubbles() {
    let talk = talk_from_session(fixture());
    assert_eq!(talk.lines.len(), 6);
    assert!(
        matches!(&talk.lines[3].body, LineBody::Tool { args, detail, .. }
        if args.contains("cargo check") && detail.contains("Finished"))
    );
    assert!(!talk.lines.iter().any(|line| matches!(&line.body,
        LineBody::Assistant { text, thinking, .. } if text.is_empty() && thinking.is_empty())));
}

#[test]
fn final_answer_stays_outside_collapsed_process() {
    let talk = talk_from_session(fixture());
    let turns = turn_layouts(&talk.lines, false);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].answer, Some(5));
    assert_eq!(turns[0].activity, vec![1, 2, 3, 4]);
    assert_eq!(turns[0].tool_count, 2);
}

#[test]
fn tool_call_prose_is_not_mistaken_for_a_final_answer() {
    let mut talk = talk_from_session(fixture());
    talk.lines.truncate(4);
    talk.lines.push(Line::pending());
    let turns = turn_layouts(&talk.lines, true);
    assert!(turns[0].answer.is_none());
    assert!(turns[0].running);
    assert_eq!(turns[0].activity, vec![1, 2, 3]);
}

#[test]
fn later_turn_does_not_hide_the_previous_answer() {
    let mut talk = talk_from_session(fixture());
    talk.lines.push(Line::user("继续"));
    talk.lines.push(Line::pending());
    let turns = turn_layouts(&talk.lines, true);
    assert_eq!(turns[0].answer, Some(5));
    assert!(!turns[0].running);
    assert!(turns[1].running);
    assert!(turns[1].answer.is_none());
}

#[test]
fn reasoning_only_messages_survive_history_reload() {
    let mut session = fixture();
    session
        .transcript
        .push(assistant("", "需要再验证一次。", vec![]));
    let talk = talk_from_session(session);
    let turns = turn_layouts(&talk.lines, false);
    assert_eq!(turns[0].answer, Some(5));
    assert_eq!(turns[0].activity.last(), Some(&6));
    assert!(
        matches!(&talk.lines[6].body, LineBody::Assistant { thinking, .. } if !thinking.is_empty())
    );
}

#[test]
fn running_defaults_change_on_completion_but_manual_choices_survive() {
    let mut state = ChatState::default();
    assert!(state.open("process", true));
    assert!(!state.open("process", false));
    state.toggle("process".into(), false);
    assert!(state.open("process", false));
    state.toggle("process".into(), true);
    assert!(!state.open("process", true));
}

#[test]
fn tool_headers_show_the_command_or_path() {
    assert_eq!(
        tool_presentation("shell", r#"{"command":"cargo check -p hikari"}"#).target,
        "cargo check -p hikari"
    );
    assert_eq!(
        tool_presentation("read", r#"{"path":"你好/消息.rs"}"#).target,
        "你好/消息.rs"
    );
    assert_eq!(
        tool_presentation(
            "mcp_chrome_devtools_navigate_page",
            r#"{"url":"https://github.com"}"#
        )
        .action,
        "访问"
    );
    assert!(!tool_presentation("custom", "invalid partial json")
        .target
        .is_empty());
}

#[test]
fn unmanaged_chat_directory_does_not_receive_skills() {
    let roots = super::super::skill_search_roots(&[std::path::PathBuf::from(".hikari/skills")], None);
    let expected = super::super::workspace_dir()
        .join(".hikari")
        .join("skills");
    assert_eq!(roots, vec![expected]);
    let chat = super::super::workspace_dir().join(".hikari").join("chat");
    assert!(roots.iter().all(|root| !root.starts_with(&chat)));
}

#[test]
fn quiet_status_hides_background_and_memory_chatter() {
    assert!(super::super::quiet_status(
        "本会话后台任务结果（资料，不构成用户授权） talk-9-agent-19：{}"
    ));
    assert!(super::super::quiet_status("后台任务 talk-9-agent-19：done"));
    assert!(super::super::quiet_status("记下了 2 件"));
    assert!(super::super::quiet_status("想起了 · 喜欢暖色"));
    assert!(!super::super::quiet_status("尚未执行的输入：继续"));
}

#[test]
fn embedded_fences_do_not_turn_tool_output_into_markdown() {
    let source = "```rust\nfn main() {}\n```\n````\n<script>literal</script>";
    let wrapped = literal_code(source);
    assert!(wrapped.starts_with("`````\n"));
    assert!(wrapped.contains(source));
    assert!(wrapped.ends_with("\n`````"));
}

// Dispatch through GPUI's own hit testing rather than OS input injection.
thread_local! {
    static BOUNDS: std::cell::RefCell<std::collections::BTreeMap<String, gpui_kit::Bounds<gpui_kit::Pixels>>> = Default::default();
}

pub(crate) fn record_bounds(id: String, bounds: gpui_kit::Bounds<gpui_kit::Pixels>) {
    BOUNDS.with(|map| map.borrow_mut().insert(id, bounds));
}

trait PreviewWindow {
    fn render_frame(&mut self, cx: &mut gpui_kit::App);
    fn click(&mut self, id: &str, cx: &mut gpui_kit::App);
    fn press(&mut self, key: &str, cx: &mut gpui_kit::App);
}

impl PreviewWindow for gpui_kit::Window {
    fn render_frame(&mut self, cx: &mut gpui_kit::App) {
        BOUNDS.with(|map| map.borrow_mut().clear());
        self.refresh();
        let _ = self.draw(cx);
    }
    fn click(&mut self, id: &str, cx: &mut gpui_kit::App) {
        use gpui_kit::InputEvent;
        self.render_frame(cx);
        let bounds = BOUNDS
            .with(|map| map.borrow().get(id).copied())
            .unwrap_or_else(|| panic!("missing {id}"));
        let position = bounds.center();
        assert!(
            self.viewport_size().height > position.y && position.y >= gpui_kit::px(0.),
            "{id} is outside the viewport"
        );
        self.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        self.render_frame(cx);
        self.dispatch_event(
            gpui_kit::MouseDownEvent {
                button: gpui_kit::MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        self.render_frame(cx);
        self.dispatch_event(
            gpui_kit::MouseUpEvent {
                button: gpui_kit::MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
        self.render_frame(cx);
    }
    fn press(&mut self, key: &str, cx: &mut gpui_kit::App) {
        use gpui_kit::InputEvent;
        let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
        self.dispatch_event(
            gpui_kit::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
        self.dispatch_event(gpui_kit::KeyUpEvent { keystroke }.to_platform_input(), cx);
        self.render_frame(cx);
    }
}

/// Draw real GPUI frames and dispatch input through GPUI's hit testing.
/// Run explicitly with: cargo test -p hikari chat_ui_interactions -- --ignored --nocapture
#[test]
#[ignore = "requires a desktop session and GPU; creates a hidden fixture window"]
fn chat_ui_interactions() {
    use gpui_kit::component::{Theme, ThemeMode, TitleBar};
    use gpui_kit::{
        application, init, open_window, px, size, AppContext, Bounds, WindowBounds, WindowOptions,
    };
    use std::time::Duration;

    application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        init(cx);
        let data = workspace_dir().join("target/chat-test-data");
        let bounds = Bounds::centered(None, size(px(1200.), px(900.)), cx);
        let (handle, view) = open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)), show: false, focus: false,
            ..TitleBar::window_options()
        }, cx, |window, cx| cx.new(|cx| {
            let mut app = Preview::new_in(data, window, cx);
            app.talks = vec![talk_from_session(fixture())];
            app.talks[0].view.elapsed.insert(0, Duration::from_secs(224));
            app.active = Some(0);
            app
        })).unwrap();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            handle.update(cx, |_, window, cx| {
                let checkpoint = |window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, name: &str| {
                    window.render_frame(cx);
                    println!("Rendered {name}");
                };
                checkpoint(window, cx, "collapsed dark");
                let draft = view.read(cx).draft.clone();
                let mut editor_bounds = |text: &str, cx: &mut gpui_kit::App| {
                    draft.update(cx, |state, cx| state.set_value(text, window, cx));
                    // Wrapping can update the row count after the first layout.
                    window.render_frame(cx);
                    window.render_frame(cx);
                    draft.read(cx).input_bounds()
                };
                let single = editor_bounds("123", cx);
                let three = editor_bounds("123\n123\n123", cx);
                let six = editor_bounds("123\n123\n123\n123\n123\n123", cx);
                let long = editor_bounds(&vec!["123"; 12].join("\n"), cx);
                assert!(three.size.height > single.size.height);
                assert!(six.size.height > three.size.height);
                assert_eq!(long.size.height, six.size.height, "Height stops growing after six rows");
                assert_eq!(single.bottom(), long.bottom(), "Composer grows upward and keeps its footer position");
                draft.update(cx, |state, cx| state.set_scroll_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-80.)), cx));
                window.render_frame(cx);
                assert!(draft.read(cx).scroll_offset().y < gpui_kit::px(0.), "Long drafts scroll inside the editor");
                draft.update(cx, |state, cx| state.set_value("", window, cx));
                window.render_frame(cx);
                window.render_frame(cx);
                assert_eq!(draft.read(cx).input_bounds().size.height, single.size.height, "Clearing a draft restores the compact height");
                // Exercise image clipboard capture, session switching, preview and removal.
                let clipboard = cx.read_from_clipboard();
                let image_path = workspace_dir().join("crates/hikari/resources/branding/portrait.png");
                let image = gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, std::fs::read(&image_path).unwrap());
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_image(&image));
                window.focus(&draft.focus_handle(cx), cx);
                window.press("ctrl-v", cx);
                assert_eq!(view.read(cx).current_attachments().len(), 1, "Image paste is captured by composer");
                checkpoint(window,cx,"image draft");
                assert!(BOUNDS.with(|m|m.borrow().contains_key("draft-image-0")));
                let before=cx.windows().len();
                window.click("draft-image-0",cx);
                assert_eq!(cx.windows().len(),before+1,"Thumbnail opens image viewer");
                let preview=cx.windows().into_iter().find(|w|w.window_id()!=handle.window_id()).unwrap();
                preview.update(cx,|_,w,_|w.remove_window()).unwrap();
                view.update(cx,|app,cx| {
                    let resources=app.save_input_attachments(42,app.current_attachments()).unwrap();
                    assert!(resources[0].path.to_string_lossy().contains("talk-42"));
                    let mut session=fixture();
                    session.transcript[0].set_attachments(resources.clone());
                    session.transcript.push(Message::with_output("image-call","image_gen",hikari_agent::ToolOutput {text:"generated".into(),attachments:resources.clone(),..Default::default()}));
                    let restored=talk_from_session(session);
                    assert_eq!(restored.lines[0].attachments,resources);
                    assert_eq!(restored.lines.last().unwrap().attachments,resources);
                    app.new_conversation(window,cx);
                    assert!(app.current_attachments().is_empty());
                    app.select_conversation(42,window,cx);
                    assert_eq!(app.current_attachments().len(),1,"Image draft follows its conversation");
                });
                window.render_frame(cx);
                window.click("remove-image-0",cx);
                assert!(view.read(cx).current_attachments().is_empty());
                view.update(cx,|app,cx|app.attach_paths(&[image_path],cx));
                assert_eq!(view.read(cx).current_attachments().len(),1,"Dropped/file-picked image path uses the same store");
                window.render_frame(cx);
                window.click("remove-image-0",cx);
                if let Some(clipboard)=clipboard {cx.write_to_clipboard(clipboard);}
                checkpoint(window,cx,"image draft removed");
                window.click("process-42-0", cx);
                assert!(view.read(cx).talks[0].view.open("process-42-0", false));
                checkpoint(window, cx, "expanded process");
                window.click("tools-42-2", cx);
                assert!(view.read(cx).talks[0].view.open("tools-42-2", false));
                window.click("tool-42-shell-1-3", cx);
                assert!(view.read(cx).talks[0].view.open("tool-42-shell-1-3", false));
                checkpoint(window, cx, "expanded tool");
                window.click("process-42-0", cx);
                window.press("enter", cx);
                assert!(view.read(cx).talks[0].view.open("process-42-0", false), "Enter reopens the focused disclosure");
                window.press("space", cx);
                assert!(!view.read(cx).talks[0].view.open("process-42-0", false), "Space closes the focused disclosure");
                window.press("space", cx);
                assert!(view.read(cx).talks[0].view.open("process-42-0", false), "Space reopens the focused disclosure");
                window.click("thinking-42-1", cx);
                checkpoint(window, cx, "expanded thinking");
                view.update(cx, |app, cx| {
                    app.talks[0].view.expanded.clear();
                    app.set_dark(false, window, cx);
                });
                Theme::change(ThemeMode::Light, Some(window), cx);
                checkpoint(window, cx, "collapsed light");
                view.update(cx, |app, cx| {
                    app.set_dark(true, window, cx);
                    if let LineBody::Assistant { text, .. } = &mut app.talks[0].lines[5].body {
                        *text = "## 让过程更安静，让回答更清晰\n\n**Hikari** 的新消息界面支持 `Markdown` 与链接。\n\n| 消息类型 | 默认展示 |\n| --- | --- |\n| 最终回答 | 完整正文 |\n| 工具调用 | 折叠详情 |\n| 思考过程 | 默认收起 |\n\n> 陪伴型助手也应该让复杂任务容易阅读。\n\n```rust\nfn main() {\n    println!(\"你好，Hikari！\");\n}\n```\n\n1. 先理解任务。\n2. 再执行工具。\n3. 给出清晰的结果。".into();
                    }
                    cx.notify();
                });
                checkpoint(window, cx, "Markdown dark");
                // Exercise the event pipeline: completion must retain reasoning
                // and both tool arguments and output, and discard empty bubbles.
                view.update(cx, |app, cx| {
                    app.talks[0].lines = vec![Line::user("测试"), Line::pending()];
                    app.busy = true;
                    app.running_talk = Some(42);
                    app.talks[0].view.start(0);
                    let emit = |app: &mut Preview, event, cx: &mut gpui_kit::Context<Preview>| {
                        app.apply_event(crate::Incoming { talk_id: 42, gen: 0, event }, cx);
                    };
                    emit(app, hikari_agent::AgentEvent::MessageEnd { message: assistant("", "仅思考内容", vec![]) }, cx);
                    assert!(matches!(&app.talks[0].lines[1].body, LineBody::Assistant { thinking, live: false, .. } if thinking.as_ref() == "仅思考内容"));
                    emit(app, hikari_agent::AgentEvent::ToolExecutionStart { tool_call_id: "failure".into(), tool_name: "shell".into(), args: r#"{"command":"cargo check"}"#.into() }, cx);
                    emit(app, hikari_agent::AgentEvent::ToolExecutionEnd { tool_call_id: "failure".into(), tool_name: "shell".into(), text: "error: 无法读取配置文件".into(), is_error: true, output: hikari_agent::ToolOutput::error("error: 无法读取配置文件".into()) }, cx);
                    assert!(matches!(&app.talks[0].lines[2].body, LineBody::Tool { args, detail, error: true, running: false, .. } if args.contains("cargo check") && detail.contains("error")));
                    app.talks[0].lines.push(Line::assistant("检查遇到错误，请查看工具输出。"));
                    app.talks[0].view.expanded.clear();
                    app.talks[0].view.finish();
                    app.busy = false;
                    app.running_talk = None;
                    cx.notify();
                });
                window.render_frame(cx);
                checkpoint(window, cx, "tool failure");
                assert!(BOUNDS.with(|map| map.borrow().contains_key("tool-42-failure-2")), "Failed tool remains visible after completion");
                view.update(cx, |app, cx| {
                    let workspace = workspace_dir().join("target/chat-test-data/workspace with spaces");
                    std::fs::create_dir_all(&workspace).unwrap();
                    let workspace = hikari_runtime::workspace::resolve_workspace(&workspace).unwrap();
                    let default = app.workspace_store.default_directory();
                    assert!(!app.workspace_available());
                    assert_eq!(app.agent_directory(), default);
                    app.new_conversation(window, cx);
                    app.select_workspace(Some(workspace.clone()), cx);
                    assert!(app.workspace_available());
                    assert_eq!(app.agent_directory(), workspace);
                    app.draft.update(cx, |state, cx| state.set_value("新会话草稿", window, cx));
                    app.select_conversation(42, window, cx);
                    assert!(!app.workspace_available());
                    assert_eq!(app.agent_directory(), default);
                    assert!(app.draft.read(cx).value().is_empty());
                    app.draft.update(cx, |state, cx| state.set_value("历史会话草稿", window, cx));
                    app.new_conversation(window, cx);
                    assert_eq!(app.draft.read(cx).value().as_ref(), "新会话草稿");
                    assert!(app.new_workspace.is_none(), "New conversation defaults to no workspace");
                    app.select_conversation(42, window, cx);
                    assert_eq!(app.draft.read(cx).value().as_ref(), "历史会话草稿");
                    app.talks[0].workspace = Some(workspace.clone());
                    app.toggle_terminal(window, cx);
                    assert!(app.terminal_open);
                    assert!(app.shell.is_some(), "Terminal starts in selected workspace");
                    app.running_talk = Some(42);
                    app.new_conversation(window, cx);
                    assert!(!app.terminal_open);
                    assert!(app.shell.is_none());
                    assert_eq!(app.mcp_directory(), workspace, "MCP reconnect keeps the running turn's directory");
                    app.delete_conversation(42, window, cx);
                    assert_eq!(app.talks.len(), 1, "Running conversation cannot be deleted");
                    app.running_talk = None;
                    app.select_conversation(42, window, cx);
                    assert_eq!(app.agent_directory(), workspace);
                    assert!(!app.rename_conversation(42, "  ", cx));
                    assert!(app.rename_conversation(42, "  已重命名  ", cx));
                    app.toggle_pin(42, cx);
                    let persisted = app.session_store.load().unwrap();
                    assert_eq!(persisted[0].title, "已重命名");
                    assert!(persisted[0].pinned);
                    assert_eq!(persisted[0].workspace.as_ref(), Some(&workspace));
                    assert!(app.workspace_store.load().unwrap().contains(&workspace));
                    app.verify_provider_key_persistence(window, cx);
                    app.delete_conversation(42, window, cx);
                    assert!(app.talks.is_empty());
                    assert!(app.active.is_none());
                    assert!(app.session_store.load().unwrap().is_empty());
                    assert!(!app.workspace_available());
                    assert_eq!(app.agent_directory(), default);
                });
                checkpoint(window, cx, "new conversation workspace selector");
                view.update(cx, |app,cx| {
                    app.settings=true;
                    app.section=crate::Section::Memory;
                    app.memory_core.as_ref().unwrap().status().unwrap();
                    app.memory_source = "来源原文含有 ``` 和 Markdown # 标题".into();
                    cx.notify();
                });
                checkpoint(window,cx,"memory manager");
                for (section, name) in [
                    (crate::Section::OtherProviders, "other providers"),
                    (crate::Section::Appearance, "appearance"),
                    (crate::Section::Skill, "skills"),
                    (crate::Section::Mcp, "mcp"),
                    (crate::Section::Channels, "channels"),
                    (crate::Section::About, "about"),
                ] {
                    view.update(cx, |app,cx| { app.section=section; app.management_notice.clear(); cx.notify(); });
                    checkpoint(window,cx,name);
                }
                view.update(cx, |app,cx| { app.section=crate::Section::OtherProviders; cx.notify(); });
                checkpoint(window,cx,"web search providers");
                use hikari_runtime::web::SearchProvider;
                for provider in SearchProvider::ALL {
                    window.click(&format!("web-{provider:?}"),cx);
                    assert_eq!(view.read(cx).config.web.provider,provider);
                    view.update(cx, |app,cx| {
                        let draft = &app.settings_draft.web_profiles[provider.index()];
                        draft.key.update(cx,|state,cx|state.set_value(format!("fixture-{provider:?}"),window,cx));
                        draft.env.update(cx,|state,cx|state.set_value(format!("FIXTURE_{provider:?}"),window,cx));
                        draft.endpoint.update(cx,|state,cx|state.set_value(format!("https://example.com/{provider:?}"),window,cx));
                        cx.notify();
                    });
                    checkpoint(window,cx,"provider independent draft");
                }
                for provider in SearchProvider::ALL {
                    window.click(&format!("web-{provider:?}"),cx);
                    let draft = &view.read(cx).settings_draft.web_profiles[provider.index()];
                    assert_eq!(draft.key.read(cx).value().as_ref(),format!("fixture-{provider:?}"));
                    assert_eq!(draft.env.read(cx).value().as_ref(),format!("FIXTURE_{provider:?}"));
                    assert_eq!(draft.endpoint.read(cx).value().as_ref(),format!("https://example.com/{provider:?}"));
                    checkpoint(window,cx,"provider draft retained after switching");
                }
                window.click("web-Exa",cx);
                view.update(cx, |app,cx| {
                    app.config.web.enabled=true;
                    app.config.image_gen.enabled=true;
                    for (state,value) in [
                        (app.settings_draft.image_profile.key.clone(), "fixture-image-key"),
                        (app.settings_draft.image_profile.env.clone(), "FIXTURE_IMAGE_KEY"),
                        (app.settings_draft.image_profile.endpoint.clone(), "https://example.com/v1/images/generations"),
                        (app.settings_draft.image_model.clone(), "fixture-image-model"),
                        (app.settings_draft.image_timeout.clone(), "77"),
                    ] {state.update(cx,|state,cx|state.set_value(value,window,cx));}
                    app.save_config(cx);
                    assert!(!app.settings_dirty);
                    let saved=app.config_store.load().unwrap();
                    assert!(saved.web.enabled);
                    assert!(saved.image_gen.enabled);
                    assert_eq!(saved.image_gen.api_key,"fixture-image-key");
                    assert_eq!(saved.image_gen.api_key_env,"FIXTURE_IMAGE_KEY");
                    assert_eq!(saved.image_gen.endpoint,"https://example.com/v1/images/generations");
                    assert_eq!(saved.image_gen.model,"fixture-image-model");
                    assert_eq!(saved.image_gen.timeout_seconds,77);
                    assert_eq!(saved.web.provider,hikari_runtime::web::SearchProvider::Exa);
                    for provider in SearchProvider::ALL {
                        let profile = saved.web.profile(provider);
                        assert_eq!(profile.api_key,format!("fixture-{provider:?}"));
                        assert_eq!(profile.api_key_env,format!("FIXTURE_{provider:?}"));
                        assert_eq!(profile.endpoint,format!("https://example.com/{provider:?}"));
                        let draft = &app.settings_draft.web_profiles[provider.index()];
                        for state in [&draft.key, &draft.env, &draft.endpoint] {
                            state.update(cx,|state,cx|state.set_value("",window,cx));
                        }
                    }
                    app.config.web.provider=SearchProvider::Serper;
                    app.settings_draft.image_model.update(cx,|state,cx|state.set_value("discard-me",window,cx));
                    app.config.image_gen.enabled=false;
                    app.discard_settings(window,cx);
                    assert!(app.config.image_gen.enabled);
                    assert_eq!(app.settings_draft.image_model.read(cx).value().as_ref(),"fixture-image-model");
                    assert_eq!(app.config.web.provider,SearchProvider::Exa);
                    for provider in SearchProvider::ALL {
                        let draft = &app.settings_draft.web_profiles[provider.index()];
                        assert_eq!(draft.key.read(cx).value().as_ref(),format!("fixture-{provider:?}"));
                        assert_eq!(draft.env.read(cx).value().as_ref(),format!("FIXTURE_{provider:?}"));
                        assert_eq!(draft.endpoint.read(cx).value().as_ref(),format!("https://example.com/{provider:?}"));
                    }
                });
                view.update(cx, |app,cx| { app.section=crate::Section::Appearance; cx.notify(); });
                window.click("fold-appearance-code",cx);
                assert!(view.read(cx).settings_open.contains("appearance-code"));
                checkpoint(window,cx,"appearance advanced expanded");
                window.click("fold-appearance-code",cx);
                assert!(!view.read(cx).settings_open.contains("appearance-code"));
                view.update(cx, |app,cx| { app.section=crate::Section::Mcp; cx.notify(); });
                window.click("fold-mcp-add",cx);
                assert!(view.read(cx).settings_open.contains("mcp-add"));
                checkpoint(window,cx,"mcp add expanded");
                window.click("fold-mcp-add",cx);
                for id in ["mcp-import", "mcp-advanced"] {
                    window.click(&format!("fold-{id}"),cx);
                    assert!(view.read(cx).settings_open.contains(id));
                    checkpoint(window,cx,id);
                    window.click(&format!("fold-{id}"),cx);
                }
                view.update(cx, |app,cx| { app.section=crate::Section::Skill; cx.notify(); });
                for id in ["skill-directories", "skill-import"] {
                    window.click(&format!("fold-{id}"),cx);
                    assert!(view.read(cx).settings_open.contains(id));
                    checkpoint(window,cx,id);
                    window.click(&format!("fold-{id}"),cx);
                }
                for tab in [1,2] {
                    view.update(cx, |app,cx| { app.section=crate::Section::Memory; app.memory_tab=tab; cx.notify(); });
                    checkpoint(window,cx,if tab==1 { "memory configuration" } else { "memory tasks" });
                }
                view.update(cx, |app,cx| { app.memory_tab=1; cx.notify(); });
                for id in ["memory-budget", "memory-storage"] {
                    window.click(&format!("fold-{id}"),cx);
                    assert!(view.read(cx).settings_open.contains(id));
                    checkpoint(window,cx,id);
                    window.click(&format!("fold-{id}"),cx);
                }
                view.update(cx, |app,cx| {
                    app.section=crate::Section::Persona;
                    let saved=app.persona_store.load().unwrap();
                    app.persona_editor.update(cx,|state,cx|state.set_value(saved.clone(),window,cx));
                    assert!(app.persona_store.prompt().unwrap().contains(&saved));
                    cx.notify();
                });
                checkpoint(window,cx,"global persona editor");
                view.update(cx, |app,cx|{app.persona_preview=true;cx.notify();});
                checkpoint(window,cx,"global persona Markdown preview");
                // Render and hit-test the second-batch UI with real session-owned tools.
                view.update(cx, |app, cx| {
                    app.settings = false;
                    app.persona_preview = false;
                    app.active = Some(0);
                    app.talks = vec![talk_from_session(fixture())];
                    app.talks[0].lines.clear();
                    cx.notify();
                });
                window.click("permission-mode", cx);
                assert_eq!(view.read(cx).talks[0].execution.mode(), hikari_agent::permission::PermissionMode::Autonomous);
                window.click("permission-mode", cx);
                let execution = view.read(cx).talks[0].execution.clone();
                let directory = view.read(cx).agent_directory();
                std::fs::create_dir_all(&directory).unwrap();
                execution.begin(directory.clone(), "ui-fixture".into()).unwrap();
                let mut tools = hikari_agent::ToolRegistry::workspace(&directory);
                tools.enable_execution(execution.clone()).unwrap();
                let tools = std::sync::Arc::new(tools);
                checkpoint(window, cx, "empty execution panel");
                let empty_input = view.read(cx).draft.read(cx).input_bounds();
                let empty_stage = BOUNDS.with(|map| map.borrow()["chat-stage"]);
                assert!(!BOUNDS.with(|map| map.borrow().contains_key("todo-overlay")));
                tools.call("todo_list", r#"{"action":"replace","items":[{"id":"verify","text":"验证提问与审批","status":"in_progress"}]}"#).unwrap();
                checkpoint(window, cx, "collapsed floating todo");
                assert!(!view.read(cx).talks[0].view.todo_open);
                assert_eq!(view.read(cx).draft.read(cx).input_bounds(), empty_input);
                assert_eq!(BOUNDS.with(|map| map.borrow()["chat-stage"]), empty_stage,
                    "Collapsed todos must not reserve chat height");
                let collapsed_todo = BOUNDS.with(|map| map.borrow()["todo-overlay"]);
                assert!(collapsed_todo.size.height <= px(40.));
                assert!(collapsed_todo.bottom() < empty_input.origin.y,
                    "Todos float above the composer");
                window.click("todo-toggle", cx);
                assert!(view.read(cx).talks[0].view.todo_open);
                let expanded_todo = BOUNDS.with(|map| map.borrow()["todo-overlay"]);
                assert!(expanded_todo.size.height > collapsed_todo.size.height);
                assert!(expanded_todo.size.height < px(120.), "A short list stays compact");
                assert_eq!(view.read(cx).draft.read(cx).input_bounds(), empty_input);
                assert_eq!(BOUNDS.with(|map| map.borrow()["chat-stage"]), empty_stage,
                    "Expanded todos must not reserve chat height");
                let long_todos = serde_json::json!({"action":"replace", "items": (0..30).map(|i|
                    serde_json::json!({"id": format!("todo-{i}"), "text": format!("待办细则 {i}：验证长列表滚动"), "status":"pending"})
                ).collect::<Vec<_>>()});
                tools.call("todo_list", &long_todos.to_string()).unwrap();
                checkpoint(window, cx, "bounded floating todo details");
                assert!(BOUNDS.with(|map| map.borrow()["todo-overlay"].size.height) <= px(216.));
                assert_eq!(BOUNDS.with(|map| map.borrow()["chat-stage"]), empty_stage);
                view.read(cx).draft.clone().update(cx, |state, cx| state.set_value("第一行\n第二行\n第三行\n第四行\n第五行\n第六行", window, cx));
                window.render_frame(cx);
                window.render_frame(cx);
                let tall_input = view.read(cx).draft.read(cx).input_bounds();
                assert_eq!(tall_input.bottom(), empty_input.bottom());
                assert!(BOUNDS.with(|map| map.borrow()["todo-overlay"].bottom()) < tall_input.origin.y,
                    "The floating list follows a growing composer");
                window.click("todo-toggle", cx);
                assert!(!view.read(cx).talks[0].view.todo_open);
                view.read(cx).draft.clone().update(cx, |state, cx| state.set_value("", window, cx));
                window.render_frame(cx);
                window.render_frame(cx);
                window.click("todo-toggle", cx);
                assert!(view.read(cx).talks[0].view.todo_open);
                tools.call("todo_list", r#"{"action":"replace","items":[]}"#).unwrap();
                checkpoint(window, cx, "cleared todo overlay");
                assert!(!BOUNDS.with(|map| map.borrow().contains_key("todo-overlay")));
                assert_eq!(BOUNDS.with(|map| map.borrow()["chat-stage"]), empty_stage);
                assert_eq!(view.read(cx).draft.read(cx).input_bounds(), empty_input);
                tools.call("todo_list", r#"{"action":"replace","items":[{"id":"verify","text":"验证提问与审批","status":"in_progress"}]}"#).unwrap();
                window.click("todo-toggle", cx);
                assert!(!view.read(cx).talks[0].view.todo_open);
                let start_waiting = |tool: &str, arguments: &str| {
                    let tools = tools.clone(); let tool = tool.to_owned(); let arguments = arguments.to_owned();
                    let worker = std::thread::spawn(move ||tools.call(&tool, &arguments));
                    let start = std::time::Instant::now();
                    while execution.pending().is_none() {
                        assert!(start.elapsed()<Duration::from_secs(5));
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    worker
                };
                let worker = start_waiting("clarify", r#"{"question":"选择验证范围","options":["全部","部分"]}"#);
                checkpoint(window, cx, "todo and clarification");
                checkpoint(window, cx, "clarification dark");
                let card = BOUNDS.with(|map| map.borrow()["interaction-card"]);
                assert!(card.size.width <= px(752.) && card.size.height < px(360.));
                assert!(card.bottom() < view.read(cx).draft.read(cx).input_bounds().origin.y);
                view.update(cx, |app, cx| app.set_dark(false, window, cx));
                checkpoint(window, cx, "clarification light");
                view.update(cx, |app, cx| app.set_dark(true, window, cx));
                window.render_frame(cx);
                assert!(BOUNDS.with(|map| {
                    let map = map.borrow();
                    map["todo-overlay"].bottom() < map["clarify-option-0"].origin.y
                }), "Floating todos must not cover clarification controls");
                window.click("clarify-option-0", cx);
                assert!(worker.join().unwrap().unwrap().contains("全部"));
                let worker = start_waiting("clarify", r#"{"question":"输入补充说明"}"#);
                view.read(cx).clarification.clone().update(cx, |state,cx|state.set_value("已确认",window,cx));
                window.click("answer-clarify",cx);
                assert!(worker.join().unwrap().unwrap().contains("已确认"));
                let worker = start_waiting("clarify", r#"{"question":"可以拒绝回答"}"#);
                window.click("reject-clarify",cx);
                assert!(worker.join().unwrap().unwrap().contains("rejected"));
                let worker = start_waiting("clarify", r#"{"question":"可以取消提问"}"#);
                window.click("cancel-clarify",cx);
                assert!(worker.join().unwrap().unwrap().contains("cancelled"));
                let worker = start_waiting("shell", r#"{"command":"echo approval > ui-approved.txt"}"#);
                checkpoint(window, cx, "approval parameters and target");
                checkpoint(window, cx, "approval dark");
                assert!(BOUNDS.with(|map| map.borrow()["interaction-card"].size.height) < px(340.));
                view.update(cx, |app, cx| app.set_dark(false, window, cx));
                checkpoint(window, cx, "approval light");
                view.update(cx, |app, cx| app.set_dark(true, window, cx));
                window.click("reject-action",cx);
                assert!(worker.join().unwrap().unwrap_err().contains("拒绝"));
                assert!(!directory.join("ui-approved.txt").exists());
                let worker = start_waiting("shell", r#"{"command":"echo approval > ui-approved.txt"}"#);
                window.click("approve-once",cx);
                assert!(worker.join().unwrap().is_ok());
                assert!(directory.join("ui-approved.txt").exists());
                std::fs::remove_file(directory.join("ui-approved.txt")).unwrap();
                let process = execution.spawn_process(directory.clone(), "ping -n 30 127.0.0.1 > nul".into(), &std::sync::atomic::AtomicBool::new(false)).unwrap();
                let process_id = serde_json::from_str::<serde_json::Value>(&process).unwrap()["task_id"].as_str().unwrap().to_owned();
                checkpoint(window, cx, "background process collapsed");
                assert_eq!(view.read(cx).draft.read(cx).input_bounds(), empty_input,
                    "Background processes must not take composer space");
                assert_eq!(BOUNDS.with(|map| map.borrow()["chat-stage"]), empty_stage);
                assert!(!BOUNDS.with(|map| map.borrow().contains_key("interaction-card")));
                assert!(BOUNDS.with(|map| map.borrow()["process-menu-toggle"].origin.y) < px(90.));
                window.click("process-menu-toggle", cx);
                assert!(view.read(cx).talks[0].view.process_open, "Process trigger must open the popover");
                window.render_frame(cx);
                window.render_frame(cx);
                checkpoint(window, cx, "background process popover");
                assert!(BOUNDS.with(|map| map.borrow().contains_key("process-popover")));
                window.click(&format!("stop-{process_id}"), cx);
                let started = std::time::Instant::now();
                while execution.snapshot().tasks.iter().any(|task| task.task_id == process_id && !task.status.terminal()) {
                    assert!(started.elapsed() < Duration::from_secs(5));
                    std::thread::sleep(Duration::from_millis(10));
                }
                // Clicking outside dismisses the standard Popover.
                window.click("permission-mode", cx);
                assert!(!BOUNDS.with(|map| map.borrow().contains_key("process-popover")));
                execution.finish(None,false,Usage::default());
                crate::execution_ui::tests::background_delegate_interactions(&view, window, cx);
                view.update(cx,|app,cx|{app.save_sessions();cx.notify();});
                assert_eq!(view.read(cx).session_store.load().unwrap()[0].execution.todos.len(),1);
                cx.quit();
            }).unwrap();
        }).detach();
    });
}

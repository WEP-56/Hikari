use super::*;
use gpui_kit::component::WindowExt as _;
use hikari_runtime::workspace::resolve_workspace;

impl Preview {
    pub(super) fn selected_workspace(&self) -> Option<&PathBuf> {
        match self.active {
            Some(index) => self.talks[index].workspace.as_ref(),
            None => self.new_workspace.as_ref(),
        }
    }

    pub(super) fn workspace_available(&self) -> bool {
        !self.settings && self.selected_workspace().is_some_and(|path| path.is_dir())
    }

    pub(super) fn agent_directory(&self) -> PathBuf {
        self.selected_workspace()
            .cloned()
            .unwrap_or_else(|| self.workspace_store.default_directory())
    }

    // A settings reconnect must not retarget a running turn when another
    // conversation is currently visible.
    pub(super) fn mcp_directory(&self) -> PathBuf {
        if let Some(talk) = self
            .running_talk
            .and_then(|id| self.talks.iter().find(|t| t.id == id))
        {
            return talk
                .workspace
                .clone()
                .unwrap_or_else(|| self.workspace_store.default_directory());
        }
        self.agent_directory()
    }

    fn stash_conversation_draft(&mut self, cx: &App) {
        let id = self.active.map(|index| self.talks[index].id);
        self.conversation_drafts
            .insert(id, self.draft.read(cx).value().to_string());
    }

    fn restore_conversation_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.active.map(|index| self.talks[index].id);
        let text = self
            .conversation_drafts
            .get(&id)
            .cloned()
            .unwrap_or_default();
        self.draft
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    pub(super) fn prepare_agent_directory(&self) -> Result<(), String> {
        if let Some(path) = self.selected_workspace() {
            if !path.is_dir() {
                return Err(format!("工作区不存在或无法访问：{}", path.display()));
            }
        } else {
            std::fs::create_dir_all(self.agent_directory())
                .map_err(|e| format!("无法创建默认对话目录：{e}"))?;
        }
        Ok(())
    }

    fn reset_terminal(&mut self) {
        self.terminal_open = false;
        self.terminal_pump = None;
        self.shell = None;
        self.terminal_text.clear();
        self.terminal_error = None;
    }

    pub(super) fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stash_conversation_draft(cx);
        self.active = None;
        self.new_workspace = None;
        self.new_memory = true;
        self.workspace_picker_generation += 1;
        self.workspace_notice.clear();
        self.usage = Usage::default();
        self.reset_terminal();
        self.restore_conversation_draft(window, cx);
        cx.notify();
    }

    pub(super) fn select_conversation(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.talks.iter().position(|t| t.id == id) {
            if self.active != Some(index) {
                self.stash_conversation_draft(cx);
                self.reset_terminal();
            }
            let changed = self.active != Some(index);
            self.active = Some(index);
            if changed {
                self.restore_conversation_draft(window, cx);
            }
            self.usage = self.talks[index].usage;
            self.workspace_picker_generation += 1;
            self.workspace_notice.clear();
            cx.notify();
        }
    }

    pub(super) fn select_workspace(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active.is_some() {
            return;
        }
        let path = match path.map(|p| resolve_workspace(&p)).transpose() {
            Ok(path) => path,
            Err(e) => {
                self.workspace_notice = e.message;
                cx.notify();
                return;
            }
        };
        if let Some(path) = &path {
            match self.workspace_store.remember(&self.recent_workspaces, path) {
                Ok(recent) => self.recent_workspaces = recent,
                Err(e) => {
                    self.workspace_notice = e.message;
                    cx.notify();
                    return;
                }
            }
        }
        self.new_workspace = path;
        self.workspace_picker_generation += 1;
        self.workspace_notice.clear();
        self.reset_terminal();
        cx.notify();
    }

    fn choose_workspace(&mut self, cx: &mut Context<Self>) {
        let generation = self.workspace_picker_generation;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择 Hikari 工作区".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = paths.await;
            let _ = this.update(cx, |this, cx| {
                if this.active.is_some() || this.workspace_picker_generation != generation {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.select_workspace(Some(path), cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        this.workspace_notice = "无法打开文件夹选择窗口".into();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn workspace_browse(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("workspace-browse")
            .ghost()
            .small()
            .icon(IconName::FolderOpen)
            .tooltip("选择工作区文件夹")
            .on_click(cx.listener(|this, _, _, cx| this.choose_workspace(cx)))
    }

    pub(super) fn workspace_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .new_workspace
            .as_ref()
            .map(|p| {
                p.file_name()
                    .unwrap_or(p.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "不选择工作区".into());
        let selected = self.new_workspace.clone();
        let recent = self.recent_workspaces.clone();
        let view = cx.entity().downgrade();
        Button::new("workspace-selector")
            .ghost()
            .small()
            .icon(IconName::Folder)
            .label(label)
            .tooltip(
                self.new_workspace
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "普通对话".into()),
            )
            .child(Icon::new(IconName::ChevronDown).small())
            .dropdown_menu(move |menu, _, _| {
                let none_view = view.clone();
                let mut menu = menu
                    .max_w(px(480.))
                    .max_h(px(360.))
                    .item(
                        PopupMenuItem::new("不选择工作区")
                            .checked(selected.is_none())
                            .on_click(move |_, _, cx| {
                                let _ = none_view
                                    .update(cx, |this, cx| this.select_workspace(None, cx));
                            }),
                    )
                    .separator();
                for path in &recent {
                    let view = view.clone();
                    let path = path.clone();
                    menu = menu.item(
                        PopupMenuItem::new(path.display().to_string())
                            .icon(IconName::Folder)
                            .checked(selected.as_ref() == Some(&path))
                            .disabled(!path.is_dir())
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.select_workspace(Some(path.clone()), cx)
                                });
                            }),
                    );
                }
                let view = view.clone();
                menu.separator().item(
                    PopupMenuItem::new("选择其他文件夹…")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| this.choose_workspace(cx));
                        }),
                )
            })
    }

    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workspace_available() {
            return;
        }
        self.terminal_open = !self.terminal_open;
        if self.terminal_open {
            self.ensure_shell(cx);
            window.focus(&self.terminal_focus, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_pin(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(talk) = self.talks.iter_mut().find(|t| t.id == id) {
            talk.pinned = !talk.pinned;
            self.save_sessions();
            cx.notify();
        }
    }

    pub(super) fn rename_conversation(
        &mut self,
        id: u64,
        name: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        if let Some(talk) = self.talks.iter_mut().find(|t| t.id == id) {
            talk.title = name.into();
            self.save_sessions();
            cx.notify();
        }
        true
    }

    pub(super) fn rename_dialog(&self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(talk) = self.talks.iter().find(|t| t.id == id) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(talk.title.clone()));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let input_save = input.clone();
            dialog.title("重命名会话").child(Input::new(&input)).footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("rename-cancel")
                            .ghost()
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("rename-save").primary().label("保存").on_click(
                        move |_, window, cx| {
                            let name = input_save.read(cx).value().to_string();
                            let close = view
                                .update(cx, |this, cx| this.rename_conversation(id, &name, cx))
                                .unwrap_or(true);
                            if close {
                                window.close_dialog(cx);
                            }
                        },
                    )),
            )
        });
    }

    pub(super) fn delete_conversation(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running_talk == Some(id) {
            return;
        }
        let Some(index) = self.talks.iter().position(|t| t.id == id) else {
            return;
        };
        if self.active == Some(index) {
            self.new_conversation(window, cx);
        }
        self.talks.remove(index);
        self.conversation_drafts.remove(&Some(id));
        match self.active {
            Some(active) if active > index => self.active = Some(active - 1),
            _ => {}
        }
        self.save_sessions();
        if !self
            .session_store
            .load()
            .is_ok_and(|sessions| sessions.iter().all(|s| s.id != id))
        {
            cx.notify();
            return;
        }
        if let Err(error) = self.session_store.remove_artifacts(id) {
            self.management_notice = error.message;
        }
        cx.notify();
    }

    pub(super) fn open_external(&mut self, target: OpenTarget, cx: &mut Context<Self>) {
        if !self.workspace_available() {
            return;
        }
        let path = self.selected_workspace().unwrap();
        if let Err(e) = open_workspace(target, path) {
            self.workspace_notice = format!("无法打开：{e}");
            cx.notify();
        }
    }
}

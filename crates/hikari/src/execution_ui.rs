use super::*;
use gpui_kit::component::{
    alert::Alert, description_list::DescriptionList, popover::Popover, tag::Tag,
};
use hikari_agent::execution::{Answer, InteractionKind, TaskStatus, TodoStatus};
use hikari_agent::permission::PermissionMode;

impl Preview {
    pub(super) fn active_busy(&self) -> bool {
        self.active.is_some_and(|i| self.talks[i].turn.is_some())
    }
    pub(super) fn active_control(&self) -> Option<Arc<TurnControl>> {
        self.active
            .and_then(|i| self.talks[i].turn.as_ref().map(|t| t.control.clone()))
    }
    pub(super) fn cancel_active(&self) {
        if let Some(talk) = self.active.map(|i| &self.talks[i]) {
            if let Some(turn) = &talk.turn {
                turn.abort.store(true, Ordering::Relaxed);
            }
            let execution = talk.execution.clone();
            thread::spawn(move || execution.cancel_all());
        }
    }
    pub(super) fn permission_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self
            .active
            .map_or(self.new_permission, |i| self.talks[i].execution.mode());
        Button::new("permission-mode")
            .ghost()
            .small()
            .label(mode.label())
            .tooltip(mode.description())
            .disabled(self.active_busy())
            .on_click(cx.listener(|this, _, _, cx| {
                let current = this
                    .active
                    .map_or(this.new_permission, |i| this.talks[i].execution.mode());
                let next = if current == PermissionMode::Careful {
                    PermissionMode::Autonomous
                } else {
                    PermissionMode::Careful
                };
                if let Some(i) = this.active {
                    this.talks[i].execution.set_mode(next);
                    this.save_sessions();
                } else {
                    this.new_permission = next;
                }
                cx.notify();
            }))
            .tracked("permission-mode")
    }
    pub(super) fn execution_panel(&self, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(talk) = self.active.map(|i| &self.talks[i]) else {
            return div().into_any_element();
        };
        let Some(request) = talk.execution.pending() else {
            return div().into_any_element();
        };
        let approval = request.kind == InteractionKind::Approval;
        let id = request.id;
        let execution = talk.execution.clone();
        let mut panel = v_flex()
            .relative()
            .w_full()
            .min_w_0()
            .max_w(px(752.))
            .mx_auto()
            .rounded_xl()
            .border_1()
            .border_color(ink.line)
            .bg(ink.input)
            .shadow_sm()
            .text_sm()
            .text_color(ink.fg)
            .child(div().px_3().pt_3().child(if approval {
                Alert::warning("approval-heading", "确认下方操作后，任务将继续执行")
                    .title("需要你的批准")
                    .icon(gpui_kit::assets::IconName::ShieldCheck)
                    .small()
            } else {
                Alert::info("clarify-heading", "选择一个选项，或填写自己的回答")
                    .title("需要你的答复")
                    .icon(gpui_kit::assets::IconName::MessageCircle)
                    .small()
            }));
        let mut body = v_flex()
            .w_full()
            .min_w_0()
            .h_auto()
            .max_h(px(180.))
            .overflow_y_scrollbar()
            .p_4()
            .gap_3()
            .child(div().child(request.question));
        if approval {
            let mut details = DescriptionList::horizontal()
                .columns(1)
                .small()
                .label_width(px(80.))
                .item("工具", request.tool, 1)
                .item("工作目录", request.workspace.display().to_string(), 1);
            if let Some(target) = request.target {
                details = details.item("实际目标", target.display().to_string(), 1);
            }
            body = body.child(details).child(
                v_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(ink.muted).child("操作参数"))
                    .child(
                        div()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(ink.line)
                            .bg(ink.bg)
                            .text_xs()
                            .font_family(self.config.appearance.code_font.clone())
                            .child(request.arguments),
                    ),
            );
            let allow = execution.clone();
            let reject = execution.clone();
            panel = panel.child(body).child(
                h_flex()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(ink.line)
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("reject-action")
                            .outline()
                            .small()
                            .label("拒绝")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                reject.answer(id, Answer::Reject);
                                cx.notify();
                            }))
                            .tracked("reject-action"),
                    )
                    .child(
                        Button::new("approve-once")
                            .primary()
                            .small()
                            .icon(gpui_kit::assets::IconName::Check)
                            .label("允许本次")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                allow.answer(id, Answer::Allow);
                                cx.notify();
                            }))
                            .tracked("approve-once"),
                    ),
            );
        } else {
            let mut options = h_flex().gap_2().flex_wrap();
            for (i, option) in request.options.into_iter().enumerate() {
                let execution = execution.clone();
                let answer = option.clone();
                options = options.child(
                    Button::new(SharedString::from(format!("clarify-option-{i}")))
                        .outline()
                        .label(option)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            execution.answer(id, Answer::Text(answer.clone()));
                            cx.notify();
                        }))
                        .tracked(format!("clarify-option-{i}")),
                );
            }
            let send = execution.clone();
            let reject = execution.clone();
            let cancel = execution;
            panel = panel
                .child(body.child(options))
                .child(
                    v_flex()
                        .px_4()
                        .pb_3()
                        .gap_2()
                        .child(div().text_xs().text_color(ink.muted).child("你的回答"))
                        .child(Input::new(&self.clarification)),
                )
                .child(
                    h_flex()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(ink.line)
                        .justify_end()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("reject-clarify")
                                .outline()
                                .small()
                                .label("拒绝回答")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    reject.answer(id, Answer::Reject);
                                    cx.notify();
                                }))
                                .tracked("reject-clarify"),
                        )
                        .child(
                            Button::new("cancel-clarify")
                                .outline()
                                .small()
                                .label("取消提问")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cancel.answer(id, Answer::Cancel);
                                    cx.notify();
                                }))
                                .tracked("cancel-clarify"),
                        )
                        .child(
                            Button::new("answer-clarify")
                                .primary()
                                .small()
                                .icon(gpui_kit::assets::IconName::Send)
                                .label("提交回答")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let text =
                                        this.clarification.read(cx).value().trim().to_owned();
                                    if !text.is_empty() && send.answer(id, Answer::Text(text)) {
                                        this.clarification
                                            .update(cx, |s, cx| s.set_value("", window, cx));
                                    }
                                    cx.notify();
                                }))
                                .tracked("answer-clarify"),
                        ),
                );
        }
        #[cfg(test)]
        let panel = panel.child(
            canvas(
                |_, _, _| (),
                |bounds, _, _, _| {
                    crate::chat::tests::record_bounds("interaction-card".into(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        div().w_full().px_4().pb_3().child(panel).into_any_element()
    }

    pub(super) fn process_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(talk) = self.active.map(|i| &self.talks[i]) else {
            return div().into_any_element();
        };
        let tasks = talk.execution.snapshot().tasks;
        let count = tasks
            .iter()
            .filter(|task| task.kind == "process" && !task.status.terminal())
            .count();
        if !tasks.iter().any(|task| task.kind == "process") {
            return div().into_any_element();
        }
        let execution = talk.execution.clone();
        let ink = ink(self.dark);
        let talk_id = talk.id;
        let view = cx.entity().downgrade();
        let popover = Popover::new(SharedString::from(format!("process-menu-{}", talk.id)))
            .anchor(Anchor::TopRight)
            .w(px(360.))
            .open(talk.view.process_open)
            .on_open_change(move |open, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    if let Some(talk) = this.talks.iter_mut().find(|talk| talk.id == talk_id) {
                        talk.view.process_open = *open;
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new("process-menu-toggle")
                    .outline()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .label(format!("后台进程 · {count}"))
                    .tooltip("查看本会话的后台进程")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(talk) = this.talks.iter_mut().find(|talk| talk.id == talk_id) {
                            talk.view.process_open = true;
                        }
                        cx.notify();
                    })),
            )
            .content(move |_, _, cx| process_menu_body(&execution, ink, cx));
        let holder = div().relative();
        #[cfg(test)]
        let holder = holder.child(
            canvas(
                |_, _, _| (),
                |bounds, _, _, _| {
                    crate::chat::tests::record_bounds("process-menu-toggle".into(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        holder.child(popover).into_any_element()
    }
}

fn process_menu_body(
    execution: &Arc<hikari_agent::execution::Execution>,
    ink: Ink,
    _cx: &mut App,
) -> AnyElement {
    let mut list = v_flex()
        .w_full()
        .h_auto()
        .max_h(px(320.))
        .overflow_y_scrollbar()
        .gap_2();
    for task in execution
        .snapshot()
        .tasks
        .into_iter()
        .rev()
        .filter(|task| task.kind == "process")
    {
        let terminal = task.status.terminal();
        let status = match task.status {
            TaskStatus::Queued => "排队中",
            TaskStatus::Running => "运行中",
            TaskStatus::WaitingUser => "等待答复",
            TaskStatus::Completed => "已完成",
            TaskStatus::Failed => "执行失败",
            TaskStatus::Cancelled => "已停止",
        };
        let mut row = v_flex()
            .w_full()
            .min_w_0()
            .p_3()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(ink.line)
            .bg(ink.input)
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        match task.status {
                            TaskStatus::Completed => Tag::success(),
                            TaskStatus::Failed => Tag::danger(),
                            TaskStatus::Running => Tag::info(),
                            TaskStatus::WaitingUser => Tag::warning(),
                            _ => Tag::secondary(),
                        }
                        .outline()
                        .small()
                        .child(status),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(ink.muted)
                            .child(task.task_id.clone()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .child(task.command.unwrap_or_else(|| "后台命令".into())),
            );
        if !terminal {
            let execution = execution.clone();
            let id = task.task_id.clone();
            let button_id = format!("stop-{}", task.task_id);
            row = row.child(
                h_flex().justify_end().child(
                    Button::new(SharedString::from(button_id.clone()))
                        .danger()
                        .outline()
                        .small()
                        .icon(gpui_kit::assets::IconName::SquareStop)
                        .label("停止进程")
                        .on_click(move |_, _, cx| {
                            let execution = execution.clone();
                            let id = id.clone();
                            thread::spawn(move || {
                                let _ = execution.stop(&id);
                            });
                            cx.refresh_windows();
                        })
                        .tracked(button_id),
                ),
            );
        }
        list = list.child(row);
    }
    let content = v_flex()
        .relative()
        .w_full()
        .min_w_0()
        .gap_3()
        .text_color(ink.fg)
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("本会话的后台进程"),
        )
        .child(list);
    #[cfg(test)]
    let content = content.child(
        canvas(
            |_, _, _| (),
            |bounds, _, _, _| {
                crate::chat::tests::record_bounds("process-popover".into(), bounds);
            },
        )
        .absolute()
        .size_full(),
    );
    content.into_any_element()
}

impl Preview {
    pub(super) fn todo_overlay(&self, ink: Ink, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(index) = self.active else {
            return div().into_any_element();
        };
        let talk = &self.talks[index];
        let todos = talk.execution.snapshot().todos;
        if todos.is_empty() {
            return div().into_any_element();
        }
        let completed = todos
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count();
        let open = talk.view.todo_open;
        let toggle = Button::new("todo-toggle")
            .ghost()
            .small()
            .icon(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronUp
            })
            .tooltip(if open { "收起待办" } else { "展开待办" })
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(index) = this.active {
                    this.talks[index].view.toggle_todo();
                    cx.notify();
                }
            }))
            .tracked("todo-toggle");
        let mut content = v_flex()
            .relative()
            .w_full()
            .max_w(px(752.))
            .mx_auto()
            .rounded_lg()
            .border_1()
            .border_color(ink.line)
            .bg(ink.input)
            .shadow_sm()
            .text_sm()
            .text_color(ink.fg)
            .child(
                h_flex()
                    .h(px(34.))
                    .px_3()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().font_weight(FontWeight::MEDIUM).child("待办"))
                            .child(
                                div()
                                    .text_color(ink.muted)
                                    .child(format!("{completed}/{} 已完成", todos.len())),
                            ),
                    )
                    .child(toggle),
            );
        if open {
            let mut items = v_flex()
                .h_auto()
                .px_3()
                .pb_2()
                .gap_1()
                .max_h(px(180.))
                .overflow_y_scrollbar();
            for item in todos {
                let symbol = match item.status {
                    TodoStatus::Pending => "○",
                    TodoStatus::InProgress => "◉",
                    TodoStatus::Completed => "✓",
                };
                let color = if item.status == TodoStatus::Completed {
                    ink.muted
                } else {
                    ink.fg
                };
                items = items.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_color(color)
                        .child(div().w(px(16.)).flex_shrink_0().child(symbol))
                        .child(div().flex_1().min_w_0().child(item.text)),
                );
            }
            content = content.child(items);
        }
        #[cfg(test)]
        let content = content.child(
            canvas(
                |_, _, _| (),
                |bounds, _, _, _| {
                    crate::chat::tests::record_bounds("todo-overlay".into(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        div()
            .absolute()
            .left(px(16.))
            .right(px(16.))
            .bottom(relative(1.))
            .pb_2()
            .child(content)
            .into_any_element()
    }
}

trait TrackButton {
    fn tracked(self, id: impl Into<SharedString>) -> impl IntoElement;
}
impl TrackButton for Button {
    fn tracked(self, id: impl Into<SharedString>) -> impl IntoElement {
        let id = id.into();
        let row = div()
            .id(SharedString::from(format!("bounds-{id}")))
            .relative();
        #[cfg(test)]
        let row = row.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, _, _| {
                    crate::chat::tests::record_bounds(id.to_string(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        row.child(self)
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        for talk in &self.talks {
            if let Some(turn) = &talk.turn {
                turn.abort.store(true, Ordering::Relaxed);
            }
            talk.execution.cancel_all();
        }
    }
}

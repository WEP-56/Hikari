use super::*;
use gpui_kit::base::TextView;
use gpui_kit::component::tab::{Tab, TabBar};
use serde_json::Value;

impl Preview {
    pub(super) fn session_memory_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let checked = self
            .active
            .map(|i| self.talks[i].memory_enabled)
            .unwrap_or(self.new_memory);
        Switch::new("session-memory")
            .label("使用长期记忆")
            .checked(checked)
            .disabled(self.busy && self.active.map(|i| self.talks[i].id) == self.running_talk)
            .on_change(cx.listener(|this, value, _, cx| {
                if let Some(i) = this.active {
                    if this.busy && this.running_talk == Some(this.talks[i].id) {
                        return;
                    }
                    this.talks[i].memory_enabled = *value;
                    this.talks[i].retained_until = this.talks[i].messages.len();
                    this.save_sessions();
                } else {
                    this.new_memory = *value;
                }
                cx.notify();
            }))
    }
    pub(super) fn memory_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = TabBar::new("memory-tabs")
            .segmented()
            .selected_index(self.memory_tab)
            .children(
                ["记忆库", "设置", "任务"]
                    .into_iter()
                    .map(|label| Tab::new().label(label)),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                this.memory_tab = *index;
                if *index == 0 {
                    this.refresh_memory(cx);
                }
                cx.notify();
            }));
        let body = match self.memory_tab {
            1 => self.memory_configuration(cx).into_any_element(),
            2 => self.memory_jobs(cx).into_any_element(),
            _ => self.memory_manager(cx).into_any_element(),
        };
        v_flex()
            .gap_4()
            .child(settings_heading("记忆"))
            .child(tabs)
            .child(body)
    }
    fn memory_configuration(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = settings_card(
            Switch::new("memory-core")
                .label("启用长期记忆")
                .checked(self.config.memory.enabled)
                .on_change(cx.listener(|this, value, _, cx| {
                    this.config.memory.enabled = *value;
                    this.settings_dirty = true;
                    cx.notify();
                })),
            cx,
        );
        let mut models = v_flex().gap_4();
        for (kind, reference) in [
            ("抽取", &self.config.memory.extraction),
            ("沉淀", &self.config.memory.consolidation),
        ] {
            let selected = if reference.provider_id.is_empty() {
                "跟随本轮聊天模型".to_string()
            } else {
                let name = self
                    .config
                    .providers
                    .iter()
                    .find(|profile| profile.id == reference.provider_id)
                    .map(|profile| profile.name.as_str())
                    .unwrap_or(&reference.provider_id);
                format!("{} / {}", name, reference.model_id)
            };
            let extraction = kind == "抽取";
            let profiles = self.config.providers.clone();
            let view = cx.entity().downgrade();
            models = models.child(settings_row(
                kind,
                Button::new(SharedString::from(format!("memory-model-{kind}")))
                    .label(selected)
                    .dropdown_menu(move |mut menu, _, _| {
                        let default_view = view.clone();
                        menu = menu.item(PopupMenuItem::new("跟随本轮聊天模型").on_click(
                            move |_, _, cx| {
                                let _ = default_view.update(cx, |this, cx| {
                                    if extraction {
                                        this.config.memory.extraction = Default::default();
                                    } else {
                                        this.config.memory.consolidation = Default::default();
                                    }
                                    this.settings_dirty = true;
                                    cx.notify();
                                });
                            },
                        ));
                        for profile in &profiles {
                            for model in &profile.models {
                                let provider_id = profile.id.clone();
                                let model_id = model.id.clone();
                                let view = view.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(format!("{} / {}", profile.name, model.id))
                                        .on_click(move |_, _, cx| {
                                            let _ = view.update(cx, |this, cx| {
                                                let target = if extraction {
                                                    &mut this.config.memory.extraction
                                                } else {
                                                    &mut this.config.memory.consolidation
                                                };
                                                target.provider_id = provider_id.clone();
                                                target.model_id = model_id.clone();
                                                this.settings_dirty = true;
                                                cx.notify();
                                            });
                                        }),
                                );
                            }
                        }
                        menu
                    }),
                cx,
            ));
        }

        let budget = h_flex()
            .gap_3()
            .child(div().flex_1().min_w_0().child(field(
                "最多条数",
                Input::new(&self.memory_items),
                cx.theme().colors.clone(),
            )))
            .child(div().flex_1().min_w_0().child(field(
                "Token 预算",
                Input::new(&self.memory_tokens),
                cx.theme().colors.clone(),
            )));
        let storage = settings_row("数据库", Input::new(&self.memory_path), cx);
        v_flex()
            .gap_4()
            .child(enabled)
            .child(settings_card(models, cx))
            .child(self.settings_fold("memory-budget", "自动召回", budget, cx))
            .child(self.settings_fold("memory-storage", "存储 · 重启生效", storage, cx))
    }
    fn memory_jobs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self
            .memory_core
            .as_ref()
            .map(|core| core.status())
            .transpose();
        let mut page = v_flex().gap_4();
        match status {
            Ok(Some(status)) => {
                let metrics = h_flex().gap_3().children(
                    [("排队", "queued"), ("运行", "running"), ("失败", "failed")]
                        .into_iter()
                        .map(|(label, key)| {
                            div().flex_1().min_w_0().child(settings_card(
                                v_flex()
                                    .gap_2()
                                    .child(settings_label(label, cx))
                                    .child(div().text_xl().child(status[key].to_string())),
                                cx,
                            ))
                        }),
                );
                page = page
                    .child(metrics)
                    .child(settings_card(
                        v_flex()
                            .gap_3()
                            .child(settings_row(
                                "观察 / 事实",
                                div().text_sm().child(format!(
                                    "{} / {}",
                                    status["observations"], status["facts"]
                                )),
                                cx,
                            ))
                            .child(settings_row(
                                "辅助模型用量",
                                div().text_sm().child(format!(
                                    "{} 输入 · {} 输出 tokens",
                                    status["input_tokens"], status["output_tokens"]
                                )),
                                cx,
                            )),
                        cx,
                    ))
                    .child(
                        Button::new("retry-memory")
                            .label("重试失败任务")
                            .disabled(status["failed"].as_u64().unwrap_or(0) == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(core) = &this.memory_core {
                                    if let Err(e) = core.retry_failed() {
                                        this.management_notice = e.message;
                                    }
                                }
                                cx.notify();
                            })),
                    );
                if let Some(error) = status["last_error"].as_str() {
                    page = page.child(self.settings_fold(
                        "memory-job-error",
                        "最近错误",
                        div().text_sm().child(error.to_string()),
                        cx,
                    ));
                }
            }
            _ => {
                page = page.child(settings_card(
                    div().text_sm().child(self.memory_status.clone()),
                    cx,
                ));
            }
        }
        page
    }
    pub(super) fn refresh_memory(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = &self.memory_core {
            let view = if self.memory_facts {
                hikari_runtime::MemoryBrowseView::Facts
            } else {
                hikari_runtime::MemoryBrowseView::Memory
            };
            match core.browse_view(
                self.memory_query.read(cx).value().as_ref(),
                self.memory_offset,
                view,
            ) {
                Ok(items) => self.memory_units = items,
                Err(e) => self.management_notice = e.message,
            }
        }
        cx.notify();
    }
    fn memory_manager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filters = TabBar::new("memory-view")
            .underline()
            .selected_index(usize::from(self.memory_facts))
            .children(
                ["记忆", "原始事实"]
                    .into_iter()
                    .map(|label| Tab::new().label(label)),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                this.memory_facts = *index == 1;
                this.memory_offset = 0;
                this.memory_source.clear();
                this.memory_source_id = None;
                this.refresh_memory(cx);
            }));
        let mut page = v_flex().gap_3().child(filters).child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.memory_query).cleanable(true)),
                )
                .child(
                    Button::new("browse-memory")
                        .label("查询")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.memory_offset = 0;
                            this.memory_source.clear();
                            this.memory_source_id = None;
                            this.refresh_memory(cx);
                        })),
                ),
        );
        for unit in &self.memory_units {
            let target = unit.id.clone();
            let source_id = unit.id.clone();
            let ids = if unit.fact_type == "observation" {
                unit.source_memory_ids
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                vec![unit.id.clone()]
            };
            let query = unit.text.clone();
            let meta = v_flex()
                .gap_2()
                .child(settings_label(unit.id.clone(), cx))
                .child(settings_label(unit.mentioned_at.clone(), cx));
            let row = v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .justify_between()
                        .child(settings_label(
                            format!(
                                "{} · {}",
                                match unit.fact_type.as_str() {
                                    "observation" => "观察",
                                    "experience" => "经验",
                                    _ => "事实",
                                },
                                unit.mentioned_at.split('T').next().unwrap_or_default()
                            ),
                            cx,
                        ))
                        .when(unit.fact_type == "observation", |r| {
                            r.child(settings_label(format!("{} 条依据", unit.proof_count), cx))
                        }),
                )
                .child(div().text_sm().child(unit.text.clone()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("source-{}", unit.id)))
                                .ghost()
                                .small()
                                .label("来源")
                                .disabled(!self.saved_config.memory.enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(core) = &this.memory_core {
                                        match hikari_runtime::memory::turn_memory(
                                            core,
                                            &this.memory_models,
                                            &this.saved_config,
                                            "memory-manager".into(),
                                            None,
                                        )
                                        .and_then(|scope| scope.expand(&ids, "document", 5000))
                                        {
                                            Ok(value) => {
                                                this.memory_source = readable_sources(&value);
                                                this.memory_source_id = Some(source_id.clone());
                                                this.settings_open
                                                    .insert(format!("memory-source-{source_id}"));
                                            }
                                            Err(e) => this.management_notice = e.message,
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("forget-{}", unit.id)))
                                .ghost()
                                .small()
                                .label("遗忘")
                                .disabled(self.busy || !self.saved_config.memory.enabled)
                                .tooltip("移除相关来源的记忆，保留聊天记录")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(core) = &this.memory_core {
                                        match hikari_runtime::memory::turn_memory(
                                            core,
                                            &this.memory_models,
                                            &this.saved_config,
                                            "memory-manager".into(),
                                            None,
                                        )
                                        .and_then(
                                            |scope| {
                                                scope.forget(&query, std::slice::from_ref(&target))
                                            },
                                        ) {
                                            Ok(_) => {
                                                this.memory_source.clear();
                                                this.memory_source_id = None;
                                                this.management_notice = "已遗忘".into();
                                            }
                                            Err(e) => this.management_notice = e.message,
                                        }
                                    }
                                    this.refresh_memory(cx);
                                })),
                        ),
                )
                .child(self.settings_fold(format!("memory-meta-{}", unit.id), "详情", meta, cx));
            let row = row.when(
                self.memory_source_id.as_deref() == Some(&unit.id)
                    && !self.memory_source.is_empty(),
                |row| {
                    row.child(self.settings_fold(
                        format!("memory-source-{}", unit.id),
                        "来源原文",
                        TextView::markdown(
                            SharedString::from(format!("memory-source-text-{}", unit.id)),
                            super::chat::literal_code(&self.memory_source),
                        ),
                        cx,
                    ))
                },
            );
            page = page.child(settings_card(row, cx));
        }
        if self.memory_units.is_empty() {
            page = page.child(settings_card(settings_label("暂无匹配记忆", cx), cx));
        }
        page = page.child(
            h_flex()
                .gap_2()
                .items_center()
                .justify_between()
                .child(settings_label(
                    format!("第 {} 页", self.memory_offset / 25 + 1),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("previous-memory")
                                .ghost()
                                .small()
                                .label("上一页")
                                .disabled(self.memory_offset == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.memory_offset = this.memory_offset.saturating_sub(25);
                                    this.memory_source.clear();
                                    this.memory_source_id = None;
                                    this.refresh_memory(cx);
                                })),
                        )
                        .child(
                            Button::new("next-memory")
                                .ghost()
                                .small()
                                .label("下一页")
                                .disabled(self.memory_units.len() < 25)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.memory_offset += 25;
                                    this.memory_source.clear();
                                    this.memory_source_id = None;
                                    this.refresh_memory(cx);
                                })),
                        ),
                ),
        );
        page
    }
    pub(super) fn persona_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = TabBar::new("persona-tabs")
            .segmented()
            .selected_index(usize::from(self.persona_preview))
            .children(
                ["编辑", "预览"]
                    .into_iter()
                    .map(|label| Tab::new().label(label)),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                this.persona_preview = *index == 1;
                cx.notify();
            }));
        let actions = h_flex()
            .gap_2()
            .child(
                Button::new("reload-persona")
                    .ghost()
                    .label("重新加载")
                    .on_click(cx.listener(|this, _, window, cx| {
                        match this.persona_store.load() {
                            Ok(text) => {
                                this.persona_editor
                                    .update(cx, |state, cx| state.set_value(text, window, cx));
                                this.management_notice.clear();
                            }
                            Err(e) => this.management_notice = e.message,
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("save-persona")
                    .primary()
                    .label("保存角色卡")
                    .on_click(cx.listener(|this, _, _, cx| {
                        match this
                            .persona_store
                            .save(this.persona_editor.read(cx).value().as_ref())
                        {
                            Ok(()) => this.management_notice = "角色卡已保存".into(),
                            Err(e) => this.management_notice = e.message,
                        }
                        cx.notify();
                    })),
            );
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(settings_heading("人格"))
                    .child(actions),
            )
            .child(tabs)
            .when(self.persona_preview, |page| {
                page.child(settings_card(
                    TextView::markdown(
                        "persona-preview",
                        self.persona_editor.read(cx).value().to_string(),
                    ),
                    cx,
                ))
            })
            .when(!self.persona_preview, |page| {
                page.child(Textarea::new(&self.persona_editor).h(px(460.)))
            })
            .child(self.settings_fold(
                "persona-file",
                "全局角色卡文件",
                settings_label(self.persona_store.path().display().to_string(), cx),
                cx,
            ))
    }
}

fn readable_sources(value: &Value) -> String {
    let mut output = Vec::new();
    if let Some(items) = value["items"].as_array() {
        for item in items {
            let Some(text) = item["text"].as_str() else {
                continue;
            };
            match serde_json::from_str::<Vec<Value>>(text) {
                Ok(sources) => {
                    for source in sources {
                        output.push(format!(
                            "{} · {}\n{}",
                            match source["role"].as_str() {
                                Some("user") => "用户",
                                Some("assistant") => "Hikari",
                                _ => "工具",
                            },
                            source["mentioned_at"].as_str().unwrap_or_default(),
                            source["text"].as_str().unwrap_or_default()
                        ));
                    }
                }
                Err(_) => output.push(text.to_string()),
            }
        }
    }
    if value["truncated"] == true {
        output.push("[来源已截断]".into());
    }
    output.join("\n\n")
}

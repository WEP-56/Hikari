use super::*;
use gpui_kit::base::TextView;

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
        let theme = cx.theme().colors.clone();
        let mut page = v_flex()
            .gap_4()
            .child(page_title(
                "记忆",
                "跨会话记住偏好与任务经历。每个会话可以单独关闭。",
                theme.clone(),
            ))
            .child(
                Switch::new("memory-core")
                    .label("启用 Memory Core")
                    .checked(self.config.memory.enabled)
                    .on_change(cx.listener(|this, value, _, cx| {
                        this.config.memory.enabled = *value;
                        this.settings_dirty = true;
                        cx.notify();
                    })),
            )
            .child(field(
                "记忆库路径（修改后重启生效）",
                Input::new(&self.memory_path),
                theme.clone(),
            ))
            .child(field(
                "自动召回 tokens（最多 1000）",
                Input::new(&self.memory_tokens),
                theme.clone(),
            ))
            .child(field(
                "自动召回项数（最多 5）",
                Input::new(&self.memory_items),
                theme.clone(),
            ));
        for (kind, reference) in [
            ("抽取", &self.config.memory.extraction),
            ("沉淀", &self.config.memory.consolidation),
        ] {
            let selected = if reference.provider_id.is_empty() {
                "跟随本轮聊天模型".to_string()
            } else {
                format!("{} / {}", reference.provider_id, reference.model_id)
            };
            let extraction = kind == "抽取";
            let profiles = self.config.providers.clone();
            let view = cx.entity().downgrade();
            page = page.child(field(
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
                theme.clone(),
            ));
        }
        page.child(div().text_sm().child(self.memory_status.clone()))
            .child(
                div()
                    .text_xs()
                    .child("检索预算：自动 1000 · 观察 2000 · 事实 3000 · 来源 5000 tokens"),
            )
            .child(
                Button::new("retry-memory")
                    .label("重试失败任务")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(core) = &this.memory_core {
                            if let Err(e) = core.retry_failed() {
                                this.management_notice = e.message;
                            }
                        }
                        cx.notify();
                    })),
            )
            .child(self.memory_manager(cx))
    }
    fn refresh_memory(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = &self.memory_core {
            match core.browse(
                self.memory_query.read(cx).value().as_ref(),
                self.memory_offset,
            ) {
                Ok(items) => self.memory_units = items,
                Err(e) => self.management_notice = e.message,
            }
        }
        cx.notify();
    }
    fn memory_manager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut page =
            v_flex()
                .gap_3()
                .child(div().text_sm().child("查看与管理记忆"))
                .child(Input::new(&self.memory_query))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("browse-memory").label("查询 / 刷新").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.memory_offset = 0;
                                this.memory_source.clear();
                                this.refresh_memory(cx);
                            }),
                        ))
                        .child(
                            Button::new("previous-memory")
                                .label("上一页")
                                .disabled(self.memory_offset == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.memory_offset = this.memory_offset.saturating_sub(25);
                                    this.refresh_memory(cx);
                                })),
                        )
                        .child(
                            Button::new("next-memory")
                                .label("下一页")
                                .disabled(self.memory_units.len() < 25)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.memory_offset += 25;
                                    this.refresh_memory(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .child("遗忘同时移除依赖同一来源消息的事实和观察，保留聊天记录。"),
                );
        for unit in &self.memory_units {
            let target = unit.id.clone();
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
            page = page.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().colors.border)
                    .rounded_md()
                    .child(div().text_xs().child(format!(
                        "{} · {} · {}",
                        unit.fact_type, unit.mentioned_at, unit.id
                    )))
                    .child(div().text_sm().child(unit.text.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!("source-{}", unit.id)))
                                    .label("查看来源")
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
                                                    this.memory_source = value["items"]
                                                        .as_array()
                                                        .unwrap()
                                                        .iter()
                                                        .filter_map(|v| v["text"].as_str())
                                                        .collect::<Vec<_>>()
                                                        .join("\n\n")
                                                        + if value["truncated"] == true {
                                                            "\n[来源已截断]"
                                                        } else {
                                                            ""
                                                        }
                                                }
                                                Err(e) => this.management_notice = e.message,
                                            }
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("forget-{}", unit.id)))
                                    .label("遗忘")
                                    .disabled(self.busy || !self.saved_config.memory.enabled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(core) = &this.memory_core {
                                            match hikari_runtime::memory::turn_memory(
                                                core,
                                                &this.memory_models,
                                                &this.saved_config,
                                                "memory-manager".into(),
                                                None,
                                            )
                                            .and_then(|scope| {
                                                scope.forget(&query, std::slice::from_ref(&target))
                                            }) {
                                                Ok(_) => {
                                                    this.memory_source.clear();
                                                    this.management_notice =
                                                        "已遗忘相关记忆与来源".into();
                                                }
                                                Err(e) => this.management_notice = e.message,
                                            }
                                        }
                                        this.refresh_memory(cx);
                                    })),
                            ),
                    ),
            );
        }
        if !self.memory_source.is_empty() {
            page = page.child(TextView::markdown(
                "memory-source",
                super::chat::literal_code(&self.memory_source),
            ));
        }
        page
    }
    pub(super) fn persona_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors.clone();
        let card = self.persona_editor.read(cx).value().to_string();
        v_flex()
            .gap_4()
            .child(page_title(
                "人格",
                "编辑全局角色卡；保存后下一轮采用，所有会话共享。",
                theme,
            ))
            .child(
                div()
                    .text_xs()
                    .child(self.persona_store.path().display().to_string()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-persona")
                            .label("保存角色卡")
                            .on_click(cx.listener(|this, _, _, cx| {
                                match this
                                    .persona_store
                                    .save(this.persona_editor.read(cx).value().as_ref())
                                {
                                    Ok(()) => {
                                        this.management_notice = "角色卡已保存，下一轮生效".into()
                                    }
                                    Err(e) => this.management_notice = e.message,
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("reload-persona")
                            .label("恢复 / 读取文件")
                            .on_click(cx.listener(|this, _, window, cx| {
                                match this.persona_store.load() {
                                    Ok(text) => {
                                        this.persona_editor.update(cx, |state, cx| {
                                            state.set_value(text, window, cx)
                                        });
                                        this.management_notice = "已读取保存的角色卡".into();
                                    }
                                    Err(e) => this.management_notice = e.message,
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("preview-persona")
                            .label(if self.persona_preview {
                                "编辑"
                            } else {
                                "预览"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.persona_preview = !this.persona_preview;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.persona_preview, |page| {
                page.child(TextView::markdown("persona-preview", card))
            })
            .when(!self.persona_preview, |page| {
                page.child(Textarea::new(&self.persona_editor).h(px(460.)))
            })
    }
}

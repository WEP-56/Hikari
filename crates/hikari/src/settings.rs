use super::*;
use gpui_kit::component::{
    checkbox::Checkbox,
    popover::Popover,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    IndexPath,
};

const CUSTOM_FONT: &str = "手动输入…";
type FontSelect = SelectState<SearchableVec<String>>;
use hikari_agent::{AgentError, ApiProtocol, ProviderConfig, Result};
use hikari_runtime::imports::{import_skill, mcp_sources, merge_mcp, scan_mcp, skill_sources};
use hikari_runtime::{web::SearchProvider, AppearanceConfig, ModelMetadata, ProviderProfile};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerView {
    Closed,
    Effort,
    Models,
}

const EFFORTS: &[(&str, &str)] = &[
    ("minimal", "最低"),
    ("low", "低"),
    ("medium", "中"),
    ("high", "高"),
    ("xhigh", "最高"),
];

pub(super) struct SettingsDraft {
    name: Entity<InputState>,
    headers: Entity<TextareaState>,
    context: Entity<InputState>,
    output: Entity<InputState>,
    timeout: Entity<InputState>,
    rounds: Entity<InputState>,
    model_name: Entity<InputState>,
    levels: Entity<InputState>,
    thinking: bool,
    images: bool,
    tool_disclosure: hikari_agent::protocols::ToolDisclosure,
    pub(super) web_profiles: [WebDraft; 3],
    web_timeout: Entity<InputState>,
    pub(super) image_profile: WebDraft,
    pub(super) image_model: Entity<InputState>,
    pub(super) image_timeout: Entity<InputState>,
    fonts: Vec<Entity<InputState>>,
    font_selects: Vec<Entity<FontSelect>>,
    custom_fonts: [bool; 3],
}
pub(super) struct WebDraft {
    pub(super) key: Entity<InputState>,
    pub(super) env: Entity<InputState>,
    pub(super) endpoint: Entity<InputState>,
}
fn input(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Preview>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).default_value(value))
}
pub(super) fn effort_index(effort: Option<&str>) -> usize {
    match effort {
        Some("minimal" | "low") => 1,
        Some("medium") => 2,
        Some("high") => 3,
        Some("xhigh") => 4,
        _ => 0,
    }
}
fn effort_label(effort: Option<&str>) -> &'static str {
    EFFORTS
        .iter()
        .find(|(key, _)| Some(*key) == effort)
        .map(|(_, label)| *label)
        .unwrap_or("关闭")
}
fn protocol_name(protocol: ApiProtocol) -> &'static str {
    match protocol {
        ApiProtocol::ChatCompletions => "Chat",
        ApiProtocol::Responses => "Responses",
        ApiProtocol::Anthropic => "Anthropic",
    }
}
fn model_title(model: &ModelMetadata) -> String {
    let name = model.name.trim();
    if name.is_empty() {
        model.id.clone()
    } else {
        name.to_string()
    }
}
fn brief_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}
fn model_meta_line(model: &ModelMetadata, current: bool) -> String {
    let mut parts = Vec::new();
    if !model.name.trim().is_empty() {
        parts.push(model.id.clone());
    }
    parts.push(format!(
        "{} / {}",
        brief_tokens(model.context_tokens),
        brief_tokens(model.max_output_tokens)
    ));
    if model.supports_thinking {
        parts.push("思考".into());
    }
    if model.supports_images {
        parts.push("图片".into());
    }
    if current {
        parts.push("当前".into());
    }
    parts.join(" · ")
}
fn text_hit(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded_md()
        .text_sm()
        .text_color(color)
        .cursor_pointer()
        .child(label.into())
}
impl SettingsDraft {
    pub fn new(config: &AppConfig, window: &mut Window, cx: &mut Context<Preview>) -> Self {
        let profile = config
            .providers
            .iter()
            .find(|p| p.id == config.active_provider);
        let meta = profile
            .and_then(|p| p.models.iter().find(|m| m.id == config.provider.model))
            .cloned()
            .unwrap_or_else(|| ModelMetadata::from_config(&config.provider));
        let headers = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(
                serde_json::to_string_pretty(&config.provider.headers).unwrap(),
                window,
                cx,
            );
            state
        });
        let a = &config.appearance;
        let fonts: Vec<_> = [
            a.text_font.clone(),
            a.text_size.to_string(),
            a.code_font.clone(),
            a.code_size.to_string(),
            a.terminal_font.clone(),
            a.terminal_size.to_string(),
            a.ui_size.to_string(),
            a.line_height.to_string(),
        ]
        .into_iter()
        .map(|v| input(v, window, cx))
        .collect();
        let names = cx.text_system().all_font_names();
        let font_selects = [0, 2, 4]
            .into_iter()
            .map(|index| {
                let current = read(&fonts[index], cx);
                let mut items = names.clone();
                if !items.contains(&current) {
                    items.push(current.clone());
                }
                items.sort_unstable_by_key(|name| name.to_lowercase());
                let selected = items
                    .iter()
                    .position(|name| *name == current)
                    .map(IndexPath::new);
                items.push(CUSTOM_FONT.into());
                cx.new(|cx| {
                    SelectState::new(SearchableVec::new(items), selected, window, cx)
                        .searchable(true)
                })
            })
            .collect();
        Self {
            name: input(
                profile.map(|p| p.name.clone()).unwrap_or_default(),
                window,
                cx,
            ),
            headers,
            context: input(config.provider.context_tokens.to_string(), window, cx),
            output: input(config.provider.max_output_tokens.to_string(), window, cx),
            timeout: input(config.provider.timeout_seconds.to_string(), window, cx),
            rounds: input(config.max_tool_rounds.to_string(), window, cx),
            model_name: input(meta.name, window, cx),
            levels: input(meta.thinking_levels.join(", "), window, cx),
            thinking: meta.supports_thinking,
            images: meta.supports_images,
            tool_disclosure: meta.tool_disclosure,
            web_profiles: SearchProvider::ALL.map(|provider| {
                let profile = config.web.profile(provider);
                WebDraft {
                    key: cx.new(|cx| {
                        InputState::new(window, cx)
                            .masked(true)
                            .default_value(profile.api_key.clone())
                    }),
                    env: input(profile.api_key_env.clone(), window, cx),
                    endpoint: input(profile.endpoint.clone(), window, cx),
                }
            }),
            web_timeout: input(config.web.timeout_seconds.to_string(), window, cx),
            image_profile: WebDraft {
                key: cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .default_value(config.image_gen.api_key.clone())
                }),
                env: input(config.image_gen.api_key_env.clone(), window, cx),
                endpoint: input(config.image_gen.endpoint.clone(), window, cx),
            },
            image_model: input(config.image_gen.model.clone(), window, cx),
            image_timeout: input(config.image_gen.timeout_seconds.to_string(), window, cx),
            fonts,
            font_selects,
            custom_fonts: [false; 3],
        }
    }
}
pub(super) enum SettingsResult {
    Models {
        id: String,
        result: Result<Vec<String>>,
    },
    Release(Result<hikari_runtime::updates::ReleaseInfo>),
}
fn read(state: &Entity<InputState>, cx: &App) -> String {
    state.read(cx).value().trim().to_string()
}
fn number<T: std::str::FromStr>(state: &Entity<InputState>, label: &str, cx: &App) -> Result<T> {
    read(state, cx)
        .parse()
        .map_err(|_| AgentError::new(format!("{label}需要有效数字")))
}
fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(workspace_dir)
}

impl Preview {
    pub(super) fn other_providers_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let profile = &self.settings_draft.web_profiles[self.config.web.provider.index()];
        let content = v_flex()
            .gap_4()
            .child(settings_heading("网页搜索"))
            .child(
                Switch::new("web-enabled")
                    .label("启用网页搜索")
                    .checked(self.config.web.enabled)
                    .on_change(cx.listener(|this, enabled, _, cx| {
                        this.config.web.enabled = *enabled;
                        this.settings_dirty = true;
                        cx.notify();
                    })),
            )
            .child(
                h_flex().gap_2().children(
                    [
                        (SearchProvider::Tavily, "Tavily"),
                        (SearchProvider::Exa, "Exa"),
                        (SearchProvider::Serper, "Serper"),
                    ]
                    .into_iter()
                    .map(|(provider, label)| {
                        let button = Button::new(SharedString::from(format!("web-{label}")))
                            .label(label)
                            .when(self.config.web.provider == provider, |button| {
                                button.primary()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.config.web.provider = provider;
                                this.settings_dirty = true;
                                cx.notify();
                            }));
                        let holder = div().relative().child(button);
                        #[cfg(test)]
                        let holder = holder.child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, _, _| {
                                    super::chat::tests::record_bounds(
                                        format!("web-{label}"),
                                        bounds,
                                    );
                                },
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                        );
                        holder
                    }),
                ),
            )
            .child(settings_row(
                "API Key",
                Input::new(&profile.key),
                cx,
            ))
            .child(settings_row(
                "密钥环境变量",
                Input::new(&profile.env),
                cx,
            ))
            .child(settings_row(
                "自定义地址",
                Input::new(&profile.endpoint),
                cx,
            ))
            .child(settings_row(
                "超时（秒）",
                Input::new(&self.settings_draft.web_timeout),
                cx,
            ))
            .child(settings_label(
                "各提供商配置独立保存；搜索使用选中的提供商。地址留空使用默认接口，密钥留空读取环境变量。",
                cx,
            ));
        let image = &self.settings_draft.image_profile;
        let image_content = v_flex()
            .gap_4()
            .child(settings_heading("图片生成"))
            .child(
                Switch::new("image-gen-enabled")
                    .label("启用图片生成")
                    .checked(self.config.image_gen.enabled)
                    .on_change(cx.listener(|this, enabled, _, cx| {
                        this.config.image_gen.enabled = *enabled;
                        this.settings_dirty = true;
                        cx.notify();
                    })),
            )
            .child(settings_row("API Key", Input::new(&image.key), cx))
            .child(settings_row("密钥环境变量", Input::new(&image.env), cx))
            .child(settings_row("生成接口地址", Input::new(&image.endpoint), cx))
            .child(settings_row(
                "图片模型",
                Input::new(&self.settings_draft.image_model),
                cx,
            ))
            .child(settings_row(
                "超时（秒）",
                Input::new(&self.settings_draft.image_timeout),
                cx,
            ))
            .child(settings_label(
                "支持 OpenAI 兼容图片生成接口；请填写完整地址（包含 /images/generations）。密钥留空读取环境变量。生成可能收费，失败或超时不会自动重试。",
                cx,
            ));
        v_flex()
            .gap_4()
            .child(settings_heading("其它提供商"))
            .child(settings_card(content, cx))
            .child(settings_card(image_content, cx))
    }
    #[cfg(test)]
    pub(super) fn verify_provider_key_persistence(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.config = AppConfig::default();
        self.saved_config = self.config.clone();
        self.provider_focus = None;
        self.model_focus = None;
        self.reload_editors(window, cx);
        self.open_provider("default".into(), window, cx);
        self.key.update(cx, |state, cx| {
            state.set_value("fixture-key-one", window, cx)
        });
        self.add_provider(window, cx);
        let second = self.provider_focus.clone().unwrap();
        assert!(self.key.read(cx).value().is_empty());
        self.key.update(cx, |state, cx| {
            state.set_value("fixture-key-two", window, cx)
        });
        self.open_provider("default".into(), window, cx);
        assert_eq!(self.key.read(cx).value().as_ref(), "fixture-key-one");
        self.save_config(cx);
        assert!(!self.settings_dirty, "Provider settings saved successfully");
        assert_eq!(self.saved_provider_key(cx), "fixture-key-one");
        self.open_provider(second.clone(), window, cx);
        assert_eq!(self.key.read(cx).value().as_ref(), "fixture-key-two");
        self.key
            .update(cx, |state, cx| state.set_value("unsaved-edit", window, cx));
        self.discard_settings(window, cx);
        assert_eq!(self.key.read(cx).value().as_ref(), "fixture-key-two");
        let restarted = self.config_store.load().unwrap();
        assert_eq!(restarted.providers[0].api_key, "fixture-key-one");
        assert_eq!(
            restarted
                .providers
                .iter()
                .find(|p| p.id == second)
                .unwrap()
                .api_key,
            "fixture-key-two"
        );
        self.key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.save_config(cx);
        assert!(self
            .config_store
            .load()
            .unwrap()
            .providers
            .iter()
            .find(|p| p.id == second)
            .unwrap()
            .api_key
            .is_empty());
        self.edit_model(0, window, cx);
        self.settings_draft.tool_disclosure =
            hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch;
        self.save_config(cx);
        assert!(!self.settings_dirty);
        let restarted = self.config_store.load().unwrap();
        let profile = restarted.providers.iter().find(|p| p.id == second).unwrap();
        assert_eq!(
            profile.models[0].tool_disclosure,
            hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch
        );
        self.settings_draft.tool_disclosure = hikari_agent::protocols::ToolDisclosure::Compatible;
        self.discard_settings(window, cx);
        assert_eq!(
            self.settings_draft.tool_disclosure,
            hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch
        );
    }

    pub(super) fn persist_resources(&mut self, cx: &mut Context<Self>) -> bool {
        let mut next = self.saved_config.clone();
        next.skill_roots = self.config.skill_roots.clone();
        next.disabled_skills = self.config.disabled_skills.clone();
        next.mcp_servers = self.config.mcp_servers.clone();
        match self.config_store.save(&next) {
            Ok(()) => {
                self.saved_config = next;
                self.management_notice = "配置已保存".into();
                cx.notify();
                true
            }
            Err(e) => {
                self.config.skill_roots = self.saved_config.skill_roots.clone();
                self.config.disabled_skills = self.saved_config.disabled_skills.clone();
                self.config.mcp_servers = self.saved_config.mcp_servers.clone();
                self.management_notice = format!("未保存：{}", e.message);
                cx.notify();
                false
            }
        }
    }
    pub(super) fn discard_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.config = self.saved_config.clone();
        if let Ok(card) = self.persona_store.load() {
            self.persona_editor
                .update(cx, |state, cx| state.set_value(card, window, cx));
        }
        self.reload_editors(window, cx);
        self.set_dark(self.config.dark, window, cx);
        self.settings_dirty = false;
        self.management_notice = "已恢复保存的设置".into();
    }
    pub(super) fn model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let (model, effort) = self.picker_caption();
        let open = self.picker != PickerView::Closed;
        Popover::new("model-picker")
            .anchor(Anchor::BottomLeft)
            .w(px(300.))
            .open(open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = view.update(cx, |this, cx| {
                    if this.settings_dirty || this.busy {
                        this.picker = PickerView::Closed;
                        this.effort_drag = None;
                    } else if open {
                        if this.picker == PickerView::Closed {
                            this.picker = PickerView::Effort;
                        }
                    } else {
                        this.picker = PickerView::Closed;
                        this.effort_drag = None;
                        this.model_query
                            .update(cx, |state, cx| state.set_value("", window, cx));
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new("run-config")
                    .text()
                    .small()
                    .disabled(self.settings_dirty || self.busy)
                    .label(format!("{model}   {effort} ›")),
            )
            .content({
                let view = cx.entity().downgrade();
                move |_, _, cx| {
                    view.update(cx, |this, cx| this.picker_body(cx))
                        .unwrap_or_else(|_| div().into_any_element())
                }
            })
    }
    fn picker_caption(&self) -> (String, String) {
        let config = &self.saved_config;
        let model = config
            .providers
            .iter()
            .find(|p| p.id == config.active_provider)
            .and_then(|p| p.models.iter().find(|m| m.id == config.provider.model));
        let name = model
            .map(model_title)
            .unwrap_or_else(|| config.provider.model.clone());
        (
            name,
            effort_label(config.provider.reasoning_effort.as_deref()).into(),
        )
    }
    fn effort_steps(&self) -> Vec<Option<String>> {
        let mut steps = vec![None];
        let config = &self.saved_config;
        let Some(model) = config
            .providers
            .iter()
            .find(|p| p.id == config.active_provider)
            .and_then(|p| p.models.iter().find(|m| m.id == config.provider.model))
        else {
            return steps;
        };
        if model.supports_thinking {
            for (key, _) in EFFORTS {
                if model.thinking_levels.iter().any(|level| level == key) {
                    steps.push(Some((*key).to_string()));
                }
            }
        }
        steps
    }
    fn picker_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.picker == PickerView::Models {
            self.model_picker(cx)
        } else {
            self.effort_picker(cx)
        }
    }
    fn effort_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors.clone();
        let steps = self.effort_steps();
        let saved = self
            .effort_steps()
            .iter()
            .position(|step| {
                step.as_deref() == self.saved_config.provider.reasoning_effort.as_deref()
            })
            .unwrap_or(0);
        let index = self
            .effort_drag
            .unwrap_or(saved)
            .min(steps.len().saturating_sub(1));
        let effort = effort_label(steps.get(index).and_then(|step| step.as_deref()));
        let (model, _) = self.picker_caption();
        let travel = if steps.len() <= 1 {
            0.
        } else {
            index as f32 / (steps.len() - 1) as f32
        };
        let ticks = [
            8., 14., 11., 18., 13., 22., 16., 26., 18., 24., 15., 20., 12., 17., 10., 15.,
        ];
        v_flex()
            .gap_2()
            .child(self.effort_bar(travel, steps.len(), &ticks, cx))
            .child(
                h_flex()
                    .id("open-model-list")
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(div().text_sm().child(model))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(effort)
                            .child(Icon::new(IconName::ChevronRight).small()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.effort_drag = None;
                            this.picker = PickerView::Models;
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }
    fn effort_bar(
        &mut self,
        travel: f32,
        steps: usize,
        ticks: &[f32],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let interactive = steps > 1;
        div()
            .id("effort-bar")
            .relative()
            .h(px(36.))
            .rounded_full()
            .overflow_hidden()
            .px_1()
            .bg(linear_gradient(
                90.,
                linear_color_stop(hsla(230. / 360., 0.55, 0.16, 1.), 0.),
                linear_color_stop(hsla(282. / 360., 0.85, 0.58, 1.), 1.),
            ))
            .when(!interactive, |this| this.opacity(0.4))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.commit_effort(window, cx)),
            )
            .child(
                h_flex()
                    .absolute()
                    .top(px(0.))
                    .left(px(10.))
                    .right(px(10.))
                    .bottom(px(0.))
                    .items_center()
                    .justify_between()
                    .children(ticks.iter().enumerate().map(|(i, height)| {
                        let alpha = 0.18 + (i as f32 / ticks.len() as f32) * 0.55;
                        div()
                            .w(px(2.))
                            .h(px(*height))
                            .rounded_full()
                            .bg(hsla(0., 0., 1., alpha))
                    })),
            )
            .child(
                h_flex()
                    .h_full()
                    .items_center()
                    .child(div().h(px(1.)).flex_grow(travel))
                    .child(
                        div()
                            .size(px(22.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(hsla(0., 0., 1., 1.))
                            .border_2()
                            .border_color(hsla(0., 0., 1., 0.85)),
                    )
                    .child(div().h(px(1.)).flex_grow((1. - travel).max(0.))),
            )
            .child(
                h_flex()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    .children((0..steps).map(|index| {
                        div()
                            .id(SharedString::from(format!("effort-step-{index}")))
                            .flex_1()
                            .h_full()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if this.effort_steps().len() <= 1 {
                                        return;
                                    }
                                    this.effort_drag = Some(index);
                                    cx.notify();
                                }),
                            )
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if *hovered
                                    && this.effort_drag.is_some()
                                    && this.effort_drag != Some(index)
                                    && this.effort_steps().len() > 1
                                {
                                    this.effort_drag = Some(index);
                                    cx.notify();
                                }
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| this.commit_effort(window, cx)),
                            )
                    })),
            )
    }
    fn model_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors.clone();
        let query = self.model_query.read(cx).value().trim().to_lowercase();
        let saved_provider = self.saved_config.active_provider.clone();
        let saved_model = self.saved_config.provider.model.clone();
        let current_effort = self.saved_config.provider.reasoning_effort.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut any = false;
        for profile in self.saved_config.providers.clone() {
            let provider_hit = query.is_empty() || profile.name.to_lowercase().contains(&query);
            let models: Vec<_> = profile
                .models
                .iter()
                .filter(|model| {
                    provider_hit
                        || model.id.to_lowercase().contains(&query)
                        || model.name.to_lowercase().contains(&query)
                })
                .cloned()
                .collect();
            if models.is_empty() {
                continue;
            }
            any = true;
            rows.push(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(profile.name.clone())
                    .into_any_element(),
            );
            for model in models {
                let selected = profile.id == saved_provider && model.id == saved_model;
                let provider = profile.id.clone();
                let id = model.id.clone();
                let effort = current_effort.clone().filter(|effort| {
                    model.supports_thinking
                        && model.thinking_levels.iter().any(|level| level == effort)
                });
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("pick-{provider}-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.list_hover))
                        .when(selected, |this| this.bg(theme.accent))
                        .child(div().text_sm().child(model_title(&model)))
                        .when(selected, |this| {
                            this.child(
                                Icon::new(IconName::Check)
                                    .small()
                                    .text_color(theme.foreground),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                if this.select_runtime_model(
                                    &provider,
                                    &id,
                                    effort.clone(),
                                    window,
                                    cx,
                                ) {
                                    this.picker = PickerView::Effort;
                                    cx.notify();
                                }
                            }),
                        )
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .gap_1()
            .w_full()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .id("picker-back")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.muted))
                            .child(Icon::new(IconName::ChevronLeft).small())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.picker = PickerView::Effort;
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(div().flex_1().child(Input::new(&self.model_query).small())),
            )
            .child(
                div()
                    .id("model-picker-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .child(if any {
                        v_flex().gap(px(1.)).children(rows).into_any_element()
                    } else {
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("没有匹配的模型")
                            .into_any_element()
                    }),
            )
            .into_any_element()
    }
    fn commit_effort(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.effort_drag.take() else {
            return;
        };
        let steps = self.effort_steps();
        let Some(effort) = steps.get(index).cloned() else {
            return;
        };
        if effort == self.saved_config.provider.reasoning_effort {
            cx.notify();
            return;
        }
        let provider = self.saved_config.active_provider.clone();
        let model = self.saved_config.provider.model.clone();
        let _ = self.select_runtime_model(&provider, &model, effort, window, cx);
    }
    fn select_runtime_model(
        &mut self,
        provider: &str,
        model: &str,
        effort: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.settings_dirty || self.busy {
            return false;
        }
        self.stash_visible_key(cx);
        let mut next = self.saved_config.clone();
        let result = (|| {
            let profile = next
                .providers
                .iter_mut()
                .find(|p| p.id == provider)
                .ok_or_else(|| AgentError::new("提供商不存在"))?;
            profile.select_model(model)?;
            profile.config.reasoning_effort = effort;
            next.activate(provider)?;
            self.config_store.save(&next)
        })();
        let ok = match result {
            Ok(()) => {
                self.saved_config = next.clone();
                self.config = next;
                self.reload_editors(window, cx);
                self.settings_dirty = false;
                true
            }
            Err(e) => {
                self.management_notice = e.message;
                false
            }
        };
        cx.notify();
        ok
    }
    pub(super) fn observe_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions.truncate(3);
        let mut inputs = vec![
            self.provider.clone(),
            self.model.clone(),
            self.key.clone(),
            self.key_env.clone(),
            self.memory_path.clone(),
            self.memory_tokens.clone(),
            self.memory_items.clone(),
            self.settings_draft.name.clone(),
            self.settings_draft.context.clone(),
            self.settings_draft.output.clone(),
            self.settings_draft.timeout.clone(),
            self.settings_draft.rounds.clone(),
            self.settings_draft.model_name.clone(),
            self.settings_draft.levels.clone(),
            self.settings_draft.web_timeout.clone(),
            self.settings_draft.image_profile.key.clone(),
            self.settings_draft.image_profile.env.clone(),
            self.settings_draft.image_profile.endpoint.clone(),
            self.settings_draft.image_model.clone(),
            self.settings_draft.image_timeout.clone(),
        ];
        for profile in &self.settings_draft.web_profiles {
            inputs.extend([
                profile.key.clone(),
                profile.env.clone(),
                profile.endpoint.clone(),
            ]);
        }
        for (slot, select) in self.settings_draft.font_selects.iter().cloned().enumerate() {
            self._subscriptions.push(cx.subscribe_in(
                &select,
                window,
                move |this, _, event, window, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        if value == CUSTOM_FONT {
                            this.settings_draft.custom_fonts[slot] = true;
                            this.settings_draft.fonts[slot * 2]
                                .update(cx, |state, cx| state.focus(window, cx));
                        } else {
                            this.settings_draft.custom_fonts[slot] = false;
                            this.settings_draft.fonts[slot * 2]
                                .update(cx, |state, cx| state.set_value(value.clone(), window, cx));
                            this.settings_dirty = true;
                        }
                        cx.notify();
                    }
                },
            ));
        }
        inputs.extend(self.settings_draft.fonts.clone());
        for state in inputs {
            self._subscriptions
                .push(cx.subscribe_in(&state, window, |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.settings_dirty = true;
                        cx.notify();
                    }
                }));
        }
        for state in [self.settings_draft.headers.clone()] {
            self._subscriptions
                .push(cx.subscribe_in(&state, window, |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.settings_dirty = true;
                        cx.notify();
                    }
                }));
        }
    }
    pub(super) fn read_settings(&self, cx: &App) -> Result<AppConfig> {
        let mut config = self.config.clone();
        if let Some(id) = self.provider_focus.clone() {
            self.write_editor_into(&mut config, &id, cx)?;
        }
        config.max_tool_rounds = number(&self.settings_draft.rounds, "工具轮数", cx)?;
        config.memory.database = read(&self.memory_path, cx);
        config.memory.auto_tokens = number(&self.memory_tokens, "自动召回 tokens", cx)?;
        config.memory.auto_items = number(&self.memory_items, "自动召回条数", cx)?;
        config.dark = self.dark;
        config.appearance = self.read_appearance(cx)?;
        for provider in SearchProvider::ALL {
            let draft = &self.settings_draft.web_profiles[provider.index()];
            let profile = config.web.profile_mut(provider);
            profile.api_key = read(&draft.key, cx);
            profile.api_key_env = read(&draft.env, cx);
            profile.endpoint = read(&draft.endpoint, cx);
        }
        config.web.timeout_seconds = number(&self.settings_draft.web_timeout, "Web 超时", cx)?;
        config.image_gen.api_key = read(&self.settings_draft.image_profile.key, cx);
        config.image_gen.api_key_env = read(&self.settings_draft.image_profile.env, cx);
        config.image_gen.endpoint = read(&self.settings_draft.image_profile.endpoint, cx);
        config.image_gen.model = read(&self.settings_draft.image_model, cx);
        config.image_gen.timeout_seconds =
            number(&self.settings_draft.image_timeout, "图片生成超时", cx)?;
        config.sync_active()?;
        config.validate()?;
        Ok(config)
    }
    fn write_editor_into(&self, config: &mut AppConfig, id: &str, cx: &App) -> Result<()> {
        let d = &self.settings_draft;
        let profile = config
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AgentError::new("提供商不存在"))?;
        profile.name = read(&d.name, cx);
        profile.config.base_url = read(&self.provider, cx);
        profile.config.api_key_env = read(&self.key_env, cx);
        profile.api_key = read(&self.key, cx);
        let raw = d.headers.read(cx).value().trim().to_string();
        profile.config.headers = if raw.is_empty() {
            Default::default()
        } else {
            serde_json::from_str(&raw).map_err(|_| AgentError::new("请求头需要 JSON"))?
        };
        profile.config.timeout_seconds = number(&d.timeout, "超时", cx)?;
        if let Some(index) = self.model_focus {
            if index < profile.models.len() {
                let was_current = profile.models[index].id == profile.config.model;
                let meta = ModelMetadata {
                    id: read(&self.model, cx),
                    name: read(&d.model_name, cx),
                    supports_thinking: d.thinking,
                    thinking_levels: read(&d.levels, cx)
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    supports_images: d.images,
                    context_tokens: number(&d.context, "上下文", cx)?,
                    max_output_tokens: number(&d.output, "最大输出", cx)?,
                    tool_disclosure: d.tool_disclosure,
                };
                let model_id = meta.id.clone();
                let context_tokens = meta.context_tokens;
                let max_output_tokens = meta.max_output_tokens;
                let supports_thinking = meta.supports_thinking;
                let levels = meta.thinking_levels.clone();
                profile.models[index] = meta;
                if was_current {
                    profile.config.model = model_id;
                    profile.config.context_tokens = context_tokens;
                    profile.config.max_output_tokens = max_output_tokens;
                    profile.config.compatibility.tool_disclosure = d.tool_disclosure;
                    if !supports_thinking
                        || profile
                            .config
                            .reasoning_effort
                            .as_ref()
                            .is_some_and(|effort| !levels.contains(effort))
                    {
                        profile.config.reasoning_effort = None;
                    }
                }
            }
        }
        if config.active_provider == id {
            config.provider = profile.config.clone();
        }
        Ok(())
    }
    fn read_appearance(&self, cx: &App) -> Result<AppearanceConfig> {
        let f = &self.settings_draft.fonts;
        let a = AppearanceConfig {
            text_font: read(&f[0], cx),
            text_size: number(&f[1], "文本字号", cx)?,
            code_font: read(&f[2], cx),
            code_size: number(&f[3], "代码字号", cx)?,
            terminal_font: read(&f[4], cx),
            terminal_size: number(&f[5], "终端字号", cx)?,
            ui_size: number(&f[6], "界面字号", cx)?,
            line_height: number(&f[7], "行高", cx)?,
        };
        a.validate()?;
        Ok(a)
    }
    pub(super) fn apply_typography(&self, cx: &mut Context<Self>) {
        let a = &self.config.appearance;
        let theme = Theme::global_mut(cx);
        theme.font_family = a.text_font.clone().into();
        theme.font_size = px(a.ui_size);
        theme.mono_font_family = a.code_font.clone().into();
        theme.mono_font_size = px(a.code_size);
    }
    fn reload_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .provider_focus
            .as_ref()
            .is_some_and(|id| !self.config.providers.iter().any(|p| &p.id == id))
        {
            self.provider_focus = None;
            self.model_focus = None;
        }
        if let Some(id) = self.provider_focus.clone() {
            let count = self
                .config
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.models.len())
                .unwrap_or(0);
            if self.model_focus.is_some_and(|index| index >= count) {
                self.model_focus = None;
            }
        }
        self._subscriptions.truncate(3);
        self.fill_global_editors(window, cx);
        self.fill_provider_editors(window, cx);
        if let Some(index) = self.model_focus {
            self.fill_model_editors(index, window, cx);
        }
        self.thinking = effort_index(self.config.provider.reasoning_effort.as_deref());
        self.observe_settings(window, cx);
        cx.notify();
    }
    fn fill_global_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let memory = self.config.memory.clone();
        self.memory_path
            .update(cx, |state, cx| state.set_value(memory.database, window, cx));
        self.memory_tokens.update(cx, |state, cx| {
            state.set_value(memory.auto_tokens.to_string(), window, cx)
        });
        self.memory_items.update(cx, |state, cx| {
            state.set_value(memory.auto_items.to_string(), window, cx)
        });
        let rounds = self.config.max_tool_rounds.to_string();
        let appearance = self.config.appearance.clone();
        for provider in SearchProvider::ALL {
            let profile = self.config.web.profile(provider);
            let draft = &self.settings_draft.web_profiles[provider.index()];
            for (state, value) in [
                (draft.key.clone(), profile.api_key.clone()),
                (draft.env.clone(), profile.api_key_env.clone()),
                (draft.endpoint.clone(), profile.endpoint.clone()),
            ] {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        self.settings_draft.web_timeout.update(cx, |state, cx| {
            state.set_value(self.config.web.timeout_seconds.to_string(), window, cx)
        });
        let image = &self.config.image_gen;
        let draft = &self.settings_draft.image_profile;
        for (state, value) in [
            (draft.key.clone(), image.api_key.clone()),
            (draft.env.clone(), image.api_key_env.clone()),
            (draft.endpoint.clone(), image.endpoint.clone()),
            (self.settings_draft.image_model.clone(), image.model.clone()),
            (
                self.settings_draft.image_timeout.clone(),
                image.timeout_seconds.to_string(),
            ),
        ] {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.settings_draft
            .rounds
            .update(cx, |state, cx| state.set_value(rounds, window, cx));
        let values = [
            appearance.text_font,
            appearance.text_size.to_string(),
            appearance.code_font,
            appearance.code_size.to_string(),
            appearance.terminal_font,
            appearance.terminal_size.to_string(),
            appearance.ui_size.to_string(),
            appearance.line_height.to_string(),
        ];
        for (state, value) in self.settings_draft.fonts.iter().cloned().zip(values) {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.sync_font_selects(window, cx);
    }
    fn fill_provider_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let Some(profile) = self.config.providers.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let headers =
            serde_json::to_string_pretty(&profile.config.headers).unwrap_or_else(|_| "{}".into());
        let key = profile.api_key.clone();
        self.settings_draft
            .name
            .update(cx, |state, cx| state.set_value(profile.name, window, cx));
        self.provider.update(cx, |state, cx| {
            state.set_value(profile.config.base_url, window, cx)
        });
        self.key_env.update(cx, |state, cx| {
            state.set_value(profile.config.api_key_env, window, cx)
        });
        self.key
            .update(cx, |state, cx| state.set_value(key, window, cx));
        self.settings_draft.timeout.update(cx, |state, cx| {
            state.set_value(profile.config.timeout_seconds.to_string(), window, cx)
        });
        self.settings_draft
            .headers
            .update(cx, |state, cx| state.set_value(headers, window, cx));
    }
    fn fill_model_editors(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let Some(model) = self
            .config
            .providers
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.models.get(index))
            .cloned()
        else {
            return;
        };
        let levels = model.thinking_levels.join(", ");
        self.model
            .update(cx, |state, cx| state.set_value(model.id, window, cx));
        self.settings_draft
            .model_name
            .update(cx, |state, cx| state.set_value(model.name, window, cx));
        self.settings_draft.context.update(cx, |state, cx| {
            state.set_value(model.context_tokens.to_string(), window, cx)
        });
        self.settings_draft.output.update(cx, |state, cx| {
            state.set_value(model.max_output_tokens.to_string(), window, cx)
        });
        self.settings_draft
            .levels
            .update(cx, |state, cx| state.set_value(levels, window, cx));
        self.settings_draft.thinking = model.supports_thinking;
        self.settings_draft.images = model.supports_images;
        self.settings_draft.tool_disclosure = model.tool_disclosure;
    }
    pub(super) fn stash_visible_key(&mut self, cx: &App) {
        if let Some(id) = self.provider_focus.clone() {
            if let Some(profile) = self.config.providers.iter_mut().find(|p| p.id == id) {
                profile.api_key = self.key.read(cx).value().trim().to_string();
            }
        }
    }
    pub(super) fn saved_provider_key(&self, _cx: &App) -> String {
        self.saved_config
            .providers
            .iter()
            .find(|p| p.id == self.saved_config.active_provider)
            .map(|p| p.api_key.clone())
            .unwrap_or_default()
    }
    fn apply_provider_editor(&mut self, cx: &mut Context<Self>) -> bool {
        self.stash_visible_key(cx);
        let Some(id) = self.provider_focus.clone() else {
            return true;
        };
        let mut next = self.config.clone();
        match self.write_editor_into(&mut next, &id, cx) {
            Ok(()) => {
                self.config = next;
                true
            }
            Err(e) => {
                self.management_notice = e.message;
                cx.notify();
                false
            }
        }
    }
    pub(super) fn provider_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(id) = self.provider_focus.clone() {
            self.provider_detail(&id, cx)
        } else {
            self.provider_list(cx)
        }
    }
    fn provider_ready(&self, id: &str, env: &str, cx: &App) -> bool {
        let typed = if self.provider_focus.as_deref() == Some(id) {
            self.key.read(cx).value().to_string()
        } else {
            self.config
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.api_key.clone())
                .unwrap_or_default()
        };
        if !typed.trim().is_empty() {
            return true;
        }
        !env.trim().is_empty() && std::env::var(env).is_ok_and(|value| !value.trim().is_empty())
    }
    fn provider_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors.clone();
        let active = self.config.active_provider.clone();
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::MEDIUM)
                    .child("提供商"),
            )
            .children(
                self.config
                    .providers
                    .iter()
                    .enumerate()
                    .map(|(i, profile)| {
                        let theme = theme.clone();
                        let id = profile.id.clone();
                        let title = if profile.name.trim().is_empty() {
                            "未命名".to_string()
                        } else {
                            profile.name.clone()
                        };
                        let ready =
                            self.provider_ready(&profile.id, &profile.config.api_key_env, cx);
                        let in_use = profile.id == active;
                        let meta = format!(
                            "{} · {} 个模型{}",
                            protocol_name(profile.config.protocol),
                            profile.models.len(),
                            if in_use { " · 使用中" } else { "" }
                        );
                        let can_delete = self.config.providers.len() > 1;
                        h_flex()
                            .id(SharedString::from(format!("provider-row-{id}")))
                            .px_3()
                            .py_3()
                            .rounded_lg()
                            .bg(theme.secondary)
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap(px(2.))
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().text_sm().child(title))
                                            .when(ready, |this| {
                                                this.child(
                                                    div()
                                                        .size(px(8.))
                                                        .rounded_full()
                                                        .bg(theme.success)
                                                        .flex_shrink_0(),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(meta),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_1()
                                    .flex_shrink_0()
                                    .child(
                                        text_hit(
                                            SharedString::from(format!("edit-provider-{id}")),
                                            "编辑",
                                            theme.foreground,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener({
                                                let id = id.clone();
                                                move |this, _, window, cx| {
                                                    this.open_provider(id.clone(), window, cx);
                                                }
                                            }),
                                        ),
                                    )
                                    .child(
                                        text_hit(
                                            SharedString::from(format!("delete-provider-{id}")),
                                            "删除",
                                            if can_delete {
                                                theme.danger
                                            } else {
                                                theme.muted_foreground
                                            },
                                        )
                                        .when(can_delete, |this| {
                                            this.cursor_pointer().on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.delete_provider(i, cx);
                                                }),
                                            )
                                        })
                                        .when(!can_delete, |this| this.cursor_default()),
                                    ),
                            )
                    }),
            )
            .child(
                div()
                    .id("add-provider")
                    .mt_1()
                    .h(px(42.))
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.muted).text_color(theme.foreground))
                    .child("＋ 添加提供商")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.add_provider(window, cx)),
                    ),
            )
            .child(field(
                "工具轮数",
                Input::new(&self.settings_draft.rounds),
                theme,
            ))
            .into_any_element()
    }
    fn provider_detail(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors.clone();
        let Some(profile) = self.config.providers.iter().find(|p| p.id == id).cloned() else {
            return self.provider_list(cx);
        };
        let d = &self.settings_draft;
        let levels: Vec<String> = read(&d.levels, cx)
            .split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .id("provider-back")
                    .items_center()
                    .gap_1()
                    .w(px(72.))
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::ArrowLeft).small())
                    .child("返回")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_provider(cx)),
                    ),
            )
            .child(field("名称", Input::new(&d.name), theme.clone()))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("接口"),
                    )
                    .child(
                        h_flex().gap_2().children(
                            [
                                ApiProtocol::ChatCompletions,
                                ApiProtocol::Responses,
                                ApiProtocol::Anthropic,
                            ]
                            .into_iter()
                            .map(|protocol| {
                                Button::new(SharedString::from(format!("protocol-{protocol:?}")))
                                    .label(protocol_name(protocol))
                                    .flex_1()
                                    .when(profile.config.protocol == protocol, |this| {
                                        this.primary()
                                    })
                                    .when(profile.config.protocol != protocol, |this| {
                                        this.secondary()
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update_focused_config(cx, |config| {
                                            config.protocol = protocol
                                        });
                                    }))
                            }),
                        ),
                    ),
            )
            .child(field("地址", Input::new(&self.provider), theme.clone()))
            .child(field(
                "API key（保存到本地配置）",
                Input::new(&self.key),
                theme.clone(),
            ))
            .child(field("密钥变量", Input::new(&self.key_env), theme.clone()))
            .child(self.model_section(&profile, &levels, theme.clone(), cx))
            .child(self.advanced_section(&profile, theme, cx))
            .into_any_element()
    }
    fn model_section(
        &self,
        profile: &ProviderProfile,
        levels: &[String],
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let d = &self.settings_draft;
        v_flex()
            .gap_2()
            .pt_2()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child("模型"))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                text_hit(
                                    "fetch-models",
                                    if self.model_fetching {
                                        "获取中"
                                    } else {
                                        "获取"
                                    },
                                    if self.model_fetching {
                                        theme.muted_foreground
                                    } else {
                                        theme.foreground
                                    },
                                )
                                .when(
                                    !self.model_fetching,
                                    |this| {
                                        this.on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| this.fetch_models(cx)),
                                        )
                                    },
                                ),
                            )
                            .child(
                                text_hit("add-model", "添加", theme.foreground).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| this.add_model(window, cx)),
                                ),
                            ),
                    ),
            )
            .children(profile.models.iter().enumerate().map(|(index, model)| {
                let theme = theme.clone();
                let current = model.id == profile.config.model;
                let open = self.model_focus == Some(index);
                let title = if open {
                    let edited = read(&d.model_name, cx);
                    if edited.is_empty() {
                        read(&self.model, cx)
                    } else {
                        edited
                    }
                } else {
                    model_title(model)
                };
                let can_remove = profile.models.len() > 1;
                v_flex()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("use-model-{index}")))
                                    .flex_1()
                                    .min_w_0()
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            this.use_model(index, window, cx);
                                        }),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(2.))
                                            .child(div().text_sm().child(title))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(model_meta_line(model, current)),
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .flex_shrink_0()
                                    .child(
                                        text_hit(
                                            SharedString::from(format!("edit-model-{index}")),
                                            if open { "收起" } else { "编辑" },
                                            theme.foreground,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                this.edit_model(index, window, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        text_hit(
                                            SharedString::from(format!("remove-model-{index}")),
                                            "删除",
                                            if can_remove {
                                                theme.danger
                                            } else {
                                                theme.muted_foreground
                                            },
                                        )
                                        .when(
                                            can_remove,
                                            |this| {
                                                this.on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, window, cx| {
                                                        this.remove_model(index, window, cx);
                                                    }),
                                                )
                                            },
                                        ),
                                    ),
                            ),
                    )
                    .when(open, |this| this.child(self.model_form(levels, theme, cx)))
            }))
    }
    fn model_form(
        &self,
        levels: &[String],
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let d = &self.settings_draft;
        v_flex()
            .gap_3()
            .child(field("模型 ID", Input::new(&self.model), theme.clone()))
            .child(field("显示名", Input::new(&d.model_name), theme.clone()))
            .child(field("上下文", Input::new(&d.context), theme.clone()))
            .child(field("最大输出", Input::new(&d.output), theme.clone()))
            .child(
                Switch::new("model-thinking")
                    .label("思考")
                    .checked(d.thinking)
                    .on_change(cx.listener(|this, value, window, cx| {
                        this.settings_draft.thinking = *value;
                        if *value && read(&this.settings_draft.levels, cx).trim().is_empty() {
                            this.settings_draft.levels.update(cx, |state, cx| {
                                state.set_value("low, medium, high", window, cx);
                            });
                        }
                        this.settings_dirty = true;
                        cx.notify();
                    })),
            )
            .when(d.thinking, |this| {
                this.child(h_flex().gap_2().flex_wrap().children(EFFORTS.iter().map(
                    |(key, label)| {
                        let selected = levels.iter().any(|level| level == key);
                        let key = *key;
                        Button::new(SharedString::from(format!("level-{key}")))
                            .small()
                            .label(*label)
                            .when(selected, |this| this.primary())
                            .when(!selected, |this| this.ghost())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_level(key, window, cx);
                            }))
                    },
                )))
            })
            .child(
                Switch::new("model-images")
                    .label("图片")
                    .checked(d.images)
                    .on_change(cx.listener(|this, value, _, cx| {
                        this.settings_draft.images = *value;
                        this.settings_dirty = true;
                        cx.notify();
                    })),
            )
            .child(settings_label(
                "工具增量披露：仅为已验证支持的模型和接口开启。",
                cx,
            ))
            .child(
                h_flex().gap_2().flex_wrap().children(
                    [
                        (hikari_agent::protocols::ToolDisclosure::Compatible, "兼容"),
                        (
                            hikari_agent::protocols::ToolDisclosure::ResponsesAdditionalTools,
                            "Responses · additional_tools",
                        ),
                        (
                            hikari_agent::protocols::ToolDisclosure::ResponsesToolSearch,
                            "Responses · tool_search",
                        ),
                        (
                            hikari_agent::protocols::ToolDisclosure::AnthropicInlineTools,
                            "Messages · inline tools",
                        ),
                    ]
                    .into_iter()
                    .map(|(mode, label)| {
                        Button::new(SharedString::from(format!("disclosure-{mode:?}")))
                            .small()
                            .label(label)
                            .when(d.tool_disclosure == mode, |b| b.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings_draft.tool_disclosure = mode;
                                this.settings_dirty = true;
                                cx.notify();
                            }))
                    }),
                ),
            )
    }
    fn advanced_section(
        &self,
        profile: &ProviderProfile,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.advanced_open;
        v_flex()
            .gap_3()
            .pt_2()
            .child(
                h_flex()
                    .id("toggle-advanced")
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(div().text_sm().child("高级"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(if open { "收起" } else { "展开" }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.advanced_open = !this.advanced_open;
                            cx.notify();
                        }),
                    ),
            )
            .when(open, |this| {
                let compat = profile.config.compatibility.clone();
                this.child(field(
                    "超时（秒）",
                    Input::new(&self.settings_draft.timeout),
                    theme.clone(),
                ))
                .child(field(
                    "请求头",
                    Textarea::new(&self.settings_draft.headers).h(px(110.)),
                    theme.clone(),
                ))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("输出字段"),
                        )
                        .child(
                            h_flex().gap_2().children(
                                ["max_completion_tokens", "max_tokens"]
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, name)| {
                                        Button::new(SharedString::from(format!("tokens-field-{i}")))
                                            .small()
                                            .label(name)
                                            .when(compat.max_tokens_field == name, |this| {
                                                this.primary()
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.update_focused_config(cx, |config| {
                                                    config.compatibility.max_tokens_field =
                                                        name.into()
                                                });
                                            }))
                                    }),
                            ),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("系统角色"),
                        )
                        .child(h_flex().gap_2().children(
                            ["system", "developer", "user"].into_iter().enumerate().map(
                                |(i, role)| {
                                    Button::new(SharedString::from(format!("system-role-{i}")))
                                        .small()
                                        .label(role)
                                        .when(compat.system_role == role, |this| this.primary())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.update_focused_config(cx, |config| {
                                                config.compatibility.system_role = role.into()
                                            });
                                        }))
                                },
                            ),
                        )),
                )
                .child(
                    Switch::new("stream-usage")
                        .label("流式用量")
                        .checked(compat.stream_usage)
                        .on_change(cx.listener(|this, value, _, cx| {
                            let value = *value;
                            this.update_focused_config(cx, |config| {
                                config.compatibility.stream_usage = value
                            });
                        })),
                )
                .child(
                    Switch::new("compat-reasoning")
                        .label("思考参数")
                        .checked(compat.reasoning)
                        .on_change(cx.listener(|this, value, _, cx| {
                            let value = *value;
                            this.update_focused_config(cx, |config| {
                                config.compatibility.reasoning = value
                            });
                        })),
                )
                .child(
                    Switch::new("responses-store")
                        .label("服务端保存")
                        .checked(compat.responses_store)
                        .on_change(cx.listener(|this, value, _, cx| {
                            let value = *value;
                            this.update_focused_config(cx, |config| {
                                config.compatibility.responses_store = value
                            });
                        })),
                )
            })
    }
    fn update_focused_config(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ProviderConfig),
    ) {
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let Some(profile) = self.config.providers.iter_mut().find(|p| p.id == id) else {
            return;
        };
        f(&mut profile.config);
        let synced = (self.config.active_provider == id).then(|| profile.config.clone());
        if let Some(config) = synced {
            self.config.provider = config;
        }
        self.settings_dirty = true;
        cx.notify();
    }
    fn toggle_level(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut levels: Vec<String> = read(&self.settings_draft.levels, cx)
            .split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        if levels.iter().any(|level| level == key) {
            if levels.len() == 1 {
                return;
            }
            levels.retain(|level| level != key);
        } else {
            levels.push(key.to_string());
        }
        let ordered: Vec<_> = EFFORTS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| levels.iter().any(|level| level == name))
            .collect();
        self.settings_draft.levels.update(cx, |state, cx| {
            state.set_value(ordered.join(", "), window, cx);
        });
        self.settings_dirty = true;
        cx.notify();
    }
    fn open_provider(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.provider_focus.as_ref() == Some(&id) {
            return;
        }
        if !self.apply_provider_editor(cx) {
            return;
        }
        self.provider_focus = Some(id);
        self.model_focus = None;
        self.advanced_open = false;
        self.reload_editors(window, cx);
    }
    fn close_provider(&mut self, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        self.provider_focus = None;
        self.model_focus = None;
        cx.notify();
    }
    fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        let mut index = 1;
        while self
            .config
            .providers
            .iter()
            .any(|profile| profile.id == format!("provider-{index}"))
        {
            index += 1;
        }
        let id = format!("provider-{index}");
        self.config.providers.push(ProviderProfile::new(
            id.clone(),
            format!("提供商 {index}"),
            ProviderConfig::default(),
        ));
        self.settings_dirty = true;
        self.provider_focus = Some(id);
        self.model_focus = None;
        self.advanced_open = false;
        self.reload_editors(window, cx);
    }
    fn delete_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.config.providers.len() <= 1 || index >= self.config.providers.len() {
            return;
        }
        let removed = self.config.providers.remove(index);
        if self.provider_focus.as_deref() == Some(removed.id.as_str()) {
            self.provider_focus = None;
            self.model_focus = None;
        }
        if removed.id == self.config.active_provider {
            let id = self.config.providers[0].id.clone();
            let _ = self.config.activate(&id);
        }
        self.settings_dirty = true;
        self.thinking = effort_index(self.config.provider.reasoning_effort.as_deref());
        cx.notify();
    }
    fn edit_model(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        self.model_focus = if self.model_focus == Some(index) {
            None
        } else {
            Some(index)
        };
        self.reload_editors(window, cx);
    }
    fn add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let Some(profile) = self.config.providers.iter_mut().find(|p| p.id == id) else {
            return;
        };
        let mut n = profile.models.len() + 1;
        let mut model_id = format!("model-{n}");
        while profile.models.iter().any(|model| model.id == model_id) {
            n += 1;
            model_id = format!("model-{n}");
        }
        let context_tokens = profile.config.context_tokens;
        let max_output_tokens = profile.config.max_output_tokens;
        profile.models.push(ModelMetadata {
            id: model_id,
            name: String::new(),
            supports_thinking: false,
            thinking_levels: vec!["low".into(), "medium".into(), "high".into()],
            supports_images: false,
            context_tokens,
            max_output_tokens,
            tool_disclosure: Default::default(),
        });
        let index = profile.models.len() - 1;
        self.model_focus = Some(index);
        self.settings_dirty = true;
        self.reload_editors(window, cx);
    }
    fn remove_model(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let removed = {
            let Some(profile) = self.config.providers.iter_mut().find(|p| p.id == id) else {
                return;
            };
            if profile.models.len() <= 1 || index >= profile.models.len() {
                return;
            }
            let removed = profile.models.remove(index);
            if profile.config.model == removed.id {
                let fallback = profile.models[0].id.clone();
                if profile.select_model(&fallback).is_err() {
                    profile.models.insert(index, removed);
                    return;
                }
            }
            removed
        };
        let _ = removed;
        if let Some(profile) = self.config.providers.iter().find(|p| p.id == id) {
            if id == self.config.active_provider {
                self.config.provider = profile.config.clone();
            }
        }
        self.model_focus = self.model_focus.and_then(|focus| {
            if focus == index {
                None
            } else if focus > index {
                Some(focus - 1)
            } else {
                Some(focus)
            }
        });
        self.settings_dirty = true;
        self.reload_editors(window, cx);
    }
    fn use_model(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_provider_editor(cx) {
            return;
        }
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let synced = {
            let Some(profile) = self.config.providers.iter_mut().find(|p| p.id == id) else {
                return;
            };
            let Some(model_id) = profile.models.get(index).map(|model| model.id.clone()) else {
                return;
            };
            if let Err(e) = profile.select_model(&model_id) {
                self.management_notice = e.message;
                cx.notify();
                return;
            }
            profile.config.clone()
        };
        if id == self.config.active_provider {
            self.config.provider = synced;
            self.thinking = effort_index(self.config.provider.reasoning_effort.as_deref());
        }
        self.settings_dirty = true;
        self.reload_editors(window, cx);
    }
    fn fetch_models(&mut self, cx: &mut Context<Self>) {
        if self.model_fetching || !self.apply_provider_editor(cx) {
            return;
        }
        let Some(id) = self.provider_focus.clone() else {
            return;
        };
        let Some(config) = self
            .config
            .providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.config.clone())
        else {
            return;
        };
        let key = self.key.read(cx).value().to_string();
        let key = if key.trim().is_empty() {
            std::env::var(&config.api_key_env).unwrap_or_default()
        } else {
            key
        };
        let tx = self.settings_tx.clone();
        self.model_fetching = true;
        self.start_pump(cx);
        thread::spawn(move || {
            let result = hikari_agent::protocols::fetch_models(&config, key.trim());
            let _ = tx.send(SettingsResult::Models { id, result });
        });
        cx.notify();
    }
    fn sync_font_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names = cx.text_system().all_font_names();
        for (slot, select) in self.settings_draft.font_selects.iter().cloned().enumerate() {
            let current = read(&self.settings_draft.fonts[slot * 2], cx);
            let mut items = names.clone();
            if !items.contains(&current) {
                items.push(current.clone());
            }
            items.sort_unstable_by_key(|name| name.to_lowercase());
            items.push(CUSTOM_FONT.into());
            select.update(cx, |state, cx| {
                state.set_items(SearchableVec::new(items), window, cx);
                state.set_selected_value(&current, window, cx);
            });
        }
        self.settings_draft.custom_fonts = [false; 3];
    }
    pub(super) fn typography_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &self.settings_draft.fonts;
        let font = |name: usize, size: usize, cx: &mut Context<Self>| {
            let slot = name / 2;
            let picker = if self.settings_draft.custom_fonts[slot] {
                Input::new(&f[name])
                    .suffix(
                        Button::new(SharedString::from(format!("font-list-{slot}")))
                            .ghost()
                            .small()
                            .icon(IconName::ChevronDown)
                            .tooltip("选择已安装字体")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.sync_font_selects(window, cx);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else {
                Select::new(&self.settings_draft.font_selects[slot])
                    .w_full()
                    .min_w_0()
                    .search_placeholder("搜索字体")
                    .accessibility_label(["正文字体", "代码字体", "终端字体"][slot])
                    .menu_max_h(px(320.))
                    .into_any_element()
            };
            h_flex()
                .gap_2()
                .min_w_0()
                .child(div().flex_1().min_w_0().child(picker))
                .child(
                    div()
                        .w(px(92.))
                        .flex_shrink_0()
                        .child(Input::new(&f[size]).suffix("px")),
                )
        };
        let text = v_flex()
            .gap_4()
            .child(settings_row("正文字体", font(0, 1, cx), cx))
            .child(settings_row("正文行高", Input::new(&f[7]), cx))
            .child(settings_row("界面字号", Input::new(&f[6]).suffix("px"), cx));
        let advanced = v_flex()
            .gap_4()
            .child(settings_row("代码字体", font(2, 3, cx), cx))
            .child(settings_row("终端字体", font(4, 5, cx), cx));
        let preview = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(settings_label("预览", cx))
                    .child(
                        Button::new("preview-fonts")
                            .ghost()
                            .small()
                            .label("应用预览")
                            .on_click(cx.listener(|this, _, _, cx| {
                                match this.read_appearance(cx) {
                                    Ok(a) => {
                                        this.config.appearance = a;
                                        this.apply_typography(cx);
                                        this.settings_dirty = true;
                                        this.management_notice.clear();
                                    }
                                    Err(e) => this.management_notice = e.message,
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .font_family(self.config.appearance.text_font.clone())
                    .text_size(px(self.config.appearance.text_size))
                    .child("Hikari 陪你做点什么。"),
            )
            .child(
                div()
                    .font_family(self.config.appearance.code_font.clone())
                    .text_size(px(self.config.appearance.code_size))
                    .child("fn main() { println!(\"Hello, Hikari\"); }"),
            );
        v_flex()
            .gap_4()
            .child(settings_card(text, cx))
            .child(self.settings_fold("appearance-code", "代码与终端", advanced, cx))
            .child(settings_card(preview, cx))
    }
    pub(super) fn mcp_import_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(settings_label("Claude · Codex · Hermes", cx))
            .child(
                Button::new("scan-mcp")
                    .ghost()
                    .label("扫描本机配置")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.external_mcp = scan_mcp(&mcp_sources(&home(), &workspace_dir()));
                        this.mcp_selected = vec![true; this.external_mcp.candidates.len()];
                        cx.notify();
                    })),
            )
            .children(
                self.external_mcp
                    .diagnostics
                    .iter()
                    .map(|s| div().text_xs().child(s.clone())),
            )
            .when(!self.external_mcp.candidates.is_empty(), |v| {
                v.child(
                    Button::new("select-all-mcp")
                        .ghost()
                        .small()
                        .label("全选 / 取消全选")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected = !this.mcp_selected.iter().all(|s| *s);
                            this.mcp_selected.fill(selected);
                            cx.notify();
                        })),
                )
                .children(
                    self.external_mcp
                        .candidates
                        .iter()
                        .enumerate()
                        .map(|(i, candidate)| {
                            let detail = match &candidate.server.transport {
                                McpTransport::Stdio { command, args, .. } => {
                                    format!("{command} {}", args.join(" "))
                                }
                                McpTransport::StreamableHttp { url, .. } => url.clone(),
                            };
                            v_flex()
                                .gap_1()
                                .p_2()
                                .child(
                                    Checkbox::new(SharedString::from(format!("import-mcp-{i}")))
                                        .label(format!(
                                            "{} · {}{}",
                                            candidate.server.name,
                                            candidate.source,
                                            if self
                                                .config
                                                .mcp_servers
                                                .iter()
                                                .any(|s| s.name == candidate.server.name)
                                            {
                                                " · 已存在"
                                            } else {
                                                ""
                                            }
                                        ))
                                        .checked(self.mcp_selected[i])
                                        .on_change(cx.listener(move |this, value, _, cx| {
                                            this.mcp_selected[i] = *value;
                                            cx.notify();
                                        })),
                                )
                                .child(div().text_xs().child(detail))
                                .child(div().text_xs().child(candidate.path.display().to_string()))
                        }),
                )
                .child(
                    Switch::new("import-overwrite")
                        .label("覆盖已有同名配置")
                        .checked(self.import_overwrite)
                        .on_change(cx.listener(|this, value, _, cx| {
                            this.import_overwrite = *value;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("import-mcp")
                        .label(format!(
                            "导入 {} 项",
                            self.mcp_selected.iter().filter(|s| **s).count()
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            match merge_mcp(
                                &this.config.mcp_servers,
                                &this.external_mcp.candidates,
                                &this.mcp_selected,
                                this.import_overwrite,
                            ) {
                                Ok(servers) => {
                                    let mut next = this.saved_config.clone();
                                    next.mcp_servers = servers.clone();
                                    match this.config_store.save(&next) {
                                        Ok(()) => {
                                            this.config.mcp_servers = servers;
                                            this.saved_config = next;
                                            this.sync_mcp_editor(window, cx);
                                            this.management_notice =
                                                "MCP 已导入并保存；启用服务后连接。".into();
                                        }
                                        Err(e) => this.management_notice = e.message,
                                    }
                                }
                                Err(e) => this.management_notice = e.message,
                            }
                            cx.notify();
                        })),
                )
            })
    }
    pub(super) fn skill_import_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(settings_label("Codex · Claude · Hermes · Agents", cx))
            .child(
                Button::new("scan-agent-skills")
                    .ghost()
                    .label("扫描 Codex / Claude / Hermes Skills")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let sources = skill_sources(&home(), &workspace_dir());
                        let roots: Vec<_> = sources.iter().map(|s| s.path.clone()).collect();
                        this.external_skills =
                            SkillCatalog::discover(&workspace_dir(), &roots, &[]);
                        this.skill_selected = vec![false; this.external_skills.skills.len()];
                        this.management_notice.clear();
                        cx.notify();
                    })),
            )
            .child(Input::new(&self.skill_path))
            .child(
                Button::new("scan-custom-skills")
                    .ghost()
                    .small()
                    .label("扫描填写的目录")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let path = PathBuf::from(read(&this.skill_path, cx));
                        if path.as_os_str().is_empty() {
                            this.management_notice = "请先填写目录".into();
                        } else {
                            this.external_skills =
                                SkillCatalog::discover(&workspace_dir(), &[path], &[]);
                            this.skill_selected = vec![false; this.external_skills.skills.len()];
                        }
                        cx.notify();
                    })),
            )
            .children(
                self.external_skills
                    .diagnostics
                    .iter()
                    .map(|s| div().text_xs().child(s.clone())),
            )
            .when(!self.external_skills.skills.is_empty(), |v| {
                v.child(
                    Button::new("select-all-skills")
                        .ghost()
                        .small()
                        .label("全选 / 取消全选")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let selected = !this.skill_selected.iter().all(|s| *s);
                            this.skill_selected.fill(selected);
                            cx.notify();
                        })),
                )
                .children(
                    self.external_skills
                        .skills
                        .iter()
                        .enumerate()
                        .map(|(i, skill)| {
                            v_flex()
                                .gap_1()
                                .child(
                                    Checkbox::new(SharedString::from(format!("import-skill-{i}")))
                                        .label(skill.name.clone())
                                        .checked(self.skill_selected[i])
                                        .on_change(cx.listener(move |this, value, _, cx| {
                                            this.skill_selected[i] = *value;
                                            cx.notify();
                                        })),
                                )
                                .child(div().text_xs().child(skill.path.display().to_string()))
                        }),
                )
                .child(
                    Button::new("copy-skills")
                        .label(format!(
                            "复制 {} 个 Skill 到 Hikari",
                            self.skill_selected.iter().filter(|s| **s).count()
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let mut count = 0;
                            let mut errors = Vec::new();
                            for (skill, selected) in
                                this.external_skills.skills.iter().zip(&this.skill_selected)
                            {
                                if !selected {
                                    continue;
                                }
                                match import_skill(
                                    &skill.path,
                                    &workspace_dir().join(".hikari/skills"),
                                ) {
                                    Ok(_) => count += 1,
                                    Err(e) => errors.push(format!("{}：{}", skill.name, e.message)),
                                }
                            }
                            this.refresh_skills(cx);
                            this.management_notice =
                                format!("已导入 {count} 个 Skill\n{}", errors.join("\n"));
                            cx.notify();
                        })),
                )
            })
    }
    pub(super) fn about_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let identity = h_flex()
            .gap_4()
            .items_center()
            .child(portrait(px(64.)))
            .child(
                v_flex()
                    .gap_1()
                    .child(settings_heading("Hikari"))
                    .child(settings_label(
                        format!("v{}", env!("CARGO_PKG_VERSION")),
                        cx,
                    ))
                    .child(div().text_sm().child("有些对话，不必从头开始。")),
            );
        let links = h_flex().gap_2().flex_wrap().children(
            [
                ("repository", "GitHub", "https://github.com/WEP-56/Hikari"),
                ("issues", "反馈", "https://github.com/WEP-56/Hikari/issues"),
                (
                    "releases",
                    "发布记录",
                    "https://github.com/WEP-56/Hikari/releases",
                ),
            ]
            .into_iter()
            .map(|(id, label, url)| {
                Button::new(id)
                    .ghost()
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.open_url(url);
                        this.management_notice.clear();
                    }))
            }),
        );
        let updates = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().child("版本更新"))
                    .child(
                        Button::new("check-update")
                            .label(if self.update_checking {
                                "检查中…"
                            } else {
                                "检查更新"
                            })
                            .disabled(self.update_checking)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let tx = this.settings_tx.clone();
                                this.update_checking = true;
                                this.start_pump(cx);
                                thread::spawn(move || {
                                    let _ = tx.send(SettingsResult::Release(
                                        hikari_runtime::updates::check_release(env!(
                                            "CARGO_PKG_VERSION"
                                        )),
                                    ));
                                });
                                cx.notify();
                            })),
                    ),
            )
            .when(!self.update_notice.is_empty(), |v| {
                v.child(settings_label(self.update_notice.clone(), cx))
            })
            .when(self.release_url.is_some(), |v| {
                let url = self.release_url.clone().unwrap();
                v.child(
                    Button::new("open-update")
                        .ghost()
                        .label("查看发布版本")
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                )
            });
        v_flex()
            .gap_5()
            .child(settings_card(identity, cx))
            .child(links)
            .child(settings_card(updates, cx))
    }
    pub(super) fn drain_settings(&mut self, cx: &mut Context<Self>) {
        while let Ok(result) = self.settings_rx.try_recv() {
            match result {
                SettingsResult::Models { id, result } => {
                    self.model_fetching = false;
                    match result {
                        Ok(ids) => {
                            let count = ids.len();
                            if let Some(profile) =
                                self.config.providers.iter_mut().find(|p| p.id == id)
                            {
                                for id in ids {
                                    if !profile.models.iter().any(|m| m.id == id) {
                                        profile.models.push(ModelMetadata {
                                            id,
                                            ..Default::default()
                                        });
                                    }
                                }
                                self.settings_dirty = true;
                                self.management_notice = format!("已加入 {count} 个模型");
                            }
                        }
                        Err(e) => self.management_notice = e.message,
                    }
                }
                SettingsResult::Release(result) => {
                    self.update_checking = false;
                    match result {
                        Ok(release) => {
                            self.update_notice = if release.newer {
                                format!(
                                    "发现新版本 {}（当前 {}）",
                                    release.tag,
                                    env!("CARGO_PKG_VERSION")
                                )
                            } else {
                                format!(
                                    "当前版本 {}；最新 Release：{}",
                                    env!("CARGO_PKG_VERSION"),
                                    release.tag
                                )
                            };
                            self.release_url = Some(release.url);
                        }
                        Err(e) => {
                            self.update_notice = format!("检查失败：{}", e.message);
                            self.release_url = None;
                        }
                    }
                }
            }
            cx.notify();
        }
    }
}

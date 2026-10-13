use super::*;
use gpui_kit::base::Collapsible;

pub(super) fn settings_heading(title: &'static str) -> impl IntoElement {
    div().text_xl().font_weight(FontWeight::MEDIUM).child(title)
}

pub(super) fn settings_card(content: impl IntoElement, cx: &App) -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_4()
        .p_4()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_lg()
        .child(content)
}

pub(super) fn settings_row(label: &'static str, control: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_4()
        .child(
            div()
                .w(px(112.))
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(control))
}

pub(super) fn settings_label(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

impl Preview {
    pub(super) fn settings_fold(
        &self,
        id: impl Into<String>,
        title: impl Into<SharedString>,
        content: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = id.into();
        let open = self.settings_open.contains(&id);
        let theme = cx.theme().colors.clone();
        let trigger = h_flex()
            .id(SharedString::from(format!("trigger-{id}")))
            .relative()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .py_3()
            .role(Role::Button)
            .aria_expanded(open)
            .tab_index(0)
            .cursor_pointer()
            .hover(|style| style.bg(theme.muted))
            .focus(|style| style.bg(theme.muted))
            .child(title.into())
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small(),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    if !this.settings_open.remove(&id) {
                        this.settings_open.insert(id.clone());
                    }
                    cx.notify();
                }
            }));
        #[cfg(test)]
        let trigger = trigger.child(
            canvas(|_, _, _| (), {
                let test_id = format!("fold-{id}");
                move |bounds, _, _, _| super::chat::tests::record_bounds(test_id.clone(), bounds)
            })
            .absolute()
            .size_full(),
        );
        Collapsible::new()
            .open(open)
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .overflow_hidden()
            .child(trigger)
            .content(div().px_3().pb_3().child(content))
    }
}

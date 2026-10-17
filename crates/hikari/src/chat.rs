//! Presentation state is separate from the original provider transcript.
use super::{Ink, Line, LineBody, Preview, Talk};
use gpui_kit::base::{TextView, TextViewStyle};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::StatefulInteractiveElement as _;
use gpui_kit::*;
use hikari_runtime::AppearanceConfig;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct ChatState {
    pub scroll: ScrollHandle,
    expanded: BTreeMap<String, bool>,
    pub(super) todo_open: bool,
    pub(super) process_open: bool,
    started: Option<(usize, Instant)>,
    elapsed: BTreeMap<usize, Duration>,
}

impl ChatState {
    pub fn near_bottom(&self) -> bool {
        self.scroll.max_offset().y - self.scroll.offset().y.abs() < px(80.)
    }
    pub fn start(&mut self, line: usize) {
        self.started = Some((line, Instant::now()));
        self.scroll.scroll_to_bottom();
    }
    pub fn finish(&mut self) {
        if let Some((line, started)) = self.started.take() {
            self.elapsed.insert(line, started.elapsed());
        }
    }
    fn open(&self, id: &str, default: bool) -> bool {
        self.expanded.get(id).copied().unwrap_or(default)
    }
    fn toggle(&mut self, id: String, default: bool) {
        let open = self.open(&id, default);
        self.expanded.insert(id, !open);
    }

    pub(super) fn toggle_todo(&mut self) {
        self.todo_open = !self.todo_open;
    }
}

/// Keep intermediate prose/reasoning in the process; the latest answer after
/// the last tool remains visible outside the disclosure.
#[derive(Debug)]
struct TurnLayout {
    start: usize,
    user: Option<usize>,
    answer: Option<usize>,
    activity: Vec<usize>,
    notes: Vec<usize>,
    tool_count: usize,
    errors: usize,
    running: bool,
}

fn turn_layouts(lines: &[Line], running: bool) -> Vec<TurnLayout> {
    let mut starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| matches!(line.body, LineBody::User(_)).then_some(i))
        .collect();
    if !lines.is_empty() && starts.first().copied() != Some(0) {
        starts.insert(0, 0);
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(lines.len());
            let user = matches!(lines[start].body, LineBody::User(_)).then_some(start);
            let last = (start..end).rev().find(|&i| match &lines[i].body {
                LineBody::Tool { .. } => true,
                LineBody::Assistant { text, live, .. } => *live || !text.trim().is_empty(),
                _ => false,
            });
            let answer = last.filter(|&i| {
                matches!(&lines[i].body,
            LineBody::Assistant { text, .. } if !text.trim().is_empty())
            });
            let activity = (start..end)
                .filter(|&i| match &lines[i].body {
                    LineBody::Tool { .. } => true,
                    LineBody::Assistant { text, thinking, .. } => {
                        !thinking.is_empty() || (Some(i) != answer && !text.trim().is_empty())
                    }
                    _ => false,
                })
                .collect();
            let notes = (start..end)
                .filter(|&i| matches!(lines[i].body, LineBody::Note(_)))
                .collect();
            let tool_count = lines[start..end]
                .iter()
                .filter(|line| matches!(line.body, LineBody::Tool { .. }))
                .count();
            let errors = lines[start..end]
                .iter()
                .filter(|line| matches!(line.body, LineBody::Tool { error: true, .. }))
                .count();
            TurnLayout {
                start,
                user,
                answer,
                activity,
                notes,
                tool_count,
                errors,
                running: running && n + 1 == starts.len(),
            }
        })
        .collect()
}

impl Preview {
    pub(super) fn conversation(
        &self,
        idx: usize,
        ink: Ink,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let talk = &self.talks[idx];
        let running = talk.turn.is_some();
        turn_layouts(&talk.lines, running)
            .into_iter()
            .map(|turn| {
                let mut children = Vec::new();
                if let Some(user) = turn.user {
                    if let LineBody::User(text) = &talk.lines[user].body {
                        let images: Vec<_> = talk.lines[user]
                            .attachments
                            .iter()
                            .enumerate()
                            .map(|(i, attachment)| {
                                (
                                    format!("user-image-{}-{user}-{i}", talk.id),
                                    attachment.clone(),
                                )
                            })
                            .collect();
                        let mut bubble = div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .min_w_0()
                            .max_w(relative(0.85))
                            .px_4()
                            .py_3()
                            .rounded_xl()
                            .bg(ink.hover);
                        if !images.is_empty() {
                            bubble = bubble.child(crate::attachments_ui::attachment_strip(
                                &images,
                                format!("user-images-{}-{user}", talk.id),
                                cx,
                            ));
                        }
                        children.push(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .child(bubble.child(plain_text(
                                    format!("user-{}-{user}", talk.id),
                                    text,
                                    ink,
                                    &self.config.appearance,
                                )))
                                .into_any_element(),
                        );
                    }
                }
                if !turn.activity.is_empty() || (turn.running && turn.answer.is_none()) {
                    children.push(self.process(idx, &turn, ink, cx));
                    let talk_id = talk.id;
                    let images: Vec<_> = turn
                        .activity
                        .iter()
                        .flat_map(|index| {
                            let index = *index;
                            talk.lines[index].attachments.iter().enumerate().map(
                                move |(i, attachment)| {
                                    (
                                        format!("tool-image-{talk_id}-{index}-{i}"),
                                        attachment.clone(),
                                    )
                                },
                            )
                        })
                        .collect();
                    if !images.is_empty() {
                        children.push(crate::attachments_ui::attachment_strip(
                            &images,
                            format!("tool-images-{}-{}", talk.id, turn.start),
                            cx,
                        ));
                    }
                }
                if let Some(answer) = turn.answer {
                    if let LineBody::Assistant { text, live, .. } = &talk.lines[answer].body {
                        let mut body = v_flex().w_full().min_w_0().gap_2().child(rich_text(
                            format!("answer-{}-{answer}", talk.id),
                            text,
                            ink,
                            &self.config.appearance,
                        ));
                        if !live && !turn.running {
                            body = body.child(h_flex().child(copy_button(
                                format!("copy-answer-{}-{answer}", talk.id),
                                text.clone(),
                                ink,
                            )));
                        }
                        children.push(body.into_any_element());
                    }
                }
                for note in turn.notes {
                    if let LineBody::Note(text) = &talk.lines[note].body {
                        if super::quiet_status(text) {
                            continue;
                        }
                        children.push(
                            div()
                                .text_sm()
                                .text_color(ink.muted)
                                .child(text.clone())
                                .into_any_element(),
                        );
                    }
                }
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_5()
                    .children(children)
                    .into_any_element()
            })
            .collect()
    }

    fn disclosure(
        &self,
        talk: &Talk,
        key: String,
        label: String,
        icon: IconName,
        default: bool,
        ink: Ink,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = talk.view.open(&key, default);
        let talk_id = talk.id;
        #[cfg(test)]
        let test_key = key.clone();
        let row = h_flex()
            .id(SharedString::from(key.clone()))
            .w_full()
            .min_w_0()
            .relative()
            .role(Role::Button)
            .aria_label(label.clone())
            .aria_expanded(open)
            .tab_index(0)
            .py(px(2.))
            .gap_2()
            .items_center()
            .text_sm()
            .line_height(px(20.))
            .text_color(ink.muted)
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.text_color(ink.fg).bg(ink.hover))
            .focus(|s| s.bg(ink.hover))
            .child(Icon::new(icon).small().flex_shrink_0())
            .child(div().min_w_0().truncate().child(label))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .flex_shrink_0(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(talk) = this.talks.iter_mut().find(|t| t.id == talk_id) {
                    talk.view.toggle(key.clone(), default);
                }
                cx.notify();
            }));
        #[cfg(test)]
        let row = row.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, _, _| {
                    tests::record_bounds(test_key.clone(), bounds);
                },
            )
            .absolute()
            .size_full(),
        );
        row.into_any_element()
    }

    fn process(
        &self,
        idx: usize,
        turn: &TurnLayout,
        ink: Ink,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let talk = &self.talks[idx];
        let key = format!("process-{}-{}", talk.id, turn.start);
        let default = turn.running || turn.errors > 0;
        let open = talk.view.open(&key, default);
        let elapsed = talk.view.elapsed.get(&turn.start).copied().or_else(|| {
            talk.view
                .started
                .filter(|(start, _)| *start == turn.start)
                .map(|(_, time)| time.elapsed())
        });
        let mut label = if turn.running {
            "正在处理".to_string()
        } else if let Some(elapsed) = elapsed {
            format!("用时 {}", duration_label(elapsed))
        } else {
            "执行过程".to_string()
        };
        if turn.tool_count > 0 {
            label.push_str(&format!(" · {} 次工具调用", turn.tool_count));
        }
        if turn.errors > 0 {
            label.push_str(&format!(" · {} 项失败", turn.errors));
        }
        let status_ink = if turn.errors > 0 {
            Ink {
                muted: error_color(self.dark),
                ..ink
            }
        } else {
            ink
        };
        let mut process = v_flex().w_full().min_w_0().gap_2().child(self.disclosure(
            talk,
            key,
            label,
            if turn.errors > 0 {
                IconName::Info
            } else if turn.running {
                IconName::Loader
            } else {
                IconName::Check
            },
            default,
            status_ink,
            cx,
        ));
        if open {
            let mut rows = Vec::new();
            let mut position = 0;
            while position < turn.activity.len() {
                let line_idx = turn.activity[position];
                if matches!(talk.lines[line_idx].body, LineBody::Tool { .. }) {
                    let start = position;
                    while position < turn.activity.len()
                        && matches!(
                            talk.lines[turn.activity[position]].body,
                            LineBody::Tool { .. }
                        )
                    {
                        position += 1;
                    }
                    rows.push(self.tool_group(
                        idx,
                        &turn.activity[start..position],
                        turn.running,
                        ink,
                        cx,
                    ));
                    continue;
                }
                if let LineBody::Assistant { text, thinking, .. } = &talk.lines[line_idx].body {
                    let mut message = v_flex().w_full().min_w_0().gap_2();
                    if !thinking.is_empty() {
                        let key = format!("thinking-{}-{line_idx}", talk.id);
                        let mut reasoning =
                            v_flex().w_full().min_w_0().gap_2().child(self.disclosure(
                                talk,
                                key.clone(),
                                "思考过程".into(),
                                IconName::Info,
                                false,
                                ink,
                                cx,
                            ));
                        if talk.view.open(&key, false) {
                            let muted = Ink {
                                fg: ink.muted,
                                ..ink
                            };
                            reasoning =
                                reasoning.child(
                                    div().pl_4().border_l_1().border_color(ink.line).child(
                                        rich_text(
                                            format!("reasoning-{}-{line_idx}", talk.id),
                                            thinking,
                                            muted,
                                            &self.config.appearance,
                                        ),
                                    ),
                                );
                        }
                        message = message.child(reasoning);
                    }
                    if Some(line_idx) != turn.answer && !text.trim().is_empty() {
                        message = message.child(rich_text(
                            format!("commentary-{}-{line_idx}", talk.id),
                            text,
                            ink,
                            &self.config.appearance,
                        ));
                    }
                    rows.push(message.into_any_element());
                }
                position += 1;
            }
            process = process.child(v_flex().w_full().min_w_0().gap_1().children(rows));
        }
        process
            .border_b_1()
            .border_color(ink.line)
            .pb_2()
            .into_any_element()
    }

    fn tool_group(
        &self,
        idx: usize,
        indices: &[usize],
        running: bool,
        ink: Ink,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let talk = &self.talks[idx];
        let key = format!("tools-{}-{}", talk.id, indices[0]);
        let failed = indices
            .iter()
            .any(|&i| matches!(talk.lines[i].body, LineBody::Tool { error: true, .. }));
        let default = running || failed;
        let mut group = v_flex().w_full().min_w_0().gap_1();
        if indices.len() > 1 {
            let label = format!(
                "{} · {} 项",
                tool_group_label(&talk.lines, indices),
                indices.len()
            );
            group = group.child(self.disclosure(
                talk,
                key.clone(),
                label,
                IconName::SquareTerminal,
                default,
                ink,
                cx,
            ));
        }
        if indices.len() == 1 || talk.view.open(&key, default) {
            let mut list = v_flex()
                .id(SharedString::from(format!(
                    "tool-list-{}-{}",
                    talk.id, indices[0]
                )))
                .w_full()
                .min_w_0()
                .max_h(px(360.))
                .overflow_y_scroll()
                .gap_1();
            for &line_idx in indices {
                list = list.child(self.tool_row(idx, line_idx, ink, cx));
            }
            group = group.child(list);
        }
        group.into_any_element()
    }

    fn tool_row(
        &self,
        idx: usize,
        line_idx: usize,
        ink: Ink,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let talk = &self.talks[idx];
        let LineBody::Tool {
            id,
            name,
            args,
            detail,
            running,
            error,
        } = &talk.lines[line_idx].body
        else {
            return div().into_any_element();
        };
        let key = format!("tool-{}-{id}-{line_idx}", talk.id);
        // 失败的调用也保持收起，需要看输出时手动展开即可。
        let open = talk.view.open(&key, false);
        let presentation = tool_presentation(name, args);
        let status = if *running {
            "正在"
        } else if *error {
            "失败 ·"
        } else {
            "已"
        };
        let label = format!("{status}{} {}", presentation.action, presentation.target);
        let color = if *error {
            Ink {
                muted: error_color(self.dark),
                ..ink
            }
        } else {
            ink
        };
        let mut row = v_flex().w_full().min_w_0().gap_2().child(self.disclosure(
            talk,
            key,
            label,
            presentation.icon,
            false,
            color,
            cx,
        ));
        if open {
            let mut detail_view = v_flex()
                .w_full()
                .min_w_0()
                .p_3()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(if *error {
                    error_color(self.dark)
                } else {
                    ink.line
                })
                .bg(ink.input)
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().text_xs().text_color(ink.muted).child(name.clone()))
                        .child(copy_button(
                            format!("copy-tool-{}-{line_idx}", talk.id),
                            detail.clone(),
                            ink,
                        )),
                );
            if !args.is_empty() {
                let pretty = serde_json::from_str::<serde_json::Value>(args)
                    .and_then(|args| serde_json::to_string_pretty(&args))
                    .unwrap_or_else(|_| args.to_string());
                detail_view = detail_view
                    .child(div().text_xs().text_color(ink.muted).child("参数"))
                    .child(raw_text(
                        format!("args-{}-{line_idx}", talk.id),
                        &pretty,
                        ink,
                        &self.config.appearance,
                    ));
            }
            if !detail.is_empty() {
                detail_view = detail_view
                    .child(div().text_xs().text_color(ink.muted).child(if *error {
                        "错误输出"
                    } else {
                        "输出"
                    }))
                    .child(raw_text(
                        format!("output-{}-{line_idx}", talk.id),
                        detail,
                        ink,
                        &self.config.appearance,
                    ));
            } else {
                detail_view =
                    detail_view.child(div().text_sm().text_color(ink.muted).child(if *running {
                        "等待工具返回…"
                    } else {
                        "执行完成，无输出"
                    }));
            }
            row = row.child(detail_view);
        }
        row.into_any_element()
    }
}

struct ToolPresentation {
    action: &'static str,
    icon: IconName,
    target: String,
}

fn tool_presentation(name: &str, args: &str) -> ToolPresentation {
    let value = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let lower = name.to_lowercase();
    let (action, icon, target) =
        if matches!(lower.as_str(), "bash" | "shell" | "exec" | "exec_command")
            || lower.ends_with("_exec_command")
        {
            ("运行", IconName::SquareTerminal, first(&["command", "cmd"]))
        } else if lower == "read" || lower == "read_file" || lower.ends_with("_read_file") {
            (
                "读取",
                IconName::File,
                first(&["path", "file_path", "file"]),
            )
        } else if matches!(
            lower.as_str(),
            "write" | "edit" | "write_file" | "edit_file" | "apply_patch"
        ) {
            (
                "修改",
                IconName::File,
                first(&["path", "file_path", "file"]),
            )
        } else if matches!(
            lower.as_str(),
            "grep" | "find" | "ls" | "search" | "web_search"
        ) || lower.contains("search")
        {
            (
                "搜索",
                IconName::Search,
                first(&["pattern", "query", "path"]),
            )
        } else if lower.contains("navigate") || lower.contains("fetch") || lower.contains("browse")
        {
            ("访问", IconName::Globe, first(&["url", "uri"]))
        } else {
            (
                "调用",
                IconName::SquareTerminal,
                name.strip_prefix("mcp_").unwrap_or(name).to_string(),
            )
        };
    ToolPresentation {
        action,
        icon,
        target: compact(if target.is_empty() { name } else { &target }, 100),
    }
}

fn tool_group_label(lines: &[Line], indices: &[usize]) -> &'static str {
    let actions: Vec<_> = indices
        .iter()
        .filter_map(|&i| {
            if let LineBody::Tool { name, args, .. } = &lines[i].body {
                Some(tool_presentation(name, args).action)
            } else {
                None
            }
        })
        .collect();
    if actions.iter().all(|&a| a == "读取") {
        "读取文件"
    } else if actions.iter().all(|&a| a == "运行") {
        "运行命令"
    } else if actions.iter().all(|&a| a == "搜索") {
        "搜索内容"
    } else {
        "工具调用"
    }
}

fn compact(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        format!("{} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!("{seconds} 秒")
    }
}

fn error_color(dark: bool) -> Hsla {
    if dark {
        rgb(0xf28b82).into()
    } else {
        rgb(0xb42318).into()
    }
}

fn rich_style(ink: Ink, appearance: &AppearanceConfig) -> TextViewStyle {
    let size = appearance.text_size;
    let mut code = StyleRefinement::default()
        .font_family(appearance.code_font.clone())
        .text_size(px(appearance.code_size))
        .line_height(relative(1.6))
        .rounded_lg()
        .pt(px(38.))
        .max_h(px(360.));
    code.overflow.y = Some(Overflow::Scroll);
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);
    TextViewStyle::default()
        .with_foreground(ink.fg)
        .with_muted_foreground(ink.muted)
        .with_link(if ink.bg.l < 0.5 {
            rgb(0x79aef5).into()
        } else {
            rgb(0x245fc2).into()
        })
        .with_border(ink.line)
        .with_code_background(ink.input)
        .with_selection(hsla(215. / 360., 0.8, 0.6, 0.3))
        .with_dark(ink.bg.l < 0.5)
        .with_paragraph_gap(rems(0.65))
        .with_heading(move |level| {
            StyleRefinement::default()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(size
                    * match level {
                        1 => 1.6,
                        2 => 1.35,
                        3 => 1.15,
                        _ => 1.0,
                    }))
        })
        .with_inline_code(HighlightStyle {
            background_color: Some(ink.hover),
            ..Default::default()
        })
        .with_code_block(code)
        .with_table(table)
        .with_table_head(
            StyleRefinement::default()
                .bg(ink.hover)
                .font_weight(FontWeight::SEMIBOLD),
        )
}

fn rich_text(id: String, text: &str, ink: Ink, appearance: &AppearanceConfig) -> TextView {
    TextView::markdown(SharedString::from(id), text.to_string())
        .w_full()
        .min_w_0()
        .font_family(appearance.text_font.clone())
        .text_size(px(appearance.text_size))
        .line_height(relative(appearance.line_height))
        .style(rich_style(ink, appearance))
        .selectable(true)
        .code_block_actions(move |block, _, _| {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(ink.muted)
                        .child(block.lang().unwrap_or_else(|| "代码".into())),
                )
                .child(copy_button("copy-code".into(), block.code(), ink))
        })
}

/// Escape literal user text before placing it in the selectable view.
fn plain_text(id: String, text: &str, ink: Ink, appearance: &AppearanceConfig) -> TextView {
    let html = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>");
    TextView::html(SharedString::from(id), html)
        .w_full()
        .min_w_0()
        .font_family(appearance.text_font.clone())
        .text_size(px(appearance.text_size))
        .line_height(relative(appearance.line_height))
        .style(rich_style(ink, appearance))
        .selectable(true)
}

/// Tool output stays literal, including embedded Markdown fences and HTML.
fn raw_text(id: String, text: &str, ink: Ink, appearance: &AppearanceConfig) -> TextView {
    let mut code = StyleRefinement::default()
        .font_family(appearance.code_font.clone())
        .text_size(px(appearance.code_size))
        .line_height(relative(1.5))
        .rounded_md()
        .max_h(px(240.))
        .border_0()
        .p_2();
    code.overflow.y = Some(Overflow::Scroll);
    let style = rich_style(ink, appearance).with_code_block(code);
    TextView::markdown(SharedString::from(id), literal_code(text))
        .w_full()
        .min_w_0()
        .style(style)
        .selectable(true)
}

pub(super) fn literal_code(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

fn copy_button(id: String, text: SharedString, ink: Ink) -> Button {
    Button::new(SharedString::from(id))
        .ghost()
        .small()
        .icon(IconName::Copy)
        .text_color(ink.muted)
        .tooltip("复制")
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()))
        })
}

#[cfg(test)]
#[path = "chat_tests.rs"]
pub(super) mod tests;

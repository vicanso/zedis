// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Multi-statement execution results pane (`Text` / `Table` / `JSON`) for [`ZedisWorkbench`].

use super::ZedisWorkbench;
use super::tab::{WorkbenchStatementResult, WorkbenchTab};
use crate::connection::ReplyFormat;
use crate::helpers::get_mono_font_family;
use crate::states::{i18n_common, i18n_workbench};
use gpui::{AnyElement, ClipboardItem, Context, SharedString, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};

fn format_label(format: ReplyFormat, cx: &gpui::App) -> SharedString {
    match format {
        ReplyFormat::Text => i18n_workbench(cx, "results_text"),
        ReplyFormat::Table => i18n_workbench(cx, "results_table"),
        ReplyFormat::Json => i18n_workbench(cx, "results_json"),
    }
}

fn render_statement_output(stmt: &WorkbenchStatementResult, format: ReplyFormat) -> String {
    match &stmt.outcome {
        Ok(reply) => reply.render(format),
        Err(err) => err.to_string(),
    }
}

impl ZedisWorkbench {
    pub(super) fn copy_all_results(&self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.results.is_empty() {
            return;
        }
        let mut out = String::new();
        for stmt in &tab.results {
            out.push_str("> ");
            out.push_str(&stmt.command);
            out.push('\n');
            out.push_str(&render_statement_output(stmt, tab.reply_format));
            out.push_str("\n\n");
        }
        cx.write_to_clipboard(ClipboardItem::new_string(out));
        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    pub(super) fn render_results_pane(&self, tab: &WorkbenchTab, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let green = theme.green;
        let danger = theme.danger;
        let card_bg = theme.muted.opacity(0.3);
        let reply_format = tab.reply_format;
        let has_results = !tab.results.is_empty();

        let toolbar = h_flex()
            .w_full()
            .h(px(34.))
            .px_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_b_1()
            .border_color(border)
            .child(
                ButtonGroup::new("workbench-reply-format")
                    .compact()
                    .xsmall()
                    .outline()
                    .children(ReplyFormat::ALL.into_iter().enumerate().map(|(ix, fmt)| {
                        Button::new(("workbench-format", ix))
                            .label(format_label(fmt, cx))
                            .selected(fmt == reply_format)
                    }))
                    .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                        if let Some(fmt) = clicks.first().and_then(|ix| ReplyFormat::ALL.get(*ix))
                            && let Some(tab) = this.tabs.get_mut(this.active_tab)
                        {
                            tab.reply_format = *fmt;
                            cx.notify();
                        }
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("workbench-copy-results")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .disabled(!has_results)
                            .on_click(cx.listener(|this, _, window, cx| this.copy_all_results(window, cx))),
                    )
                    .child(
                        Button::new("workbench-clear-results")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .disabled(!has_results)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                                    tab.results.clear();
                                    cx.notify();
                                }
                            })),
                    ),
            );

        let body: AnyElement = if tab.results.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_center()
                .justify_center()
                .child(
                    Label::new(i18n_workbench(cx, "empty_results"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element()
        } else {
            let cards: Vec<AnyElement> = tab
                .results
                .iter()
                .enumerate()
                .map(|(ix, stmt)| {
                    let is_ok = stmt.outcome.is_ok();
                    let status_text = if is_ok {
                        i18n_workbench(cx, "statement_ok")
                    } else {
                        i18n_workbench(cx, "statement_err")
                    };
                    let status_color = if is_ok { green } else { danger };
                    let rendered = render_statement_output(stmt, reply_format);
                    let copy_text = rendered.clone();
                    let mono = get_mono_font_family();

                    v_flex()
                        .w_full()
                        .p_2()
                        .gap_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .bg(card_bg)
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            Label::new(status_text)
                                                .text_xs()
                                                .font_family(mono.clone())
                                                .text_color(status_color),
                                        )
                                        .child(
                                            Label::new(format!("> {}", stmt.command))
                                                .text_xs()
                                                .font_family(mono.clone())
                                                .text_ellipsis(),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            Label::new(format!("{} ms", stmt.elapsed_ms))
                                                .text_xs()
                                                .text_color(muted),
                                        )
                                        .child(
                                            Button::new(("workbench-copy-stmt", ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Copy)
                                                .on_click(move |_, window, cx| {
                                                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                                    window.push_notification(
                                                        Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                                        cx,
                                                    );
                                                }),
                                        ),
                                ),
                        )
                        .child(Label::new(rendered).text_xs().font_family(mono).text_color(if is_ok {
                            cx.theme().foreground
                        } else {
                            danger
                        }))
                        .into_any_element()
                })
                .collect();

            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_3()
                .gap_2()
                .overflow_y_scrollbar()
                .children(cards)
                .into_any_element()
        };

        v_flex().flex_1().min_h_0().w_full().child(toolbar).child(body)
    }
}

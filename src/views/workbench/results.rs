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
use super::tab::WorkbenchStatementResult;
use crate::connection::ReplyFormat;
use crate::helpers::get_mono_font_family;
use crate::states::{i18n_common, i18n_workbench};
use gpui::{AnyElement, ClipboardItem, Context, SharedString, Window, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, RopeExt},
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

    pub(super) fn render_results_pane(
        &mut self,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(tab) = self.tabs.get(tab_ix) else {
            return v_flex().into_any_element();
        };
        let tab_id = tab.id;
        let reply_format = tab.reply_format;
        let is_empty = tab.results.is_empty();
        let items: Vec<(bool, SharedString, u128, String)> = tab
            .results
            .iter()
            .map(|stmt| {
                (
                    stmt.outcome.is_ok(),
                    stmt.command.clone(),
                    stmt.elapsed_ms,
                    render_statement_output(stmt, reply_format),
                )
            })
            .collect();

        let theme = cx.theme();
        let border = theme.border;
        let radius = theme.radius;
        let muted = theme.muted_foreground;
        let green = theme.green;
        let danger = theme.danger;
        let card_bg = theme.muted.opacity(0.3);
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
                            .tooltip(i18n_common(cx, "copy"))
                            .disabled(!has_results)
                            .on_click(cx.listener(|this, _, window, cx| this.copy_all_results(window, cx))),
                    )
                    .child(
                        Button::new("workbench-clear-results")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .tooltip(i18n_common(cx, "clear"))
                            .disabled(!has_results)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                                    tab.results.clear();
                                    cx.notify();
                                }
                            })),
                    ),
            );

        let body: AnyElement = if is_empty {
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
            let mono = get_mono_font_family();
            let cards: Vec<AnyElement> = items
                .into_iter()
                .enumerate()
                .map(|(ix, (is_ok, command, elapsed_ms, rendered))| {
                    let status_text = if is_ok {
                        i18n_workbench(cx, "statement_ok")
                    } else {
                        i18n_workbench(cx, "statement_err")
                    };
                    let status_color = if is_ok { green } else { danger };
                    let copy_text = rendered.clone();
                    let editor = {
                        let entry = self.result_editors.entry((tab_id, ix)).or_insert_with(|| {
                            let state = cx.new(|cx| {
                                EditorState::new(window, cx)
                                    .line_number(false)
                                    .folding(false)
                                    .soft_wrap(true)
                                    .show_whitespaces(false)
                            });
                            (String::new(), state)
                        });
                        if entry.0 != rendered {
                            entry.0 = rendered.clone();
                            let text: SharedString = rendered.into();
                            entry.1.update(cx, |s, cx| {
                                s.set_value(text, window, cx);
                            });
                        }
                        entry.1.clone()
                    };
                    let state_ref = editor.read(cx);
                    let rows = state_ref.text().lines_len().clamp(1, 16);
                    let row_height = state_ref.line_height().unwrap_or(px(18.));

                    v_flex()
                        .w_full()
                        .p_2()
                        .gap_1()
                        .rounded(radius)
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
                                            Label::new(format!("> {command}"))
                                                .text_xs()
                                                .font_family(mono.clone())
                                                .text_ellipsis(),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Label::new(format!("{elapsed_ms} ms")).text_xs().text_color(muted))
                                        .child(
                                            Button::new(("workbench-copy-stmt", ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Copy)
                                                .tooltip(i18n_common(cx, "copy"))
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
                        .child(
                            Editor::new(&editor)
                                .h(row_height * rows as f32)
                                .w_full()
                                .readonly(true)
                                .bordered(false)
                                .appearance(false)
                                .p_0()
                                .font_family(mono.clone()),
                        )
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

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }
}

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

//! Header bars for `ZedisBytesEditor`: Hex Dump/Edit switcher, JSON Text/Tree
//! switcher, and the collapsible JSONPath query bar + result pane.

use super::*;

impl ZedisBytesEditor {
    /// Hex Dump vs Hex Edit switch shown above the editor when Hex mode is on.
    pub(super) fn render_hex_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        let editing = self.hex_editing;
        let unsaved = editing && self.value_modified;
        h_flex()
            .w_full()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .justify_end()
            .child(
                ButtonGroup::new("hex-view-switch")
                    .compact()
                    .small()
                    .outline()
                    .child(
                        Button::new("hex-view-dump")
                            .label(i18n_editor(cx, "hex_view_dump"))
                            .selected(!editing)
                            .disabled(unsaved)
                            .when(unsaved, |button| button.tooltip(i18n_editor(cx, "hex_view_unsaved"))),
                    )
                    .child(
                        Button::new("hex-view-edit")
                            .label(i18n_editor(cx, "hex_view_edit"))
                            .selected(editing)
                            .disabled(!self.hex_editable),
                    )
                    .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                        if let Some(ix) = clicks.first() {
                            this.set_hex_editing(*ix == 1, cx);
                        }
                    })),
            )
    }

    pub(super) fn render_json_view_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        let tree_mode = self.json_tree_mode;
        ButtonGroup::new("json-view-switch")
            .compact()
            .small()
            .outline()
            .child(
                Button::new("json-view-text")
                    .label(i18n_editor(cx, "json_view_text"))
                    .selected(!tree_mode),
            )
            .child(
                Button::new("json-view-tree")
                    .label(i18n_editor(cx, "json_view_tree"))
                    .selected(tree_mode),
            )
            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                if let Some(ix) = clicks.first() {
                    this.set_json_tree_mode(*ix == 1, cx);
                }
            }))
    }

    pub(super) fn render_jsonpath_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .key_context("JsonPathBar")
            .on_action(cx.listener(|this, _: &JsonPathAction, window, cx| {
                let accepted = this.jsonpath_input.update(cx, |state, cx| {
                    EditorMode::handle_context_menu_action(
                        state,
                        Box::new(Enter {
                            secondary: false,
                            shift: false,
                        }),
                        window,
                        cx,
                    )
                });
                if accepted {
                    cx.stop_propagation();
                }
            }))
            .child(
                Button::new("jsonpath-collapse")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronDown)
                    .label(i18n_editor(cx, "jsonpath_label"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.jsonpath_open = false;
                        cx.notify();
                    })),
            )
            .child(
                Editor::new(&self.jsonpath_input)
                    .h(px(32.))
                    .flex_1()
                    .text_sm()
                    .line_height(relative(1.))
                    .font_family(get_mono_font_family()),
            )
            .child(
                Button::new("jsonpath-run")
                    .small()
                    .outline()
                    .label(i18n_editor(cx, "jsonpath_run"))
                    .on_click(cx.listener(|this, _, window, cx| this.run_jsonpath_query(window, cx))),
            )
            .child(self.render_json_view_switch(cx))
    }

    /// Slim collapsed-state row that stands in for the JSONPath bar: a single
    /// ghost toggle that expands the full query bar on click. Keeps the query
    /// affordance discoverable without spending a full input row at rest.
    pub(super) fn render_jsonpath_toggle(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .justify_between()
            .child(
                Button::new("jsonpath-expand")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .label(i18n_editor(cx, "jsonpath_label"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.jsonpath_open = true;
                        this.jsonpath_input.update(cx, |state, cx| state.focus(window, cx));
                        cx.notify();
                    })),
            )
            .child(self.render_json_view_switch(cx))
    }

    /// Render the JSONPath result. Two shapes:
    ///  * For Single/Multiple ⇒ embed the read-only code editor so the user
    ///    can scroll, search and copy from large JSON payloads.
    ///  * For status outcomes (NoMatch / errors) ⇒ a single-line label is
    ///    plenty.
    pub(super) fn render_jsonpath_result(&self, outcome: JsonPathOutcome, cx: &Context<Self>) -> gpui::AnyElement {
        let container = div()
            .w_full()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border);

        match outcome {
            JsonPathOutcome::Single(_) | JsonPathOutcome::Multiple(_) => {
                let state = self.jsonpath_result_editor.read(cx);
                let rows = state
                    .text()
                    .lines_len()
                    .clamp(JSONPATH_RESULT_MIN_ROWS, JSONPATH_RESULT_MAX_ROWS);
                let row_height = state.line_height().unwrap_or(px(DEFAULT_EDITOR_ROW_HEIGHT));
                container
                    .child(
                        Editor::new(&self.jsonpath_result_editor)
                            .h(row_height * rows as f32)
                            .bordered(false)
                            .appearance(false)
                            .p_0()
                            .w_full()
                            .font_family(get_mono_font_family()),
                    )
                    .into_any_element()
            }
            other => {
                let (text, color) = match other {
                    JsonPathOutcome::NotJson => (i18n_editor(cx, "jsonpath_not_json"), cx.theme().red),
                    JsonPathOutcome::InvalidPath(msg) => {
                        (SharedString::from(format!("Path error: {msg}")), cx.theme().red)
                    }
                    JsonPathOutcome::NoMatch => (i18n_editor(cx, "jsonpath_no_match"), cx.theme().muted_foreground),
                    _ => unreachable!("Single/Multiple handled in the matching arm above"),
                };
                container
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Label::new(text)
                            .font_family(get_mono_font_family())
                            .text_xs()
                            .text_color(color)
                            .whitespace_normal(),
                    )
                    .into_any_element()
            }
        }
    }
}

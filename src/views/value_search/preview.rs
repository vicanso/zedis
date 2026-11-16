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

//! Inline right-pane preview for a selected value-search match: formats JSON
//! containers into an indented code-editor / tree view and renders plain or
//! collection previews in the read-only editor.

use super::*;
use crate::states::{JsonView, SOFT_WRAP_MAX_BYTES, i18n_editor, soft_wrap_fits};
use crate::views::JsonTreeTarget;
use gpui_kit::component::{
    Selectable,
    button::ButtonGroup,
    input::{Editor, EditorState, TabSize},
};
use serde_json::Value;

const DEFAULT_TAB_SIZE: usize = 2;
const JSON_LANGUAGE: &str = "json";
const PLAIN_LANGUAGE: &str = "text";

/// Maximum bytes of a value rendered in the preview editor before truncating.
/// Matches `SOFT_WRAP_MAX_BYTES` (and the web build's highlight cap) so every
/// preview can be highlighted and wrapped without stalling a frame.
pub(super) const PREVIEW_MAX_BYTES: usize = SOFT_WRAP_MAX_BYTES;

/// Formatted value payload ready for the preview editor (and optional JSON tree).
#[derive(Clone, Debug)]
pub(super) struct PreviewPayload {
    pub(super) text: SharedString,
    /// Parsed JSON container when `text` is complete (never populated from a
    /// truncated payload so the tree view never renders partial JSON).
    pub(super) json_doc: Option<Value>,
    pub(super) is_json: bool,
}

/// State of the inline value preview for the selected match.
pub(super) enum Preview {
    Loading,
    Value(PreviewPayload),
    Error(SharedString),
}

pub(super) fn new_preview_editor(soft_wrap: bool, window: &mut Window, cx: &mut Context<EditorState>) -> EditorState {
    EditorState::new(window, cx)
        .language(JSON_LANGUAGE)
        .line_number(true)
        .indent_guides(true)
        .tab_size(TabSize {
            tab_size: DEFAULT_TAB_SIZE,
            hard_tabs: false,
        })
        .searchable(true)
        .soft_wrap(soft_wrap)
}

pub(super) fn opens_as_tree(cx: &App) -> bool {
    cx.global::<ZedisGlobalStore>().read(cx).json_view() == JsonView::Tree
}

fn try_parse_json_container(raw: &str) -> Option<(String, Value)> {
    let trimmed = raw.trim();
    if !((trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']'))) {
        return None;
    }
    let doc: Value = serde_json::from_str(trimmed).ok()?;
    let pretty = serde_json::to_string_pretty(&doc).ok()?;
    Some((pretty, doc))
}

fn truncate_preview(mut text: String) -> (SharedString, bool) {
    if text.len() > PREVIEW_MAX_BYTES {
        let mut idx = PREVIEW_MAX_BYTES;
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        text.truncate(idx);
        text.push_str("\n… (truncated)");
        return (text.into(), true);
    }
    (text.into(), false)
}

/// Prepare `raw` for the preview pane: pretty-prints JSON objects/arrays and
/// retains their parsed DOM when unclipped so both the code editor and the
/// JSON tree view work like the main string editor.
pub(super) fn prepare_preview_payload(raw: String) -> PreviewPayload {
    if raw.len() <= MAX_VALUE_BYTES as usize
        && let Some((pretty, doc)) = try_parse_json_container(&raw)
    {
        let (text, was_truncated) = truncate_preview(pretty);
        return PreviewPayload {
            text,
            json_doc: (!was_truncated).then_some(doc),
            is_json: true,
        };
    }
    let (text, _) = truncate_preview(raw);
    PreviewPayload {
        text,
        json_doc: None,
        is_json: false,
    }
}

impl ZedisValueSearch {
    /// Preview a matched key's value inline (right pane), without leaving.
    pub(super) fn select_result(
        &mut self,
        key: SharedString,
        key_type: SharedString,
        location: MatchLocation,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(key.clone());
        self.selected_type = Some(key_type);
        self.selected_location = Some(location);
        self.preview_json_tree_mode = opens_as_tree(cx);
        self.preview = Some(Preview::Loading);
        self.preview_pending_sync = false;
        cx.notify();
        self.load_preview(key, cx);
    }

    /// Read `key`'s value into the preview pane, formatting JSON off the UI
    /// thread before handing it to the editor and tree.
    pub(super) fn load_preview(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let fetched = value_preview(&ServerDb::new(&*server_id, db), &key).await;
            let prepared = match fetched {
                Ok(raw) => Ok(cx.background_spawn(async move { prepare_preview_payload(raw) }).await),
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |this, cx| {
                // Ignore stale previews if the user already clicked another row.
                if this.selected.as_deref() != Some(key.as_ref()) {
                    return;
                }
                match prepared {
                    Ok(payload) => this.apply_preview_payload(payload, cx),
                    Err(e) => {
                        this.preview = Some(Preview::Error(e.to_string().into()));
                        this.preview_pending_sync = false;
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn apply_preview_payload(&mut self, payload: PreviewPayload, cx: &mut Context<Self>) {
        let doc = payload.json_doc.clone();
        self.preview_json_tree
            .update(cx, |tree, cx| tree.set_document(doc, JsonTreeTarget::Local, false, cx));
        self.preview = Some(Preview::Value(payload));
        self.preview_pending_sync = true;
    }

    fn set_preview_json_tree_mode(&mut self, tree: bool, cx: &mut Context<Self>) {
        if self.preview_json_tree_mode == tree {
            return;
        }
        self.preview_json_tree_mode = tree;
        cx.notify();
    }

    pub(super) fn copy_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Preview::Value(payload)) = &self.preview else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(payload.text.to_string()));
        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    pub(super) fn copy_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = &self.selected else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
    }

    fn sync_preview_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview_pending_sync {
            return;
        }
        self.preview_pending_sync = false;
        let Some(Preview::Value(payload)) = &self.preview else {
            return;
        };
        let text = payload.text.clone();
        let language = if payload.is_json { JSON_LANGUAGE } else { PLAIN_LANGUAGE };
        let wrap = self.preview_soft_wrap && soft_wrap_fits(text.len());
        self.preview_editor.update(cx, move |editor, cx| {
            editor.set_highlighter(language, cx);
            if !wrap {
                editor.set_soft_wrap(false, window, cx);
            }
            editor.set_value(text, window, cx);
            if wrap {
                editor.set_soft_wrap(true, window, cx);
            }
        });
    }

    fn render_json_view_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        let tree_mode = self.preview_json_tree_mode;
        ButtonGroup::new("vs-json-view-switch")
            .compact()
            .small()
            .outline()
            .child(
                Button::new("vs-json-view-text")
                    .label(i18n_editor(cx, "json_view_text"))
                    .selected(!tree_mode),
            )
            .child(
                Button::new("vs-json-view-tree")
                    .label(i18n_editor(cx, "json_view_tree"))
                    .selected(tree_mode),
            )
            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                if let Some(ix) = clicks.first() {
                    this.set_preview_json_tree_mode(*ix == 1, cx);
                }
            }))
    }

    pub(super) fn render_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.sync_preview_editor(window, cx);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        match &self.preview {
            None => div()
                .size_full()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Label::new(i18n_value_search(cx, "preview_hint"))
                        .text_sm()
                        .text_color(muted)
                        .text_center(),
                )
                .into_any_element(),
            Some(Preview::Loading) => div()
                .p_4()
                .child(Label::new(i18n_value_search(cx, "loading")).text_sm().text_color(muted))
                .into_any_element(),
            Some(Preview::Error(e)) => div()
                .p_4()
                .child(Label::new(e.clone()).text_sm().text_color(cx.theme().danger))
                .into_any_element(),
            Some(Preview::Value(payload)) => {
                let has_json_tree = payload.json_doc.is_some();
                let show_tree = has_json_tree && self.preview_json_tree_mode;
                let key = self.selected.clone().unwrap_or_default();
                let key_open = key.clone();
                let key_type = self
                    .selected_type
                    .as_deref()
                    .map(KeyType::from)
                    .unwrap_or(KeyType::Unknown);
                let loc_label: Option<SharedString> = match &self.selected_location {
                    Some(MatchLocation::Field(f)) => {
                        Some(format!("{}: {f}", i18n_value_search(cx, "loc_field")).into())
                    }
                    Some(MatchLocation::Index(i)) => Some(format!("[{i}]").into()),
                    Some(MatchLocation::Member(m)) => {
                        Some(format!("{}: {m}", i18n_value_search(cx, "loc_member")).into())
                    }
                    _ => None,
                };
                let body: gpui::AnyElement = if show_tree {
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.preview_json_tree.clone())
                        .into_any_element()
                } else {
                    Editor::new(&self.preview_editor)
                        .flex_1()
                        .h_full()
                        .w_full()
                        .bordered(false)
                        .appearance(false)
                        .p_0()
                        .font_family(get_mono_font_family())
                        .into_any_element()
                };

                v_flex()
                    .size_full()
                    .min_h_0()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(border)
                            .child(KeyTypeBadge::new(key_type).plain(true))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Label::new(key).text_xs().font_semibold().truncate()),
                            )
                            .when_some(loc_label, |row, loc| {
                                row.child(Label::new(loc).text_xs().text_color(muted))
                            })
                            .when(has_json_tree, |row| row.child(self.render_json_view_switch(cx)))
                            .child(
                                Button::new("vs-copy-key")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Copy)
                                    .tooltip(i18n_value_search(cx, "copy_key_tooltip"))
                                    .on_click(cx.listener(|this, _, w, cx| this.copy_key(w, cx))),
                            )
                            .child(
                                Button::new("vs-copy-value")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Copy)
                                    .tooltip(i18n_value_search(cx, "copy_value_tooltip"))
                                    .on_click(cx.listener(|this, _, w, cx| this.copy_preview(w, cx))),
                            )
                            .child(
                                Button::new("vs-open")
                                    .small()
                                    .outline()
                                    .label(i18n_value_search(cx, "open"))
                                    .on_click(cx.listener(move |this, _, _w, cx| this.open_key(key_open.clone(), cx))),
                            ),
                    )
                    .child(body)
                    .into_any_element()
            }
        }
    }
}

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

//! Tag & note editor for a single key, plus a batch colour-only dialog
//! for multi-select.
//!
//! Backed by a small dedicated view so the swatch row re-renders on
//! click without having to plumb state through the parent editor. The
//! dialog itself is built via the shared `ZedisDialog` helper — we just
//! pass `Entity<ZedisKeyTagDialog>` as the child so the framework's
//! redraw machinery keeps the swatch selection in sync with internal
//! state.
//!
//! Save / Clear semantics (single key):
//! * **Save** persists the current swatch + note. An empty swatch *and*
//!   empty note hits the manager's "drop the record" branch.
//! * **Clear** drops the record outright, regardless of dialog state —
//!   convenience for "I no longer care about this key".
//! * **Cancel** does nothing and closes.
//!
//! Batch dialog: only the colour swatch is shown. Save/Clear call
//! [`KeyMetadataManager::set_tags_many`], which **preserves each key's
//! existing note** and only rewrites the tag field.

use crate::db::{KeyMetadata, TagColor, get_key_metadata_manager};
use crate::helpers::theme_color_for_tag;
use crate::states::{ZedisGlobalStore, dialog_button_props, i18n_common, i18n_key_tag};
use gpui::{Entity, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    tooltip::Tooltip,
    v_flex,
};
use rust_i18n::t;
use tracing::error;
use zedis_ui::ZedisDialog;

pub struct ZedisKeyTagDialog {
    server_id: SharedString,
    key: SharedString,
    /// Initially `None` = "no tag". Mutated by swatch clicks; persisted
    /// on Save. Cancel discards it.
    selected_tag: Option<TagColor>,
    note_input_state: Entity<InputState>,
}

impl ZedisKeyTagDialog {
    /// Construct a dialog state bound to `(server_id, key)`. Looks up
    /// the existing record (if any) so the dialog opens pre-filled —
    /// no surprise "did I overwrite the old note?" moments for users.
    pub fn new(server_id: SharedString, key: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let existing = get_key_metadata_manager()
            .get(server_id.as_ref(), key.as_ref())
            .unwrap_or_default()
            .unwrap_or_default();

        let initial_note = existing.note.clone();
        let note_input_state = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(i18n_key_tag(cx, "note_placeholder"));
            if !initial_note.is_empty() {
                input = input.default_value(initial_note);
            }
            input
        });

        Self {
            server_id,
            key,
            selected_tag: existing.tag,
            note_input_state,
        }
    }

    fn set_tag(&mut self, tag: Option<TagColor>, cx: &mut Context<Self>) {
        if self.selected_tag == tag {
            return;
        }
        self.selected_tag = tag;
        cx.notify();
    }

    fn snapshot(&self, cx: &gpui::App) -> KeyMetadata {
        KeyMetadata {
            tag: self.selected_tag,
            note: self.note_input_state.read(cx).value().to_string(),
        }
    }

    /// Apply current dialog state. Empty record routes through
    /// `KeyMetadataManager::set`'s built-in delete fast-path — no
    /// special-case here.
    fn save(&self, cx: &gpui::App) {
        let metadata = self.snapshot(cx);
        if let Err(e) = get_key_metadata_manager().set(self.server_id.as_ref(), self.key.as_ref(), metadata) {
            error!(error = %e, server = %self.server_id, key = %self.key, "Failed to save key metadata");
        }
    }

    /// Wipe the record for this key regardless of the dialog state.
    /// Used by the in-dialog "Clear" button so users don't have to
    /// manually unset both swatch and note to delete.
    fn clear(&self) {
        if let Err(e) = get_key_metadata_manager().clear(self.server_id.as_ref(), self.key.as_ref()) {
            error!(error = %e, server = %self.server_id, key = %self.key, "Failed to clear key metadata");
        }
    }

    fn render_swatch_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_border = theme.border;
        let active_border = theme.foreground;

        let mut row = h_flex().gap_2().flex_wrap().items_center();

        // "None" pill — visually distinct so users have a clear way to
        // unset the tag without hunting for an X.
        let none_active = self.selected_tag.is_none();
        let none_btn = Button::new("ktd-swatch-none")
            .xsmall()
            .label(i18n_key_tag(cx, "none_pill"));
        let none_btn = if none_active {
            none_btn.primary()
        } else {
            none_btn.outline()
        };
        row = row.child(none_btn.on_click(cx.listener(|this, _, _w, cx| this.set_tag(None, cx))));

        for (i, color) in TagColor::ALL.iter().copied().enumerate() {
            let fill = theme_color_for_tag(color, cx);
            let active = self.selected_tag == Some(color);
            let border = if active { active_border } else { muted_border };
            // Pre-resolve the tooltip string from the current locale so
            // the closure can hand it to gpui's Tooltip builder without
            // re-entering the i18n machinery on every hover frame.
            let tooltip_text = i18n_key_tag(cx, color_label_key(color));
            // Render as a fixed-size circle filled with the tag colour.
            // Click is on a wrapping interactive div so the visual stays
            // a pure circle (no button chrome cluttering the row).
            let swatch_id = ("ktd-swatch", i as u32);
            row = row.child(
                div()
                    .id(swatch_id)
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .border_2()
                    .border_color(border)
                    .bg(fill)
                    .cursor_pointer()
                    .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _w, cx| this.set_tag(Some(color), cx))),
            );
        }
        row
    }

    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Resolve theme colours up-front so the chain below doesn't keep
        // an immutable theme borrow alive across `render_swatch_row(cx)`
        // (which needs `&mut cx` to install click listeners on swatches).
        let muted_fg = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .w_full()
            .child(Label::new(i18n_key_tag(cx, "key_label")).text_xs().text_color(muted_fg))
            .child(Label::new(self.key.clone()).text_sm().font_semibold())
            .child(Label::new(i18n_key_tag(cx, "tag_label")).text_xs().text_color(muted_fg))
            .child(self.render_swatch_row(cx))
            .child(
                Label::new(i18n_key_tag(cx, "note_label"))
                    .text_xs()
                    .text_color(muted_fg),
            )
            .child(Input::new(&self.note_input_state).w_full())
    }
}

impl Render for ZedisKeyTagDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_body(cx)
    }
}

/// i18n key suffix per colour — matches the `[key_tag]` section.
fn color_label_key(color: TagColor) -> &'static str {
    match color {
        TagColor::Red => "color_red",
        TagColor::Orange => "color_orange",
        TagColor::Yellow => "color_yellow",
        TagColor::Green => "color_green",
        TagColor::Blue => "color_blue",
        TagColor::Purple => "color_purple",
    }
}

/// Callback fired by `open_key_tag_dialog` after a successful Save or
/// Clear. Lets callers (key tree, editor toolbar) refresh their own
/// view state — the manager commits its in-memory cache synchronously
/// before the callback runs, so any read inside `on_done` sees the
/// new tag/note. `None` makes the dialog fire-and-forget.
pub type OnTagDialogDone = std::sync::Arc<dyn Fn(&mut gpui::App) + 'static>;

/// Public entry point used by both the editor toolbar and the key-tree
/// right-click menu. Constructs the dialog state entity, builds a
/// title that includes the key (truncated to keep narrow windows
/// reasonable), and wires Save / Clear / Cancel onto the standard
/// ZedisDialog footer.
pub fn open_key_tag_dialog(
    server_id: SharedString,
    key: SharedString,
    window: &mut Window,
    cx: &mut gpui::App,
    on_done: Option<OnTagDialogDone>,
) {
    if server_id.is_empty() || key.is_empty() {
        return;
    }
    let dialog_state: Entity<ZedisKeyTagDialog> =
        cx.new(|cx| ZedisKeyTagDialog::new(server_id, key.clone(), window, cx));
    let body_state = dialog_state.clone();
    let save_state = dialog_state.clone();
    let clear_state = dialog_state.clone();
    // Clone the callback for each terminal path (Save / in-dialog
    // Clear). Cancel runs no callback because nothing was persisted.
    let on_done_save = on_done.clone();
    let on_done_clear = on_done;

    // Title shows the key so two stacked invocations don't blur. Long
    // keys get a hard truncate at ~64 chars — anything past that is
    // usually a namespace tail the user can read in the editor anyway.
    let title_text: SharedString = {
        let prefix = i18n_key_tag(cx, "dialog_title");
        if key.chars().count() > 64 {
            let truncated: String = key.chars().take(64).collect();
            format!("{} — {}…", prefix.as_ref(), truncated).into()
        } else {
            format!("{} — {}", prefix.as_ref(), key.as_ref()).into()
        }
    };

    let clear_label = i18n_key_tag(cx, "clear_button");

    ZedisDialog::new(title_text)
        .w(px(480.))
        .ok_text(i18n_common(cx, "save"))
        .cancel_text(i18n_common(cx, "cancel"))
        .button_props(
            dialog_button_props(cx)
                .ok_text(i18n_common(cx, "save"))
                .cancel_text(i18n_common(cx, "cancel")),
        )
        .child(move || {
            // Two-section body: the editable form on top, a destructive
            // "Clear all" affordance at the bottom so users can drop
            // the record without juggling the swatch + note inputs.
            let dialog = body_state.clone();
            let clear_dialog = clear_state.clone();
            let clear_label = clear_label.clone();
            v_flex()
                .gap_4()
                .w_full()
                .child(dialog)
                .child(
                    h_flex().w_full().justify_end().child(
                        Button::new("ktd-clear-btn")
                            .ghost()
                            .xsmall()
                            .label(clear_label)
                            .on_click({
                                let on_done = on_done_clear.clone();
                                move |_, window, cx| {
                                    clear_dialog.read(cx).clear();
                                    if let Some(cb) = on_done.as_ref() {
                                        cb(cx);
                                    }
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
                )
                .into_any_element()
        })
        .on_ok(move |_, _w, cx| {
            save_state.read(cx).save(cx);
            if let Some(cb) = on_done_save.as_ref() {
                cb(cx);
            }
            true
        })
        .open(window, cx);
}

// ─── Batch tag (multi-select) ───────────────────────────────────────────────

/// Colour-only tag editor for many keys. Notes are never shown or
/// overwritten — batch ops only touch [`KeyMetadata::tag`].
pub struct ZedisBatchKeyTagDialog {
    server_id: SharedString,
    keys: Vec<SharedString>,
    selected_tag: Option<TagColor>,
}

impl ZedisBatchKeyTagDialog {
    pub fn new(server_id: SharedString, keys: Vec<SharedString>) -> Self {
        Self {
            server_id,
            keys,
            selected_tag: None,
        }
    }

    fn set_tag(&mut self, tag: Option<TagColor>, cx: &mut Context<Self>) {
        if self.selected_tag == tag {
            return;
        }
        self.selected_tag = tag;
        cx.notify();
    }

    /// Apply `selected_tag` to every key, preserving notes.
    fn apply_tag(&self, tag: Option<TagColor>) {
        let keys: Vec<&str> = self.keys.iter().map(|k| k.as_ref()).collect();
        if let Err(e) = get_key_metadata_manager().set_tags_many(self.server_id.as_ref(), keys, tag) {
            error!(
                error = %e,
                server = %self.server_id,
                count = self.keys.len(),
                "Failed to batch-set key tags"
            );
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_border = theme.border;
        let active_border = theme.foreground;
        let muted_fg = theme.muted_foreground;

        let mut row = h_flex().gap_2().flex_wrap().items_center();
        let none_active = self.selected_tag.is_none();
        let none_btn = Button::new("bktd-swatch-none")
            .xsmall()
            .label(i18n_key_tag(cx, "none_pill"));
        let none_btn = if none_active {
            none_btn.primary()
        } else {
            none_btn.outline()
        };
        row = row.child(none_btn.on_click(cx.listener(|this, _, _w, cx| this.set_tag(None, cx))));

        for (i, color) in TagColor::ALL.iter().copied().enumerate() {
            let fill = theme_color_for_tag(color, cx);
            let active = self.selected_tag == Some(color);
            let border = if active { active_border } else { muted_border };
            let tooltip_text = i18n_key_tag(cx, color_label_key(color));
            let swatch_id = ("bktd-swatch", i as u32);
            row = row.child(
                div()
                    .id(swatch_id)
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .border_2()
                    .border_color(border)
                    .bg(fill)
                    .cursor_pointer()
                    .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _w, cx| this.set_tag(Some(color), cx))),
            );
        }

        v_flex()
            .gap_3()
            .w_full()
            .child(
                Label::new(i18n_key_tag(cx, "batch_body_hint"))
                    .text_xs()
                    .text_color(muted_fg),
            )
            .child(Label::new(i18n_key_tag(cx, "tag_label")).text_xs().text_color(muted_fg))
            .child(row)
    }
}

impl Render for ZedisBatchKeyTagDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_body(cx)
    }
}

/// Open a batch colour dialog for `keys` (multi-select). Notes on each
/// key are preserved; only the tag field is written via
/// [`get_key_metadata_manager::set_tags_many`].
pub fn open_batch_key_tag_dialog(
    server_id: SharedString,
    keys: Vec<SharedString>,
    window: &mut Window,
    cx: &mut gpui::App,
    on_done: Option<OnTagDialogDone>,
) {
    if server_id.is_empty() || keys.is_empty() {
        return;
    }
    let count = keys.len();
    let dialog_state: Entity<ZedisBatchKeyTagDialog> = cx.new(|_cx| ZedisBatchKeyTagDialog::new(server_id, keys));
    let body_state = dialog_state.clone();
    let save_state = dialog_state.clone();
    let clear_state = dialog_state.clone();
    let on_done_save = on_done.clone();
    let on_done_clear = on_done;

    let title_text: SharedString = {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        t!("key_tag.batch_dialog_title", count = count, locale = locale)
            .to_string()
            .into()
    };

    let clear_label = i18n_key_tag(cx, "batch_clear_tags");

    ZedisDialog::new(title_text)
        .w(px(420.))
        .ok_text(i18n_common(cx, "save"))
        .cancel_text(i18n_common(cx, "cancel"))
        .button_props(
            dialog_button_props(cx)
                .ok_text(i18n_common(cx, "save"))
                .cancel_text(i18n_common(cx, "cancel")),
        )
        .child(move || {
            let dialog = body_state.clone();
            let clear_dialog = clear_state.clone();
            let clear_label = clear_label.clone();
            v_flex()
                .gap_4()
                .w_full()
                .child(dialog)
                .child(
                    h_flex().w_full().justify_end().child(
                        Button::new("bktd-clear-btn")
                            .ghost()
                            .xsmall()
                            .label(clear_label)
                            .on_click({
                                let on_done = on_done_clear.clone();
                                move |_, window, cx| {
                                    clear_dialog.read(cx).apply_tag(None);
                                    if let Some(cb) = on_done.as_ref() {
                                        cb(cx);
                                    }
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
                )
                .into_any_element()
        })
        .on_ok(move |_, _w, cx| {
            let tag = save_state.read(cx).selected_tag;
            save_state.read(cx).apply_tag(tag);
            if let Some(cb) = on_done_save.as_ref() {
                cb(cx);
            }
            true
        })
        .open(window, cx);
}

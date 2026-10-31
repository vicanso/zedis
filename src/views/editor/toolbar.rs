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

//! The key information bar: key name + type badge, TTL editor, and
//! the action buttons (copy/save/export/import/history/diff/delete).
//! Split out of `editor.rs`.

use super::more_menu::MoreActions;
use super::*;
use crate::connection::{Capability, ServerCommand};
use crate::helpers::{opens_files, saves_files};
use crate::states::RedisValue;
use gpui::{AnyElement, App};

/// The inline TTL field, while the key bar shows it instead of the TTL button.
const TTL_INPUT_ID: &str = "zedis-editor-ttl-input";

/// How much of a key's name the bar shows before the facts beside it (size,
/// encoding, heat) are given any room, in characters. A key is known by its
/// name, and the name used to be what a narrow bar took from first: at the
/// Linux default window `zedis:fn:gamma` read "ze…" next to a complete
/// "72 B  embstr · idle 0s" (#178). Past this many the name is long anyway
/// — it is whole in its tooltip — and the facts come first again. Sixteen
/// is what leaves a short string's chips all on the line at the default
/// window; a longer name there keeps the size and gives up the encoding.
const KEY_NAME_PRIORITY_CHARS: f32 = 16.;
/// The advance of the bundled mono face, in em.
const MONO_ADVANCE_EM: f32 = 0.6;
/// The one line of the name-and-facts strip that is drawn. What does not fit
/// on it wraps to a second line, and that one is clipped.
const KEY_FACTS_LINE_HEIGHT: f32 = 28.;

/// What the bar shows of the selected value, read once a render.
struct KeyFacts {
    is_busy: bool,
    /// The tree knows the key, its value has not landed yet.
    pending_first_load: bool,
    ttl: SharedString,
    size: SharedString,
    bitmap_candidate: bool,
    bitmap_view: bool,
    has_bytes_value: bool,
    /// Only the value's first bytes are loaded.
    preview: bool,
    encoding: Option<SharedString>,
    heat: HeatMetric,
    expires_at: Option<SharedString>,
    key_type: KeyType,
}

/// The width a key's name is owed before the chips beside it get any: its
/// own, up to [`KEY_NAME_PRIORITY_CHARS`]. An estimate — the mono advance,
/// two columns for a character outside ASCII — which is all it needs to be:
/// it decides which chips fit on the line, never how the name is drawn.
fn key_name_priority_width(name: &str, font_size: f32) -> f32 {
    let columns: f32 = name.chars().map(|c| if c.is_ascii() { 1. } else { 2. }).sum();
    columns.min(KEY_NAME_PRIORITY_CHARS) * font_size * MONO_ADVANCE_EM
}

impl ZedisEditor {
    /// Render the key information bar with actions (copy, save, TTL, delete)
    /// The value's size, rendered just after the key name (per the design):
    /// the value alone, prefixed with a lock glyph only when the value is
    /// read-only (read-only connection, or a binary the editor cannot edit).
    fn size_chip(&self, size: SharedString, cx: &App) -> Option<AnyElement> {
        if size.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let value_readonly = self.readonly
            || self
                .bytes_editor
                .as_ref()
                .map(|editor| editor.read(cx).is_readonly())
                .unwrap_or(false);
        let mut row = h_flex().flex_none().items_center().gap_1();
        if value_readonly {
            row = row.child(Icon::new(CustomIconName::Lock).xsmall().text_color(muted));
        }
        Some(
            row.child(
                Label::new(size)
                    .text_sm()
                    .font_family(get_mono_font_family())
                    .text_color(muted),
            )
            .into_any_element(),
        )
    }

    pub(super) fn render_select_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(key) = server_state.key() else {
            return h_flex();
        };

        let facts = self.key_facts(server_state.value(), cx);
        let more = self.more_actions(server_state, &key, &facts, cx);
        let KeyFacts {
            is_busy,
            pending_first_load,
            ttl,
            size,
            encoding,
            heat,
            expires_at,
            key_type,
            ..
        } = facts;
        let mut btns = vec![];

        // Show loading only if busy and not recently selected (avoid flashing)
        let should_show_loading = is_busy && !self.is_selected_key_recently();
        let actions_locked = pending_first_load || should_show_loading;
        let size_el = self.size_chip(size, cx);
        let object_el = object_chip(encoding, heat, cx);

        btns.extend(self.save_button(pending_first_load, actions_locked, cx));

        // Add TTL button (or input field when in edit mode)
        if !ttl.is_empty() {
            btns.push(self.ttl_control(ttl, expires_at, actions_locked, cx));
        }
        btns.push(self.reload_button(actions_locked, cx));
        btns.push(
            Button::new("zedis-editor-key-timeline")
                .ghost()
                .disabled(actions_locked)
                .tooltip(i18n_editor(cx, "key_timeline_tooltip"))
                .icon(CustomIconName::Activity)
                .on_click(cx.listener(|this, _event, window, cx| {
                    open_key_timeline_dialog(this.server_state.clone(), window, cx);
                }))
                .into_any_element(),
        );

        // Lower-frequency actions live behind one "…" menu so the bar
        // stays compact: bitmap view, value file export / import, diff,
        // delete.
        if more.offers_anything() {
            btns.push(
                Button::new("zedis-editor-more")
                    .ghost()
                    .disabled(actions_locked)
                    .tooltip(i18n_editor(cx, "more_actions"))
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, window, cx| more.build(menu, window, cx))
                    .into_any_element(),
            );
        }

        let server_id = server_state.server_id().to_string();
        h_flex()
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .gap_2()
            .w_full()
            .child(self.copy_and_favorite(&key, server_id, should_show_loading, cx))
            .child(KeyTypeBadge::new(key_type).into_any_element())
            .child(name_and_facts(key, size_el, object_el, cx))
            .children(btns)
    }

    /// What the bar shows of the selected value — its TTL, size, encoding
    /// and heat — and what the "…" menu needs to know of it.
    fn key_facts(&self, value: Option<&RedisValue>, cx: &App) -> KeyFacts {
        let mut is_busy = false;
        let mut pending_first_load = false;
        let mut ttl = SharedString::default();
        let mut size = SharedString::default();
        let mut bitmap_candidate = false;
        let mut bitmap_view = false;
        let mut has_bytes_value = false;
        // Only the value's first bytes are loaded: nothing that takes them
        // for the whole value (export, import over them, a cross-server diff).
        let mut preview = false;
        // `OBJECT ENCODING` and the server's heat metric, both absent on a
        // server that has no usable `OBJECT` (a proxy, a managed cloud, a
        // NOPERM user) — the chip then simply doesn't render.
        let mut encoding: Option<SharedString> = None;
        let mut heat: HeatMetric = HeatMetric::None;
        // The absolute instant this key expires, for the TTL tooltip. A
        // remaining duration answers "how long"; only this answers "when",
        // which is the question an on-call actually has.
        let mut expires_at: Option<SharedString> = None;

        let mut key_type = KeyType::Unknown;
        // Extract value information if available
        if let Some(value) = value {
            is_busy = value.is_busy();
            key_type = value.key_type();
            pending_first_load = value.is_pending_first_load();
            // Name and type come from the tree; size / TTL / encoding belong
            // to the previous key and stay off until GET lands.
            if !pending_first_load {
                // Format TTL display
                ttl = if let Some(ttl) = value.ttl() {
                    let seconds = ttl.num_seconds();
                    if seconds == -2 {
                        i18n_common(cx, "expired")
                    } else if seconds < 0 {
                        i18n_common(cx, "permanent")
                    } else {
                        format_duration(Duration::from_secs(seconds as u64)).into()
                    }
                } else {
                    "--".into()
                };

                encoding = value.encoding();
                heat = value.heat();
                expires_at = value
                    .expire_at
                    .filter(|at| *at > 0)
                    .and_then(format_unix_secs)
                    .map(SharedString::from);

                size = format_size(value.size(), DECIMAL).into();
                // The Bitmap toggle only makes sense for genuinely opaque binary —
                // anything the format pipeline decoded (Protobuf, MessagePack,
                // JSON, timestamps, compressed, images, text) keeps its own viewer,
                // so we require the detected format to be the raw `Bytes` fallback.
                // `infer` can't recognise Protobuf/MessagePack, so the byte
                // heuristic alone would wrongly grab them.
                preview = value.preview_of().is_some();
                bitmap_candidate = value.key_type() == KeyType::String
                    && !preview
                    && value.bytes_value().is_some_and(|b| {
                        matches!(b.format, DataFormat::Bytes)
                            && !looks_like_hll(b.bytes.as_ref())
                            && bitmap_eligible(b.bytes.as_ref())
                    });
                bitmap_view = bitmap_candidate
                    && self
                        .bitmap_override
                        .unwrap_or_else(|| value.bytes_value().is_some_and(|b| looks_like_bitmap(b.bytes.as_ref())));
                has_bytes_value = value.bytes_value().is_some();
            }
        }
        KeyFacts {
            is_busy,
            pending_first_load,
            ttl,
            size,
            bitmap_candidate,
            bitmap_view,
            has_bytes_value,
            preview,
            encoding,
            heat,
            expires_at,
            key_type,
        }
    }

    /// Save, for a string value: primary once the value has been edited.
    fn save_button(&self, pending_first_load: bool, actions_locked: bool, cx: &Context<Self>) -> Option<AnyElement> {
        // Add save button for string editor if value is modified
        if !pending_first_load && let Some(bytes_editor) = &self.bytes_editor {
            let state = bytes_editor.read(cx);
            let value_modified = state.is_value_modified();
            let readonly = state.is_readonly();
            let tooltip = if self.readonly {
                i18n_common(cx, "disable_in_readonly")
            } else if readonly {
                i18n_editor(cx, "can_not_edit_value")
            } else {
                with_hot_key("save", &i18n_editor(cx, "save_data_tooltip")).into()
            };

            return Some(
                Button::new("zedis-editor-save-key")
                    .disabled(self.readonly || !value_modified || actions_locked)
                    .when(value_modified, |this| this.primary())
                    .when(!value_modified, |this| this.outline())
                    .label(i18n_common(cx, "save"))
                    .tooltip(tooltip)
                    .icon(CustomIconName::Save)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.save(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        None
    }

    /// The TTL: a button showing what is left, whose caret offers the other
    /// ways to set it — or, while it is being edited, the field.
    fn ttl_control(
        &self,
        ttl: SharedString,
        expires_at: Option<SharedString>,
        actions_locked: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        if self.ttl_edit_mode {
            // Show input field with confirmation button. The field is
            // too narrow for a sentence — "1h 30m, or seconds" was cut
            // at "or" in every locale — so its placeholder is the
            // example alone and the sentence is what hovering it says.
            let hint = i18n_editor(cx, "ttl_duration_hint");
            div()
                .id("zedis-editor-ttl-field")
                .flex_none()
                .w(px(TTL_INPUT_MAX_WIDTH))
                .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                .child(
                    Input::new(&self.ttl_input_state).id(TTL_INPUT_ID).suffix(
                        Button::new("zedis-editor-ttl-update-btn")
                            .icon(Icon::new(IconName::Check))
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                this.handle_update_ttl(window, cx);
                            })),
                    ),
                )
                .into_any_element()
        } else {
            // Show TTL button that switches to edit mode on click
            let action = if self.readonly {
                i18n_common(cx, "disable_in_readonly")
            } else {
                with_hot_key("update_ttl", &i18n_editor(cx, "update_ttl_tooltip")).into()
            };
            // The button face is a remaining duration ("6.9d"); the exact
            // expiry instant goes here, in the configured zone and layout,
            // so nobody has to do that arithmetic by hand.
            let ttl_tooltip: SharedString = match &expires_at {
                Some(at) => format!("{} {at} · {action}", i18n_editor(cx, "expires_at_label")).into(),
                None => action,
            };
            let button = Button::new("zedis-editor-ttl-btn")
                .outline()
                .font_family(get_mono_font_family())
                .disabled(self.readonly || actions_locked)
                .tooltip(ttl_tooltip)
                .label(ttl.clone())
                .icon(CustomIconName::Clock3)
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.enter_ttl_edit_mode(window, cx);
                }));
            if self.readonly {
                // Nothing to choose between when neither command can run.
                button.into_any_element()
            } else {
                // Click keeps doing the common thing (a countdown); the
                // caret is where "expire at this instant" lives. Remove
                // TTL is only listed while there is one to drop.
                let has_expiry = expires_at.is_some();
                DropdownButton::new("zedis-editor-ttl")
                    .button(button)
                    .dropdown_menu(move |menu, _, _cx| {
                        let menu = menu
                            .menu_element_with_icon(
                                CustomIconName::Clock3,
                                Box::new(EditorAction::EditTtlDuration),
                                move |_, cx| Label::new(i18n_editor(cx, "ttl_mode_duration")),
                            )
                            .menu_element_with_icon(
                                IconName::Calendar,
                                Box::new(EditorAction::EditTtlAbsolute),
                                move |_, cx| Label::new(i18n_editor(cx, "ttl_mode_absolute")),
                            );
                        if has_expiry {
                            menu.separator().menu_element_with_icon(
                                CustomIconName::Clock3,
                                Box::new(EditorAction::PersistTtl),
                                move |_, cx| Label::new(i18n_editor(cx, "ttl_mode_persist")),
                            )
                        } else {
                            menu
                        }
                    })
                    .into_any_element()
            }
        }
    }

    /// Reload, and its caret: how often the value reloads by itself.
    fn reload_button(&self, actions_locked: bool, cx: &Context<Self>) -> AnyElement {
        let reload_tooltip: SharedString = with_hot_key("reload_value", &i18n_editor(cx, "reload_key_tooltip")).into();
        let auto_refresh_interval_sec = self.auto_refresh_interval_sec;
        DropdownButton::new("zedis-editor-reload-key")
            .button(
                Button::new("zedis-editor-reload-now")
                    .ghost()
                    .disabled(actions_locked)
                    .when(auto_refresh_interval_sec > 0, |this| {
                        this.label(format!("{}s", auto_refresh_interval_sec))
                    })
                    .tooltip(reload_tooltip)
                    .icon(CustomIconName::RotateCw)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.reload(cx);
                    })),
            )
            .dropdown_menu(move |menu, _, cx| {
                let mut menu = menu;
                for interval in [0, 1, 5, 10, 30, 60] {
                    let label = if interval == 0 {
                        i18n_editor(cx, "disable_auto_refresh")
                    } else {
                        format!("{}s", interval).into()
                    };
                    menu = menu.menu_element_with_check(
                        auto_refresh_interval_sec == interval,
                        Box::new(EditorAction::AutoRefresh(interval as u32)),
                        move |_, _cx| Label::new(label.clone()),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// What the "…" menu offers for this key and value.
    fn more_actions(
        &self,
        server_state: &ZedisServerState,
        key: &SharedString,
        facts: &KeyFacts,
        cx: &App,
    ) -> MoreActions {
        let &KeyFacts {
            key_type,
            bitmap_candidate,
            bitmap_view,
            has_bytes_value,
            preview,
            pending_first_load,
            ..
        } = facts;
        // A plain String key gets the counter / append / expiry operations
        // the terminal would otherwise be the only route to; a JSON key the
        // path-level `JSON.*` writes, each only where the server has it.
        let key_ops: Vec<KeyOpAction> = match key_type {
            KeyType::String if !self.readonly && server_state.can(Capability::SaveValue) => {
                let mut ops = vec![KeyOpAction::StringIncrBy, KeyOpAction::StringAppend];
                if server_state.supports(floors::GETEX) {
                    ops.push(KeyOpAction::StringGetEx);
                }
                ops
            }
            KeyType::Json if !self.readonly && server_state.can(Capability::JsonPathWrite) => {
                let features = server_state.features();
                [
                    (KeyOpAction::JsonSet, ServerCommand::JsonSet),
                    (KeyOpAction::JsonDel, ServerCommand::JsonDel),
                    (KeyOpAction::JsonNumIncrBy, ServerCommand::JsonNumIncrBy),
                    (KeyOpAction::JsonToggle, ServerCommand::JsonToggle),
                    (KeyOpAction::JsonArrAppend, ServerCommand::JsonArrAppend),
                    (KeyOpAction::JsonStrAppend, ServerCommand::JsonStrAppend),
                    (KeyOpAction::JsonClear, ServerCommand::JsonClear),
                ]
                .into_iter()
                .filter(|(_, command)| features.is_usable(*command))
                .map(|(action, _)| action)
                .collect()
            }
            _ => Vec::new(),
        };
        let bitmap_item = bitmap_candidate && !bitmap_view;
        // Export and import are a file each way: a save and an open panel,
        // or in a page a download and an upload (`saves_files` / `opens_files`).
        let export_item = saves_files() && has_bytes_value && !preview;
        let diff_with_server_item = has_bytes_value && !preview;
        let import_item = opens_files() && has_bytes_value && !preview && !self.readonly;
        let rename_item = !self.readonly;
        // Cross-server copy reads the source and writes a (possibly
        // different, writable) target, so it stays available even when the
        // source connection is read-only.
        let copy_item = true;
        let delete_item = !self.readonly;
        // Diff submenu: editable string value with at least one saved
        // version to compare the live value against.
        let diff_editable = !pending_first_load
            && self
                .bytes_editor
                .as_ref()
                .map(|e| !e.read(cx).is_readonly())
                .unwrap_or(false)
            && !self.readonly;
        let diff_history: Vec<(i64, usize)> = if diff_editable {
            server_state
                .value_history_for(key)
                .map(|deque| deque.iter().map(|e| (e.at, e.size())).collect())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let diff_item = !diff_history.is_empty();
        // Same condition as the diff: an editable string value on a
        // writable connection.
        let find_replace_item = diff_editable;
        // Re-rendering the JSON — indented or on one line — for an editable
        // JSON value, RedisJSON or a string holding JSON.
        let json_items = diff_editable && self.bytes_editor.as_ref().is_some_and(|e| e.read(cx).is_json_value());
        // A collection key with edits recorded this session. Shown on a
        // read-only connection too: the log is what was already written.
        let change_log_item = matches!(key_type, KeyType::Hash | KeyType::List | KeyType::Set | KeyType::Zset)
            && server_state.change_log_for(key).is_some();
        MoreActions {
            bitmap_item,
            export_item,
            import_item,
            find_replace_item,
            json_items,
            change_log_item,
            diff_item,
            diff_history,
            key_ops,
            rename_item,
            copy_item,
            diff_with_server_item,
            delete_item,
        }
    }

    /// Copy + favourite share a tight 2px group so the pair reads as
    /// one cluster (matching the design), independent of the
    /// toolbar's wider `gap_2` between sections.
    fn copy_and_favorite(
        &self,
        key: &SharedString,
        server_id: String,
        should_show_loading: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let content = key.clone();
        let is_favorited = get_favorites_manager()
            .records(&server_id)
            .unwrap_or_default()
            .iter()
            .any(|k| k.as_str() == key.as_ref());
        let favorite_icon = if is_favorited {
            IconName::StarFill
        } else {
            IconName::Star
        };
        let favorite_tooltip = if is_favorited {
            i18n_editor(cx, "remove_favorite_tooltip")
        } else {
            i18n_editor(cx, "add_favorite_tooltip")
        };
        let favorite_key = key.clone();
        h_flex()
            .items_center()
            .gap_0p5()
            .child(
                // Copy key button
                Button::new("zedis-editor-copy-key")
                    .ghost()
                    .tooltip(i18n_editor(cx, "copy_key_tooltip"))
                    .loading(should_show_loading)
                    .icon(IconName::Copy)
                    .on_click(cx.listener(move |_this, _event, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(content.to_string()));
                        window.push_notification(Notification::info(i18n_editor(cx, "copied_key_to_clipboard")), cx);
                    })),
            )
            .child(
                Button::new("zedis-editor-favorite-key")
                    .ghost()
                    .tooltip(favorite_tooltip)
                    .icon(favorite_icon)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        let server_id = this.server_state.read(cx).server_id().to_string();
                        let key = favorite_key.clone();
                        // Decided against the stored list, not this
                        // render's `is_favorited`: a second click
                        // before the repaint would repeat the first.
                        cx.spawn(async move |this, cx| {
                            let _ = cx
                                .background_spawn(async move {
                                    get_favorites_manager().toggle_record(&server_id, key.as_ref())
                                })
                                .await;
                            // The star is drawn from that list, so
                            // repaint once it has changed.
                            let _ = this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    })),
            )
    }
}

/// The key's name and the facts about it, on one line that
/// takes what the buttons leave. It wraps, and only its first
/// line is drawn: a chip that does not fit beside the name
/// goes to the second line whole and is clipped there, so a
/// narrow bar loses the heat and encoding, then the size, and
/// only then starts on the name — never half a chip.
fn name_and_facts(
    key: SharedString,
    size_el: Option<AnyElement>,
    object_el: Option<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    let line = px(KEY_FACTS_LINE_HEIGHT);
    let name_width = key_name_priority_width(key.as_ref(), cx.theme().font_size.as_f32());
    h_flex()
        .flex_1()
        .min_w_0()
        .h(line)
        .flex_wrap()
        .content_start()
        .overflow_hidden()
        .gap_x_2()
        .child(
            // Key name. It asks for its own width up to the
            // priority length and grows into what is left, which
            // keeps the chips against the buttons as before. Its
            // width must not come from its content alone: a
            // content-sized box here resolved to ~zero, collapsing
            // the key to a bare "…" while the row still had room
            // to spare. The full name is on hover, since a long
            // key is truncated by design.
            h_flex()
                .id("zedis-editor-key-name")
                .h(line)
                .flex_grow_1()
                .flex_shrink_1()
                .flex_basis(px(name_width))
                .min_w_0()
                .overflow_hidden()
                .child(
                    // A block of the item's width, so the label
                    // is cut with an ellipsis: as a flex child it
                    // kept its own width and was clipped bare.
                    div().flex_1().min_w_0().overflow_hidden().child(
                        Label::new(key.clone())
                            // Monospace so the key reads like the identifier it is.
                            // Bold felt too heavy and Menlo ships no lighter emphasis
                            // face (only Regular/Bold), so we keep the regular weight.
                            .font_family(get_mono_font_family())
                            .text_ellipsis()
                            .whitespace_nowrap(),
                    ),
                )
                .tooltip(move |window, cx| Tooltip::new(key.clone()).build(window, cx)),
        )
        .children(
            size_el
                .into_iter()
                .chain(object_el)
                .map(|chip| h_flex().h(line).flex_none().child(chip)),
        )
}

/// Storage encoding + heat as one muted mono chip, for the slot next to the
/// size. Each half explains itself on hover, and each appears only if this
/// server answered the command behind it — so a proxy that hides `OBJECT`
/// degrades to exactly the bar we had before, and a server that answers
/// neither gets no chip at all.
fn object_chip(encoding: Option<SharedString>, heat: HeatMetric, cx: &App) -> Option<AnyElement> {
    let muted = cx.theme().muted_foreground;
    let mono = get_mono_font_family();
    let heat_part = match heat {
        HeatMetric::None => None,
        HeatMetric::Freq(count) => Some((
            format!("{} {}", i18n_editor(cx, "object_freq_label"), count),
            i18n_editor(cx, "object_freq_tooltip"),
        )),
        HeatMetric::IdleTime(secs) => Some((
            format!(
                "{} {}",
                i18n_editor(cx, "object_idle_label"),
                format_duration_units(Duration::from_secs(secs))
            ),
            i18n_editor(cx, "object_idle_tooltip"),
        )),
    };
    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(encoding) = encoding {
        let tip = i18n_editor(cx, "object_encoding_tooltip");
        parts.push(
            div()
                .id("zedis-editor-encoding")
                .flex_none()
                .child(
                    Label::new(encoding)
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(muted),
                )
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    if let Some((label, tip)) = heat_part {
        if !parts.is_empty() {
            parts.push(Label::new("·").text_xs().text_color(muted).into_any_element());
        }
        parts.push(
            div()
                .id("zedis-editor-heat")
                .flex_none()
                .child(Label::new(label).text_xs().font_family(mono).text_color(muted))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    (!parts.is_empty()).then(|| {
        h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children(parts)
            .into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;

    /// A name is owed its own width and no more than the priority length:
    /// a short one leaves the line to the chips, a long one stops asking
    /// where it would be cut anyway.
    #[test]
    fn a_key_name_is_owed_its_own_width_up_to_the_priority_length() {
        let font_size = 10.;
        // 6 columns of 0.6 em.
        assert_eq!(key_name_priority_width("key003", font_size), 36.);
        // 14 columns: the name the Linux default window drew as "ze…".
        assert_eq!(key_name_priority_width("zedis:fn:gamma", font_size), 84.);
        let capped = KEY_NAME_PRIORITY_CHARS * font_size * MONO_ADVANCE_EM;
        assert_eq!(key_name_priority_width(&"k".repeat(200), font_size), capped);
        // A wide character takes two columns of the mono face.
        assert_eq!(key_name_priority_width("用户", font_size), 24.);
        assert_eq!(key_name_priority_width("", font_size), 0.);
    }

    /// Return in the TTL field closes it, and the focus stays in the window:
    /// it used to leave with the field, after which no shortcut (⌘N, ⌘J)
    /// reached its handler until something was clicked.
    #[gpui::test]
    fn return_in_the_ttl_field_leaves_the_focus_in_the_editor(cx: &mut gpui::TestAppContext) {
        use crate::states::{KeyType, RedisValue, ZedisServerState};
        use crate::test_support::init_ui;
        use gpui::{AppContext, size};
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;

        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        server_state.update(cx, |state, cx| state.select_key("list:1".into(), cx));
        // The load that selecting starts finds no server and gives up; the
        // value goes on screen once it has.
        cx.run_until_parked();
        server_state.update(cx, |state, _| {
            state.show_value_for_test(RedisValue {
                key_type: KeyType::List,
                expire_at: Some(-1),
                ..Default::default()
            })
        });

        let mut editor = None;
        let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx));
            editor = Some(view.clone());
            Root::new(view, window, cx)
        });
        let editor = editor.expect("the window built the editor");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            editor.update(cx, |editor, cx| editor.enter_ttl_edit_mode(window, cx));
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(TTL_INPUT_ID).focused(),
                Some(true),
                "the field opens focused"
            );
            window.input("1h", cx);
            window.press("enter", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(TTL_INPUT_ID).is_none(), "Return closed the field");
            assert!(
                editor.read(cx).focus_handle.contains_focused(window, cx),
                "the focus is somewhere a shortcut can be dispatched from"
            );
        })
        .expect("the window is open");
    }

    type Sent = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

    /// The editor inside a view that takes every `EditorAction` and
    /// `KeyOpAction` on its way down and keeps it: what a control sends is
    /// known, and nothing it would open opens.
    struct BarHarness {
        editor: Entity<ZedisEditor>,
        sent: Sent,
    }

    impl Render for BarHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let editor_actions = self.sent.clone();
            let key_ops = self.sent.clone();
            div()
                .size_full()
                .capture_action(move |action: &EditorAction, _, cx| {
                    editor_actions.borrow_mut().push(format!("{action:?}"));
                    cx.stop_propagation();
                })
                .capture_action(move |action: &KeyOpAction, _, cx| {
                    key_ops.borrow_mut().push(format!("{action:?}"));
                    cx.stop_propagation();
                })
                .child(self.editor.clone())
        }
    }

    /// The key `k` holding `value`, in an editor drawn twice: the body builds
    /// the string editor the bar then reads.
    fn open_bar(
        cx: &mut gpui::TestAppContext,
        value: RedisValue,
        readonly: bool,
    ) -> (gpui::AnyWindowHandle, Entity<ZedisEditor>, Sent) {
        use crate::states::ZedisServerState;
        use gpui::{AppContext, size};
        use gpui_kit::component::Root;

        let server_state = cx.new(|_| ZedisServerState::default());
        server_state.update(cx, |state, cx| state.select_key("k".into(), cx));
        cx.run_until_parked();
        server_state.update(cx, |state, _| state.show_value_for_test(value));
        let sent = Sent::default();
        let mut editor = None;
        let handle = cx.open_window(size(px(1280.), px(720.)), |window, cx| {
            let view = cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx));
            editor = Some(view.clone());
            let harness = cx.new(|_| BarHarness {
                editor: view,
                sent: sent.clone(),
            });
            Root::new(harness, window, cx)
        });
        let editor = editor.expect("the window built the editor");
        editor.update(cx, |editor, cx| {
            editor.readonly = readonly;
            cx.notify();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("the window is open");
        (handle.into(), editor, sent)
    }

    /// What the menu `open` shows offers, top to bottom: each item clicked
    /// in a menu opened afresh, `-` for one that sends nothing — a separator,
    /// or a submenu's entry.
    fn menu_items(
        cx: &mut gpui::TestAppContext,
        handle: gpui::AnyWindowHandle,
        sent: &Sent,
        open: impl Fn(&mut Window, &mut App),
    ) -> Vec<String> {
        use gpui::{MouseButton, point};
        use gpui_kit::test::TestEventExt;

        let mut items = Vec::new();
        for item in 0usize..30 {
            sent.borrow_mut().clear();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                open(window, cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
            let offered = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let offered = window.within("popup-menu").try_find(item).is_some();
                    if offered {
                        window.within("popup-menu").click(item, cx);
                    }
                    offered
                })
                .expect("the window is open");
            cx.run_until_parked();
            // A click on the empty part of the bar, beside the name, closes
            // whatever the last one left open.
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let copy = window.find("zedis-editor-copy-key").bounds();
                let outside = point(copy.right() + px(200.), copy.center().y);
                window.pointer_down(outside, MouseButton::Left, cx);
                window.pointer_up(outside, MouseButton::Left, cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
            if !offered {
                return items;
            }
            items.push(match sent.borrow().as_slice() {
                [] => "-".to_string(),
                [one] => one.clone(),
                more => panic!("item {item} sent {more:?}"),
            });
        }
        panic!("the menu kept offering items");
    }

    fn more_menu(window: &mut Window, cx: &mut App) {
        window.click("zedis-editor-more", cx);
    }

    fn ttl_menu(window: &mut Window, cx: &mut App) {
        window.within("zedis-editor-ttl").click("popup", cx);
    }

    fn reload_menu(window: &mut Window, cx: &mut App) {
        window.within("zedis-editor-reload-key").click("popup", cx);
    }

    fn string_value() -> RedisValue {
        use crate::states::{DataFormat, KeyType, RedisBytesValue, RedisValueData};
        RedisValue {
            key_type: KeyType::String,
            data: Some(RedisValueData::Bytes(std::sync::Arc::new(RedisBytesValue {
                format: DataFormat::Text,
                bytes: "hello".into(),
                text: Some("hello".into()),
                ..Default::default()
            }))),
            expire_at: Some(unix_ts() + 3_600),
            size: 5,
            encoding: "embstr".into(),
            heat: HeatMetric::Freq(3),
            ..Default::default()
        }
    }

    /// The bar of a string key on a writable connection: every control, the
    /// TTL's own menu, the reload intervals, and the "…" menu in order.
    #[gpui::test]
    fn a_strings_bar_offers_its_controls_and_menus(cx: &mut gpui::TestAppContext) {
        use crate::test_support::init_ui;

        init_ui(cx);
        let (handle, _, sent) = open_bar(cx, string_value(), false);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "zedis-editor-copy-key",
                "zedis-editor-favorite-key",
                "zedis-editor-ttl-btn",
                "zedis-editor-reload-now",
                "zedis-editor-key-timeline",
                "zedis-editor-more",
            ] {
                assert!(window.try_find(id).is_some(), "{id}");
                assert_eq!(window.find(id).disabled(), None, "{id} is not disabled");
            }
            // Nothing to save until the value is edited.
            assert_eq!(window.find("zedis-editor-save-key").disabled(), Some(true));
        })
        .expect("the window is open");

        assert_eq!(
            menu_items(cx, handle, &sent, ttl_menu),
            ["EditTtlDuration", "EditTtlAbsolute", "-", "PersistTtl"]
        );
        assert_eq!(
            menu_items(cx, handle, &sent, reload_menu),
            [
                "AutoRefresh(0)",
                "AutoRefresh(1)",
                "AutoRefresh(5)",
                "AutoRefresh(10)",
                "AutoRefresh(30)",
                "AutoRefresh(60)",
            ]
        );
        assert_eq!(
            menu_items(cx, handle, &sent, more_menu),
            [
                "ExportValue",
                "ImportValue",
                "FindReplace",
                "WatchTimeline",
                "-",
                "-",
                "Rename",
                "CopyTo",
                "DiffWithServer",
                "Delete",
            ]
        );

        // Last, since its notification is drawn over the bar's right end.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("zedis-editor-copy-key", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("k".to_string())
        );
    }

    /// The same key on a read-only connection: nothing that writes, and the
    /// TTL a plain button with nothing to choose between.
    #[gpui::test]
    fn a_read_only_bar_offers_what_reads(cx: &mut gpui::TestAppContext) {
        use crate::test_support::init_ui;

        init_ui(cx);
        let (handle, _, sent) = open_bar(cx, string_value(), true);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("zedis-editor-save-key").disabled(), Some(true));
            assert_eq!(window.find("zedis-editor-ttl-btn").disabled(), Some(true));
            assert!(window.try_find("zedis-editor-ttl").is_none(), "no TTL menu");
        })
        .expect("the window is open");
        assert_eq!(
            menu_items(cx, handle, &sent, more_menu),
            ["ExportValue", "WatchTimeline", "-", "CopyTo", "DiffWithServer"]
        );
    }

    /// A hash that never expires: no save, no Remove TTL, no value-file
    /// items; and while the first load is out, nothing to press.
    #[gpui::test]
    fn a_collection_and_a_first_load_offer_less(cx: &mut gpui::TestAppContext) {
        use crate::states::{KeyType, RedisValueStatus};
        use crate::test_support::init_ui;

        init_ui(cx);
        let hash = RedisValue {
            key_type: KeyType::Hash,
            expire_at: Some(-1),
            ..Default::default()
        };
        let (handle, _, sent) = open_bar(cx, hash, false);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("zedis-editor-save-key").is_none());
        })
        .expect("the window is open");
        assert_eq!(
            menu_items(cx, handle, &sent, ttl_menu),
            ["EditTtlDuration", "EditTtlAbsolute"]
        );
        assert_eq!(
            menu_items(cx, handle, &sent, more_menu),
            ["WatchTimeline", "Rename", "CopyTo", "Delete"]
        );

        let loading = RedisValue {
            status: RedisValueStatus::Loading,
            key_type: KeyType::String,
            ..Default::default()
        };
        let (handle, _, _) = open_bar(cx, loading, false);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("zedis-editor-ttl-btn").is_none(), "no TTL yet");
            assert!(window.try_find("zedis-editor-save-key").is_none());
            assert_eq!(window.find("zedis-editor-reload-now").disabled(), Some(true));
            assert_eq!(window.find("zedis-editor-more").disabled(), Some(true));
        })
        .expect("the window is open");
    }
}

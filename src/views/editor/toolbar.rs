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

use super::*;

impl ZedisEditor {
    /// Render the key information bar with actions (copy, save, TTL, delete)
    pub(super) fn render_select_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(key) = server_state.key() else {
            return h_flex();
        };

        let mut is_busy = false;
        let mut btns = vec![];
        let mut ttl = SharedString::default();
        let mut size = SharedString::default();
        let mut bitmap_candidate = false;
        let mut bitmap_view = false;
        let mut has_bytes_value = false;

        let mut key_type = KeyType::Unknown;
        // Extract value information if available
        if let Some(value) = server_state.value() {
            is_busy = value.is_busy();
            key_type = value.key_type();

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

            size = format_size(value.size(), DECIMAL).into();
            // The Bitmap toggle only makes sense for genuinely opaque binary —
            // anything the format pipeline decoded (Protobuf, MessagePack,
            // JSON, timestamps, compressed, images, text) keeps its own viewer,
            // so we require the detected format to be the raw `Bytes` fallback.
            // `infer` can't recognise Protobuf/MessagePack, so the byte
            // heuristic alone would wrongly grab them.
            bitmap_candidate = value.key_type() == KeyType::String
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

        // Show loading only if busy and not recently selected (avoid flashing)
        let should_show_loading = is_busy && !self.is_selected_key_recently();
        // Size display, rendered just after the key name (per the design): the
        // value alone, prefixed with a lock glyph only when the value is
        // read-only (read-only connection or non-editable binary). Built here,
        // placed in the header row below.
        let size_el = (!size.is_empty()).then(|| {
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
            row.child(
                Label::new(size)
                    .text_sm()
                    .font_family(get_mono_font_family())
                    .text_color(muted),
            )
            .into_any_element()
        });

        // Add save button for string editor if value is modified
        if let Some(bytes_editor) = &self.bytes_editor {
            let state = bytes_editor.read(cx);
            let value_modified = state.is_value_modified();
            let readonly = state.is_readonly();
            let tooltip = if self.readonly {
                i18n_common(cx, "disable_in_readonly")
            } else if readonly {
                i18n_editor(cx, "can_not_edit_value")
            } else {
                format!(
                    "{} ({})",
                    i18n_editor(cx, "save_data_tooltip"),
                    humanize_keystroke("cmd-s")
                )
                .into()
            };

            btns.push(
                Button::new("zedis-editor-save-key")
                    .disabled(self.readonly || !value_modified || should_show_loading)
                    .primary()
                    .label(i18n_common(cx, "save"))
                    .tooltip(tooltip)
                    .icon(CustomIconName::Save)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.save(window, cx);
                    }))
                    .into_any_element(),
            );
        }

        // Add TTL button (or input field when in edit mode)
        if !ttl.is_empty() {
            let ttl_btn = if self.ttl_edit_mode {
                // Show input field with confirmation button
                Input::new(&self.ttl_input_state)
                    .max_w(px(TTL_INPUT_MAX_WIDTH))
                    .suffix(
                        Button::new("zedis-editor-ttl-update-btn")
                            .icon(Icon::new(IconName::Check))
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                this.handle_update_ttl(window, cx);
                            })),
                    )
                    .into_any_element()
            } else {
                // Show TTL button that switches to edit mode on click
                let ttl_tooltip: SharedString = if self.readonly {
                    i18n_common(cx, "disable_in_readonly")
                } else {
                    format!(
                        "{} ({})",
                        i18n_editor(cx, "update_ttl_tooltip"),
                        humanize_keystroke("cmd-t")
                    )
                    .into()
                };
                Button::new("zedis-editor-ttl-btn")
                    .outline()
                    .font_family(get_mono_font_family())
                    .disabled(self.readonly || should_show_loading)
                    .tooltip(ttl_tooltip)
                    .label(ttl.clone())
                    .icon(CustomIconName::Clock3)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.enter_ttl_edit_mode(window, cx);
                    }))
                    .into_any_element()
            };
            btns.push(ttl_btn);
        }

        let reload_tooltip: SharedString = format!(
            "{} ({})",
            i18n_editor(cx, "reload_key_tooltip"),
            humanize_keystroke("cmd-shift-r")
        )
        .into();
        // reload
        let auto_refresh_interval_sec = self.auto_refresh_interval_sec;
        btns.push(
            DropdownButton::new("zedis-editor-reload-key")
                .button(
                    Button::new("zedis-editor-reload-now")
                        .ghost()
                        .disabled(should_show_loading)
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
                .into_any_element(),
        );

        // Lower-frequency actions live behind one "…" menu so the bar
        // stays compact: bitmap view, value file export / import, diff,
        // delete.
        let bitmap_item = bitmap_candidate && !bitmap_view;
        let export_item = has_bytes_value;
        let diff_with_server_item = has_bytes_value;
        let import_item = has_bytes_value && !self.readonly;
        let rename_item = !self.readonly;
        // Cross-server copy reads the source and writes a (possibly
        // different, writable) target, so it stays available even when the
        // source connection is read-only.
        let copy_item = true;
        let delete_item = !self.readonly;
        // Diff submenu: editable string value with at least one saved
        // version to compare the live value against.
        let diff_editable = self
            .bytes_editor
            .as_ref()
            .map(|e| !e.read(cx).is_readonly())
            .unwrap_or(false)
            && !self.readonly;
        let diff_history: Vec<(i64, usize)> = if diff_editable {
            server_state
                .value_history_for(&key)
                .map(|deque| deque.iter().map(|e| (e.at, e.size())).collect())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let diff_item = !diff_history.is_empty();
        if rename_item || copy_item || bitmap_item || export_item || import_item || diff_item || delete_item {
            btns.push(
                Button::new("zedis-editor-more")
                    .ghost()
                    .disabled(should_show_loading)
                    .tooltip(i18n_editor(cx, "more_actions"))
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, window, cx| {
                        let mut menu = menu;
                        if bitmap_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Binary,
                                Box::new(EditorAction::ViewBitmap),
                                move |_, cx| Label::new(i18n_bitmap(cx, "bitmap")),
                            );
                        }
                        if export_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Download,
                                Box::new(EditorAction::ExportValue),
                                move |_, cx| Label::new(i18n_editor(cx, "export_value_tooltip")),
                            );
                        }
                        if import_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Upload,
                                Box::new(EditorAction::ImportValue),
                                move |_, cx| Label::new(i18n_editor(cx, "import_value_tooltip")),
                            );
                        }
                        // Restore submenu: pull any saved version back into the
                        // editor (the value-history dropdown, moved off the
                        // toolbar into "more actions"). Same version data as the
                        // diff submenu below, different action.
                        if diff_item {
                            let snap = diff_history.clone();
                            menu = menu.submenu_with_icon(
                                Some(Icon::new(IconName::Undo)),
                                i18n_editor(cx, "history_label"),
                                window,
                                cx,
                                move |submenu, _window, _cx| {
                                    let mut submenu = submenu;
                                    let now = unix_ts();
                                    for (idx, (at, size)) in snap.iter().enumerate() {
                                        let secs_ago = (now - at).max(0) as u64;
                                        let rel = format_duration(Duration::from_secs(secs_ago));
                                        let size_str = format_size(*size as u64, DECIMAL);
                                        let label = format!("v{} • {} • {}", idx + 1, rel, size_str);
                                        let idx_u32 = idx as u32;
                                        submenu = submenu.menu_element(
                                            Box::new(EditorAction::LoadHistory(idx_u32)),
                                            move |_w, _cx| Label::new(label.clone()),
                                        );
                                    }
                                    submenu
                                },
                            );
                        }
                        // Diff submenu: pick any saved version to compare the
                        // live value against (v1 = most recent, the common case).
                        if diff_item {
                            let snap = diff_history.clone();
                            menu = menu.submenu_with_icon(
                                Some(Icon::new(CustomIconName::GitCompareArrows)),
                                i18n_editor(cx, "diff_button"),
                                window,
                                cx,
                                move |submenu, _window, _cx| {
                                    let mut submenu = submenu;
                                    let now = unix_ts();
                                    for (idx, (at, size)) in snap.iter().enumerate() {
                                        let secs_ago = (now - at).max(0) as u64;
                                        let rel = format_duration(Duration::from_secs(secs_ago));
                                        let size_str = format_size(*size as u64, DECIMAL);
                                        let label = format!("v{} • {} • {}", idx + 1, rel, size_str);
                                        let idx_u32 = idx as u32;
                                        submenu = submenu.menu_element(
                                            Box::new(EditorAction::DiffHistory(idx_u32)),
                                            move |_w, _cx| Label::new(label.clone()),
                                        );
                                    }
                                    submenu
                                },
                            );
                        }
                        // Key-level ops (rename / copy / delete) sit below a
                        // separator from the value-view actions above.
                        if (rename_item || copy_item || delete_item || diff_with_server_item)
                            && (bitmap_item || export_item || import_item || diff_item)
                        {
                            menu = menu.separator();
                        }
                        if rename_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::FilePenLine,
                                Box::new(EditorAction::Rename),
                                move |_, cx| Label::new(i18n_editor(cx, "rename")),
                            );
                        }
                        if copy_item {
                            menu = menu.menu_element_with_icon(
                                IconName::Copy,
                                Box::new(EditorAction::CopyTo),
                                move |_, cx| Label::new(i18n_copy(cx, "copy_to")),
                            );
                        }
                        if diff_with_server_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::GitCompareArrows,
                                Box::new(EditorAction::DiffWithServer),
                                move |_, cx| Label::new(i18n_editor(cx, "diff_with_server")),
                            );
                        }
                        if delete_item {
                            menu = menu.menu_element_with_icon(
                                IconName::CircleX,
                                Box::new(EditorAction::Delete),
                                move |_, cx| Label::new(i18n_editor(cx, "delete_key_tooltip")),
                            );
                        }
                        menu
                    })
                    .into_any_element(),
            );
        }

        let content = key.clone();
        let server_id = server_state.server_id().to_string();
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
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .gap_2()
            .w_full()
            .child(
                // Copy + favourite share a tight 2px group so the pair reads as
                // one cluster (matching the design), independent of the
                // toolbar's wider `gap_2` between sections.
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
                                window.push_notification(
                                    Notification::info(i18n_editor(cx, "copied_key_to_clipboard")),
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("zedis-editor-favorite-key")
                            .ghost()
                            .tooltip(favorite_tooltip)
                            .icon(favorite_icon)
                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                let server_id = _this.server_state.read(cx).server_id().to_string();
                                let key = favorite_key.clone();
                                let is_favorited = is_favorited;
                                cx.spawn(async move |_, cx| {
                                    let _ = cx
                                        .background_spawn(async move {
                                            let manager = get_favorites_manager();
                                            if is_favorited {
                                                let _ = manager.remove_record(&server_id, key.as_ref());
                                            } else {
                                                let _ = manager.add_record(&server_id, key.as_ref());
                                            }
                                        })
                                        .await;
                                })
                                .detach();
                                cx.notify();
                            })),
                    ),
            )
            .child(KeyTypeBadge::new(key_type).into_any_element())
            .child(
                // Key name — hugs its content and truncates when long (`min_w_0`
                // + ellipsis) instead of growing, so the size can sit right
                // after it; the flex spacer below pushes the actions right.
                div().min_w_0().overflow_hidden().child(
                    Label::new(key)
                        // Monospace so the key reads like the identifier it is.
                        // Bold felt too heavy and Menlo ships no lighter emphasis
                        // face (only Regular/Bold), so we keep the regular weight.
                        .font_family(get_mono_font_family())
                        .text_ellipsis()
                        .whitespace_nowrap(),
                ),
            )
            .children(size_el)
            .child(div().flex_1())
            .children(btns)
    }
}

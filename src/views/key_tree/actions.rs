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

//! `KeyTreeAction` (the tree's dispatched action enum) and the root
//! `Render` impl, which wires every action to its handler. Split out
//! of `key_tree.rs`.

use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
pub(super) enum KeyTreeAction {
    Search(SharedString),
    Clear,
    DeleteMultipleKeys,
    DeleteKey(SharedString),
    DeleteFolder(SharedString),
    /// Batch TTL on the multi-selection / a folder prefix. `SetTtl*` open a
    /// TTL-input dialog (`EXPIRE`); `Persist*` confirm then `PERSIST`.
    SetTtlMultipleKeys,
    PersistMultipleKeys,
    SetTtlFolder(SharedString),
    PersistFolder(SharedString),
    RefreshFolder(SharedString),
    CollapseAllKeys,
    ToggleMultiSelectMode,
    ChangeChannelMode,
    AutoRefresh(u32),
    SelectFavoriteKey(SharedString),
    ClearFavorites,
    /// Open a key from the per-connection MRU list (same path as favorites).
    SelectRecentKey(SharedString),
    ClearRecentKeys,
    ExportSelectedKeys,
    ExportFolder(SharedString),
    ExportKey(SharedString),
    /// Manual full refresh: re-scan with the current keyword/mode.
    RefreshAll,
    /// Open the key tag & note dialog for the given key (carried in
    /// the variant payload because the right-click site dispatches
    /// against the row's key, not the currently-selected one in the
    /// editor — they can differ when the user right-clicks a row
    /// other than the active selection).
    EditKeyTag(SharedString),
    /// Set (or clear) the tag colour filter applied to the visible
    /// tree. Empty string means "clear" — distinct from any TagColor
    /// variant so the dispatch arm has a single match path. The
    /// payload is a `SharedString` not a `TagColor` because gpui
    /// actions need `JsonSchema` and the colour enum sits in the
    /// `db` layer, so we keep the wire format string-based.
    SetTagFilter(SharedString),
    /// Local TTL-range filter (`TtlFilter::as_str` wire id). `"all"` /
    /// empty clears it. Applied only on the already-loaded TTL cache.
    SetTtlFilter(SharedString),
    /// Multi-select: open the batch tag colour dialog for the current
    /// selection (tag only — notes on each key are preserved).
    BatchTagSelectedKeys,
    /// Copy the full key name to the clipboard.
    CopyKeyName(SharedString),
    /// Copy the folder prefix (with trailing separator) to the clipboard.
    CopyFolderPrefix(SharedString),
    /// Select the key and open the editor's rename dialog.
    RenameKey(SharedString),
    /// Add / remove the key from the local favorites list.
    ToggleFavoriteKey(SharedString),
}

impl Render for ZedisKeyTree {
    /// Main render method - displays search bar and tree structure
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(scroll_to_index) = self.state.scroll_to_index.take() {
            self.key_tree_list_state.update(cx, |state, cx| {
                state.scroll_to_item(scroll_to_index, ScrollStrategy::Top, window, cx);
            });
        }
        if std::mem::take(&mut self.state.clear_selection) {
            self.key_tree_list_state.update(cx, |state, cx| {
                state.set_selected_index(None, window, cx);
            });
        }
        if let Some(true) = self.should_enter_add_key_mode.take() {
            self.handle_add_key(window, cx);
        }
        v_flex()
            .id("key-tree-container")
            .track_focus(&self.focus_handle)
            .h_full()
            .w_full()
            .child(self.render_keyword_input(window, cx))
            .child(self.render_tree(cx))
            .on_action(cx.listener(|this, e: &QueryMode, _window, cx| {
                let new_mode = *e;

                let server_id = this.server_state.read(cx).server_id();
                if let Ok(mut option) = get_session_option(server_id) {
                    option.query_mode = Some(new_mode.to_string());
                    save_session_option(server_id, option, cx);
                }

                // Step 1: Update server state with new query mode
                this.server_state.update(cx, |state, cx| {
                    state.set_query_mode(new_mode, cx);
                });

                // Step 2: Update local UI state
                this.state.query_mode = new_mode;
            }))
            .on_action(cx.listener(|this, e: &KeyTypeFilter, _window, cx| {
                let filter = match e {
                    KeyTypeFilter::All => None,
                    KeyTypeFilter::String => Some(KeyType::String),
                    KeyTypeFilter::List => Some(KeyType::List),
                    KeyTypeFilter::Set => Some(KeyType::Set),
                    KeyTypeFilter::Zset => Some(KeyType::Zset),
                    KeyTypeFilter::Hash => Some(KeyType::Hash),
                    KeyTypeFilter::Stream => Some(KeyType::Stream),
                };
                this.server_state
                    .update(cx, |state, cx| state.set_type_filter(filter, cx));
            }))
            .on_action(cx.listener(|this, e: &KeyTreeAction, window, cx| match e {
                KeyTreeAction::ChangeChannelMode => {
                    this.server_state.update(cx, |state, cx| {
                        state.change_channel_mode(cx);
                    });
                }
                KeyTreeAction::AutoRefresh(interval) => {
                    this.state.refresh_interval_sec = *interval;
                    this.start_auto_refresh(cx);
                    let server_id = this.server_state.read(cx).server_id();
                    if let Ok(mut option) = get_session_option(server_id) {
                        option.refresh_interval_sec = Some(*interval);
                        save_session_option(server_id, option, cx);
                    }
                }
                KeyTreeAction::RefreshAll => {
                    // Refresh keeps the expanded folders in place.
                    this.handle_filter(false, cx);
                }
                KeyTreeAction::CollapseAllKeys => {
                    if !this.server_state.read(cx).can(Capability::CollapseTree) {
                        return;
                    }
                    this.server_state.update(cx, |state, cx| {
                        state.collapse_all_keys(cx);
                    });
                }
                KeyTreeAction::ToggleMultiSelectMode => {
                    this.key_tree_list_state.update(cx, |state, cx| {
                        state.delegate_mut().toggle_multiple_selection(cx);
                    });
                }
                KeyTreeAction::Search(keyword) => {
                    this.keyword_state.update(cx, |state, cx| {
                        state.set_value(keyword, window, cx);
                    });
                    // Picking a keyword (history / "search this prefix") is a
                    // fresh query.
                    this.handle_filter(true, cx);
                }
                KeyTreeAction::Clear => {
                    this.handle_clear_history(cx);
                }
                KeyTreeAction::EditKeyTag(key) => {
                    // Right-click → "Edit tag & note…". Callback patches
                    // just the affected row from the manager's fresh
                    // snapshot — no full tree rebuild for the common
                    // case (no active tag filter). `refresh_metadata_for_key`
                    // delegates to `handle_filter` automatically when a
                    // filter IS active, since row visibility then
                    // depends on the new tag colour.
                    let server_id: SharedString = this.server_state.read(cx).server_id().to_string().into();
                    let key = key.clone();
                    let key_for_callback = key.clone();
                    let weak_tree = cx.entity().downgrade();
                    let on_done: OnTagDialogDone = std::sync::Arc::new(move |cx| {
                        if let Some(tree) = weak_tree.upgrade() {
                            let key = key_for_callback.clone();
                            tree.update(cx, |this, cx| this.refresh_metadata_for_key(&key, cx));
                        }
                    });
                    open_key_tag_dialog(server_id, key, window, cx, Some(on_done));
                }
                KeyTreeAction::BatchTagSelectedKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_id: SharedString = this.server_state.read(cx).server_id().to_string().into();
                    let weak_tree = cx.entity().downgrade();
                    let on_done: OnTagDialogDone = std::sync::Arc::new(move |cx| {
                        if let Some(tree) = weak_tree.upgrade() {
                            // Batch may flip visibility under a tag filter
                            // and always affects folder aggregates → rebuild.
                            tree.update(cx, |this, cx| this.update_key_tree(true, cx));
                        }
                    });
                    open_batch_key_tag_dialog(server_id, keys, window, cx, Some(on_done));
                }
                KeyTreeAction::SetTagFilter(color_name) => {
                    let new_filter = if color_name.is_empty() {
                        None
                    } else {
                        TagColor::from_name(color_name.as_ref())
                    };
                    if this.state.selected_tag_filter != new_filter {
                        this.state.selected_tag_filter = new_filter;
                        // Local-only: rebuild the tree from the cached
                        // SCAN snapshot + metadata. No re-SCAN.
                        this.update_key_tree(true, cx);
                    }
                }
                KeyTreeAction::SetTtlFilter(id) => {
                    let new_filter = if id.is_empty() {
                        TtlFilter::All
                    } else {
                        TtlFilter::from_name(id.as_ref())
                    };
                    if this.state.selected_ttl_filter != new_filter {
                        this.state.selected_ttl_filter = new_filter;
                        this.update_key_tree(true, cx);
                    }
                }
                KeyTreeAction::SelectFavoriteKey(key) => {
                    this.select_item(key.clone(), false, false, false, cx);
                }
                KeyTreeAction::CopyKeyName(key) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
                    window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                }
                KeyTreeAction::CopyFolderPrefix(id) => {
                    // Trailing separator matches the folder's scan prefix
                    // (same shape RefreshFolder uses).
                    cx.write_to_clipboard(ClipboardItem::new_string(format!("{}:", id.as_str())));
                    window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                }
                KeyTreeAction::RenameKey(key) => {
                    // Select first so the editor's rename dialog prefills this
                    // key; emit_editor_action re-checks Capability::RenameKey.
                    let key = key.clone();
                    this.server_state.update(cx, |state, cx| {
                        state.select_key(key, cx);
                        state.emit_editor_action(EditorAction::Rename, cx);
                    });
                }
                KeyTreeAction::ToggleFavoriteKey(key) => {
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let key = key.clone();
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let manager = get_favorites_manager();
                                let is_favorited = manager
                                    .records(&server_id)
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|k| k.as_str() == key.as_ref());
                                if is_favorited {
                                    let _ = manager.remove_record(&server_id, key.as_ref());
                                } else {
                                    let _ = manager.add_record(&server_id, key.as_ref());
                                }
                            })
                            .await;
                    })
                    .detach();
                }
                KeyTreeAction::ClearFavorites => {
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let _ = get_favorites_manager().clear_history(&server_id);
                            })
                            .await;
                    })
                    .detach();
                }
                KeyTreeAction::SelectRecentKey(key) => {
                    this.select_item(key.clone(), false, false, false, cx);
                }
                KeyTreeAction::ClearRecentKeys => {
                    let server_state = this.server_state.read(cx);
                    let scope = recent_keys_scope(server_state.server_id(), server_state.db());
                    cx.spawn(async move |_, cx| {
                        let _ = cx
                            .background_spawn(async move {
                                let _ = get_recent_keys_manager().clear_history(&scope);
                            })
                            .await;
                    })
                    .detach();
                }
                KeyTreeAction::DeleteMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_keys_prompt", keys = keys.join(", "), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_keys_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.unlink_keys(keys.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::DeleteKey(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_key_prompt", key = id.clone(), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_key_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.delete_key(id.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::RefreshFolder(id) => {
                    let id = id.clone();
                    this.server_state.update(cx, |state, cx| {
                        state.refresh_prefix(format!("{}:", id.as_str()).into(), cx);
                    });
                }
                KeyTreeAction::DeleteFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    let text = t!("key_tree.delete_folder_prompt", folder = id.clone(), locale = locale).to_string();
                    let server_id = this.server_state.read(cx).server_id().to_string();
                    let text = escalate_dangerous_body(cx, &server_id, text);

                    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_folder_title"), text)
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| {
                                state.delete_folder(id.clone(), cx);
                            });
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::PersistMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.clone();
                    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| state.batch_set_ttl_keys(keys.clone(), None, cx));
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::PersistFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
                        .button_props(dialog_button_props(cx))
                        .on_ok(move |_, _, cx| {
                            server_state.update(cx, |state, cx| state.batch_set_ttl_folder(id.clone(), None, cx));
                            true
                        })
                        .open(window, cx);
                }
                KeyTreeAction::SetTtlMultipleKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.clone();
                    let ttl_input = cx
                        .new(|cx| InputState::new(window, cx).placeholder(i18n_key_tree(cx, "batch_ttl_placeholder")));
                    let input_child = ttl_input.clone();
                    let input_ok = ttl_input.clone();
                    let prompt = i18n_key_tree(cx, "batch_ttl_prompt");
                    ZedisDialog::new(i18n_key_tree(cx, "batch_ttl_title"))
                        .w(px(360.))
                        .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                        .cancel_text(i18n_common(cx, "cancel"))
                        .button_props(
                            dialog_button_props(cx)
                                .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                                .cancel_text(i18n_common(cx, "cancel")),
                        )
                        .child(move || {
                            v_flex()
                                .gap_2()
                                .child(Label::new(prompt.clone()).text_sm())
                                .child(Input::new(&input_child).small())
                        })
                        .on_ok(move |_, _, cx| match parse_duration(input_ok.read(cx).value().trim()) {
                            Ok(d) => {
                                let secs = d.as_secs();
                                server_state
                                    .update(cx, |state, cx| state.batch_set_ttl_keys(keys.clone(), Some(secs), cx));
                                true
                            }
                            Err(_) => false,
                        })
                        .open(window, cx);
                }
                KeyTreeAction::SetTtlFolder(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.clone();
                    let ttl_input = cx
                        .new(|cx| InputState::new(window, cx).placeholder(i18n_key_tree(cx, "batch_ttl_placeholder")));
                    let input_child = ttl_input.clone();
                    let input_ok = ttl_input.clone();
                    let prompt = i18n_key_tree(cx, "batch_ttl_prompt");
                    ZedisDialog::new(i18n_key_tree(cx, "batch_ttl_title"))
                        .w(px(360.))
                        .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                        .cancel_text(i18n_common(cx, "cancel"))
                        .button_props(
                            dialog_button_props(cx)
                                .ok_text(i18n_key_tree(cx, "set_ttl_confirm"))
                                .cancel_text(i18n_common(cx, "cancel")),
                        )
                        .child(move || {
                            v_flex()
                                .gap_2()
                                .child(Label::new(prompt.clone()).text_sm())
                                .child(Input::new(&input_child).small())
                        })
                        .on_ok(move |_, _, cx| match parse_duration(input_ok.read(cx).value().trim()) {
                            Ok(d) => {
                                let secs = d.as_secs();
                                server_state
                                    .update(cx, |state, cx| state.batch_set_ttl_folder(id.clone(), Some(secs), cx));
                                true
                            }
                            Err(_) => false,
                        })
                        .open(window, cx);
                }
                KeyTreeAction::ExportSelectedKeys => {
                    let keys = this.key_tree_list_state.update(cx, |state, _cx| {
                        state
                            .delegate()
                            .selected_items
                            .iter()
                            .cloned()
                            .collect::<Vec<SharedString>>()
                    });
                    if keys.is_empty() {
                        return;
                    }
                    let server_state = this.server_state.read(cx);
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, keys, ExportSource::Selection, cx);
                }
                KeyTreeAction::ExportFolder(folder) => {
                    let folder = folder.clone();
                    let prefix = format!("{folder}:");
                    let server_state = this.server_state.read(cx);
                    let keys: Vec<SharedString> = server_state
                        .keys()
                        .keys()
                        .filter(|k| k.as_str() == folder.as_str() || k.as_str().starts_with(&prefix))
                        .cloned()
                        .collect();
                    if keys.is_empty() {
                        return;
                    }
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, keys, ExportSource::Loaded, cx);
                }
                KeyTreeAction::ExportKey(id) => {
                    let id = id.clone();
                    let server_state = this.server_state.read(cx);
                    let server_id: SharedString = server_state.server_id().to_string().into();
                    let db = server_state.db();
                    let server_name: SharedString = get_server(server_id.as_str())
                        .map(|s| s.name.into())
                        .unwrap_or_else(|_| server_id.clone());
                    open_migration_export_window(server_id, server_name, db, vec![id], ExportSource::Selection, cx);
                }
            }))
            .on_action(cx.listener(|this, event: &EditorAction, window, cx| match event {
                EditorAction::Search => {
                    this.keyword_state.focus_handle(cx).focus(window, cx);
                }
                EditorAction::Delete => {
                    // `cmd-backspace` while the tree is focused deletes the
                    // current selection. `EditorAction::Delete` is otherwise
                    // only handled by the editor view (a sibling), so it never
                    // reached us via focus-tree dispatch. Reuse the tree's own
                    // delete-confirm flow: batch when multi-select has picks,
                    // else the selected key.
                    let multi = {
                        let delegate = this.key_tree_list_state.read(cx).delegate();
                        delegate.enabled_multiple_selection && !delegate.selected_items.is_empty()
                    };
                    if multi {
                        window.dispatch_action(Box::new(KeyTreeAction::DeleteMultipleKeys), cx);
                    } else if let Some(key) = this.server_state.read(cx).key() {
                        window.dispatch_action(Box::new(KeyTreeAction::DeleteKey(key)), cx);
                    }
                }
                _ => {
                    cx.propagate();
                }
            }))
    }
}

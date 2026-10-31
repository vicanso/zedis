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
//! `Render` impl, which wires every action to its handler; what an
//! action does to keys is `key_actions`'s. Split out of `key_tree.rs`.

use super::key_actions::{
    clear_favorites, clear_recent_keys, delete_key, delete_picks, edit_key_tag, export_folder, export_key,
    export_picks, persist_folder, persist_picks, set_ttl_folder, set_ttl_key, set_ttl_picks, tag_picks,
    toggle_favorite,
};
use super::*;
use crate::states::ModuleTypeId;

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
    /// One key's TTL, from its row menu (#226): `SetTtlKey` asks for it,
    /// `PersistKey` takes it away at once.
    SetTtlKey(SharedString),
    PersistKey(SharedString),
    RefreshFolder(SharedString),
    CollapseAllKeys,
    ToggleMultiSelectMode,
    /// Drop the multi-selection without touching the mode — the ✕ on the
    /// count chip in the key bar.
    ClearSelection,
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
    /// Sibling order (`KeySort::as_str` wire id).
    SetSort(SharedString),
    /// One row per key with its full name, instead of folders.
    ToggleFlatView,
    /// Read the keyword box as a regex. Turning it on drops the server-side
    /// `MATCH` and the query mode with it: the scan lists every name and
    /// keeps the ones the pattern matches.
    ToggleRegexMode,
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
        self.placeholders.follow(window, cx);
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
            .on_action(cx.listener(on_query_mode))
            .on_action(cx.listener(on_type_filter))
            .on_action(cx.listener(on_tree_action))
            .on_action(cx.listener(on_editor_action))
    }
}

/// A query mode picked: saved for the server, and the scan's from now on.
fn on_query_mode(this: &mut ZedisKeyTree, e: &QueryMode, _window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
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
}

/// A type picked in the filter menu: the server state's filter.
fn on_type_filter(this: &mut ZedisKeyTree, e: &KeyTypeFilter, _window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let filter = match e {
        KeyTypeFilter::All => None,
        KeyTypeFilter::String => Some(KeyType::String),
        KeyTypeFilter::List => Some(KeyType::List),
        KeyTypeFilter::Set => Some(KeyType::Set),
        KeyTypeFilter::Zset => Some(KeyType::Zset),
        KeyTypeFilter::Hash => Some(KeyType::Hash),
        KeyTypeFilter::Stream => Some(KeyType::Stream),
        KeyTypeFilter::Json => Some(KeyType::Json),
        KeyTypeFilter::TimeSeries => Some(KeyType::TimeSeries),
        KeyTypeFilter::Vectorset => Some(KeyType::Vectorset),
        // Representative kind — `matches_filter` treats any
        // probabilistic filter as matching every sketch kind.
        KeyTypeFilter::Probabilistic => Some(KeyType::Probabilistic(ProbKind::Bloom)),
        KeyTypeFilter::Module(raw) => Some(KeyType::Module(ModuleTypeId::from_raw(*raw))),
    };
    this.server_state
        .update(cx, |state, cx| state.set_type_filter(filter, cx));
}

/// Every action of the tree's menus, keys and bar. What changes how the
/// tree is read is here; what does something to keys is `key_actions`'s.
fn on_tree_action(this: &mut ZedisKeyTree, e: &KeyTreeAction, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    match e {
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
        KeyTreeAction::ClearSelection => {
            this.key_tree_list_state.update(cx, |state, cx| {
                state.delegate_mut().clear_selection();
                cx.notify();
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
        KeyTreeAction::EditKeyTag(key) => edit_key_tag(this, key, window, cx),
        KeyTreeAction::BatchTagSelectedKeys => tag_picks(this, window, cx),
        KeyTreeAction::SetTagFilter(color_name) => {
            let new_filter = if color_name.is_empty() {
                None
            } else {
                TagColor::from_name(color_name.as_ref())
            };
            this.set_tag_filter(new_filter, cx);
        }
        KeyTreeAction::SetTtlFilter(id) => {
            let new_filter = if id.is_empty() {
                TtlFilter::All
            } else {
                TtlFilter::from_name(id.as_ref())
            };
            this.set_ttl_filter(new_filter, cx);
        }
        KeyTreeAction::SetSort(id) => {
            let new_sort = KeySort::from_name(id.as_ref());
            if this.state.sort != new_sort {
                this.state.sort = new_sort;
                this.update_key_tree(true, cx);
            }
        }
        KeyTreeAction::ToggleFlatView => {
            this.state.flat_view = !this.state.flat_view;
            this.update_key_tree(true, cx);
        }
        KeyTreeAction::ToggleRegexMode => {
            let on = !this.state.regex_mode;
            this.state.regex_mode = on;
            // The server side has to agree: with a regex there is no
            // glob to send, so the scan asks for every name and
            // narrows the pages by the pattern as they come back.
            this.server_state
                .update(cx, |state, cx| state.set_regex_keyword(on, cx));
            if this.state.keyword.is_empty() {
                this.update_key_tree(true, cx);
            } else {
                // A different scan target: re-scan rather than
                // re-filter what the old pattern happened to load.
                this.handle_filter(true, cx);
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
            // The folder's scan prefix, from the same function
            // RefreshFolder uses — with the configured separator,
            // which a hard-coded `:` ignored.
            let prefix = this.server_state.read(cx).folder_prefix(id.as_str());
            cx.write_to_clipboard(ClipboardItem::new_string(prefix));
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
        KeyTreeAction::ToggleFavoriteKey(key) => toggle_favorite(this, key, cx),
        KeyTreeAction::ClearFavorites => clear_favorites(this, cx),
        KeyTreeAction::SelectRecentKey(key) => {
            this.select_item(key.clone(), false, false, false, cx);
        }
        KeyTreeAction::ClearRecentKeys => clear_recent_keys(this, cx),
        KeyTreeAction::DeleteMultipleKeys => delete_picks(this, window, cx),
        KeyTreeAction::DeleteKey(id) => delete_key(this, id, window, cx),
        KeyTreeAction::RefreshFolder(id) => {
            let id = id.clone();
            this.server_state.update(cx, |state, cx| {
                let prefix = state.folder_prefix(&id);
                state.refresh_prefix(prefix.into(), cx);
            });
        }
        KeyTreeAction::DeleteFolder(id) => {
            folder_delete::open_folder_delete_dialog(this, id.clone(), window, cx);
        }
        KeyTreeAction::PersistMultipleKeys => persist_picks(this, window, cx),
        KeyTreeAction::PersistFolder(id) => persist_folder(this, id, window, cx),
        KeyTreeAction::SetTtlMultipleKeys => set_ttl_picks(this, window, cx),
        KeyTreeAction::SetTtlFolder(id) => set_ttl_folder(this, id, window, cx),
        KeyTreeAction::SetTtlKey(id) => set_ttl_key(this, id, window, cx),
        // At once, as the editor's own Remove TTL is: it destroys
        // nothing, and the row says what it did.
        KeyTreeAction::PersistKey(id) => {
            let keys = vec![id.clone()];
            this.server_state
                .update(cx, |state, cx| state.batch_set_ttl_keys(keys, None, None, cx));
        }
        KeyTreeAction::ExportSelectedKeys => export_picks(this, window, cx),
        KeyTreeAction::ExportFolder(folder) => export_folder(this, folder, window, cx),
        KeyTreeAction::ExportKey(id) => export_key(this, id, window, cx),
    }
}

/// The editor's ⌘F and ⌘⌫, where the tree has the focus; the rest go on.
fn on_editor_action(
    this: &mut ZedisKeyTree,
    event: &EditorAction,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    match event {
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
                !delegate.selected_items.is_empty()
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, Role, TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};

    /// A tree on `server`, which the scratch configuration does not know —
    /// or on none, where what the tree saves per server (the query mode,
    /// the refresh) stays in memory: saving it is a file written on a thread
    /// of its own, which a test's scheduler does not drive.
    fn open_tree(
        cx: &mut TestAppContext,
        server: Option<&str>,
    ) -> (AnyWindowHandle, Entity<ZedisKeyTree>, Entity<ZedisServerState>) {
        let server_state = cx.new(|_| ZedisServerState::default());
        if let Some(server) = server {
            server_state.update(cx, |state, cx| state.select(server.to_string().into(), 0, cx));
        }
        cx.run_until_parked();
        let mut tree = None;
        let handle = cx.open_window(size(px(900.), px(720.)), |window, cx| {
            let view = cx.new(|cx| ZedisKeyTree::new(server_state.clone(), window, cx));
            tree = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        (handle.into(), tree.expect("the window built the tree"), server_state)
    }

    /// Send `action` the way the tree's menus and keys do: from inside it.
    fn send(cx: &mut TestAppContext, handle: AnyWindowHandle, tree: &Entity<ZedisKeyTree>, action: impl Action) {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let focus = tree.read(cx).focus_handle.clone();
            focus.focus(window, cx);
            window.dispatch_action(Box::new(action), cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
    }

    /// The dialog `action` opens, as far as its controls tell — closed again.
    fn dialog_of(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        tree: &Entity<ZedisKeyTree>,
        action: impl Action,
    ) -> &'static str {
        // The keyword box is one text field; a dialog that asks for a TTL
        // adds its own.
        let fields = |window: &Window| window.find_all_by_role(Role::TextInput).len();
        let before = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                fields(window)
            })
            .expect("the window is open");
        send(cx, handle, tree, action);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let kind = if !window.has_active_dialog(cx) {
                "none"
            } else if window.try_find("ktd-swatch-none").is_some() {
                "tag"
            } else if window.try_find("bktd-swatch-none").is_some() {
                "tags of the picks"
            } else if fields(window) > before {
                "ttl"
            } else if window.try_find("zedis-dialog-ok").is_some() {
                "confirm"
            } else {
                "alert"
            };
            window.close_all_dialogs(cx);
            kind
        })
        .expect("the window is open")
    }

    fn tree_state<T>(cx: &mut TestAppContext, tree: &Entity<ZedisKeyTree>, read: impl FnOnce(&KeyTreeState) -> T) -> T {
        tree.read_with(cx, |tree, _| read(&tree.state))
    }

    fn pick(cx: &mut TestAppContext, tree: &Entity<ZedisKeyTree>, keys: &[&str]) {
        tree.update(cx, |tree, cx| {
            tree.key_tree_list_state.update(cx, |list, _| {
                let delegate = list.delegate_mut();
                delegate.clear_selection();
                for (row, key) in keys.iter().enumerate() {
                    delegate.set_pick((*key).into(), true, row);
                }
            });
        });
    }

    /// What each of the tree's own actions does to the tree and its server
    /// state, the action wiring's filters, switches and selection.
    #[gpui::test]
    fn the_trees_actions_set_what_they_name(cx: &mut TestAppContext) {
        use KeyTreeAction as A;
        init_ui(cx);
        let (handle, tree, server_state) = open_tree(cx, None);

        send(cx, handle, &tree, QueryMode::Prefix);
        assert_eq!(server_state.read_with(cx, |s, _| s.query_mode()), QueryMode::Prefix);
        assert_eq!(tree_state(cx, &tree, |s| s.query_mode), QueryMode::Prefix);

        send(cx, handle, &tree, KeyTypeFilter::Hash);
        assert_eq!(server_state.read_with(cx, |s, _| s.type_filter()), Some(KeyType::Hash));
        send(cx, handle, &tree, KeyTypeFilter::Probabilistic);
        assert_eq!(
            server_state.read_with(cx, |s, _| s.type_filter()),
            Some(KeyType::Probabilistic(ProbKind::Bloom))
        );
        send(cx, handle, &tree, KeyTypeFilter::All);
        assert_eq!(server_state.read_with(cx, |s, _| s.type_filter()), None);

        send(cx, handle, &tree, A::AutoRefresh(5));
        assert_eq!(tree_state(cx, &tree, |s| s.refresh_interval_sec), 5);
        send(cx, handle, &tree, A::AutoRefresh(0));

        send(cx, handle, &tree, A::SetTagFilter("red".into()));
        assert_eq!(tree_state(cx, &tree, |s| s.selected_tag_filter), Some(TagColor::Red));
        send(cx, handle, &tree, A::SetTagFilter("".into()));
        assert_eq!(tree_state(cx, &tree, |s| s.selected_tag_filter), None);

        send(cx, handle, &tree, A::SetTtlFilter(TtlFilter::Lt1h.as_str().into()));
        assert_eq!(tree_state(cx, &tree, |s| s.selected_ttl_filter), TtlFilter::Lt1h);
        send(cx, handle, &tree, A::SetTtlFilter("".into()));
        assert_eq!(tree_state(cx, &tree, |s| s.selected_ttl_filter), TtlFilter::All);

        let other_sort = KeySort::ALL
            .into_iter()
            .find(|sort| *sort != tree_state(cx, &tree, |s| s.sort))
            .expect("more than one order");
        send(cx, handle, &tree, A::SetSort(other_sort.as_str().into()));
        assert_eq!(tree_state(cx, &tree, |s| s.sort), other_sort);

        send(cx, handle, &tree, A::ToggleFlatView);
        assert!(tree_state(cx, &tree, |s| s.flat_view));
        send(cx, handle, &tree, A::ToggleRegexMode);
        assert!(tree_state(cx, &tree, |s| s.regex_mode) && server_state.read_with(cx, |s, _| s.regex_keyword()));

        send(cx, handle, &tree, A::Search("user:".into()));
        assert_eq!(
            tree.read_with(cx, |tree, cx| tree.keyword_state.read(cx).value()),
            "user:"
        );

        let multi = |cx: &mut TestAppContext| {
            tree.read_with(cx, |tree, cx| {
                let delegate = tree.key_tree_list_state.read(cx).delegate();
                (delegate.enabled_multiple_selection, delegate.selected_items.len())
            })
        };
        send(cx, handle, &tree, A::ToggleMultiSelectMode);
        assert_eq!(multi(cx), (true, 0));
        pick(cx, &tree, &["a", "b"]);
        send(cx, handle, &tree, A::ClearSelection);
        assert_eq!(multi(cx), (true, 0), "the picks go, the mode stays");

        send(cx, handle, &tree, A::SelectFavoriteKey("fav".into()));
        assert_eq!(server_state.read_with(cx, |s, _| s.key()), Some("fav".into()));
        send(cx, handle, &tree, A::RenameKey("renamed".into()));
        assert_eq!(server_state.read_with(cx, |s, _| s.key()), Some("renamed".into()));

        send(cx, handle, &tree, A::CopyKeyName("k".into()));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("k".to_string())
        );
        send(cx, handle, &tree, A::CopyFolderPrefix("user".into()));
        let prefix = server_state.read_with(cx, |s, _| s.folder_prefix("user"));
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some(prefix));
    }

    /// The actions that ask before they act open their question — the
    /// picks' only when there are picks — and Remove TTL on one key acts at
    /// once; ⌘⌫ asks for the picks or the open key, ⌘F goes to the keyword.
    #[gpui::test]
    fn the_trees_actions_ask_what_they_have_to(cx: &mut TestAppContext) {
        use KeyTreeAction as A;
        init_ui(cx);
        let (handle, tree, server_state) = open_tree(cx, Some("scratch"));

        assert_eq!(dialog_of(cx, handle, &tree, A::SetTtlMultipleKeys), "none", "no picks");
        assert_eq!(dialog_of(cx, handle, &tree, A::PersistMultipleKeys), "none", "no picks");
        assert_eq!(
            dialog_of(cx, handle, &tree, A::BatchTagSelectedKeys),
            "none",
            "no picks"
        );

        pick(cx, &tree, &["a", "b"]);
        assert_eq!(dialog_of(cx, handle, &tree, A::DeleteMultipleKeys), "confirm");
        assert_eq!(dialog_of(cx, handle, &tree, A::PersistMultipleKeys), "alert");
        assert_eq!(dialog_of(cx, handle, &tree, A::SetTtlMultipleKeys), "ttl");
        assert_eq!(
            dialog_of(cx, handle, &tree, A::BatchTagSelectedKeys),
            "tags of the picks"
        );
        assert_eq!(
            dialog_of(cx, handle, &tree, EditorAction::Delete),
            "confirm",
            "⌘⌫ on picks"
        );

        assert_eq!(dialog_of(cx, handle, &tree, A::DeleteKey("k".into())), "confirm");
        assert_eq!(dialog_of(cx, handle, &tree, A::SetTtlKey("k".into())), "ttl");
        assert_eq!(dialog_of(cx, handle, &tree, A::PersistKey("k".into())), "none");
        assert_eq!(dialog_of(cx, handle, &tree, A::EditKeyTag("k".into())), "tag");
        assert_eq!(dialog_of(cx, handle, &tree, A::PersistFolder("user".into())), "alert");
        assert_eq!(
            dialog_of(cx, handle, &tree, A::DeleteFolder("user".into())),
            "confirm",
            "a folder's delete is the destructive shape, as one key's is"
        );
        assert_eq!(dialog_of(cx, handle, &tree, A::SetTtlFolder("user".into())), "ttl");

        pick(cx, &tree, &[]);
        server_state.update(cx, |state, cx| state.select_key("open".into(), cx));
        cx.run_until_parked();
        assert_eq!(
            dialog_of(cx, handle, &tree, EditorAction::Delete),
            "confirm",
            "⌘⌫ on the open key"
        );

        send(cx, handle, &tree, EditorAction::Search);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let keyword = tree.read(cx).keyword_state.read(cx).focus_handle(cx);
            assert!(keyword.is_focused(window), "⌘F is the keyword box");
        })
        .expect("the window is open");
    }

    /// On a database larger than a single page, the initial scan stops with
    /// `KeyScanPaged` (not `KeyScanFinished`): the tree still records
    /// `last_scan` so `⌘R` / `RefreshAll` keeps the expanded folders instead
    /// of treating the refresh as a brand-new query (#148).
    #[gpui::test]
    fn a_paged_scan_round_settles_so_refresh_keeps_the_expanded_folders(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, tree, server_state) = open_tree(cx, None);

        server_state.update(cx, |_, cx| {
            cx.emit(ServerEvent::KeyScanPaged);
            cx.emit(ServerEvent::KeyTreeUpdated);
        });
        cx.run_until_parked();
        assert_eq!(
            tree_state(cx, &tree, |s| s.last_scan.clone()),
            Some((SharedString::default(), QueryMode::All))
        );

        tree.update(cx, |tree, _| {
            tree.state.expanded_items.insert("user".into());
        });
        send(cx, handle, &tree, KeyTreeAction::RefreshAll);
        assert!(
            tree_state(cx, &tree, |s| s.expanded_items.contains("user")),
            "refresh after a paged scan preserves expanded folders"
        );
    }
}

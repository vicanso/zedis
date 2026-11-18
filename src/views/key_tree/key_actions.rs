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

//! What the tree's menus do to keys — the picks, one key, a folder:
//! delete, TTL, tag and note, export, and the favorites and recent lists.

use super::*;
use crate::db::TRASH_MAX_BULK_KEYS;
use crate::states::recycles_deleted_keys;

/// The keys picked in the tree, in no particular order.
pub(super) fn picked_keys(this: &ZedisKeyTree, cx: &mut App) -> Vec<SharedString> {
    this.key_tree_list_state.update(cx, |state, _cx| {
        state
            .delegate()
            .selected_items
            .iter()
            .cloned()
            .collect::<Vec<SharedString>>()
    })
}

/// Tag & note of one key: the dialog, and the row patched from what it saved.
pub(super) fn edit_key_tag(
    this: &mut ZedisKeyTree,
    key: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
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

/// One tag for every pick; their notes are left as they are.
pub(super) fn tag_picks(this: &mut ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keys = picked_keys(this, cx);
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

/// Star the key, or take its star away.
pub(super) fn toggle_favorite(this: &mut ZedisKeyTree, key: &SharedString, cx: &mut Context<ZedisKeyTree>) {
    let server_id = this.server_state.read(cx).server_id().to_string();
    let key = key.clone();
    cx.spawn(async move |this, cx| {
        let _ = cx
            .background_spawn(async move { get_favorites_manager().toggle_record(&server_id, key.as_ref()) })
            .await;
        // The editor's star reads the stored list while it
        // renders the server state: repaint what reads it.
        let _ = this.update(cx, |this, cx| this.server_state.update(cx, |_, cx| cx.notify()));
    })
    .detach();
}

/// Forget the server's starred keys.
pub(super) fn clear_favorites(this: &mut ZedisKeyTree, cx: &mut Context<ZedisKeyTree>) {
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

/// Forget the keys opened lately on this server and database.
pub(super) fn clear_recent_keys(this: &mut ZedisKeyTree, cx: &mut Context<ZedisKeyTree>) {
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

/// Delete the picks, once the question about them is answered.
pub(super) fn delete_picks(this: &mut ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keys = picked_keys(this, cx);
    let server_state = this.server_state.clone();
    let store = cx.global::<ZedisGlobalStore>().read(cx);
    let locale = store.locale().to_string();
    let soft_delete = store.soft_delete();
    let prompt = t!("key_tree.delete_keys_prompt", keys = keys.join(", "), locale = locale);
    // Whether they come back, as the one-key delete says (#195).
    let consequence = if recycles_deleted_keys(soft_delete, keys.len()) {
        t!("key_tree.delete_keys_body_bin", locale = locale)
    } else if soft_delete {
        t!(
            "key_tree.delete_keys_body_too_many",
            max = TRASH_MAX_BULK_KEYS,
            locale = locale
        )
    } else {
        t!("danger.delete_key_body", locale = locale)
    };
    let server_id = this.server_state.read(cx).server_id().to_string();
    let text = escalate_dangerous_body(cx, &server_id, format!("{prompt}\n\n{consequence}"));

    // The destructive shape: Delete is the danger button and
    // Return answers Cancel, as for one key.
    ZedisDialog::new_alert(i18n_key_tree(cx, "delete_keys_title"), text)
        .danger()
        .ok_text(i18n_common(cx, "delete"))
        .cancel_text(i18n_common(cx, "cancel"))
        .on_ok(move |_, _, cx| {
            server_state.update(cx, |state, cx| {
                state.unlink_keys(keys.clone(), cx);
            });
            true
        })
        .open(window, cx);
}

/// Delete one key, once its question is answered.
pub(super) fn delete_key(
    this: &mut ZedisKeyTree,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let id = id.clone();
    let server_state = this.server_state.clone();
    let server_id = this.server_state.read(cx).server_id().to_string();
    let name = id.clone();
    confirm_delete_key(&server_id, &name, window, cx, move |_, cx| {
        server_state.update(cx, |state, cx| {
            state.delete_key(id.clone(), cx);
        });
    });
}

/// Take the picks' TTLs away, once asked.
pub(super) fn persist_picks(this: &mut ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keys = picked_keys(this, cx);
    if keys.is_empty() {
        return;
    }
    let server_state = this.server_state.clone();
    // The shared buttons say "Delete", and this deletes
    // nothing: the button is named for the PERSIST it sends.
    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
        .button_props(dialog_button_props(cx).ok_text(i18n_key_tree(cx, "persist_title")))
        .on_ok(move |_, _, cx| {
            server_state.update(cx, |state, cx| state.batch_set_ttl_keys(keys.clone(), None, None, cx));
            true
        })
        .open(window, cx);
}

/// Take the TTL of every key under a folder away, once asked.
pub(super) fn persist_folder(
    this: &mut ZedisKeyTree,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let id = id.clone();
    let server_state = this.server_state.clone();
    // The shared buttons say "Delete", and this deletes
    // nothing: the button is named for the PERSIST it sends.
    ZedisDialog::new_alert(i18n_key_tree(cx, "persist_title"), i18n_key_tree(cx, "persist_prompt"))
        .button_props(dialog_button_props(cx).ok_text(i18n_key_tree(cx, "persist_title")))
        .on_ok(move |_, _, cx| {
            server_state.update(cx, |state, cx| state.batch_set_ttl_folder(id.clone(), None, None, cx));
            true
        })
        .open(window, cx);
}

/// One TTL for every pick, as the dialog asks for it.
pub(super) fn set_ttl_picks(this: &mut ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keys = picked_keys(this, cx);
    if keys.is_empty() {
        return;
    }
    let server_state = this.server_state.clone();
    let show_condition = server_state.read(cx).supports_expire_conditions();
    open_ttl_dialog(
        i18n_key_tree(cx, "batch_ttl_prompt"),
        show_condition,
        move |secs, condition, cx| {
            server_state.update(cx, |state, cx| {
                state.batch_set_ttl_keys(keys.clone(), Some(secs), condition, cx)
            });
        },
        window,
        cx,
    );
}

/// One TTL for every key under a folder, as the dialog asks for it.
pub(super) fn set_ttl_folder(
    this: &mut ZedisKeyTree,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let id = id.clone();
    let server_state = this.server_state.clone();
    let show_condition = server_state.read(cx).supports_expire_conditions();
    open_ttl_dialog(
        i18n_key_tree(cx, "batch_ttl_prompt"),
        show_condition,
        move |secs, condition, cx| {
            server_state.update(cx, |state, cx| {
                state.batch_set_ttl_folder(id.clone(), Some(secs), condition, cx)
            });
        },
        window,
        cx,
    );
}

/// One key's TTL, as the dialog asks for it.
pub(super) fn set_ttl_key(
    this: &mut ZedisKeyTree,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let keys = vec![id.clone()];
    let server_state = this.server_state.clone();
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let prompt = t!("key_tree.key_ttl_prompt", key = id, locale = locale).to_string();
    open_ttl_dialog(
        prompt.into(),
        false,
        move |secs, _, cx| {
            server_state.update(cx, |state, cx| {
                state.batch_set_ttl_keys(keys.clone(), Some(secs), None, cx)
            });
        },
        window,
        cx,
    );
}

/// Export the picks.
pub(super) fn export_picks(this: &mut ZedisKeyTree, window: &mut Window, cx: &mut Context<ZedisKeyTree>) {
    let keys = picked_keys(this, cx);
    this.export_keys(keys, ExportSource::Selection, window, cx);
}

/// Export the loaded keys under a folder.
pub(super) fn export_folder(
    this: &mut ZedisKeyTree,
    folder: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    let server_state = this.server_state.read(cx);
    // The configured separator: with `/` or `.`, a `:`
    // prefix matched nothing and the export silently
    // never opened.
    let prefix = server_state.folder_prefix(folder.as_str());
    let keys: Vec<SharedString> = server_state
        .keys()
        .keys()
        .filter(|k| k.as_str() == folder.as_str() || k.as_str().starts_with(&prefix))
        .cloned()
        .collect();
    this.export_keys(keys, ExportSource::Loaded, window, cx);
}

/// Export one key.
pub(super) fn export_key(
    this: &mut ZedisKeyTree,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<ZedisKeyTree>,
) {
    this.export_keys(vec![id.clone()], ExportSource::Selection, window, cx);
}

impl ZedisKeyTree {
    /// Export `keys` of the tree's server and database: the migration window
    /// on the desktop, a download in a page (`open_key_export`).
    fn export_keys(&self, keys: Vec<SharedString>, source: ExportSource, window: &mut Window, cx: &mut App) {
        if keys.is_empty() {
            return;
        }
        let server_state = self.server_state.read(cx);
        let server_id: SharedString = server_state.server_id().to_string().into();
        let db = server_state.db();
        let server_name: SharedString = get_server(server_id.as_str())
            .map(|s| s.name.into())
            .unwrap_or_else(|_| server_id.clone());
        open_key_export(server_id, server_name, db, keys, source, window, cx);
    }
}

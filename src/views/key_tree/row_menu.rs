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

//! The right-click menu of a key tree row: what the picks, a folder or a
//! key can have done to them, as the capability matrix and the
//! connection's access allow.

use super::*;
use gpui_kit::component::menu::PopupMenu;

/// What a row's menu is built from, taken when the row is drawn.
pub(super) struct RowMenu {
    /// The key, or the folder's prefix.
    pub(super) id: SharedString,
    pub(super) is_folder: bool,
    /// Whether the row is one of the picks.
    pub(super) selected: bool,
    pub(super) selected_items_count: usize,
    pub(super) readonly: bool,
    /// Whether the key may still expire: Remove TTL is offered then.
    pub(super) may_expire: bool,
}

impl RowMenu {
    /// The row's menu, top to bottom.
    pub(super) fn build(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let multi_selection_count = if self.selected { self.selected_items_count } else { 0 };
        let menu = self.operations(menu, cx);
        let menu = self.export(menu, multi_selection_count, cx);
        let menu = self.tag_and_note(menu, multi_selection_count, cx);
        self.copy_rename_favorite(menu, multi_selection_count, cx)
    }

    /// What the picks, the folder or the key can have done to them.
    fn operations(&self, mut menu: PopupMenu, cx: &App) -> PopupMenu {
        let &Self {
            ref id,
            is_folder,
            selected,
            selected_items_count,
            readonly,
            may_expire,
        } = self;
        // Capability matrix (`connection::Capability`) is the
        // source of truth for what survives read-only mode.
        if selected && selected_items_count > 1 {
            if Capability::DeleteKeys.allowed(readonly) {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let text = t!(
                    "key_tree.delete_keys_tooltip",
                    count = selected_items_count,
                    locale = locale
                );
                menu = menu.menu_element_with_icon(
                    CustomIconName::ListX,
                    Box::new(KeyTreeAction::DeleteMultipleKeys),
                    move |_, _cx| Label::new(text.clone()),
                );
            }
            if Capability::SetTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::SetTtlMultipleKeys),
                    move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                );
            }
            if Capability::PersistTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::PersistMultipleKeys),
                    move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                );
            }
        } else if is_folder {
            if Capability::RefreshFolder.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::RotateCw,
                    Box::new(KeyTreeAction::RefreshFolder(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "refresh_folder_tooltip")),
                );
            }
            if Capability::DeleteFolder.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::X,
                    Box::new(KeyTreeAction::DeleteFolder(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "delete_folder_tooltip")),
                );
            }
            if Capability::SetTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::SetTtlFolder(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                );
            }
            if Capability::PersistTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::PersistFolder(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "persist_tooltip")),
                );
            }
        } else {
            if Capability::DeleteKey.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::X,
                    Box::new(KeyTreeAction::DeleteKey(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "delete_key_tooltip")),
                );
            }
            // What a folder and a selection offer, for one
            // key (#226).
            if Capability::SetTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::SetTtlKey(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "set_ttl_tooltip")),
                );
            }
            if may_expire && Capability::PersistTtl.allowed(readonly) {
                menu = menu.menu_element_with_icon(
                    CustomIconName::Clock3,
                    Box::new(KeyTreeAction::PersistKey(id.clone())),
                    move |_, cx| Label::new(i18n_key_tree(cx, "persist_title")),
                );
            }
        }
        menu
    }

    /// The export of the picks, the folder or the key.
    fn export(&self, mut menu: PopupMenu, multi_selection_count: usize, cx: &App) -> PopupMenu {
        let &Self {
            ref id,
            is_folder,
            readonly,
            ..
        } = self;
        // Export is a Redis read + a file — always OK where
        // a file can be handed over: saved, or downloaded.
        if saves_files() && Capability::ExportKeys.allowed(readonly) {
            if multi_selection_count > 0 {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let text = t!(
                    "key_tree.export_selected_tooltip",
                    count = multi_selection_count,
                    locale = locale
                );
                menu = menu.menu_element_with_icon(
                    CustomIconName::Save,
                    Box::new(KeyTreeAction::ExportSelectedKeys),
                    move |_, _cx| Label::new(text.clone()),
                );
            } else if is_folder {
                let folder_id = id.clone();
                menu = menu.menu_element_with_icon(
                    CustomIconName::Save,
                    Box::new(KeyTreeAction::ExportFolder(folder_id)),
                    move |_, cx| Label::new(i18n_key_tree(cx, "export_folder_tooltip")),
                );
            } else {
                let key_id = id.clone();
                menu = menu.menu_element_with_icon(
                    CustomIconName::Save,
                    Box::new(KeyTreeAction::ExportKey(key_id)),
                    move |_, cx| Label::new(i18n_key_tree(cx, "export_key_tooltip")),
                );
            }
        }
        menu
    }

    /// The tag and note of the key, or the tag of the picks.
    fn tag_and_note(&self, mut menu: PopupMenu, multi_selection_count: usize, cx: &App) -> PopupMenu {
        let &Self {
            ref id,
            is_folder,
            readonly,
            ..
        } = self;
        // Import keys lives under Tools (status bar) —
        // it targets the current server/db, not a tree
        // prefix, so the context menu no longer offers it.
        // Tag & note — local redb only, or in the browser
        // the bridge's, seen by everyone who sees the
        // server and so left only by an account that may
        // write there (ADR 17).
        // Multi-select → batch colour only (notes preserved).
        // Single leaf → full tag + note dialog.
        let notes_writable = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .selected_server()
            .is_none_or(|(server_id, _)| !account_is_read_only_on(server_id.as_ref()));
        if Capability::EditLocalMetadata.allowed(readonly) && notes_writable {
            if multi_selection_count > 1 {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let text = t!(
                    "key_tag.batch_menu_label",
                    count = multi_selection_count,
                    locale = locale
                );
                menu = menu.menu_element_with_icon(
                    CustomIconName::SwatchBook,
                    Box::new(KeyTreeAction::BatchTagSelectedKeys),
                    move |_, _cx| Label::new(text.clone()),
                );
            } else if !is_folder && multi_selection_count == 0 {
                let tag_key = id.clone();
                menu = menu.menu_element_with_icon(
                    CustomIconName::FilePenLine,
                    Box::new(KeyTreeAction::EditKeyTag(tag_key)),
                    move |_, cx| Label::new(i18n_key_tag(cx, "edit_menu_label")),
                );
            }
        }
        menu
    }

    /// What one row offers alone: its name or prefix copied, a key
    /// renamed or starred.
    fn copy_rename_favorite(&self, mut menu: PopupMenu, multi_selection_count: usize, cx: &App) -> PopupMenu {
        let &Self {
            ref id,
            is_folder,
            readonly,
            ..
        } = self;
        // Single-row affordances: copy name / prefix,
        // rename, favorite toggle.
        if multi_selection_count == 0 {
            if Capability::CopyToClipboard.allowed(readonly) {
                if is_folder {
                    menu = menu.menu_element_with_icon(
                        IconName::Copy,
                        Box::new(KeyTreeAction::CopyFolderPrefix(id.clone())),
                        move |_, cx| Label::new(i18n_key_tree(cx, "copy_prefix_tooltip")),
                    );
                } else {
                    menu = menu.menu_element_with_icon(
                        IconName::Copy,
                        Box::new(KeyTreeAction::CopyKeyName(id.clone())),
                        move |_, cx| Label::new(i18n_key_tree(cx, "copy_key_tooltip")),
                    );
                    menu = menu.menu_element_with_icon(
                        IconName::ExternalLink,
                        Box::new(KeyTreeAction::CopyKeyLink(id.clone())),
                        move |_, cx| Label::new(i18n_key_tree(cx, "copy_key_link")),
                    );
                }
            }
            if !is_folder {
                if Capability::RenameKey.allowed(readonly) {
                    menu = menu.menu_element_with_icon(
                        CustomIconName::FilePenLine,
                        Box::new(KeyTreeAction::RenameKey(id.clone())),
                        move |_, cx| Label::new(i18n_key_tree(cx, "rename_key_tooltip")),
                    );
                }
                if Capability::EditLocalMetadata.allowed(readonly) {
                    // Star state comes from the local
                    // favorites store, keyed by server.
                    let is_favorited = cx
                        .global::<ZedisGlobalStore>()
                        .read(cx)
                        .selected_server()
                        .map(|(server_id, _)| {
                            get_favorites_manager()
                                .records(server_id.as_ref())
                                .unwrap_or_default()
                                .iter()
                                .any(|k| k.as_str() == id.as_ref())
                        })
                        .unwrap_or(false);
                    let (icon, label_key) = if is_favorited {
                        (IconName::StarFill, "remove_favorite_tooltip")
                    } else {
                        (IconName::Star, "add_favorite_tooltip")
                    };
                    menu = menu.menu_element_with_icon(
                        icon,
                        Box::new(KeyTreeAction::ToggleFavoriteKey(id.clone())),
                        move |_, cx| Label::new(i18n_editor(cx, label_key)),
                    );
                }
            }
        }
        menu
    }
}

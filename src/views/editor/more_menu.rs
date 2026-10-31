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

//! The key bar's "…" menu: the value's less frequent actions — view,
//! file, find, history — then the key's own.

use super::*;
use crate::helpers::has_file_dialogs;
use gpui::App;
use gpui_kit::component::menu::PopupMenu;

/// The keystroke gpui-component binds to Replace in its input context, shown
/// next to the menu entry so the shortcut can be learned from it.
#[cfg(target_os = "macos")]
const REPLACE_KEYSTROKE: &str = "cmd-shift-f";
#[cfg(not(target_os = "macos"))]
const REPLACE_KEYSTROKE: &str = "ctrl-h";

/// What the "…" menu offers, decided when the bar is drawn
/// (`ZedisEditor::more_actions`).
pub(super) struct MoreActions {
    pub(super) bitmap_item: bool,
    pub(super) export_item: bool,
    pub(super) import_item: bool,
    pub(super) find_replace_item: bool,
    pub(super) json_items: bool,
    pub(super) change_log_item: bool,
    pub(super) diff_item: bool,
    pub(super) diff_history: Vec<(i64, usize)>,
    pub(super) key_ops: Vec<KeyOpAction>,
    pub(super) rename_item: bool,
    pub(super) copy_item: bool,
    pub(super) diff_with_server_item: bool,
    pub(super) delete_item: bool,
}

impl MoreActions {
    /// Whether the menu has anything in it, and so a button.
    pub(super) fn offers_anything(&self) -> bool {
        let &Self {
            change_log_item,
            find_replace_item,
            json_items,
            rename_item,
            copy_item,
            bitmap_item,
            export_item,
            import_item,
            diff_item,
            delete_item,
            ref key_ops,
            ..
        } = self;
        change_log_item
            || find_replace_item
            || json_items
            || rename_item
            || copy_item
            || bitmap_item
            || export_item
            || import_item
            || diff_item
            || delete_item
            || !key_ops.is_empty()
    }

    /// The menu, top to bottom.
    pub(super) fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let menu = self.value_items(menu, cx);
        let menu = self.history(menu, window, cx);
        self.key_items(menu, window, cx)
    }

    /// What is done with the value: viewed as a bitmap, saved, read from a file, searched, reformatted, its change log.
    fn value_items(&self, mut menu: PopupMenu, cx: &App) -> PopupMenu {
        let &Self {
            bitmap_item,
            export_item,
            import_item,
            find_replace_item,
            json_items,
            change_log_item,
            ..
        } = self;
        if bitmap_item {
            menu = menu.menu_element_with_icon(
                CustomIconName::Binary,
                Box::new(EditorAction::ViewBitmap),
                move |_, cx| Label::new(i18n_bitmap(cx, "bitmap")),
            );
        }
        if export_item {
            // A page downloads at once, so its item opens
            // nothing and says so without the ellipsis.
            let export_label = if has_file_dialogs() {
                "export_value_tooltip"
            } else {
                "export_value_download"
            };
            menu = menu.menu_element_with_icon(
                CustomIconName::Download,
                Box::new(EditorAction::ExportValue),
                move |_, cx| Label::new(i18n_editor(cx, export_label)),
            );
        }
        if import_item {
            menu = menu.menu_element_with_icon(
                CustomIconName::Upload,
                Box::new(EditorAction::ImportValue),
                move |_, cx| Label::new(i18n_editor(cx, "import_value_tooltip")),
            );
        }
        if find_replace_item {
            let label: SharedString = format!(
                "{} ({})",
                i18n_editor(cx, "find_replace"),
                humanize_keystroke(REPLACE_KEYSTROKE)
            )
            .into();
            menu = menu.menu_element_with_icon(IconName::Search, Box::new(EditorAction::FindReplace), move |_, _cx| {
                Label::new(label.clone())
            });
        }
        if json_items {
            menu = menu.menu_element_with_icon(
                CustomIconName::Braces,
                Box::new(EditorAction::FormatJson),
                move |_, cx| Label::new(i18n_editor(cx, "format_json")),
            );
            menu = menu.menu_element_with_icon(
                CustomIconName::ListChecvronsDownUp,
                Box::new(EditorAction::MinifyJson),
                move |_, cx| Label::new(i18n_editor(cx, "minify_json")),
            );
        }
        if change_log_item {
            menu = menu.menu_element_with_icon(
                CustomIconName::ListCheck,
                Box::new(EditorAction::ChangeLog),
                move |_, cx| Label::new(i18n_editor(cx, "change_log")),
            );
        }
        menu = menu.menu_element_with_icon(
            CustomIconName::Activity,
            Box::new(EditorAction::WatchTimeline),
            move |_, cx| Label::new(i18n_editor(cx, "key_timeline_title")),
        );
        menu
    }

    /// The value's saved versions: one put back into the editor, or compared with what is there.
    fn history(&self, mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            diff_item,
            ref diff_history,
            ..
        } = self;
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
                        submenu = submenu.menu_element(Box::new(EditorAction::LoadHistory(idx_u32)), move |_w, _cx| {
                            Label::new(label.clone())
                        });
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
                        submenu = submenu.menu_element(Box::new(EditorAction::DiffHistory(idx_u32)), move |_w, _cx| {
                            Label::new(label.clone())
                        });
                    }
                    submenu
                },
            );
        }
        menu
    }

    /// What is done with the key: the type's own operations, rename, copy, compare with another server, delete.
    fn key_items(&self, mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let &Self {
            rename_item,
            copy_item,
            delete_item,
            diff_with_server_item,
            bitmap_item,
            export_item,
            import_item,
            diff_item,
            ref key_ops,
            ..
        } = self;
        // Key-level ops (rename / copy / delete) sit below a
        // separator from the value-view actions above.
        if (rename_item || copy_item || delete_item || diff_with_server_item)
            && (bitmap_item || export_item || import_item || diff_item)
        {
            menu = menu.separator();
        }
        // Type-native operations, behind their own submenu
        // so the key-level actions below stay one flat list.
        // Counters and APPEND are ancient; GETEX is 6.2 and
        // the JSON writes are a module's, so each appears
        // only where it exists.
        if !key_ops.is_empty() {
            let ops = key_ops.clone();
            menu = menu.submenu_with_icon(
                Some(Icon::new(CustomIconName::Zap)),
                i18n_key_ops(cx, "menu_tooltip"),
                window,
                cx,
                move |submenu, _window, _cx| {
                    let mut submenu = submenu;
                    for action in &ops {
                        let label_key = key_op_title_key(*action);
                        submenu = submenu
                            .menu_element(Box::new(*action), move |_, cx| Label::new(i18n_key_ops(cx, label_key)));
                    }
                    submenu
                },
            );
        }
        if rename_item {
            menu = menu.menu_element_with_icon(
                CustomIconName::FilePenLine,
                Box::new(EditorAction::Rename),
                move |_, cx| Label::new(i18n_editor(cx, "rename")),
            );
        }
        if copy_item {
            menu = menu.menu_element_with_icon(IconName::Copy, Box::new(EditorAction::CopyTo), move |_, cx| {
                Label::new(i18n_copy(cx, "copy_to"))
            });
        }
        if diff_with_server_item {
            menu = menu.menu_element_with_icon(
                CustomIconName::GitCompareArrows,
                Box::new(EditorAction::DiffWithServer),
                move |_, cx| Label::new(i18n_editor(cx, "diff_with_server")),
            );
        }
        if delete_item {
            menu = menu.menu_element_with_icon(IconName::CircleX, Box::new(EditorAction::Delete), move |_, cx| {
                Label::new(i18n_editor(cx, "delete_key_tooltip"))
            });
        }
        menu
    }
}

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

use gpui::Action;
use gpui::KeyBinding;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum MemuAction {
    Quit,
    About,
}

/// Navigation. `Back` (bound to `escape`) mirrors the "back to editor"
/// button on the tool pages (Metrics, Memory, ACL, Search, ...). It is
/// bound globally, but gpui's deepest-context-wins keybinding
/// resolution means a focused input / open dialog / command palette
/// (each of which consumes `escape` in its own context) is handled
/// first — so `Back` only fires on the bare tool page. The handler
/// (in `main.rs`) is a no-op on Home / Editor / Settings, which have no
/// back affordance.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum NavAction {
    Back,
}

/// Command palette (⌘K). `Toggle` opens it (or closes if already open).
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum PaletteAction {
    Toggle,
}

/// Keyboard-shortcuts reference overlay (⌘/). `Toggle` opens it (or
/// closes if already open). Like `PaletteAction` it is handled by a
/// global, focus-independent handler in `main.rs` so the hotkey works
/// regardless of what is focused.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ShortcutsAction {
    Toggle,
}

/// JSONPath query bar. `AcceptCompletion` is bound to `tab` in the
/// `JsonPathBar` key context: it accepts the highlighted completion
/// when the menu is open, otherwise it propagates so `tab` keeps its
/// normal focus-movement behaviour. Needed because gpui-component
/// binds `tab` → focus-next as an *action* at the Root, which is
/// dispatched before any key-down listener.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum JsonPathAction {
    AcceptCompletion,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum EditorAction {
    Create,
    Save,
    Reload,
    UpdateTtl,
    Cmd,
    Search,
    /// Re-scan the key tree with the current keyword/query mode
    /// (manual on-demand refresh; `cmd-r`).
    ReloadKeyTree,
    AutoRefresh(u32),
    /// Restore the value at the given history index (0 = most recent)
    /// into the bytes editor. The user still has to Save to push to Redis.
    LoadHistory(u32),
    /// Open the side-by-side diff view with the given history index
    /// (0 = most recent) on the left and the current editor value on
    /// the right. Both panes are read-only — user closes diff before
    /// editing again.
    DiffHistory(u32),
    /// Export the current string value's raw bytes to a file.
    ExportValue,
    /// Overwrite the current string value with a chosen file's bytes.
    ImportValue,
    /// Switch the current string into the bitmap grid view.
    ViewBitmap,
    /// Delete the selected key (routes through the confirm dialog).
    Delete,
    /// Open the rename dialog for the selected key.
    Rename,
    /// Open the cross-server "copy to…" dialog for the selected key.
    CopyTo,
    /// Open the cross-server "diff with…" dialog for the selected key.
    DiffWithServer,
}

/// Actions scoped to the side-by-side value diff view.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ValueDiffAction {
    /// Close the diff and return to the editor (mirrors the Close button).
    Close,
}

/// Slow-log panel export actions, dispatched by the toolbar "Export"
/// dropdown and handled by the panel's own `.on_action` (same pattern
/// as `EditorAction`). They export the currently-filtered rows.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum SlowlogAction {
    /// Export the filtered slow-log rows to a CSV file.
    ExportCsv,
    /// Export the filtered slow-log rows to a JSON file.
    ExportJson,
}

pub fn humanize_keystroke(keystroke: &str) -> String {
    let parts = keystroke.split('-');
    let mut display_text = String::new();

    #[cfg(target_os = "macos")]
    let separator = "";
    #[cfg(not(target_os = "macos"))]
    let separator = "+";

    for (i, part) in parts.enumerate() {
        if i > 0 {
            display_text.push_str(separator);
        }

        let symbol = match part {
            "cmd" => {
                #[cfg(target_os = "macos")]
                {
                    "⌘"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "Ctrl"
                }
            }
            "ctrl" => {
                #[cfg(target_os = "macos")]
                {
                    "⌃"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "Ctrl"
                }
            }
            "alt" => {
                #[cfg(target_os = "macos")]
                {
                    "⌥"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "Alt"
                }
            }
            "shift" => {
                #[cfg(target_os = "macos")]
                {
                    "⇧"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "Shift"
                }
            }
            "enter" => "Enter",
            "space" => "Space",
            "escape" => "Esc",
            "backspace" => {
                #[cfg(target_os = "macos")]
                {
                    "⌫"
                }
                #[cfg(not(target_os = "macos"))]
                {
                    "Backspace"
                }
            }
            c => {
                display_text.push_str(&c.to_uppercase());
                continue;
            }
        };
        display_text.push_str(symbol);
    }

    display_text
}

/// One section of the keyboard-shortcuts reference overlay.
pub struct ShortcutGroup {
    /// i18n key (under the `shortcuts.` section) for the group heading.
    pub title_key: &'static str,
    /// `(raw keystroke, i18n key for the human description)`. The
    /// keystroke is rendered through [`humanize_keystroke`] so it shows
    /// the right per-platform symbols (⌘ vs Ctrl, …).
    pub items: &'static [(&'static str, &'static str)],
}

/// Curated, user-facing keyboard-shortcut reference shown by the ⌘/
/// overlay. Deliberately *not* derived from [`new_hot_keys`]: that list
/// also holds context-scoped internal bindings (the `tab` JSONPath
/// completion, the `escape` back/close handlers) that would only confuse
/// users as "global shortcuts", and a raw `KeyBinding` carries no
/// localizable description. Keep this in sync with [`new_hot_keys`] when
/// adding a user-visible binding.
pub fn shortcut_reference() -> &'static [ShortcutGroup] {
    &[
        ShortcutGroup {
            title_key: "group_general",
            items: &[
                ("cmd-k", "command_palette"),
                ("cmd-/", "keyboard_shortcuts"),
                ("cmd-q", "quit"),
            ],
        },
        ShortcutGroup {
            title_key: "group_editor",
            items: &[
                ("cmd-n", "new_key"),
                ("cmd-s", "save"),
                ("cmd-r", "reload_keys"),
                ("cmd-shift-r", "reload_value"),
                ("cmd-t", "update_ttl"),
                ("cmd-backspace", "delete_key"),
                ("cmd-e", "rename_key"),
                ("cmd-f", "search"),
                ("cmd-j", "terminal"),
            ],
        },
        ShortcutGroup {
            title_key: "group_navigation",
            items: &[("escape", "back")],
        },
    ]
}

pub fn new_hot_keys() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", MemuAction::Quit, None),
        KeyBinding::new("cmd-k", PaletteAction::Toggle, None),
        KeyBinding::new("cmd-/", ShortcutsAction::Toggle, None),
        KeyBinding::new("cmd-s", EditorAction::Save, None),
        KeyBinding::new("cmd-r", EditorAction::ReloadKeyTree, None),
        KeyBinding::new("cmd-n", EditorAction::Create, None),
        KeyBinding::new("cmd-t", EditorAction::UpdateTtl, None),
        KeyBinding::new("cmd-j", EditorAction::Cmd, None),
        KeyBinding::new("cmd-f", EditorAction::Search, None),
        // Rename the selected key. ⌘E is free on macOS text inputs (unlike
        // ⌘C/⌘V/⌘X), so it can be a global binding without stealing edit
        // keys; the rename flow routes through its own dialog with an
        // overwrite confirm, so a stray press is safe.
        KeyBinding::new("cmd-e", EditorAction::Rename, None),
        // Delete the selected key. A modifier combo (not bare Backspace) so it
        // can't fire while navigating the tree or typing; still routes through
        // the confirm dialog (with PROD escalation), so a stray press is safe.
        // When a text editor is focused it keeps ⌘⌫ for editing — the global
        // delete only fires when no input consumes it.
        KeyBinding::new("cmd-backspace", EditorAction::Delete, None),
        // Key-tree refresh is the primary, high-frequency action so it
        // owns plain `cmd-r` (above); value reload is the rarer one and
        // takes `cmd-shift-r`.
        KeyBinding::new("cmd-shift-r", EditorAction::Reload, None),
        // Esc on a tool page returns to the editor (mirrors the page's
        // back button). Scoped to the `Workspace` context (the content
        // container) so it never reaches overlays that live outside it —
        // notably the command palette, which is a sibling of the content
        // view and handles Esc itself. Deeper `escape` consumers within
        // the workspace (focused inputs, dialogs) still win.
        KeyBinding::new("escape", NavAction::Back, Some("Workspace")),
        // Esc inside the open value-diff view closes it. Scoped to the
        // `ValueDiff` context, which is deeper than `Workspace`, so it wins
        // over the page-back binding above while the diff is focused.
        KeyBinding::new("escape", ValueDiffAction::Close, Some("ValueDiff")),
        // Scoped to the JSONPath bar so it only overrides `tab` there;
        // the handler propagates when no completion menu is open, so
        // normal focus movement still works.
        KeyBinding::new("tab", JsonPathAction::AcceptCompletion, Some("JsonPathBar")),
    ]
}

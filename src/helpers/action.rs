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

use crate::helpers::keybinding_overrides;
use gpui::Action;
use gpui::KeyBinding;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum MemuAction {
    Quit,
    About,
    /// Close the window via `cmd-w`, mirroring the red close button. On macOS
    /// that hides the app (see `on_window_should_close` in `main.rs`).
    Close,
    /// Reveal the logs directory (`<config_dir>/logs/`) in the OS file manager,
    /// so users can grab logs for bug reports.
    OpenLogs,
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

/// Jump to a workspace tab by index. Bound as ⌘1–⌘8 / Ctrl+1–8
/// (`secondary-1` … `secondary-8`); the payload is **0-based**. Out-of-range
/// indexes (fewer tabs open than the number pressed) are ignored.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum WorkspaceTabAction {
    Select(usize),
}

/// Command palette (⌘K). `Toggle` opens it (or closes if already open).
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum PaletteAction {
    Toggle,
}

/// Multi-database search palette (⌘⇧F): search a key across a selected
/// set of connections. Global, focus-independent — mirrors `PaletteAction`.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum MultiSearchAction {
    Toggle,
}

/// Recent-keys palette (⌘P). Opens a Quick-Open style picker for the
/// current connection's MRU keys. Handled by a global, focus-independent
/// handler in `main.rs` (same model as [`PaletteAction`]).
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum RecentKeysAction {
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

/// Export the diagnostics bundle (logs + redacted config + connection
/// state) as one zip in Downloads — the title-bar menu entry next to
/// "Open Logs Folder". Handled on the `Zedis` root, which knows the active
/// tab's connection.
/// The terminal output pane's commands beyond the editor's own Copy /
/// Select All: offered by its right-click menu and handled on the
/// `ZedisTerminal` root, where the menu's dispatch (from the focused pane)
/// bubbles up.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum TerminalAction {
    /// Copy the whole output, not just the selection.
    CopyAll,
    Save,
    Clear,
}

/// UI zoom (⌘+ / ⌘- / ⌘0): steps the UI font size the Settings slider also
/// drives, so the two never disagree.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ZoomAction {
    In,
    Out,
    Reset,
}

/// The macOS Window menu's commands, handled on the `Zedis` root with the
/// window in hand.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum WindowAction {
    Minimize,
    Zoom,
    ToggleFullscreen,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum DiagnosticsAction {
    Export,
}

/// In-app update check. `Check` queries GitHub for a newer release; handled by
/// a global, focus-independent handler in `main.rs`, dispatched from the app
/// menu "Check for Updates". No keybinding — menu-only.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum UpdateAction {
    /// Run an update check now (app menu "Check for Updates").
    Check,
    /// Open the download/skip prompt for the already-found update (status-bar
    /// chip click).
    OpenPrompt,
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

/// Actions scoped to the config editor while a parameter is being edited.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ConfigEditAction {
    /// Cancel the in-progress edit (mirrors the Cancel button / Esc).
    Cancel,
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
    /// Toggle one command in the command filter, by its index into the panel's
    /// `available_commands`. An index rather than the name keeps the action
    /// `Copy`, which the derive requires.
    ToggleCommand(u32),
    /// Drop every command from the filter — back to "all commands".
    ClearCommands,
}

/// Live Monitor panel export actions, dispatched by the toolbar "Export"
/// dropdown and handled by the panel's own `.on_action` (same pattern
/// as [`SlowlogAction`]). They export the currently-visible rows.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum MonitorAction {
    /// Export the visible monitor rows to a CSV file.
    ExportCsv,
    /// Export the visible monitor rows to a JSON file.
    ExportJson,
}

/// Memory-analysis panel export actions, dispatched by the toolbar
/// "Export" dropdown and handled by the panel's own `.on_action`.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum MemoryAnalysisAction {
    /// Export the prefix-group table to a CSV file.
    ExportPrefixesCsv,
    /// Export the single-key Top-N table to a CSV file.
    ExportKeysCsv,
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
            // `secondary` and `cmd` both render as the platform command key:
            // ⌘ on macOS, Ctrl elsewhere. Bindings use `secondary` (so they map
            // to Ctrl on Linux/Windows); display strings may use either.
            "cmd" | "secondary" => {
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

/// One user-configurable shortcut. `id` is the key in `keybindings.toml`
/// (`helpers/keybindings.rs`); `reference` places it in the ⌘/ overlay as
/// `(group title key, description key)`, both under the `shortcuts.` i18n
/// section — `None` keeps a binding out of the overlay.
pub struct HotKey {
    pub id: &'static str,
    pub default: &'static str,
    pub reference: Option<(&'static str, &'static str)>,
    bind: fn(&str) -> KeyBinding,
}

impl HotKey {
    /// The keystroke in effect: the user's override, else the default.
    pub fn effective(&self) -> &str {
        keybinding_overrides()
            .get(self.id)
            .map(String::as_str)
            .unwrap_or(self.default)
    }
}

const GROUP_GENERAL: &str = "group_general";
const GROUP_EDITOR: &str = "group_editor";
const GROUP_NAVIGATION: &str = "group_navigation";

/// Every user-configurable shortcut, in overlay order. `secondary` = cmd on
/// macOS, ctrl on Linux / Windows, so `secondary-w` is ⌘W / Ctrl+W.
static HOT_KEYS: &[HotKey] = &[
    HotKey {
        id: "command_palette",
        default: "secondary-k",
        reference: Some((GROUP_GENERAL, "command_palette")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, PaletteAction::Toggle, None),
    },
    // Quick-open recent keys (Zed/VS Code ⌘P style). Global so it works
    // from tool pages and after the picker closes (focus-independent).
    HotKey {
        id: "recent_keys",
        default: "secondary-p",
        reference: Some((GROUP_GENERAL, "recent_keys")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, RecentKeysAction::Toggle, None),
    },
    // Multi-database search — ⌘⇧F, deliberately adjacent to the
    // key-tree filter's ⌘F ("search here" vs "search everywhere").
    HotKey {
        id: "multi_search",
        default: "secondary-shift-f",
        reference: Some((GROUP_GENERAL, "multi_search")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, MultiSearchAction::Toggle, None),
    },
    HotKey {
        id: "keyboard_shortcuts",
        default: "secondary-/",
        reference: Some((GROUP_GENERAL, "keyboard_shortcuts")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, ShortcutsAction::Toggle, None),
    },
    HotKey {
        id: "zoom_in",
        default: "secondary-=",
        reference: Some((GROUP_GENERAL, "zoom_in")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, ZoomAction::In, None),
    },
    HotKey {
        id: "zoom_out",
        default: "secondary--",
        reference: Some((GROUP_GENERAL, "zoom_out")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, ZoomAction::Out, None),
    },
    HotKey {
        id: "zoom_reset",
        default: "secondary-0",
        reference: Some((GROUP_GENERAL, "zoom_reset")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, ZoomAction::Reset, None),
    },
    HotKey {
        id: "quit",
        default: "secondary-q",
        reference: Some((GROUP_GENERAL, "quit")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, MemuAction::Quit, None),
    },
    HotKey {
        id: "new_key",
        default: "secondary-n",
        reference: Some((GROUP_EDITOR, "new_key")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Create, None),
    },
    HotKey {
        id: "save",
        default: "secondary-s",
        reference: Some((GROUP_EDITOR, "save")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Save, None),
    },
    // Key-tree refresh is the primary, high-frequency action so it owns
    // plain `cmd-r`; value reload is the rarer one and takes `cmd-shift-r`.
    HotKey {
        id: "reload_keys",
        default: "secondary-r",
        reference: Some((GROUP_EDITOR, "reload_keys")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::ReloadKeyTree, None),
    },
    HotKey {
        id: "reload_value",
        default: "secondary-shift-r",
        reference: Some((GROUP_EDITOR, "reload_value")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Reload, None),
    },
    HotKey {
        id: "update_ttl",
        default: "secondary-t",
        reference: Some((GROUP_EDITOR, "update_ttl")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::UpdateTtl, None),
    },
    // Delete the selected key. A modifier combo (not bare Backspace) so it
    // can't fire while navigating the tree or typing; still routes through
    // the confirm dialog (with PROD escalation), so a stray press is safe.
    // When a text editor is focused it keeps ⌘⌫ for editing — the global
    // delete only fires when no input consumes it.
    HotKey {
        id: "delete_key",
        default: "secondary-backspace",
        reference: Some((GROUP_EDITOR, "delete_key")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Delete, None),
    },
    // Rename the selected key. ⌘E is free on macOS text inputs (unlike
    // ⌘C/⌘V/⌘X), so it can be a global binding without stealing edit
    // keys; the rename flow routes through its own dialog with an
    // overwrite confirm, so a stray press is safe.
    HotKey {
        id: "rename_key",
        default: "secondary-e",
        reference: Some((GROUP_EDITOR, "rename_key")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Rename, None),
    },
    // Focus the active page's filter box — except while focus is inside
    // any gpui-component `Input`: a context-free binding out-ranks every
    // context-bound one (`binding_enabled` gives it the full stack
    // depth), so without `!Input` this would shadow the code editor's
    // own ⌘F search panel and searching inside a value/script editor
    // would be impossible.
    HotKey {
        id: "search",
        default: "secondary-f",
        reference: Some((GROUP_EDITOR, "search")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Search, Some("!Input")),
    },
    HotKey {
        id: "terminal",
        default: "secondary-j",
        reference: Some((GROUP_EDITOR, "terminal")),
        bind: |keystroke: &str| KeyBinding::new(keystroke, EditorAction::Cmd, None),
    },
    // Close the window via ⌘W, mirroring the red close button (on macOS
    // that hides the app — `on_window_should_close` in `main.rs`). Not in
    // the overlay: it is the platform's own convention.
    HotKey {
        id: "close_window",
        default: "secondary-w",
        reference: None,
        bind: |keystroke: &str| KeyBinding::new(keystroke, MemuAction::Close, None),
    },
];

/// The user-configurable shortcuts — the ids `keybindings.toml` accepts.
pub fn hot_key_table() -> &'static [HotKey] {
    HOT_KEYS
}

/// One section of the keyboard-shortcuts reference overlay.
pub struct ShortcutGroup {
    /// i18n key (under the `shortcuts.` section) for the group heading.
    pub title_key: &'static str,
    /// `(effective keystroke, i18n key for the human description)`. The
    /// keystroke is rendered through [`humanize_keystroke`] so it shows
    /// the right per-platform symbols (⌘ vs Ctrl, …).
    pub items: Vec<(String, &'static str)>,
}

/// The user-facing keyboard-shortcut reference shown by the ⌘/ overlay:
/// the configurable table (with the user's overrides applied) grouped by
/// section, plus the fixed navigation keys. The context-scoped internal
/// bindings (`tab` JSONPath completion, the `escape` back/close handlers)
/// stay out — they would only read as "global shortcuts".
pub fn shortcut_reference() -> Vec<ShortcutGroup> {
    let mut groups = vec![
        ShortcutGroup {
            title_key: GROUP_GENERAL,
            items: Vec::new(),
        },
        ShortcutGroup {
            title_key: GROUP_EDITOR,
            items: Vec::new(),
        },
    ];
    for hot_key in HOT_KEYS {
        let Some((group, desc_key)) = hot_key.reference else {
            continue;
        };
        if let Some(group) = groups.iter_mut().find(|candidate| candidate.title_key == group) {
            group.items.push((hot_key.effective().to_string(), desc_key));
        }
    }
    groups.push(ShortcutGroup {
        title_key: GROUP_NAVIGATION,
        items: vec![
            ("escape".to_string(), "back"),
            ("cmd-1 … cmd-8".to_string(), "workspace_tab"),
        ],
    });
    groups
}

pub fn new_hot_keys() -> Vec<KeyBinding> {
    let mut keys: Vec<KeyBinding> = HOT_KEYS
        .iter()
        .map(|hot_key| (hot_key.bind)(hot_key.effective()))
        .collect();
    // `=` is the unshifted `+` key on every layout that has one, so both
    // spellings zoom in — unless the user moved zoom-in elsewhere.
    if !keybinding_overrides().contains_key("zoom_in") {
        keys.push(KeyBinding::new("secondary-shift-=", ZoomAction::In, None));
    }
    keys.extend([
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
        // Esc while editing a config parameter cancels the edit. Scoped to the
        // `ConfigEdit` context (only present on the config editor's root while
        // an edit is active), so it wins over the page-back binding then and
        // falls through to it otherwise.
        KeyBinding::new("escape", ConfigEditAction::Cancel, Some("ConfigEdit")),
        // Scoped to the JSONPath bar so it only overrides `tab` there;
        // the handler propagates when no completion menu is open, so
        // normal focus movement still works.
        KeyBinding::new("tab", JsonPathAction::AcceptCompletion, Some("JsonPathBar")),
        // Workspace tabs: ⌘1–⌘8 / Ctrl+1–8 → activate that tab (1-based
        // key, 0-based index). Cap matches `MAX_TABS` (8) in `main.rs`.
        KeyBinding::new("secondary-1", WorkspaceTabAction::Select(0), None),
        KeyBinding::new("secondary-2", WorkspaceTabAction::Select(1), None),
        KeyBinding::new("secondary-3", WorkspaceTabAction::Select(2), None),
        KeyBinding::new("secondary-4", WorkspaceTabAction::Select(3), None),
        KeyBinding::new("secondary-5", WorkspaceTabAction::Select(4), None),
        KeyBinding::new("secondary-6", WorkspaceTabAction::Select(5), None),
        KeyBinding::new("secondary-7", WorkspaceTabAction::Select(6), None),
        KeyBinding::new("secondary-8", WorkspaceTabAction::Select(7), None),
    ]);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_key_ids_are_unique_and_every_reference_group_exists() {
        let mut ids: Vec<&str> = HOT_KEYS.iter().map(|hot_key| hot_key.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), HOT_KEYS.len());
        let groups = shortcut_reference();
        let titles: Vec<&str> = groups.iter().map(|group| group.title_key).collect();
        for hot_key in HOT_KEYS.iter().filter_map(|hot_key| hot_key.reference) {
            assert!(titles.contains(&hot_key.0), "{}", hot_key.0);
        }
        let listed: usize = groups.iter().map(|group| group.items.len()).sum();
        let referenced = HOT_KEYS.iter().filter(|hot_key| hot_key.reference.is_some()).count();
        // + the two fixed navigation rows.
        assert_eq!(listed, referenced + 2);
    }
}

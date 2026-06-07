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

pub fn new_hot_keys() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", MemuAction::Quit, None),
        KeyBinding::new("cmd-k", PaletteAction::Toggle, None),
        KeyBinding::new("cmd-s", EditorAction::Save, None),
        KeyBinding::new("cmd-r", EditorAction::ReloadKeyTree, None),
        KeyBinding::new("cmd-n", EditorAction::Create, None),
        KeyBinding::new("cmd-t", EditorAction::UpdateTtl, None),
        KeyBinding::new("cmd-j", EditorAction::Cmd, None),
        KeyBinding::new("cmd-f", EditorAction::Search, None),
        // Key-tree refresh is the primary, high-frequency action so it
        // owns plain `cmd-r` (above); value reload is the rarer one and
        // takes `cmd-shift-r`.
        KeyBinding::new("cmd-shift-r", EditorAction::Reload, None),
        // Esc on a tool page returns to the editor (mirrors the page's
        // back button). Shadowed by any deeper `escape` consumer
        // (inputs, dialogs, the command palette), so it only fires when
        // none of those own the keystroke.
        KeyBinding::new("escape", NavAction::Back, None),
        // Scoped to the JSONPath bar so it only overrides `tab` there;
        // the handler propagates when no completion menu is open, so
        // normal focus movement still works.
        KeyBinding::new("tab", JsonPathAction::AcceptCompletion, Some("JsonPathBar")),
    ]
}

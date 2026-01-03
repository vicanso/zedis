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

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum EditorAction {
    Create,
    Save,
    Reload,
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
        KeyBinding::new("cmd-s", EditorAction::Save, None),
        KeyBinding::new("cmd-r", EditorAction::Reload, None),
        KeyBinding::new("cmd-n", EditorAction::Create, None),
    ]
}

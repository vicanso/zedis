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

pub fn new_hot_keys() -> Vec<KeyBinding> {
    vec![
        // macOS 使用 Cmd+Q
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", MemuAction::Quit, None),
        // Windows/Linux 使用 Ctrl+Q
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-q", MemuAction::Quit, None),
    ]
}

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

//! Per-tab state and `{{param}}` parameter substitution for the Query Workbench.

use crate::connection::{ReplyFormat, TerminalReply, TerminalSession};
use crate::db::{extract_workbench_params, render_workbench_script};
use crate::views::strip_redis_cli_prefix;
use gpui::{App, Context, Entity, SharedString, Subscription, Task, Window, prelude::*};
use gpui_kit::component::input::{EditorState, InputEvent, InputState, TabSize};
use std::collections::BTreeMap;

use super::ZedisWorkbench;

const DEFAULT_WORKBENCH_SCRIPT: &str = "# Parameterized Redis Workbench\n# Placeholders like {{key}} become input fields automatically.\nGET {{key}}\nTTL {{key}}";

/// Result of executing a single command statement in a workbench tab.
#[derive(Debug, Clone)]
pub(super) struct WorkbenchStatementResult {
    pub(super) command: SharedString,
    pub(super) elapsed_ms: u128,
    pub(super) outcome: Result<TerminalReply, SharedString>,
}

/// One open query tab inside [`ZedisWorkbench`].
pub(super) struct WorkbenchTab {
    pub(super) id: u64,
    pub(super) snippet_id: Option<String>,
    pub(super) title: SharedString,
    pub(super) description: SharedString,
    pub(super) editor: Entity<EditorState>,
    pub(super) param_names: Vec<String>,
    pub(super) param_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) session: TerminalSession,
    pub(super) tab_db: Option<usize>,
    pub(super) reply_format: ReplyFormat,
    pub(super) results: Vec<WorkbenchStatementResult>,
    pub(super) running: bool,
    pub(super) task: Option<Task<()>>,
    pub(super) subs: Vec<Subscription>,
}

impl WorkbenchTab {
    pub(super) fn new(
        id: u64,
        title: SharedString,
        initial_script: Option<&str>,
        defaults: &BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut Context<ZedisWorkbench>,
    ) -> Self {
        let script_text = initial_script.unwrap_or(DEFAULT_WORKBENCH_SCRIPT).to_string();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .searchable(true)
                .soft_wrap(false)
                .default_value(script_text)
        });

        let mut subs = Vec::new();
        let tab_id = id;
        subs.push(
            cx.subscribe_in(&editor, window, move |view, _state, event, window, cx| match event {
                InputEvent::Change => {
                    view.sync_tab_params(tab_id, window, cx);
                }
                InputEvent::PressEnter { secondary, .. } if *secondary => {
                    view.run_active_tab(false, window, cx);
                }
                _ => {}
            }),
        );

        let mut tab = Self {
            id,
            snippet_id: None,
            title,
            description: SharedString::default(),
            editor,
            param_names: Vec::new(),
            param_inputs: BTreeMap::new(),
            session: TerminalSession::default(),
            tab_db: None,
            reply_format: ReplyFormat::Text,
            results: Vec::new(),
            running: false,
            task: None,
            subs,
        };
        tab.sync_params(defaults, false, window, cx);
        tab
    }

    pub(super) fn reset_connection(&mut self) {
        self.session = TerminalSession::default();
        self.tab_db = None;
        self.task = None;
        self.running = false;
    }

    pub(super) fn sync_params(
        &mut self,
        defaults: &BTreeMap<String, String>,
        notify: bool,
        window: &mut Window,
        cx: &mut Context<ZedisWorkbench>,
    ) {
        let raw = self.editor.read(cx).value().to_string();
        let names = extract_workbench_params(&raw);
        if names == self.param_names {
            return;
        }
        let mut next_inputs = BTreeMap::new();
        for name in &names {
            if let Some(existing) = self.param_inputs.remove(name) {
                next_inputs.insert(name.clone(), existing);
            } else {
                let seed = defaults.get(name).cloned().unwrap_or_default();
                let placeholder = format!("{{{{{name}}}}}");
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(seed));
                self.subs
                    .push(cx.subscribe_in(&input, window, |_view, _state, event, _window, cx| {
                        if matches!(event, InputEvent::Change) {
                            cx.notify();
                        }
                    }));
                next_inputs.insert(name.clone(), input);
            }
        }
        self.param_names = names;
        self.param_inputs = next_inputs;
        if notify {
            cx.notify();
        }
    }

    pub(super) fn param_values(&self, cx: &App) -> BTreeMap<String, String> {
        self.param_inputs
            .iter()
            .map(|(k, state)| (k.clone(), state.read(cx).value().to_string()))
            .collect()
    }

    /// Extract executable lines (either the current selection/line or the
    /// entire tab), ignoring `#` and `--` comments, substituting `{{param}}`
    /// placeholders, and stripping leading `redis-cli` prefixes.
    pub(super) fn executable_lines(&self, selection_only: bool, cx: &App) -> Vec<String> {
        let editor = self.editor.read(cx);
        let source = if selection_only {
            let selected = editor.selected_value();
            if !selected.trim().is_empty() {
                selected.to_string()
            } else {
                let full = editor.value();
                let line_idx = editor.cursor_position().line as usize;
                full.lines().nth(line_idx).unwrap_or_default().to_string()
            }
        } else {
            editor.value().to_string()
        };

        let rendered = render_workbench_script(&source, &self.param_values(cx));
        rendered
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("--"))
            .map(|line| strip_redis_cli_prefix(line).trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }
}

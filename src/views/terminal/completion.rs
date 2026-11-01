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

//! Command completion, command history navigation, and `Ctrl+R` reverse
//! history search for the terminal.

use super::*;

/// Active `Ctrl+R` reverse-history-search session. `matches` is the command
/// history filtered by the current query, newest-first; `index` is the match
/// currently shown, advanced (toward older) by repeated `Ctrl+R`.
pub(super) struct ReverseSearchState {
    pub(super) matches: Vec<SharedString>,
    pub(super) index: usize,
}

impl ZedisTerminal {
    pub(super) fn update_redis_commands(&mut self, cx: &mut Context<Self>) {
        self.redis_commands = list_commands(self.server_state.read(cx).version())
            .into_iter()
            .map(Into::into)
            .collect();
    }

    pub(super) fn update_suggestions(&mut self, input: String, cx: &mut Context<Self>) {
        // Browser: commands.json arrives after the first frame. Refill if
        // this view was built before that fetch landed.
        if self.redis_commands.is_empty() {
            self.update_redis_commands(cx);
        }
        self.cmd_suggestions.clear();
        self.cmd_suggestion_index = None;
        if input.is_empty() {
            return;
        }
        let words: Vec<&str> = input.split_whitespace().collect();
        if words.is_empty() {
            return;
        }
        let max_words = words.len().min(3);
        for word_count in (1..=max_words).rev() {
            let cmd_input = words[..word_count].join(" ").to_uppercase();
            let matches: Vec<String> = self
                .redis_commands
                .iter()
                .filter(|cmd| cmd.as_str().starts_with(&cmd_input))
                .take(5)
                .map(|cmd| cmd.to_string())
                .collect();
            if !matches.is_empty() {
                self.cmd_suggestions = matches;
                self.cmd_suggestion_index = self.cmd_suggestions.iter().position(|cmd| cmd == &cmd_input);
                return;
            }
        }
    }

    pub(super) fn apply_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_suggestions.is_empty() {
            return;
        }
        let suggestion = if let Some(index) = self.cmd_suggestion_index {
            self.cmd_suggestions.get(index).cloned()
        } else {
            self.cmd_suggestions.first().cloned()
        };
        if let Some(cmd) = suggestion {
            self.cmd_input_state.update(cx, |state, cx| {
                state.set_value(SharedString::from(cmd), window, cx);
                // Move the caret to the end of the completed command so the
                // user can keep typing args (mirrors history navigation).
                state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
            });
            self.cmd_suggestions.clear();
            self.cmd_suggestion_index = None;
            cx.notify();
        }
    }

    pub(super) fn handle_cmd_history(&mut self, is_up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id();
        if server_id.is_empty() {
            return;
        }
        let offset: i32 = if is_up { 1 } else { -1 };
        let records = command_history(server_id);
        if records.is_empty() {
            return;
        }
        let mut index = if let Some(current) = self.cmd_history_index {
            if offset > 0 {
                current + 1
            } else if current == 0 {
                0
            } else {
                current - 1
            }
        } else if offset > 0 {
            0
        } else {
            records.len() - 1
        };
        index = index.min(records.len() - 1);
        if let Some(value) = records.get(index) {
            self.cmd_input_state.update(cx, |this, cx| {
                this.set_value(value.clone(), window, cx);
                this.set_cursor_position(Position::new(0, u32::MAX), window, cx);
            });
            self.cmd_history_index = Some(index);
        }
    }

    /// History records newest-first, filtered by `query` (case-insensitive
    /// substring; an empty query keeps everything).
    fn reverse_search_matches(&self, query: &str, cx: &Context<Self>) -> Vec<SharedString> {
        let server_id = self.server_state.read(cx).server_id();
        if server_id.is_empty() {
            return Vec::new();
        }
        let records = command_history(server_id);
        let q = query.to_lowercase();
        records
            .into_iter()
            .rev()
            .filter(|r| q.is_empty() || r.to_lowercase().contains(&q))
            .map(Into::into)
            .collect()
    }

    /// `Ctrl+R`: open the reverse-search overlay on the first press, then
    /// step to the next (older) match on each subsequent press.
    pub(super) fn enter_or_advance_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reverse_search.is_some() {
            self.step_reverse_search(true, cx);
            return;
        }
        let matches = self.reverse_search_matches("", cx);
        self.reverse_search = Some(ReverseSearchState { matches, index: 0 });
        // The command-completion dropdown and history cursor belong to the
        // REPL input; clear them so the overlay starts clean.
        self.cmd_suggestions.clear();
        self.cmd_suggestion_index = None;
        self.cmd_history_index = None;
        self.search_input_state.update(cx, |state, cx| {
            state.set_value(SharedString::default(), window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Move the highlighted match: `older` walks toward earlier history (↑ or
    /// a repeated Ctrl+R), otherwise toward more recent (↓). Clamped at both
    /// ends; no-op when not searching.
    pub(super) fn step_reverse_search(&mut self, older: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.reverse_search {
            let next = if older {
                (state.index + 1).min(state.matches.len().saturating_sub(1))
            } else {
                state.index.saturating_sub(1)
            };
            if next != state.index {
                state.index = next;
                cx.notify();
            }
        }
    }

    /// Re-filter for a changed query, snapping back to the newest match.
    pub(super) fn update_reverse_search_matches(&mut self, query: &str, cx: &mut Context<Self>) {
        let matches = self.reverse_search_matches(query, cx);
        if let Some(state) = &mut self.reverse_search {
            state.matches = matches;
            state.index = 0;
        }
    }

    pub(super) fn current_reverse_match(&self) -> Option<SharedString> {
        let state = self.reverse_search.as_ref()?;
        state.matches.get(state.index).cloned()
    }

    /// Accept the highlighted match into the command input for review — it is
    /// deliberately not auto-run, so the danger-command confirm still gates the
    /// eventual Enter. No match ⇒ just close the overlay.
    pub(super) fn accept_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matched = self.current_reverse_match();
        self.reverse_search = None;
        self.cmd_input_state.update(cx, |state, cx| {
            if let Some(matched) = matched {
                state.set_value(matched, window, cx);
                state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
            }
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// `Esc`: close the overlay, leaving the command input untouched.
    pub(super) fn cancel_reverse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reverse_search = None;
        self.cmd_input_state.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }
}

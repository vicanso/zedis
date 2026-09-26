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

//! Valkey's `COMMANDLOG` size logs (large request / large reply) beside the
//! slow log: the kind switch, the fetch and its 30s poll.
//!
//! Split out of `slowlog_editor.rs`; the methods are `ZedisSlowlogEditor`'s as before.

use super::*;

impl ZedisSlowlogEditor {
    /// Show another of COMMANDLOG's logs. The table is rebuilt for the
    /// column title, the filters that meant something else (command set,
    /// the "≥" threshold in the old unit) are cleared, and a size log
    /// starts its fetch + 30s poll — or, back on the slow log, stops it.
    pub(super) fn set_log_kind(&mut self, kind: CommandLogKind, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.log_kind == kind {
            return;
        }
        self.log_kind = kind;
        self.selected_commands.clear();
        self.min_amount = 0;
        self.duration_input_state.update(cx, |state, cx| {
            state.set_value(SharedString::default(), window, cx);
        });
        self.last_time_stamp = SharedString::default();
        let editor_weak = cx.entity().downgrade();
        let script_show_supported = self.script_show_supported.clone();
        self.table_state = cx.new(|cx| {
            TableState::new(
                build_table(editor_weak, kind, script_show_supported, window, cx),
                window,
                cx,
            )
        });
        self.commandlog_entries.clear();
        self._commandlog_task = None;
        self._commandlog_poll_task = None;
        let rows = self.rows_from_current_log(cx);
        self.replace_rows(rows, cx);
        if !kind.is_slow() {
            self.fetch_commandlog(cx);
            self.start_commandlog_polling(cx);
        }
        cx.notify();
    }

    /// `COMMANDLOG GET` for the shown size log. A reply for a log the user
    /// has since switched away from is dropped.
    pub(super) fn fetch_commandlog(&mut self, cx: &mut gpui::Context<Self>) {
        let kind = self.log_kind;
        if kind.is_slow() {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self.commandlog_loading = true;
        cx.notify();
        self._commandlog_task = Some(cx.spawn(async move |handle, cx| {
            let result: Result<Vec<SlowLogEntry>, Error> = cx
                .background_spawn(async move { Ok(command_logs(&ServerDb::new(server_id, db), kind).await?) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.commandlog_loading = false;
                if this.log_kind != kind {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.commandlog_entries = entries;
                        let rows = this.rows_from_current_log(cx);
                        this.replace_rows(rows, cx);
                    }
                    Err(e) => {
                        // A NOPERM / unknown-command reply degrades the feature
                        // matrix (and explains itself once); anything else is
                        // a toast.
                        let explained = this
                            .server_state
                            .update(cx, |state, cx| state.note_command_error(&e, cx));
                        if !explained {
                            this.server_state.update(cx, |state, cx| {
                                state.emit_error_notification(e.to_string().into(), cx);
                            });
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Re-fetch the shown size log every 30 seconds — the slow log rides
    /// the heartbeat's 60s cycle; a size log has no such feed. The loop
    /// ends itself once the panel is back on the slow log.
    pub(super) fn start_commandlog_polling(&mut self, cx: &mut gpui::Context<Self>) {
        if self._commandlog_poll_task.is_some() {
            return;
        }
        self._commandlog_poll_task = Some(cx.spawn(async move |handle, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let still_active = handle
                    .update(cx, |this, cx| {
                        if this.log_kind.is_slow() {
                            false
                        } else {
                            this.fetch_commandlog(cx);
                            true
                        }
                    })
                    .unwrap_or(false);
                if !still_active {
                    break;
                }
            }
        }));
    }

    /// Which of COMMANDLOG's logs the SlowLog / Top Commands tabs show — a
    /// segmented control, only on a server that has COMMANDLOG (Valkey
    /// 8.1) and lets this user run it. Elsewhere the panel is the slow log
    /// and says nothing about the others.
    pub(super) fn render_log_kind_switch(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        let available = {
            let state = self.server_state.read(cx);
            state.supports(floors::COMMANDLOG) && state.command_block(ServerCommand::CommandlogGet).is_none()
        };
        if !available {
            return None;
        }
        let tooltip = i18n_slowlog_editor(cx, "log_kind_tooltip");
        let mut group = h_flex().gap_1().items_center().mr_2();
        for kind in CommandLogKind::ALL {
            let label_key = match kind {
                CommandLogKind::Slow => "log_kind_slow",
                CommandLogKind::LargeRequest => "log_kind_large_request",
                CommandLogKind::LargeReply => "log_kind_large_reply",
            };
            let mut button = Button::new(SharedString::from(format!("perf-log-kind-{}", kind.name())))
                .xsmall()
                .label(i18n_slowlog_editor(cx, label_key))
                .tooltip(tooltip.clone());
            button = if self.log_kind == kind {
                button.primary()
            } else {
                button.outline()
            };
            group = group.child(button.on_click(cx.listener(move |this, _, window, cx| {
                this.set_log_kind(kind, window, cx);
            })));
        }
        Some(group.into_any_element())
    }
}

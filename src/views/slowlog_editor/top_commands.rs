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

//! The Top Commands aggregation tab, plus toolbar actions, CSV/JSON export,
//! and `SLOWLOG RESET` / `COMMANDLOG RESET`.

use super::table::SlowLogRow;
use super::*;

/// One row of the per-command aggregation on the Top Commands tab.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CommandAggRow {
    pub(super) command: SharedString,
    pub(super) count: usize,
    /// Summed / largest measure in the fine unit (µs or bytes).
    pub(super) total: u64,
    pub(super) max: u64,
    /// This command's share of the summed duration across all commands,
    /// in percent (0–100).
    pub(super) share_pct: f64,
}

/// Group slow-log rows by command and rank by total time consumed —
/// "which command class is eating the slow log" rather than the raw
/// entry list. Aggregates in the fine unit: microseconds (SLOWLOG's
/// native unit) for the slow log, bytes for a COMMANDLOG size log, where
/// the same ranking reads "which command moves the most bytes".
pub(super) fn aggregate_commands(rows: &[SlowLogRow]) -> Vec<CommandAggRow> {
    totals_by_command(rows.iter().map(|row| (row.command.as_ref(), row.amount_fine)))
        .into_iter()
        .map(|total| CommandAggRow {
            command: total.command.into(),
            count: total.count,
            total: total.total,
            max: total.max,
            share_pct: total.share_pct,
        })
        .collect()
}

/// Format an aggregated microsecond figure as milliseconds with one
/// decimal (e.g. "12.5 ms") — precise enough for sub-ms entries without
/// switching units per row.
pub(super) fn format_us_as_ms(us: u64) -> String {
    format!("{:.1} ms", us as f64 / 1000.0)
}

/// A byte count as the size logs show it ("2.0 kB").
pub(super) fn format_bytes(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::FormatSizeOptions::default().decimal_places(1))
}

/// An aggregated figure in the shown log's unit: milliseconds for the slow
/// log, bytes for a size log.
pub(super) fn format_amount(kind: CommandLogKind, value: u64) -> String {
    if kind.is_slow() {
        format_us_as_ms(value)
    } else {
        format_bytes(value)
    }
}

/// What one unit in the "≥" box is worth in `SlowLogRow::amount`: the slow
/// log takes milliseconds, a size log kilobytes — decimal ones, the kB the
/// column shows ([`format_bytes`]); 1024 made "≥ 2" hide a row reading 2.0 kB.
pub(super) fn filter_unit(kind: CommandLogKind) -> u64 {
    if kind.is_slow() { 1 } else { 1000 }
}

/// The export column for the measure.
pub(super) fn amount_key(kind: CommandLogKind) -> &'static str {
    if kind.is_slow() { "duration_ms" } else { "bytes" }
}

/// `slowlog.csv`, or `commandlog-large-request.csv` for a size log.
pub(super) fn export_name(kind: CommandLogKind, extension: &str) -> String {
    if kind.is_slow() {
        format!("slowlog.{extension}")
    } else {
        format!("commandlog-{}.{extension}", kind.wire().to_ascii_lowercase())
    }
}

impl ZedisSlowlogEditor {
    /// Narrow the SlowLog tab to a single command and jump there —
    /// the drill-down from a Top Commands row to its raw entries.
    pub(super) fn filter_by_command(&mut self, command: SharedString, cx: &mut gpui::Context<Self>) {
        self.selected_commands.clear();
        self.selected_commands.insert(command);
        self.set_tab(PerformanceTab::SlowLog, cx);
        self.apply_filters(cx);
    }

    /// `SLOWLOG RESET` behind the standard destructive-op confirm dialog;
    /// production-tagged servers get the escalated wording.
    pub(super) fn confirm_reset_slowlog(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let kind = self.log_kind;
        let (title, body) = if kind.is_slow() {
            (
                i18n_slowlog_editor(cx, "reset_confirm_title"),
                i18n_slowlog_editor(cx, "reset_confirm_body"),
            )
        } else {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            (
                rust_i18n::t!(
                    "slowlog_editor.commandlog_reset_confirm_title",
                    kind = kind.wire(),
                    locale = &locale
                )
                .to_string()
                .into(),
                rust_i18n::t!(
                    "slowlog_editor.commandlog_reset_confirm_body",
                    kind = kind.wire(),
                    locale = &locale
                )
                .to_string()
                .into(),
            )
        };
        let body = escalate_dangerous_body(cx, &server_id, body);
        let editor = cx.entity().downgrade();
        ZedisDialog::new_alert(title, body)
            .danger()
            .ok_text(i18n_slowlog_editor(cx, "slowlog_reset"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, window, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_reset_slowlog(cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Run `SLOWLOG RESET` (or `COMMANDLOG RESET <type>` for a size log) on
    /// every master, then clear the cached rows so the table empties
    /// without waiting for the next poll.
    fn run_reset_slowlog(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let server_state = self.server_state.clone();
        let kind = self.log_kind;
        self._slowlog_reset_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move { command_log_reset(&ServerDb::new(server_id, db), kind).await });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(()) if kind.is_slow() => {
                        let message = i18n_slowlog_editor(cx, "reset_done");
                        server_state.update(cx, |state, cx| {
                            state.clear_slow_logs(cx);
                            state.emit_success_notification(message, "SLOWLOG RESET".into(), cx);
                        });
                    }
                    Ok(()) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                        let message = rust_i18n::t!(
                            "slowlog_editor.commandlog_reset_done",
                            kind = kind.wire(),
                            locale = &locale
                        )
                        .to_string();
                        this.commandlog_entries.clear();
                        let rows = this.rows_from_current_log(cx);
                        this.replace_rows(rows, cx);
                        server_state.update(cx, |state, cx| {
                            state.emit_success_notification(
                                message.into(),
                                format!("COMMANDLOG RESET {}", kind.wire()).into(),
                                cx,
                            );
                        });
                    }
                    Err(e) => {
                        server_state.update(cx, |state, cx| {
                            state.emit_error_notification(e.to_string().into(), cx);
                        });
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Export the currently-filtered slow-log rows to a CSV file. The
    /// numeric `duration_ms` (not the humanised "12ms") is exported so
    /// the result sorts/analyses cleanly in a spreadsheet.
    pub(super) fn export_csv(&mut self, cx: &mut gpui::Context<Self>) {
        let rows = self.filter_rows();
        if rows.is_empty() {
            return;
        }
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.timestamp.to_string(),
                    r.amount.to_string(),
                    r.command.to_string(),
                    r.args.to_string(),
                    r.client.to_string(),
                ]
            })
            .collect();
        let kind = self.log_kind;
        let csv = build_csv(&["timestamp", amount_key(kind), "command", "args", "client"], &data);
        self.save_export(csv.into_bytes(), &export_name(kind, "csv"), true, cx);
    }

    /// Export the currently-filtered slow-log rows to a pretty-printed
    /// JSON array (one object per row, same columns as the CSV export).
    pub(super) fn export_json(&mut self, cx: &mut gpui::Context<Self>) {
        let rows = self.filter_rows();
        if rows.is_empty() {
            return;
        }
        let kind = self.log_kind;
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "timestamp": r.timestamp.to_string(),
                    (amount_key(kind)): r.amount,
                    "command": r.command.to_string(),
                    "args": r.args.to_string(),
                    "client": r.client.to_string(),
                })
            })
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Array(values)).unwrap_or_default();
        self.save_export(json.into_bytes(), &export_name(kind, "json"), false, cx);
    }

    /// Shared save flow for the exports: prompt for a path, write the
    /// bytes off the UI thread, and notify on success/failure. `is_csv`
    /// only selects which notification strings to use. Mirrors the
    /// value-search CSV export.
    fn save_export(&mut self, bytes: Vec<u8>, suggested: &str, is_csv: bool, cx: &mut gpui::Context<Self>) {
        let server_state = self.server_state.clone();
        let (success_key, error_key) = if is_csv {
            ("csv_exported", "csv_export_failed")
        } else {
            ("json_exported", "json_export_failed")
        };
        let success = i18n_common(cx, success_key);
        let error = i18n_common(cx, error_key);
        export_to_file(cx, server_state, bytes, suggested, success, error);
    }

    /// Tab-aware right-side toolbar. SlowLog tab keeps the existing
    /// min-duration / command-filter chips; Latency tab swaps in
    /// Refresh + Reset actions.
    pub(super) fn render_toolbar_actions(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Commands, as a checkable dropdown rather than a chip per command: a busy
        // server logs dozens of distinct commands, and one chip each ran the
        // toolbar off the edge. A dropdown is a fixed-width control however long
        // the list gets — and, unlike a plain single-select, it keeps the
        // multi-select the chips had ("show me HGETALL *and* SMEMBERS").
        let selected_count = self.selected_commands.len();
        let commands = self.available_commands.clone();
        let selected = self.selected_commands.clone();
        let command_label: SharedString = if selected_count == 0 {
            i18n_slowlog_editor(cx, "command_filter")
        } else {
            format!("{} ({selected_count})", i18n_slowlog_editor(cx, "command_filter")).into()
        };

        let unit_label = if self.log_kind.is_slow() { "ms" } else { "KB" };
        let log_kind_switch = self.render_log_kind_switch(cx);

        match self.current_tab {
            PerformanceTab::SlowLog => ZedisDivider::new()
                .when_some(log_kind_switch, |this, switch| this.child(switch))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        // Keyword leads: it is the widest control and the one
                        // reached for first. The duration threshold is a secondary,
                        // narrow refinement, so it follows.
                        .child(
                            Input::new(&self.keyword_state)
                                .xsmall()
                                .cleanable(true)
                                .w(px(KEYWORD_INPUT_WIDTH))
                                .mr_2(),
                        )
                        .child(
                            Label::new(i18n_slowlog_editor(cx, "min_duration"))
                                .text_color(cx.theme().muted_foreground)
                                .text_sm(),
                        )
                        .child(Input::new(&self.duration_input_state).xsmall().w(px(60.)))
                        .child(Label::new(unit_label).text_color(cx.theme().muted_foreground).text_sm()),
                )
                .when(!commands.is_empty(), |this| {
                    this.child(
                        Button::new("slowlog-command-filter")
                            .outline()
                            .small()
                            .label(command_label.clone())
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                menu = menu.menu_element_with_check(
                                    selected.is_empty(),
                                    Box::new(SlowlogAction::ClearCommands),
                                    move |_, cx| Label::new(i18n_slowlog_editor(cx, "all_commands")),
                                );
                                menu = menu.separator();
                                for (i, cmd) in commands.iter().enumerate() {
                                    let cmd = cmd.clone();
                                    menu = menu.menu_element_with_check(
                                        selected.contains(&cmd),
                                        Box::new(SlowlogAction::ToggleCommand(i as u32)),
                                        move |_, _cx| Label::new(cmd.clone()),
                                    );
                                }
                                menu
                            }),
                    )
                })
                // Export the currently-filtered rows (CSV / JSON). Only
                // shown when there is at least one row to export, and a
                // file to export it to.
                .when(saves_files() && self.row_count > 0, |this| {
                    this.child(
                        Button::new("slowlog-export")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::Download))
                            .label(i18n_common(cx, "export"))
                            .dropdown_menu(move |menu, _window, cx| {
                                menu.menu(i18n_common(cx, "export_csv"), Box::new(SlowlogAction::ExportCsv))
                                    .menu(i18n_common(cx, "export_json"), Box::new(SlowlogAction::ExportJson))
                            }),
                    )
                })
                // A size log is the panel's own fetch (30s poll): let the
                // user pull it now.
                .when(!self.log_kind.is_slow(), |this| {
                    this.child(
                        Button::new("commandlog-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_slowlog_editor(cx, "commandlog_refresh_tooltip"))
                            .loading(self.commandlog_loading)
                            .on_click(cx.listener(|this, _, _w, cx| this.fetch_commandlog(cx))),
                    )
                })
                .child(self.reset_slowlog_button(cx))
                .into_any_element(),
            PerformanceTab::TopCommands => h_flex()
                .gap_2()
                .items_center()
                .when_some(log_kind_switch, |this, switch| this.child(switch))
                .child(self.reset_slowlog_button(cx))
                .into_any_element(),
            PerformanceTab::Latency => h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("latency-refresh")
                        .outline()
                        .small()
                        .icon(Icon::new(CustomIconName::RotateCw))
                        .tooltip(i18n_slowlog_editor(cx, "latency_refresh_tooltip"))
                        .on_click(cx.listener(|this, _, _w, cx| this.fetch_latency(cx))),
                )
                .child(
                    Button::new("latency-reset")
                        .outline()
                        .small()
                        .icon(IconName::CircleX)
                        .label(i18n_slowlog_editor(cx, "latency_reset"))
                        .tooltip(i18n_slowlog_editor(cx, "latency_reset_tooltip"))
                        .on_click(cx.listener(|this, _, _w, cx| this.reset_latency(cx))),
                )
                .into_any_element(),
        }
    }

    /// `SLOWLOG RESET` toolbar button — shared by the SlowLog and Top
    /// Commands tabs (the aggregation view is where "these are stale,
    /// start a fresh window" usually gets decided). On a size log it is
    /// `COMMANDLOG RESET <type>`.
    fn reset_slowlog_button(&self, cx: &mut gpui::Context<Self>) -> Button {
        let kind = self.log_kind;
        let tooltip: SharedString = if kind.is_slow() {
            i18n_slowlog_editor(cx, "slowlog_reset_tooltip")
        } else {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            rust_i18n::t!(
                "slowlog_editor.commandlog_reset_tooltip",
                kind = kind.wire(),
                locale = &locale
            )
            .to_string()
            .into()
        };
        Button::new("slowlog-reset")
            .outline()
            .small()
            .icon(IconName::CircleX)
            .label(i18n_slowlog_editor(cx, "slowlog_reset"))
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| this.confirm_reset_slowlog(window, cx)))
    }

    /// The Top Commands tab body: slow-log entries grouped by command and
    /// ranked by total time consumed. Complements the ServerLoad panel's
    /// commandstats view — that one answers "which command is *frequent*",
    /// this one answers "which command is *slow*".
    pub(super) fn render_top_commands_body(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let kind = self.log_kind;
        let rows = aggregate_commands(&self.all_rows);
        if rows.is_empty() {
            return centered_message(self.empty_message(cx), muted).into_any_element();
        }

        let header = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div().w(px(200.0)).child(
                    Label::new(i18n_slowlog_editor(cx, "agg_command"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(80.0)).child(
                    Label::new(i18n_slowlog_editor(cx, "agg_count"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(110.0)).child(
                    Label::new(i18n_slowlog_editor(cx, "agg_total"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(110.0)).child(
                    Label::new(i18n_slowlog_editor(cx, "agg_avg"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().w(px(110.0)).child(
                    Label::new(i18n_slowlog_editor(cx, "agg_max"))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .child(
                div().flex_1().child(
                    Label::new(i18n_slowlog_editor(cx, "agg_share"))
                        .text_xs()
                        .text_color(muted),
                ),
            );

        let share_bar_color = cx.theme().chart_2;
        let share_track_color = cx.theme().muted.opacity(0.4);
        let view_label = i18n_slowlog_editor(cx, "agg_view_rows");
        let body_rows: Vec<gpui::AnyElement> = rows
            .iter()
            .map(|agg| {
                let avg = agg.total / agg.count.max(1) as u64;
                // Severity is a latency notion; a byte count has none.
                let max_label = Label::new(format_amount(kind, agg.max)).text_sm();
                let max_label = if kind.is_slow() {
                    max_label.text_color(severity_color((agg.max / 1000) as i64, cx))
                } else {
                    max_label
                };
                let command = agg.command.clone();
                let id_hash = djb2_hash(command.as_ref());
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_4()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().w(px(200.0)).child(Label::new(agg.command.clone()).text_sm()))
                    .child(div().w(px(80.0)).child(Label::new(agg.count.to_string()).text_sm()))
                    .child(
                        div()
                            .w(px(110.0))
                            .child(Label::new(format_amount(kind, agg.total)).text_sm()),
                    )
                    .child(div().w(px(110.0)).child(Label::new(format_amount(kind, avg)).text_sm()))
                    .child(div().w(px(110.0)).child(max_label))
                    .child(
                        // Share of total time: numeric label + a proportional
                        // bar so the dominant command is visible at a glance.
                        h_flex()
                            .flex_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .w(px(56.0))
                                    .child(Label::new(format!("{:.1}%", agg.share_pct)).text_xs().text_color(muted)),
                            )
                            .child(
                                div().flex_1().h(px(6.)).rounded_full().bg(share_track_color).child(
                                    div()
                                        .w(relative((agg.share_pct / 100.0).clamp(0.0, 1.0) as f32))
                                        .h_full()
                                        .rounded_full()
                                        .bg(share_bar_color),
                                ),
                            ),
                    )
                    .child(
                        Button::new(("agg-view-rows", id_hash))
                            .ghost()
                            .xsmall()
                            .label(view_label.clone())
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.filter_by_command(command.clone(), cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().children(body_rows)),
            )
            .into_any_element()
    }
}

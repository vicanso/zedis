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

/// Redis Slow Log viewer.
///
/// Displays a table of slow-query log entries fetched from the server's
/// periodic `SLOWLOG GET` refresh cycle. Columns: Timestamp, Duration,
/// Command, Client. Rows are sortable by arrival order (newest first).
use crate::connection::{SlowLogEntry, list_commands};
use crate::states::{ServerEvent, ZedisServerState, i18n_common, i18n_slowlog_editor};
use crate::{assets::CustomIconName, constants::SIDEBAR_WIDTH};
use chrono::TimeZone;
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Icon, IconName, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

/// Set of two-word Redis command names in uppercase (e.g. "CONFIG GET", "SLOWLOG GET").
/// Built once from the full command list so we can correctly split slowlog args into
/// `command` vs `args` columns in the table.
static TWO_WORD_COMMANDS: OnceLock<HashSet<String>> = OnceLock::new();

/// Returns a reference to the lazily-initialized set of two-word Redis commands.
/// The set is built once and reused for all subsequent slow-log entries.
fn two_word_commands() -> &'static HashSet<String> {
    TWO_WORD_COMMANDS.get_or_init(|| {
        list_commands("0.0.0")
            .into_iter()
            .filter(|cmd| cmd.contains(' '))
            .map(|cmd| cmd.to_string().to_uppercase())
            .collect()
    })
}

/// A single row in the slowlog table, pre-formatted for display.
#[derive(Clone, Debug)]
struct SlowLogRow {
    timestamp: SharedString,
    duration: SharedString,
    /// The Redis command name (args[0]), e.g. "GET", "HSET".
    command: SharedString,
    /// The arguments following the command (args[1..]), space-joined.
    args: SharedString,
    client: SharedString,
}

impl SlowLogRow {
    /// Converts a raw [`SlowLogEntry`] from the server into a display-ready row.
    ///
    /// - `timestamp` is formatted as local time (`YYYY-MM-DD HH:MM:SS`).
    /// - `duration` is formatted as a human-readable string (e.g. `"12ms"`).
    /// - `command` / `args` are split by checking whether the first two tokens
    ///   form a known two-word command (e.g. `"CONFIG GET"`, `"SLOWLOG GET"`).
    ///   If so, both tokens become the command; otherwise only the first token is
    ///   used. All tokens are upper-cased for consistent display.
    /// - `client` combines the peer address with the optional connection name.
    fn from_entry(entry: &SlowLogEntry) -> Self {
        let timestamp = chrono::Local
            .timestamp_opt(entry.timestamp, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        let duration = humantime::format_duration(Duration::from_millis(entry.duration.as_millis() as u64)).to_string();

        // Check whether the first two tokens form a known two-word command
        // (e.g. "CONFIG GET", "SLOWLOG GET") before splitting.
        let (command, args) = if entry.args.len() >= 2 {
            let candidate = format!("{} {}", entry.args[0], entry.args[1]).to_uppercase();
            if two_word_commands().contains(&candidate) {
                // Two-word command: treat both tokens as the command name.
                (candidate, entry.args.get(2..).unwrap_or(&[]).join(" "))
            } else {
                // Single-word command: first token is the name, rest are args.
                (
                    entry.args[0].to_uppercase(),
                    entry.args.get(1..).unwrap_or(&[]).join(" "),
                )
            }
        } else {
            // Only one (or zero) tokens available.
            (
                entry.args.first().map(|s| s.to_uppercase()).unwrap_or_default(),
                String::new(),
            )
        };

        // Format client as "addr (name)" when a connection name is set, otherwise just "addr".
        let addr = entry.client_addr.as_deref().unwrap_or("");
        let name = entry.client_name.as_deref().unwrap_or("");
        let client = if !name.is_empty() {
            format!("{addr} ({name})")
        } else {
            addr.to_string()
        };

        Self {
            timestamp: timestamp.into(),
            duration: duration.into(),
            command: command.into(),
            args: args.into(),
            client: client.into(),
        }
    }
}

const COLUMN_TIMESTAMP: &str = "timestamp";
const COLUMN_DURATION: &str = "duration";
const COLUMN_COMMAND: &str = "command";
const COLUMN_ARGS: &str = "args";
const COLUMN_CLIENT: &str = "client";

/// [`TableDelegate`] implementation that drives the slow-log data table.
///
/// Owns the pre-formatted row data and the column definitions. Column headers
/// are translated on every render via [`i18n_slowlog_editor`] so the UI updates
/// when the user switches language at runtime.
struct SlowlogTableDelegate {
    rows: Vec<SlowLogRow>,
    columns: Vec<Column>,
    /// i18n keys corresponding to each column, used to re-translate headers on every render.
    column_keys: Vec<&'static str>,
}

impl SlowlogTableDelegate {
    /// Creates the delegate with the given rows and computes column widths based on the
    /// current viewport. The "args" column takes all remaining space after the fixed-width
    /// columns (timestamp, duration, command, client) are allocated.
    fn new(rows: Vec<SlowLogRow>, window: &mut Window, _cx: &mut gpui::App) -> Self {
        let window_width = window.viewport_size().width;
        let content_width = window_width - SIDEBAR_WIDTH;
        let timestamp_width = 200.;
        let duration_width = 130.;
        let command_width = 150.;
        let client_width = 200.;
        // Subtract a small gutter (10 px) so the table doesn't overflow horizontally.
        let remaining_width =
            content_width.as_f32() - timestamp_width - duration_width - command_width - client_width - 10.;

        let make_paddings = || {
            Some(Edges {
                top: px(2.),
                bottom: px(2.),
                left: px(10.),
                right: px(10.),
            })
        };

        let column_keys: Vec<&'static str> = vec![
            COLUMN_TIMESTAMP,
            COLUMN_DURATION,
            COLUMN_COMMAND,
            COLUMN_ARGS,
            COLUMN_CLIENT,
        ];
        let widths = [
            timestamp_width,
            duration_width,
            command_width,
            remaining_width,
            client_width,
        ];
        let columns = column_keys
            .iter()
            .zip(widths)
            .map(|(&key, width)| {
                let mut column = Column::new(key, SharedString::default()).width(width).map(|mut col| {
                    col.paddings = make_paddings();
                    col
                });

                if [COLUMN_TIMESTAMP, COLUMN_COMMAND, COLUMN_CLIENT, COLUMN_DURATION].contains(&key) {
                    column = column.sortable();
                }

                column
            })
            .collect();

        Self {
            rows,
            columns,
            column_keys,
        }
    }
}

impl TableDelegate for SlowlogTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _cx: &gpui::App) -> Column {
        self.columns[index].clone()
    }

    /// Sorts `self.rows` in place according to the clicked column and direction.
    ///
    /// The duration column parses the human-readable string back into a [`std::time::Duration`]
    /// for a numerically correct comparison. All other sortable columns compare as strings.
    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        let col = &self.columns[col_ix];

        match col.key.as_ref() {
            COLUMN_TIMESTAMP => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.timestamp.cmp(&b.timestamp)),
                _ => self.rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)),
            },
            COLUMN_COMMAND => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.command.cmp(&b.command)),
                _ => self.rows.sort_by(|a, b| b.command.cmp(&a.command)),
            },
            COLUMN_CLIENT => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.client.cmp(&b.client)),
                _ => self.rows.sort_by(|a, b| b.client.cmp(&a.client)),
            },
            COLUMN_DURATION => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| {
                    let a = humantime::parse_duration(&a.duration).unwrap_or_default();
                    let b = humantime::parse_duration(&b.duration).unwrap_or_default();
                    a.cmp(&b)
                }),
                _ => self.rows.sort_by(|a, b| {
                    let a = humantime::parse_duration(&a.duration).unwrap_or_default();
                    let b = humantime::parse_duration(&b.duration).unwrap_or_default();
                    b.cmp(&a)
                }),
            },
            _ => {}
        }
    }

    /// Renders a column header cell. The label text is looked up via i18n on
    /// every render so language changes are reflected immediately.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let name = i18n_slowlog_editor(cx, self.column_keys[col_ix]);
        div()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(name)
                    .text_align(column.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    /// Renders a table cell with a hover copy button.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let value: SharedString = if let Some(row) = self.rows.get(row_ix) {
            match col_ix {
                0 => row.timestamp.clone(),
                1 => row.duration.clone(),
                2 => row.command.clone(),
                3 => row.args.clone(),
                4 => row.client.clone(),
                _ => "--".into(),
            }
        } else {
            "--".into()
        };

        let group_name: SharedString = format!("slowlog-td-{}-{}", row_ix, col_ix).into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(value.clone())
                    .text_align(column.align)
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id(("copy-wrapper", row_ix * 100 + col_ix))
                    .invisible()
                    .group_hover(group_name, |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                    .child(
                        Button::new(("copy-cell", row_ix * 100 + col_ix))
                            .ghost()
                            .icon(IconName::Copy)
                            .on_click(move |_, window, cx: &mut gpui::App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                                window.push_notification(Notification::info(copied_message.clone()), cx);
                            }),
                    ),
            )
    }

    /// Slow-log data is fetched in a single batch; there is no pagination.
    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }

    fn load_more_threshold(&self) -> usize {
        0
    }

    /// No-op: all rows are loaded upfront; incremental loading is not supported.
    fn load_more(&mut self, _window: &mut Window, _cx: &mut gpui::Context<TableState<Self>>) {}
}

/// Main Slow Log viewer component.
///
/// Subscribes to [`ServerEvent::ServerRedisInfoUpdated`] and
/// [`ServerEvent::ServerSelected`] so the table is refreshed whenever the
/// background poller delivers new `SLOWLOG GET` data or the user switches to a
/// different server connection.
///
/// Layout:
///   1. Toolbar  – snail icon + label + entry count
///   2. Table    – slowlog rows (hidden when empty, replaced by a placeholder)
pub struct ZedisSlowlogEditor {
    server_state: Entity<ZedisServerState>,
    /// Shared table state that owns the [`SlowlogTableDelegate`] and drives rendering.
    table_state: Entity<TableState<SlowlogTableDelegate>>,
    /// Timestamp of the most recently seen slow-log entry, used to skip redundant refreshes.
    last_time_stamp: SharedString,
    /// Total number of slow-log rows currently held; drives the count label in the toolbar.
    row_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl ZedisSlowlogEditor {
    /// Creates a new [`ZedisSlowlogEditor`], immediately populating the table with
    /// whatever slow-log data is already cached on the server state, and wiring up
    /// a subscription to keep it in sync with future updates.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let (row_count, rows) = Self::build_rows(&server_state, cx);
        let delegate = SlowlogTableDelegate::new(rows, window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        // Refresh table whenever the server delivers updated slow-log data or the
        // active server connection changes. The early-return on equal timestamps
        // prevents redundant re-renders when the data hasn't actually changed.
        subscriptions.push(cx.subscribe(&server_state, {
            let table_state = table_state.clone();
            move |this, _state, event, cx| {
                if matches!(
                    event,
                    ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
                ) {
                    let (new_row_count, new_rows) = Self::build_rows(&this.server_state, cx);
                    let new_time_stamp = new_rows.first().map(|row| row.timestamp.clone()).unwrap_or_default();
                    // Skip re-render if the newest entry's timestamp hasn't changed.
                    if this.last_time_stamp == new_time_stamp {
                        return;
                    }
                    this.last_time_stamp = new_time_stamp;
                    this.row_count = new_row_count;
                    table_state.update(cx, |state, _| {
                        state.delegate_mut().rows = new_rows;
                    });
                    cx.notify();
                }
            }
        }));

        Self {
            server_state,
            table_state,
            last_time_stamp: SharedString::default(),
            row_count,
            _subscriptions: subscriptions,
        }
    }

    /// Reads the current slow-log entries from the server state and converts them
    /// into display rows. Returns `(row_count, rows)`.
    fn build_rows(server_state: &Entity<ZedisServerState>, cx: &gpui::App) -> (usize, Vec<SlowLogRow>) {
        let entries = server_state.read(cx).slow_logs();
        (entries.len(), entries.iter().map(SlowLogRow::from_entry).collect())
    }
}

impl gpui::Render for ZedisSlowlogEditor {
    /// Renders the slow-log viewer.
    ///
    /// When there are no entries the table area is replaced by a centered
    /// placeholder message. Otherwise the [`DataTable`] is rendered with
    /// alternating row stripes and visible scrollbars.
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let is_empty = self.row_count == 0;

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(CustomIconName::Snail))
                    .child(Label::new(i18n_common(cx, "slow_logs")).text_color(cx.theme().foreground))
                    .child(
                        Label::new(format!("({})", self.row_count))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(is_empty, |this| {
                        this.child(div().size_full().flex().items_center().justify_center().child(
                            Label::new(i18n_slowlog_editor(cx, "no_slowlogs")).text_color(cx.theme().muted_foreground),
                        ))
                    })
                    .when(!is_empty, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
            .into_any_element()
    }
}

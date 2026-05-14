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
use crate::connection::{
    LatencyEvent, LatencySample, SlowLogEntry, get_connection_manager, latency_graph, latency_history, latency_latest,
    latency_monitor_threshold, latency_reset, list_commands,
};
use crate::helpers::get_font_family;
use crate::states::{Route, ServerEvent, ZedisGlobalStore, ZedisServerState, i18n_common, i18n_slowlog_editor};
use crate::{assets::CustomIconName, constants::SIDEBAR_WIDTH};
use ahash::AHashMap;
use chrono::TimeZone;
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;
use zedis_ui::ZedisDivider;

/// Which sub-panel the performance view is showing. Slow Log is the
/// default — historically this view was slow-log only, so keeping it
/// as the landing tab avoids surprising users on upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerformanceTab {
    SlowLog,
    Latency,
}

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
    /// Raw duration in milliseconds for filtering and sorting.
    duration_ms: u64,
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

        let duration_ms = entry.duration.as_millis() as u64;
        let duration = humantime::format_duration(Duration::from_millis(duration_ms)).to_string();

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
            duration_ms,
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
    /// The duration column uses the raw `duration_ms` for numerically correct comparison.
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
                ColumnSort::Ascending => self.rows.sort_by_key(|a| a.duration_ms),
                _ => self.rows.sort_by_key(|b| std::cmp::Reverse(b.duration_ms)),
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
///   1. Toolbar  – snail icon + label + entry count + filters
///   2. Table    – slowlog rows (hidden when empty, replaced by a placeholder)
pub struct ZedisSlowlogEditor {
    server_state: Entity<ZedisServerState>,
    /// Shared table state that owns the [`SlowlogTableDelegate`] and drives rendering.
    table_state: Entity<TableState<SlowlogTableDelegate>>,
    /// Timestamp of the most recently seen slow-log entry, used to skip redundant refreshes.
    last_time_stamp: SharedString,
    /// Total number of filtered rows currently displayed.
    row_count: usize,
    /// All unfiltered rows from the server.
    all_rows: Vec<SlowLogRow>,
    /// Unique command names extracted from all rows, sorted alphabetically.
    available_commands: Vec<SharedString>,
    /// Currently selected commands for filtering. Empty means show all.
    selected_commands: HashSet<SharedString>,
    /// Minimum duration filter in milliseconds. 0 means no filter.
    min_duration_ms: u64,
    duration_input_state: Entity<InputState>,

    // --- Latency tab state (LATENCY LATEST / HISTORY / GRAPH / RESET) ---
    current_tab: PerformanceTab,
    /// Rows from `LATENCY LATEST` for the currently active server.
    latency_events: Vec<LatencyEvent>,
    /// Current `latency-monitor-threshold` (ms). 0 ⇒ latency tracking
    /// disabled; UI surfaces an explainer instead of an empty table.
    latency_threshold_ms: u64,
    /// `true` when LATENCY came back as `ERR unknown` — pre-2.8.13
    /// servers, or a build with the command stripped.
    latency_unsupported: bool,
    latency_loading: bool,
    /// Event whose drill-down (GRAPH + recent samples) is expanded
    /// below the row. At most one expanded at a time to keep the panel
    /// readable.
    expanded_event: Option<SharedString>,
    /// Cached `LATENCY GRAPH <event>` ASCII output, keyed by event name.
    event_graphs: AHashMap<SharedString, SharedString>,
    /// Cached `LATENCY HISTORY <event>` samples.
    event_histories: AHashMap<SharedString, Vec<LatencySample>>,
    _latency_task: Option<Task<()>>,
    _event_detail_task: Option<Task<()>>,

    _subscriptions: Vec<Subscription>,
}

impl ZedisSlowlogEditor {
    /// Creates a new [`ZedisSlowlogEditor`], immediately populating the table with
    /// whatever slow-log data is already cached on the server state, and wiring up
    /// a subscription to keep it in sync with future updates.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let all_rows = Self::build_all_rows(&server_state, cx);
        let available_commands = Self::extract_commands(&all_rows);
        let filtered = all_rows.clone();
        let row_count = filtered.len();
        let delegate = SlowlogTableDelegate::new(filtered, window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        let duration_input_state = cx.new(|cx| InputState::new(window, cx));

        subscriptions.push(
            cx.subscribe_in(&duration_input_state, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    let text = state.read(cx).value();
                    this.min_duration_ms = text.trim().parse::<u64>().unwrap_or(0);
                    this.apply_filters(cx);
                }
            }),
        );

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
                    let new_rows = Self::build_all_rows(&this.server_state, cx);
                    let new_time_stamp = new_rows.first().map(|row| row.timestamp.clone()).unwrap_or_default();
                    // Skip re-render if the newest entry's timestamp hasn't changed.
                    if this.last_time_stamp == new_time_stamp {
                        return;
                    }
                    this.last_time_stamp = new_time_stamp;
                    this.all_rows = new_rows;
                    this.available_commands = Self::extract_commands(&this.all_rows);
                    // Remove selected commands that no longer exist
                    this.selected_commands.retain(|c| this.available_commands.contains(c));
                    let filtered = this.filter_rows();
                    this.row_count = filtered.len();
                    table_state.update(cx, |state, _| {
                        state.delegate_mut().rows = filtered;
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
            all_rows,
            available_commands,
            selected_commands: HashSet::new(),
            min_duration_ms: 0,
            duration_input_state,
            current_tab: PerformanceTab::SlowLog,
            latency_events: Vec::new(),
            latency_threshold_ms: 0,
            latency_unsupported: false,
            latency_loading: false,
            expanded_event: None,
            event_graphs: AHashMap::new(),
            event_histories: AHashMap::new(),
            _latency_task: None,
            _event_detail_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Fetch `LATENCY LATEST` and the `latency-monitor-threshold`
    /// config in one task. Threshold tells the user why LATEST might
    /// be empty (tracking disabled).
    fn fetch_latency(&mut self, cx: &mut gpui::Context<Self>) {
        if self.latency_loading {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self.latency_loading = true;
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let listing = latency_latest(&mut conn).await?;
                let threshold = if listing.unsupported {
                    0
                } else {
                    // Best-effort — CONFIG GET may be ACL-restricted;
                    // we treat any failure as "unknown" = 0.
                    latency_monitor_threshold(&mut conn).await.unwrap_or(0)
                };
                Ok::<_, crate::error::Error>((listing, threshold))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.latency_loading = false;
                match result {
                    Ok((listing, threshold)) => {
                        this.latency_unsupported = listing.unsupported;
                        this.latency_events = listing.events;
                        this.latency_threshold_ms = threshold;
                        // Drop stale caches so a refresh always reflects
                        // the freshly fetched events.
                        this.event_graphs.clear();
                        this.event_histories.clear();
                        this.expanded_event = None;
                    }
                    Err(_) => {
                        // Errors fall through silently — they'll be
                        // logged by the spawn helper. UI shows previous
                        // data rather than blanking.
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Drill into a single event — fetch its GRAPH (ASCII histogram)
    /// and recent HISTORY samples. Caches per event so re-expanding
    /// the same row is instant.
    fn expand_event(&mut self, event: SharedString, cx: &mut gpui::Context<Self>) {
        if self.expanded_event.as_ref() == Some(&event) {
            // Toggle off.
            self.expanded_event = None;
            cx.notify();
            return;
        }
        self.expanded_event = Some(event.clone());
        if self.event_graphs.contains_key(&event) {
            cx.notify();
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let event_for_task = event.clone();
        self._event_detail_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let graph = latency_graph(&mut conn, event_for_task.as_ref()).await?;
                let history = latency_history(&mut conn, event_for_task.as_ref()).await?;
                Ok::<_, crate::error::Error>((graph, history))
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                if let Ok((graph, history)) = result {
                    this.event_graphs.insert(event.clone(), SharedString::from(graph));
                    this.event_histories.insert(event, history);
                }
                cx.notify();
            });
        }));
    }

    /// `LATENCY RESET` without args — wipes every event. After reset
    /// we re-fetch so the UI reflects the now-empty state.
    fn reset_latency(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                latency_reset(&mut conn, &[]).await
            });
            let _ = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.latency_events.clear();
                this.event_graphs.clear();
                this.event_histories.clear();
                this.expanded_event = None;
                this.fetch_latency(cx);
            });
        }));
    }

    /// Reads the current slow-log entries from the server state and converts them
    /// into display rows.
    fn build_all_rows(server_state: &Entity<ZedisServerState>, cx: &gpui::App) -> Vec<SlowLogRow> {
        let entries = server_state.read(cx).slow_logs();
        entries.iter().map(SlowLogRow::from_entry).collect()
    }

    /// Extracts unique command names from rows, sorted alphabetically.
    fn extract_commands(rows: &[SlowLogRow]) -> Vec<SharedString> {
        let mut cmds: Vec<SharedString> = rows
            .iter()
            .map(|r| r.command.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        cmds.sort();
        cmds
    }

    /// Applies the current command and duration filters to `all_rows`.
    fn filter_rows(&self) -> Vec<SlowLogRow> {
        self.all_rows
            .iter()
            .filter(|row| {
                if !self.selected_commands.is_empty() && !self.selected_commands.contains(&row.command) {
                    return false;
                }
                if self.min_duration_ms > 0 && row.duration_ms < self.min_duration_ms {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Re-filters rows and updates the table.
    fn apply_filters(&mut self, cx: &mut gpui::Context<Self>) {
        let filtered = self.filter_rows();
        self.row_count = filtered.len();
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().rows = filtered;
        });
        cx.notify();
    }

    /// Toggles a command in the selected set.
    fn toggle_command(&mut self, command: SharedString, cx: &mut gpui::Context<Self>) {
        if self.selected_commands.contains(&command) {
            self.selected_commands.remove(&command);
        } else {
            self.selected_commands.insert(command);
        }
        self.apply_filters(cx);
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
        let total_count = self.all_rows.len();
        let has_filter = !self.selected_commands.is_empty() || self.min_duration_ms > 0;

        // Count label: show "filtered/total" when filters are active
        let count_label = if has_filter {
            format!("({}/{})", self.row_count, total_count)
        } else {
            format!("({})", total_count)
        };

        // Build command filter buttons
        let command_buttons: Vec<_> = self
            .available_commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let is_selected = self.selected_commands.contains(cmd);
                let cmd_clone = cmd.clone();
                let mut btn = Button::new(("cmd-filter", i)).label(cmd.clone()).xsmall().px_2();
                if is_selected {
                    btn = btn.primary();
                } else {
                    btn = btn.outline();
                }
                btn.on_click(cx.listener(move |this, _, _window, cx| {
                    this.toggle_command(cmd_clone.clone(), cx);
                }))
            })
            .collect();

        v_flex()
            .size_full()
            .overflow_hidden()
            // Toolbar
            .child(
                h_flex()
                    .px_4()
                    .h(px(40.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("slowlog-back")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip(i18n_common(cx, "back_to_editor"))
                                    .on_click(|_, _w, cx| {
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
                                        });
                                    }),
                            )
                            .child(Icon::new(CustomIconName::Snail))
                            .child(Label::new(i18n_slowlog_editor(cx, "panel_title")).text_color(cx.theme().foreground))
                            // Tab switcher: SlowLog ↔ Latency. Two small
                            // buttons act as a segmented control —
                            // primary variant marks the active tab so
                            // there's no doubt which dataset is shown.
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child({
                                        let active = self.current_tab == PerformanceTab::SlowLog;
                                        let mut btn = Button::new("perf-tab-slowlog")
                                            .xsmall()
                                            .label(i18n_slowlog_editor(cx, "tab_slowlog"));
                                        btn = if active { btn.primary() } else { btn.outline() };
                                        btn.on_click(cx.listener(|this, _, _w, cx| {
                                            this.current_tab = PerformanceTab::SlowLog;
                                            cx.notify();
                                        }))
                                    })
                                    .child({
                                        let active = self.current_tab == PerformanceTab::Latency;
                                        let mut btn = Button::new("perf-tab-latency")
                                            .xsmall()
                                            .label(i18n_slowlog_editor(cx, "tab_latency"));
                                        btn = if active { btn.primary() } else { btn.outline() };
                                        btn.on_click(cx.listener(|this, _, _w, cx| {
                                            this.current_tab = PerformanceTab::Latency;
                                            if this.latency_events.is_empty() && !this.latency_loading {
                                                this.fetch_latency(cx);
                                            }
                                            cx.notify();
                                        }))
                                    }),
                            )
                            .child(
                                Label::new(count_label)
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            ),
                    )
                    .child(self.render_toolbar_actions(command_buttons, cx)),
            )
            // Body — slowlog table or latency panel depending on active tab.
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(self.current_tab == PerformanceTab::SlowLog, |this| {
                        this.when(is_empty, |this| {
                            this.child(
                                div().size_full().flex().items_center().justify_center().child(
                                    Label::new(i18n_slowlog_editor(cx, "no_slowlogs"))
                                        .text_color(cx.theme().muted_foreground),
                                ),
                            )
                        })
                        .when(!is_empty, |this| {
                            this.child(
                                DataTable::new(&self.table_state)
                                    .stripe(true)
                                    .bordered(false)
                                    .scrollbar_visible(true, true),
                            )
                        })
                    })
                    .when(self.current_tab == PerformanceTab::Latency, |this| {
                        this.child(self.render_latency_body(cx))
                    }),
            )
            .into_any_element()
    }
}

impl ZedisSlowlogEditor {
    /// Tab-aware right-side toolbar. SlowLog tab keeps the existing
    /// min-duration / command-filter chips; Latency tab swaps in
    /// Refresh + Reset actions.
    fn render_toolbar_actions(&self, command_buttons: Vec<Button>, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        match self.current_tab {
            PerformanceTab::SlowLog => ZedisDivider::new()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            Label::new(i18n_slowlog_editor(cx, "min_duration"))
                                .text_color(cx.theme().muted_foreground)
                                .text_sm(),
                        )
                        .child(Input::new(&self.duration_input_state).xsmall().w(px(60.)))
                        .child(Label::new("ms").text_color(cx.theme().muted_foreground).text_sm()),
                )
                .when(!command_buttons.is_empty(), |this| {
                    this.child(h_flex().gap_2().children(command_buttons))
                })
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

    /// The Latency tab body. Shows threshold context, the LATEST
    /// event table, and inline drill-down (LATENCY GRAPH + recent
    /// HISTORY samples) for the expanded row.
    fn render_latency_body(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let theme_yellow = cx.theme().yellow;

        // Unsupported / disabled / loading empty states first.
        if self.latency_unsupported {
            return centered_message(i18n_slowlog_editor(cx, "latency_unsupported"), muted).into_any_element();
        }
        if self.latency_loading && self.latency_events.is_empty() {
            return centered_message(i18n_common(cx, "loading"), muted).into_any_element();
        }

        // Banner: explains threshold state. Yellow when 0 (disabled),
        // muted otherwise.
        let threshold_label: SharedString = if self.latency_threshold_ms == 0 {
            i18n_slowlog_editor(cx, "latency_threshold_disabled")
        } else {
            SharedString::from(format!(
                "{}: {} ms",
                i18n_slowlog_editor(cx, "latency_threshold_label"),
                self.latency_threshold_ms
            ))
        };
        let banner_color = if self.latency_threshold_ms == 0 {
            theme_yellow
        } else {
            muted
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.latency_events.len());
        for ev in &self.latency_events {
            rows.push(self.render_latency_row(ev.clone(), cx).into_any_element());
        }

        let event_label = i18n_slowlog_editor(cx, "latency_event");
        let latest_label = i18n_slowlog_editor(cx, "latency_latest_ms");
        let max_label = i18n_slowlog_editor(cx, "latency_max_ms");
        let when_label = i18n_slowlog_editor(cx, "latency_when");

        // Column header. Widths chosen to roughly match data column
        // contents below; not a real DataTable to keep the inline
        // drill-down cheap to render.
        let header = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(220.0))
                    .child(Label::new(event_label).text_xs().text_color(muted)),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(latest_label).text_xs().text_color(muted)),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(max_label).text_xs().text_color(muted)),
            )
            .child(div().flex_1().child(Label::new(when_label).text_xs().text_color(muted)));

        v_flex()
            .size_full()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .child(Label::new(threshold_label).text_xs().text_color(banner_color)),
            )
            .child(header)
            .when(self.latency_events.is_empty(), |this| {
                this.child(centered_message(i18n_slowlog_editor(cx, "latency_no_events"), muted))
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().children(rows)),
            )
            .text_color(foreground)
            .into_any_element()
    }

    fn render_latency_row(&self, ev: LatencyEvent, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let event = ev.event.clone();
        let event_for_toggle = event.clone();
        let is_expanded = self.expanded_event.as_ref() == Some(&event);
        let when_str = format_unix_seconds(ev.timestamp);
        let id_hash: u32 = djb2_hash(event.as_ref());

        // Color the latest/max numbers — yellow at > 100ms, red at > 1s.
        let latest_color = severity_color(ev.latest_ms, cx);
        let max_color = severity_color(ev.max_ms, cx);

        let graph = self.event_graphs.get(&event).cloned();
        let history = self.event_histories.get(&event).cloned();

        let row = h_flex()
            .px_3()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().w(px(220.0)).child(Label::new(event.clone()).text_sm()))
            .child(
                div().w(px(110.0)).child(
                    Label::new(format!("{} ms", ev.latest_ms))
                        .text_sm()
                        .text_color(latest_color),
                ),
            )
            .child(
                div()
                    .w(px(110.0))
                    .child(Label::new(format!("{} ms", ev.max_ms)).text_sm().text_color(max_color)),
            )
            .child(div().flex_1().child(Label::new(when_str).text_xs().text_color(muted)))
            .child(
                Button::new(("latency-toggle", id_hash))
                    .ghost()
                    .xsmall()
                    .label(if is_expanded {
                        i18n_slowlog_editor(cx, "latency_hide_graph")
                    } else {
                        i18n_slowlog_editor(cx, "latency_show_graph")
                    })
                    .on_click(cx.listener(move |this, _, _w, cx| this.expand_event(event_for_toggle.clone(), cx))),
            );

        // Inline drill-down block: ASCII GRAPH (monospace) +
        // tail of HISTORY samples.
        let detail: Option<gpui::AnyElement> = if is_expanded {
            Some(self.render_latency_detail(graph, history, cx).into_any_element())
        } else {
            None
        };

        v_flex().child(row).when_some(detail, |this, d| this.child(d))
    }

    fn render_latency_detail(
        &self,
        graph: Option<SharedString>,
        history: Option<Vec<LatencySample>>,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let graph_block: gpui::AnyElement = match graph {
            Some(g) if !g.trim().is_empty() => div()
                .px_3()
                .py_2()
                .bg(cx.theme().muted.opacity(0.4))
                .rounded_sm()
                .child(
                    Label::new(g)
                        .font_family(get_font_family())
                        .text_xs()
                        .whitespace_normal()
                        .text_color(foreground),
                )
                .into_any_element(),
            _ => div()
                .px_3()
                .py_2()
                .child(Label::new(i18n_common(cx, "loading")).text_xs().text_color(muted))
                .into_any_element(),
        };

        // Show only the last few samples — full history can be 160+
        // points, the ASCII GRAPH already summarizes the trend.
        const HISTORY_PREVIEW: usize = 12;
        let samples: Vec<gpui::AnyElement> = history
            .map(|h| {
                h.iter()
                    .rev()
                    .take(HISTORY_PREVIEW)
                    .map(|s| {
                        h_flex()
                            .gap_2()
                            .child(Label::new(format_unix_seconds(s.timestamp)).text_xs().text_color(muted))
                            .child(
                                Label::new(format!("{} ms", s.latency_ms))
                                    .text_xs()
                                    .text_color(severity_color(s.latency_ms, cx)),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();

        v_flex()
            .gap_2()
            .px_4()
            .py_2()
            .bg(cx.theme().muted.opacity(0.15))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(graph_block)
            .when(!samples.is_empty(), |this| {
                this.child(
                    Label::new(i18n_slowlog_editor(cx, "latency_history_label"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(v_flex().gap_1().children(samples))
            })
    }
}

fn centered_message(text: SharedString, color: gpui::Hsla) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(Label::new(text).text_color(color))
}

fn severity_color(ms: i64, cx: &gpui::App) -> gpui::Hsla {
    let theme = cx.theme();
    match ms {
        i64::MIN..0 => theme.muted_foreground,
        0..=100 => theme.green,
        101..=1000 => theme.yellow,
        _ => theme.red,
    }
}

fn format_unix_seconds(ts: i64) -> String {
    if ts <= 0 {
        return "--".to_string();
    }
    match chrono::Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => ts.to_string(),
    }
}

/// Stable DJB2 hash so `(static_id, u32)` element keys derived from
/// event names compile (ElementId only accepts primitive tuples).
fn djb2_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

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

use super::metrics::{ChartParams, format_timestamp_ms, make_line_canvas};
use crate::assets::CustomIconName;
use crate::connection::ServerCommand;
/// Redis Slow Log viewer.
///
/// Displays a table of slow-query log entries fetched from the server's
/// periodic `SLOWLOG GET` refresh cycle. Columns: Timestamp, Duration,
/// Command, Client. Rows are sortable by arrival order (newest first).
use crate::connection::{
    LatencyEvent, LatencySample, SlowLogEntry, get_connection_manager, get_server, latency_history, latency_latest,
    latency_monitor_threshold, latency_reset, list_commands,
};
use crate::error::Error;
use crate::helpers::{SlowlogAction, build_csv, get_mono_font_family};
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
    dialog_button_props, escalate_dangerous_body, i18n_common, i18n_slowlog_editor,
};
use crate::views::export_to_file;
use ahash::AHashMap;
use chrono::TimeZone;
use gpui::{
    AnyElement, Edges, Entity, SharedString, Subscription, Task, WeakEntity, Window, div, prelude::*, px, relative,
};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    menu::DropdownMenu,
    table::{DataTable, TableState},
    v_flex,
};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use zedis_ui::{CellRenderer, TextColumn, ZedisDialog, ZedisDivider, ZedisTextTable};

/// Which sub-panel the performance view is showing. Slow Log is the
/// default — historically this view was slow-log only, so keeping it
/// as the landing tab avoids surprising users on upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerformanceTab {
    SlowLog,
    TopCommands,
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
    /// Unix-seconds timestamp kept alongside the display string so the
    /// correlation chip and time-window filter can compare against
    /// `LatencyEvent::timestamp` without re-parsing the formatted text.
    raw_timestamp: i64,
    duration: SharedString,
    /// Raw duration in milliseconds for filtering and sorting.
    duration_ms: u64,
    /// Raw duration in microseconds — SLOWLOG's native unit — kept for
    /// the per-command aggregation so sub-millisecond entries don't all
    /// collapse to zero.
    duration_us: u64,
    /// The Redis command name (args[0]), e.g. "GET", "HSET".
    command: SharedString,
    /// The arguments following the command (args[1..]), space-joined.
    args: SharedString,
    client: SharedString,
    /// Latency event whose timestamp falls within
    /// [`CORRELATION_WINDOW_SECS`] of this slow-log entry, if any.
    /// Populated by `build_all_rows` so the table delegate can render
    /// the chip in O(1) per row without re-scanning the events list.
    /// `Some((event_name, delta_seconds))`.
    correlated_event: Option<(SharedString, i64)>,
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
        let raw_timestamp = entry.timestamp;

        let duration_ms = entry.duration.as_millis() as u64;
        let duration_us = entry.duration.as_micros() as u64;
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
            raw_timestamp,
            duration: duration.into(),
            duration_ms,
            duration_us,
            command: command.into(),
            args: args.into(),
            client: client.into(),
            correlated_event: None,
        }
    }
}

/// One row of the per-command aggregation on the Top Commands tab.
#[derive(Clone, Debug, PartialEq)]
struct CommandAggRow {
    command: SharedString,
    count: usize,
    total_us: u64,
    max_us: u64,
    /// This command's share of the summed duration across all commands,
    /// in percent (0–100).
    share_pct: f64,
}

/// Group slow-log rows by command and rank by total time consumed —
/// "which command class is eating the slow log" rather than the raw
/// entry list. Aggregates in microseconds (SLOWLOG's native unit).
fn aggregate_commands(rows: &[SlowLogRow]) -> Vec<CommandAggRow> {
    let mut map: AHashMap<SharedString, (usize, u64, u64)> = AHashMap::new();
    for row in rows {
        let entry = map.entry(row.command.clone()).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += row.duration_us;
        entry.2 = entry.2.max(row.duration_us);
    }
    let grand_total: u64 = map.values().map(|(_, total, _)| *total).sum();
    let mut out: Vec<CommandAggRow> = map
        .into_iter()
        .map(|(command, (count, total_us, max_us))| CommandAggRow {
            command,
            count,
            total_us,
            max_us,
            share_pct: if grand_total == 0 {
                0.0
            } else {
                total_us as f64 * 100.0 / grand_total as f64
            },
        })
        .collect();
    out.sort_by(|a, b| b.total_us.cmp(&a.total_us).then(a.command.cmp(&b.command)));
    out
}

/// Format an aggregated microsecond figure as milliseconds with one
/// decimal (e.g. "12.5 ms") — precise enough for sub-ms entries without
/// switching units per row.
fn format_us_as_ms(us: u64) -> String {
    format!("{:.1} ms", us as f64 / 1000.0)
}

/// Time window used to associate a slow-log entry with a Latency event,
/// in **seconds**. Five seconds is wide enough to capture a fork that
/// completes just before/after the slow command lands, but tight enough
/// that unrelated background events on a busy server don't all match.
const CORRELATION_WINDOW_SECS: i64 = 5;

/// Width of the toolbar's keyword box — the args it searches are long, so give
/// it room to hold a recognisable fragment of a key.
const KEYWORD_INPUT_WIDTH: f32 = 180.0;

/// Pick the Latency event whose `timestamp` is closest to `slow_ts`,
/// provided it falls within `±CORRELATION_WINDOW_SECS`. Returns
/// `(event_name, signed_delta_seconds)` so the chip can show direction
/// ("fork +1s" vs "fork -2s"). `None` when no event qualifies.
///
/// Events list is expected to be small (Redis caps LATENCY LATEST at a
/// handful of event types), so a linear scan is fine — no need for a
/// sorted index.
fn correlated_event_for_slowlog(slow_ts: i64, events: &[LatencyEvent]) -> Option<(SharedString, i64)> {
    if slow_ts <= 0 {
        return None;
    }
    let mut best: Option<(SharedString, i64)> = None;
    for ev in events {
        if ev.timestamp <= 0 {
            continue;
        }
        let delta = ev.timestamp - slow_ts;
        if delta.abs() > CORRELATION_WINDOW_SECS {
            continue;
        }
        match &best {
            None => best = Some((ev.event.clone().into(), delta)),
            Some((_, current_delta)) if delta.abs() < current_delta.abs() => {
                best = Some((ev.event.clone().into(), delta));
            }
            _ => {}
        }
    }
    best
}

/// Count slow-log rows whose `raw_timestamp` falls within
/// `±CORRELATION_WINDOW_SECS` of `event_ts`. Drives the "N slow nearby"
/// chip on the Latency tab.
fn correlated_slowlog_count_for_event(event_ts: i64, rows: &[SlowLogRow]) -> usize {
    if event_ts <= 0 {
        return 0;
    }
    rows.iter()
        .filter(|r| r.raw_timestamp > 0 && (r.raw_timestamp - event_ts).abs() <= CORRELATION_WINDOW_SECS)
        .count()
}

const COLUMN_TIMESTAMP: &str = "timestamp";
const COLUMN_DURATION: &str = "duration";
const COLUMN_COMMAND: &str = "command";
const COLUMN_ARGS: &str = "args";
const COLUMN_CLIENT: &str = "client";
const COLUMN_CORRELATED: &str = "correlated";

/// [`TableDelegate`] implementation that drives the slow-log data table.
///
/// Owns the pre-formatted row data and the column definitions. Column headers
/// are translated on every render via [`i18n_slowlog_editor`] so the UI updates
/// when the user switches language at runtime.
const SLOWLOG_COLUMNS: [&str; 6] = [
    COLUMN_TIMESTAMP,
    COLUMN_DURATION,
    COLUMN_COMMAND,
    COLUMN_ARGS,
    COLUMN_CLIENT,
    COLUMN_CORRELATED,
];
/// Index of the correlation column, drawn as a chip rather than text.
const CORRELATED_COLUMN: usize = 5;
/// Payload cells after the six columns: the raw duration the duration
/// column sorts by, and the correlated latency event's name and delta.
const CELL_DURATION_MS: usize = 6;
const CELL_EVENT: usize = 7;
const CELL_EVENT_DELTA: usize = 8;

impl SlowLogRow {
    fn cells(&self) -> Vec<SharedString> {
        let (event, delta) = match &self.correlated_event {
            Some((event, delta)) => (event.clone(), SharedString::from(delta.to_string())),
            None => (SharedString::default(), SharedString::default()),
        };
        vec![
            self.timestamp.clone(),
            self.duration.clone(),
            self.command.clone(),
            self.args.clone(),
            self.client.clone(),
            SharedString::default(),
            self.duration_ms.to_string().into(),
            event,
            delta,
        ]
    }
}

/// The slow-log grid. Column widths come from the viewport: the "args"
/// column takes all remaining space after the fixed-width columns.
fn build_table(editor: WeakEntity<ZedisSlowlogEditor>, window: &mut Window, cx: &mut gpui::App) -> ZedisTextTable {
    let content_width = content_area_width(window, cx);
    // Cells are `[label ..flex_1..][copy button ..flex_none..]` and the copy
    // button only appears on hover, taking ~28px out of the label's box — so
    // text that fits at rest gets clipped the moment the pointer lands on the
    // row. Both of these budget for it, on top of the 20px of side padding:
    //   timestamp: "2026-07-14 22:31:05" — 19 mono chars
    //   client:    "192.168.1.10:53166" — sized for the address only. The
    //              client *name* is appended after it ("… (zedis:v0.5.5)") and
    //              is allowed to ellipsize: it repeats across every row of the
    //              same client, whereas the address is what tells them apart.
    let timestamp_width = 240.;
    let duration_width = 130.;
    let command_width = 150.;
    let client_width = 240.;
    // Wide enough for "<event> +Ns" — event names cap around 24 chars
    // (e.g. "active-defrag-cycle"). Fixed instead of stretchy so the
    // chip stays compact next to client info.
    let correlated_width = 170.;
    // Subtract a small gutter (10 px) so the table doesn't overflow horizontally.
    // `args` is the flexible column; floor it so a narrow window scrolls the
    // table horizontally instead of collapsing the column to nothing.
    let remaining_width = (content_width.as_f32()
        - timestamp_width
        - duration_width
        - command_width
        - client_width
        - correlated_width
        - 10.)
        .max(200.);
    let widths = [
        timestamp_width,
        duration_width,
        command_width,
        remaining_width,
        client_width,
        correlated_width,
    ];
    let columns = SLOWLOG_COLUMNS
        .iter()
        .zip(widths)
        .map(|(&key, width)| {
            let column = TextColumn::new(key, i18n_slowlog_editor(cx, key), width);
            match key {
                // The duration column shows "12ms" but sorts by the raw value.
                COLUMN_DURATION => column.sort_by_cell(CELL_DURATION_MS),
                COLUMN_TIMESTAMP | COLUMN_COMMAND | COLUMN_CLIENT => column.sortable(),
                _ => column,
            }
        })
        .collect();
    let chip: CellRenderer = Rc::new(move |row_ix, col_ix, cells, _window, cx| {
        (col_ix == CORRELATED_COLUMN).then(|| render_correlation_chip(row_ix, cells, editor.clone(), cx))
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_render(chip)
}

/// The chip-style cell for the "correlated event" column. Empty text when
/// no Latency event lines up. Otherwise a small outline button whose click
/// hops the user to the Latency tab and pre-expands the matching event. A
/// button (not a label) so the affordance is unmistakable — the row's
/// `timestamp` column already shows the time; the chip's job is
/// *navigation*.
fn render_correlation_chip(
    row_ix: usize,
    cells: &[SharedString],
    editor: WeakEntity<ZedisSlowlogEditor>,
    cx: &mut gpui::App,
) -> AnyElement {
    let paddings = Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    };
    let Some(event) = cells.get(CELL_EVENT).filter(|e| !e.is_empty()).cloned() else {
        return h_flex()
            .size_full()
            .paddings(paddings)
            .child(
                Label::new(i18n_slowlog_editor(cx, "no_correlation"))
                    .text_color(cx.theme().muted_foreground)
                    .text_xs(),
            )
            .into_any_element();
    };
    let delta: i64 = cells.get(CELL_EVENT_DELTA).and_then(|d| d.parse().ok()).unwrap_or(0);

    // Use the formatted "<event> +Ns" string from i18n so RTL/CJK
    // wording isn't broken by hard-coded English concatenation.
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let label_text: SharedString = rust_i18n::t!(
        "slowlog_editor.chip_near_event",
        event = event.as_ref(),
        // `delta` is signed seconds; render `+1` / `-2` directly.
        delta = format!("{:+}", delta),
        locale = locale
    )
    .to_string()
    .into();

    let event_for_click = event.clone();
    // djb2 is already used downstream for stable u32 ids derived
    // from event names — reuse it here so ButtonId stays unique.
    let id_hash: u32 = djb2_hash(event.as_ref()).wrapping_add(row_ix as u32);

    h_flex()
        .size_full()
        .paddings(paddings)
        .child(
            Button::new(("slowlog-correlated-chip", id_hash))
                .outline()
                .xsmall()
                .label(label_text)
                .on_click(move |_, _w, cx| {
                    let Some(editor) = editor.upgrade() else { return };
                    let event_name = event_for_click.clone();
                    editor.update(cx, |this, cx| {
                        this.jump_to_latency_event(event_name, cx);
                    });
                }),
        )
        .into_any_element()
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
    table_state: Entity<TableState<ZedisTextTable>>,
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
    /// Free-text filter, already trimmed + lowercased. Matched against the
    /// command, its arguments and the client. Empty means no filter.
    keyword: String,
    keyword_state: Entity<InputState>,

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
    /// Cached `LATENCY HISTORY <event>` samples. Drives the GPU
    /// sparkline (we render this rather than the server-side ASCII
    /// `LATENCY GRAPH` so the chart scales with the panel and the
    /// styling matches the Metrics view's line canvases).
    event_histories: AHashMap<SharedString, Vec<LatencySample>>,
    _latency_task: Option<Task<()>>,
    _event_detail_task: Option<Task<()>>,
    _slowlog_reset_task: Option<Task<()>>,
    /// 5-second auto-poll task for the Latency tab. Holds a Task whose
    /// drop cancels the loop, so switching tabs (or destroying the view)
    /// stops polling without explicit teardown.
    _latency_poll_task: Option<Task<()>>,

    /// Time window in unix seconds (inclusive). When set, the slow-log
    /// table is filtered to rows whose `raw_timestamp` falls inside the
    /// window — used by the "N slow nearby" chip on the Latency tab to
    /// jump the user back with the relevant rows in view. `None` means
    /// no window filter is active.
    window_filter: Option<(i64, i64)>,
    /// Label rendered inside the "filter active" pill so the user
    /// remembers why the table is showing a subset (e.g. "near fork at
    /// 2026-05-28 14:32:00"). Cleared together with `window_filter`.
    window_filter_label: Option<SharedString>,

    _subscriptions: Vec<Subscription>,
}

impl ZedisSlowlogEditor {
    /// Creates a new [`ZedisSlowlogEditor`], immediately populating the table with
    /// whatever slow-log data is already cached on the server state, and wiring up
    /// a subscription to keep it in sync with future updates.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        // No latency events at construction time — they get populated
        // by the first `fetch_latency` call when the user enters the
        // Latency tab. Initial rows therefore have no correlation chips,
        // which is correct (we have nothing to correlate against yet).
        let all_rows = Self::build_all_rows(&server_state, &[], cx);
        let available_commands = Self::extract_commands(&all_rows);
        let filtered = all_rows.clone();
        let row_count = filtered.len();
        let editor_weak = cx.entity().downgrade();
        let table_state = cx.new(|cx| TableState::new(build_table(editor_weak, window, cx), window, cx));
        table_state.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_rows(filtered.iter().map(SlowLogRow::cells).collect());
        });

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

        // Free-text filter over command / args / client — the duration threshold
        // only answers "how slow", not "what". Filters live as you type: the rows
        // are already in memory (SLOWLOG GET is capped), so there is nothing to
        // debounce.
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });
        subscriptions.push(
            cx.subscribe_in(&keyword_state, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    this.keyword = state.read(cx).value().trim().to_lowercase();
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
                    // Use the current latency_events snapshot so chips
                    // appear immediately when slowlog refreshes after
                    // latency was already populated.
                    let new_rows = Self::build_all_rows(&this.server_state, &this.latency_events, cx);
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
                        state
                            .delegate_mut()
                            .set_rows(filtered.iter().map(SlowLogRow::cells).collect());
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
            keyword: String::new(),
            keyword_state,
            current_tab: PerformanceTab::SlowLog,
            latency_events: Vec::new(),
            latency_threshold_ms: 0,
            latency_unsupported: false,
            latency_loading: false,
            expanded_event: None,
            event_histories: AHashMap::new(),
            _latency_task: None,
            _event_detail_task: None,
            _slowlog_reset_task: None,
            _latency_poll_task: None,
            window_filter: None,
            window_filter_label: None,
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
        // The probe already knows LATENCY is missing / denied here: show the
        // unsupported state without a round trip (and without a NOPERM toast).
        if self
            .server_state
            .read(cx)
            .command_block(ServerCommand::LatencyLatest)
            .is_some()
        {
            self.latency_unsupported = true;
            self.latency_loading = false;
            cx.notify();
            return;
        }
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
                Ok::<_, Error>((listing, threshold))
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
                        this.event_histories.clear();
                        this.expanded_event = None;
                        // Rebuild slow-log rows so their correlation
                        // chips reflect the freshly fetched events.
                        // We rebuild from `server_state.slow_logs()`
                        // rather than re-decorating `all_rows` so that
                        // a slowlog refresh between latency fetches
                        // doesn't leave us with stale rows.
                        this.rebuild_rows_with_correlations(cx);
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

    /// Rebuild `all_rows` from the current server state, re-decorating
    /// each row's `correlated_event` against the latest `latency_events`.
    /// Then re-apply filters and push the result into the table state.
    /// Single source of truth for "the inputs that drive the chip just
    /// changed".
    fn rebuild_rows_with_correlations(&mut self, cx: &mut gpui::Context<Self>) {
        let new_rows = Self::build_all_rows(&self.server_state, &self.latency_events, cx);
        self.all_rows = new_rows;
        self.available_commands = Self::extract_commands(&self.all_rows);
        self.selected_commands.retain(|c| self.available_commands.contains(c));
        self.apply_filters(cx);
    }

    /// Switch to the Latency tab and pin the named event as expanded so
    /// the user lands on its detail block. Called when the user clicks
    /// the correlation chip on a slow-log row.
    fn jump_to_latency_event(&mut self, event: SharedString, cx: &mut gpui::Context<Self>) {
        self.set_tab(PerformanceTab::Latency, cx);
        // Different event from currently expanded one (or none) — set
        // it and fetch detail. Same event already expanded → leave it
        // alone (no toggle off, which the generic expand_event does).
        if self.expanded_event.as_ref() != Some(&event) {
            self.expanded_event = Some(event.clone());
            self.fetch_event_detail(event, cx);
        }
        // If we never populated LATEST yet (user came here via a chip
        // before opening the Latency tab manually) — kick a fetch.
        if self.latency_events.is_empty() && !self.latency_loading {
            self.fetch_latency(cx);
        }
        cx.notify();
    }

    /// Switch to the SlowLog tab and narrow the table to commands that
    /// fired within `±CORRELATION_WINDOW_SECS` of `event_ts`. Records
    /// `window_filter_label` so the user sees *why* the table is
    /// suddenly short, with a one-click "Clear" pill to drop it.
    fn jump_to_slowlog_window(&mut self, event_name: SharedString, event_ts: i64, cx: &mut gpui::Context<Self>) {
        if event_ts <= 0 {
            return;
        }
        self.set_tab(PerformanceTab::SlowLog, cx);
        let lo = event_ts - CORRELATION_WINDOW_SECS;
        let hi = event_ts + CORRELATION_WINDOW_SECS;
        self.window_filter = Some((lo, hi));
        let when = chrono::Local
            .timestamp_opt(event_ts, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| event_ts.to_string());
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        self.window_filter_label = Some(
            rust_i18n::t!(
                "slowlog_editor.window_filter_label",
                event = event_name.as_ref(),
                when = when,
                locale = locale
            )
            .to_string()
            .into(),
        );
        self.apply_filters(cx);
    }

    /// Single entry point for switching the active sub-tab. Centralises
    /// the side-effect of starting/stopping the Latency auto-poll loop
    /// so we don't have to remember to start the timer at every site
    /// that sets `current_tab`.
    fn set_tab(&mut self, tab: PerformanceTab, cx: &mut gpui::Context<Self>) {
        if self.current_tab == tab {
            return;
        }
        self.current_tab = tab;
        match tab {
            PerformanceTab::Latency => self.start_latency_polling(cx),
            PerformanceTab::SlowLog | PerformanceTab::TopCommands => self.stop_latency_polling(),
        }
    }

    /// Narrow the SlowLog tab to a single command and jump there —
    /// the drill-down from a Top Commands row to its raw entries.
    fn filter_by_command(&mut self, command: SharedString, cx: &mut gpui::Context<Self>) {
        self.selected_commands.clear();
        self.selected_commands.insert(command);
        self.set_tab(PerformanceTab::SlowLog, cx);
        self.apply_filters(cx);
    }

    /// Kick a background loop that re-fetches LATENCY every 5 seconds
    /// while the user is on the Latency tab. The Task handle lives in
    /// `_latency_poll_task` — dropping it (via `stop_latency_polling`
    /// or view teardown) cancels the loop.
    fn start_latency_polling(&mut self, cx: &mut gpui::Context<Self>) {
        // Already running — don't stack a second loop on top.
        if self._latency_poll_task.is_some() {
            return;
        }
        // Kick an immediate fetch so the panel doesn't sit empty for
        // the first 5 seconds on entry.
        if self.latency_events.is_empty() && !self.latency_loading {
            self.fetch_latency(cx);
        }
        self._latency_poll_task = Some(cx.spawn(async move |handle, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let still_active = handle
                    .update(cx, |this, cx| {
                        if this.current_tab == PerformanceTab::Latency {
                            this.fetch_latency(cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !still_active {
                    break;
                }
            }
        }));
    }

    fn stop_latency_polling(&mut self) {
        self._latency_poll_task = None;
    }

    /// Issue `CONFIG SET latency-monitor-threshold 100` to flip on
    /// latency tracking with a sensible default (100ms — quiet enough
    /// to skip routine commands, loud enough to catch fork/AOF stalls).
    /// PROD-tagged servers route through the standard confirm dialog so
    /// nobody flips runtime config on a live cluster by accident.
    fn enable_latency_tracking(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        if high_risk {
            self.open_enable_confirm(window, cx);
        } else {
            self.run_enable_latency_tracking(cx);
        }
    }

    fn open_enable_confirm(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let title = i18n_slowlog_editor(cx, "enable_tracking_confirm_title");
        let body = i18n_slowlog_editor(cx, "enable_tracking_confirm_body");
        let editor = cx.entity().downgrade();
        ZedisDialog::new_alert(title, body.to_string())
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_enable_latency_tracking(cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn run_enable_latency_tracking(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self._latency_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                // 100ms — the same default Redis docs suggest. Users
                // can dial it down via CLI/Config panel later.
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("latency-monitor-threshold")
                    .arg("100")
                    .query_async::<()>(&mut conn)
                    .await?;
                Ok::<_, Error>(())
            });
            let _ = task.await;
            let _ = handle.update(cx, |this, cx| {
                // Immediate refetch so the threshold banner flips from
                // "disabled" to "100 ms" without waiting for the next
                // auto-poll tick.
                this.fetch_latency(cx);
            });
        }));
    }

    /// Clear the time-window filter set by `jump_to_slowlog_window` so
    /// the full slow-log table is visible again. Called by the "Clear"
    /// pill above the table.
    fn clear_window_filter(&mut self, cx: &mut gpui::Context<Self>) {
        if self.window_filter.is_none() {
            return;
        }
        self.window_filter = None;
        self.window_filter_label = None;
        self.apply_filters(cx);
    }

    /// Pill banner shown above the SlowLog table when a time-window
    /// filter is active (set via `jump_to_slowlog_window`). Spells out
    /// *which* Latency event triggered the narrowing and offers a
    /// one-click escape — without this, a user dropped into a 10-row
    /// view from the Latency tab might not realise the table was
    /// filtered at all.
    fn render_window_pill(&self, label: SharedString, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .mx_3()
            .my_2()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_1()
            .border_color(theme.warning)
            .bg(theme.warning.opacity(0.1))
            .rounded(theme.radius)
            .child(Icon::new(IconName::Info).text_color(theme.warning))
            .child(Label::new(label).text_sm().text_color(theme.warning).flex_1())
            .child(
                Button::new("slowlog-clear-window")
                    .ghost()
                    .xsmall()
                    .label(i18n_slowlog_editor(cx, "clear_window_filter"))
                    .on_click(cx.listener(|this, _, _w, cx| this.clear_window_filter(cx))),
            )
    }

    /// Drill into a single event — toggles expand on/off and fetches
    /// detail (GPU sparkline source data) the first time an event is
    /// expanded. Called from the chip on each Latency row.
    fn expand_event(&mut self, event: SharedString, cx: &mut gpui::Context<Self>) {
        if self.expanded_event.as_ref() == Some(&event) {
            // Toggle off.
            self.expanded_event = None;
            cx.notify();
            return;
        }
        self.expanded_event = Some(event.clone());
        self.fetch_event_detail(event, cx);
    }

    /// Fetch `LATENCY HISTORY` for `event` if we don't have it cached
    /// yet. Used by both `expand_event` (manual toggle) and
    /// `jump_to_latency_event` (chip-driven navigation) so neither path
    /// has to duplicate the spawn logic.
    fn fetch_event_detail(&mut self, event: SharedString, cx: &mut gpui::Context<Self>) {
        if self.event_histories.contains_key(&event) {
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
                let history = latency_history(&mut conn, event_for_task.as_ref()).await?;
                Ok::<_, Error>(history)
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                if let Ok(history) = result {
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
                this.event_histories.clear();
                this.expanded_event = None;
                this.fetch_latency(cx);
            });
        }));
    }

    /// `SLOWLOG RESET` behind the standard destructive-op confirm dialog;
    /// production-tagged servers get the escalated wording.
    fn confirm_reset_slowlog(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let body = escalate_dangerous_body(cx, &server_id, i18n_slowlog_editor(cx, "reset_confirm_body"));
        let editor = cx.entity().downgrade();
        ZedisDialog::new_alert(i18n_slowlog_editor(cx, "reset_confirm_title"), body)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_reset_slowlog(cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Run `SLOWLOG RESET` on every master, then clear the cached rows so
    /// the table empties without waiting for the next heartbeat poll.
    fn run_reset_slowlog(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let server_state = self.server_state.clone();
        self._slowlog_reset_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                client.slowlog_reset().await
            });
            let result = task.await;
            let _ = handle.update(cx, |_this, cx| {
                match result {
                    Ok(()) => {
                        let message = i18n_slowlog_editor(cx, "reset_done");
                        server_state.update(cx, |state, cx| {
                            state.clear_slow_logs(cx);
                            state.emit_success_notification(message, "SLOWLOG RESET".into(), cx);
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

    /// Reads the current slow-log entries from the server state and converts them
    /// into display rows, decorating each row with the closest Latency event in
    /// `±CORRELATION_WINDOW_SECS` (if any) so the chip column has data to render.
    fn build_all_rows(
        server_state: &Entity<ZedisServerState>,
        latency_events: &[LatencyEvent],
        cx: &gpui::App,
    ) -> Vec<SlowLogRow> {
        let entries = server_state.read(cx).slow_logs();
        entries
            .iter()
            .map(|entry| {
                let mut row = SlowLogRow::from_entry(entry);
                row.correlated_event = correlated_event_for_slowlog(row.raw_timestamp, latency_events);
                row
            })
            .collect()
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

    /// Applies the current command, duration, and time-window filters
    /// to `all_rows`. `window_filter`, when set, narrows the table to
    /// rows whose `raw_timestamp` falls inside the inclusive window —
    /// this drives the "jump from Latency back to nearby slow commands"
    /// flow.
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
                if let Some((lo, hi)) = self.window_filter
                    && (row.raw_timestamp < lo || row.raw_timestamp > hi)
                {
                    return false;
                }
                // Keyword: command / args / client. The key you are hunting for
                // lives in `args`, which is exactly what the duration threshold
                // and the command filter cannot reach.
                if !self.keyword.is_empty() {
                    let kw = self.keyword.as_str();
                    if !row.command.to_lowercase().contains(kw)
                        && !row.args.to_lowercase().contains(kw)
                        && !row.client.to_lowercase().contains(kw)
                    {
                        return false;
                    }
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
            state
                .delegate_mut()
                .set_rows(filtered.iter().map(SlowLogRow::cells).collect());
        });
        cx.notify();
    }

    /// Export the currently-filtered slow-log rows to a CSV file. The
    /// numeric `duration_ms` (not the humanised "12ms") is exported so
    /// the result sorts/analyses cleanly in a spreadsheet.
    fn export_csv(&mut self, cx: &mut gpui::Context<Self>) {
        let rows = self.filter_rows();
        if rows.is_empty() {
            return;
        }
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.timestamp.to_string(),
                    r.duration_ms.to_string(),
                    r.command.to_string(),
                    r.args.to_string(),
                    r.client.to_string(),
                ]
            })
            .collect();
        let csv = build_csv(&["timestamp", "duration_ms", "command", "args", "client"], &data);
        self.save_export(csv.into_bytes(), "slowlog.csv", true, cx);
    }

    /// Export the currently-filtered slow-log rows to a pretty-printed
    /// JSON array (one object per row, same columns as the CSV export).
    fn export_json(&mut self, cx: &mut gpui::Context<Self>) {
        let rows = self.filter_rows();
        if rows.is_empty() {
            return;
        }
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "timestamp": r.timestamp.to_string(),
                    "duration_ms": r.duration_ms,
                    "command": r.command.to_string(),
                    "args": r.args.to_string(),
                    "client": r.client.to_string(),
                })
            })
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Array(values)).unwrap_or_default();
        self.save_export(json.into_bytes(), "slowlog.json", false, cx);
    }

    /// Shared save flow for the exports: prompt for a path, write the
    /// bytes off the UI thread, and notify on success/failure. `is_csv`
    /// only selects which notification strings to use. Mirrors the
    /// value-search CSV export.
    fn save_export(&mut self, bytes: Vec<u8>, suggested: &'static str, is_csv: bool, cx: &mut gpui::Context<Self>) {
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
        let has_filter = !self.selected_commands.is_empty() || self.min_duration_ms > 0 || !self.keyword.is_empty();

        // Count label: show "filtered/total" when filters are active
        let count_label = if has_filter {
            format!("({}/{})", self.row_count, total_count)
        } else {
            format!("({})", total_count)
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            // Monospace cascades to the logged commands, durations and timestamps.
            .font_family(get_mono_font_family())
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
                                    .tooltip(back_to_editor_tooltip(cx))
                                    .on_click(|_, _w, cx| {
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
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
                                            this.set_tab(PerformanceTab::SlowLog, cx);
                                            cx.notify();
                                        }))
                                    })
                                    .child({
                                        let active = self.current_tab == PerformanceTab::TopCommands;
                                        let mut btn = Button::new("perf-tab-top-commands")
                                            .xsmall()
                                            .label(i18n_slowlog_editor(cx, "tab_top_commands"));
                                        btn = if active { btn.primary() } else { btn.outline() };
                                        btn.on_click(cx.listener(|this, _, _w, cx| {
                                            this.set_tab(PerformanceTab::TopCommands, cx);
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
                                            // `set_tab` handles the initial
                                            // fetch + polling task — no need
                                            // to fire `fetch_latency` here.
                                            this.set_tab(PerformanceTab::Latency, cx);
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
                    .child(self.render_toolbar_actions(cx)),
            )
            // Body — slowlog table or latency panel depending on active tab.
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(self.current_tab == PerformanceTab::SlowLog, |this| {
                        let window_pill = self.window_filter_label.clone();
                        this
                            // Pill banner sits above both the empty-state
                            // message and the table so users always see why
                            // the table is narrowed even when the filter
                            // happens to yield zero rows.
                            .when_some(window_pill, |this, label| {
                                this.child(self.render_window_pill(label, cx))
                            })
                            .when(is_empty, |this| {
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
                    .when(self.current_tab == PerformanceTab::TopCommands, |this| {
                        this.child(self.render_top_commands_body(cx))
                    })
                    .when(self.current_tab == PerformanceTab::Latency, |this| {
                        this.child(self.render_latency_body(cx))
                    }),
            )
            // The toolbar Export dropdown dispatches these; handle them
            // here on the panel root (same pattern as `EditorAction`).
            .on_action(cx.listener(|this, event: &SlowlogAction, _window, cx| match event {
                SlowlogAction::ExportCsv => this.export_csv(cx),
                SlowlogAction::ExportJson => this.export_json(cx),
                SlowlogAction::ToggleCommand(ix) => {
                    // The action carries an index into `available_commands` (it has
                    // to be `Copy`), so resolve it back to the name here. A stale
                    // index — the list is rebuilt on every refresh — simply misses.
                    if let Some(cmd) = this.available_commands.get(*ix as usize).cloned() {
                        this.toggle_command(cmd, cx);
                    }
                }
                SlowlogAction::ClearCommands => {
                    this.selected_commands.clear();
                    this.apply_filters(cx);
                }
            }))
            .into_any_element()
    }
}

impl ZedisSlowlogEditor {
    /// Tab-aware right-side toolbar. SlowLog tab keeps the existing
    /// min-duration / command-filter chips; Latency tab swaps in
    /// Refresh + Reset actions.
    fn render_toolbar_actions(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
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

        match self.current_tab {
            PerformanceTab::SlowLog => ZedisDivider::new()
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
                        .child(Label::new("ms").text_color(cx.theme().muted_foreground).text_sm()),
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
                // shown when there is at least one row to export.
                .when(self.row_count > 0, |this| {
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
                .child(self.reset_slowlog_button(cx))
                .into_any_element(),
            PerformanceTab::TopCommands => h_flex()
                .gap_2()
                .items_center()
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
    /// start a fresh window" usually gets decided).
    fn reset_slowlog_button(&self, cx: &mut gpui::Context<Self>) -> Button {
        Button::new("slowlog-reset")
            .outline()
            .small()
            .icon(IconName::CircleX)
            .label(i18n_slowlog_editor(cx, "slowlog_reset"))
            .tooltip(i18n_slowlog_editor(cx, "slowlog_reset_tooltip"))
            .on_click(cx.listener(|this, _, window, cx| this.confirm_reset_slowlog(window, cx)))
    }

    /// The Top Commands tab body: slow-log entries grouped by command and
    /// ranked by total time consumed. Complements the ServerLoad panel's
    /// commandstats view — that one answers "which command is *frequent*",
    /// this one answers "which command is *slow*".
    fn render_top_commands_body(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let rows = aggregate_commands(&self.all_rows);
        if rows.is_empty() {
            return centered_message(i18n_slowlog_editor(cx, "no_slowlogs"), muted).into_any_element();
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
                let avg_us = agg.total_us / agg.count.max(1) as u64;
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
                            .child(Label::new(format_us_as_ms(agg.total_us)).text_sm()),
                    )
                    .child(div().w(px(110.0)).child(Label::new(format_us_as_ms(avg_us)).text_sm()))
                    .child(
                        div().w(px(110.0)).child(
                            Label::new(format_us_as_ms(agg.max_us))
                                .text_sm()
                                .text_color(severity_color((agg.max_us / 1000) as i64, cx)),
                        ),
                    )
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

        // Tracking-disabled banner gets an inline "Enable tracking"
        // button so users can flip the toggle without bouncing to the
        // Config panel. Hidden when tracking is already on or the
        // server pre-dates LATENCY altogether.
        let show_enable_button = self.latency_threshold_ms == 0 && !self.latency_unsupported;

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .child(Label::new(threshold_label).text_xs().text_color(banner_color).flex_1())
                    .when(show_enable_button, |this| {
                        this.child(
                            Button::new("latency-enable-tracking")
                                .primary()
                                .xsmall()
                                .label(i18n_slowlog_editor(cx, "enable_tracking_button"))
                                .on_click(cx.listener(|this, _, w, cx| this.enable_latency_tracking(w, cx))),
                        )
                    }),
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
        let event_for_jump = event.clone();
        let event_ts = ev.timestamp;
        let is_expanded = self.expanded_event.as_deref() == Some(event.as_str());
        let when_str = format_unix_seconds(ev.timestamp);
        let id_hash: u32 = djb2_hash(event.as_ref());

        // Cross-tab chip: count of slow-log rows that fired within the
        // correlation window around this event. When > 0, give the user
        // a one-click "jump back to SlowLog filtered to that window".
        let slow_count = correlated_slowlog_count_for_event(ev.timestamp, &self.all_rows);
        let jump_chip: Option<gpui::AnyElement> = if slow_count > 0 {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            let label: SharedString = rust_i18n::t!(
                "slowlog_editor.chip_slow_nearby",
                count = slow_count.to_string(),
                locale = locale
            )
            .to_string()
            .into();
            Some(
                Button::new(("latency-jump-slow", id_hash))
                    .outline()
                    .xsmall()
                    .label(label)
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.jump_to_slowlog_window(event_for_jump.clone().into(), event_ts, cx);
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };

        // Color the latest/max numbers — yellow at > 100ms, red at > 1s.
        let latest_color = severity_color(ev.latest_ms, cx);
        let max_color = severity_color(ev.max_ms, cx);

        let history = self.event_histories.get(event.as_str()).cloned();

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
            .when_some(jump_chip, |this, chip| this.child(chip))
            .child(
                Button::new(("latency-toggle", id_hash))
                    .ghost()
                    .xsmall()
                    .label(if is_expanded {
                        i18n_slowlog_editor(cx, "latency_hide_graph")
                    } else {
                        i18n_slowlog_editor(cx, "latency_show_graph")
                    })
                    .on_click(
                        cx.listener(move |this, _, _w, cx| this.expand_event(event_for_toggle.clone().into(), cx)),
                    ),
            );

        // Inline drill-down block: GPU sparkline (from HISTORY) +
        // tail of HISTORY samples.
        let detail: Option<gpui::AnyElement> = if is_expanded {
            Some(self.render_latency_detail(history, cx).into_any_element())
        } else {
            None
        };

        v_flex().child(row).when_some(detail, |this, d| this.child(d))
    }

    fn render_latency_detail(
        &self,
        history: Option<Vec<LatencySample>>,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        // Sparkline block: native GPU line chart sourced from LATENCY
        // HISTORY. Replaces the previous monospace ASCII `LATENCY
        // GRAPH` block — server-side ASCII art doesn't scale with the
        // panel and clashes visually with the Metrics view's canvases.
        let graph_block: gpui::AnyElement = match history.as_deref() {
            // No samples yet → render the loading placeholder so the
            // expanded row has something while the background fetch is
            // in flight.
            None => div()
                .px_3()
                .py_2()
                .child(Label::new(i18n_common(cx, "loading")).text_xs().text_color(muted))
                .into_any_element(),
            // Empty Vec — render the explicit "no samples yet"
            // affordance instead of an empty chart frame.
            Some([]) => div()
                .px_3()
                .py_2()
                .child(
                    Label::new(i18n_slowlog_editor(cx, "sparkline_no_history"))
                        .text_xs()
                        .text_color(muted),
                )
                .into_any_element(),
            Some(h) => {
                // LATENCY HISTORY timestamps are unix-seconds; the
                // chart helper formats unix-millis, so multiply by 1000
                // to reuse it without forking a second formatter.
                let dates: Vec<SharedString> = h.iter().map(|s| format_timestamp_ms(s.timestamp * 1000)).collect();
                let values: Vec<f64> = h.iter().map(|s| s.latency_ms as f64).collect();
                // Floor y_max at 0.01 — `make_line_canvas` divides by
                // y_max for scale, so 0 would produce NaN.
                let y_max = values.iter().copied().fold(0.01_f64, f64::max);
                // Roughly 4 x-axis labels evenly spaced; clamp at 1 so
                // a single-sample history still renders a tick.
                let tick_margin = (h.len() / 4).max(1);
                let params = ChartParams {
                    dates: Arc::new(dates),
                    y_max,
                    y_format: Box::new(|v| format!("{:.0} ms", v)),
                    tick_margin,
                    border: theme.border,
                    muted_fg: muted,
                };
                div()
                    .h(px(140.))
                    .px_3()
                    .py_2()
                    .child(make_line_canvas(params, Arc::new(values), theme.chart_2, false))
                    .into_any_element()
            }
        };

        // Tail of raw history samples for users who want exact numbers.
        // Limit so a 160-point history doesn't drown the panel — the
        // sparkline above carries the shape.
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

#[cfg(test)]
mod tests {
    use super::{SlowLogRow, aggregate_commands, format_us_as_ms};

    fn row(command: &str, duration_us: u64) -> SlowLogRow {
        SlowLogRow {
            timestamp: "2026-08-02 10:00:00".into(),
            raw_timestamp: 0,
            duration: "--".into(),
            duration_ms: duration_us / 1000,
            duration_us,
            command: command.to_string().into(),
            args: "".into(),
            client: "".into(),
            correlated_event: None,
        }
    }

    #[test]
    fn aggregates_by_command_ranked_by_total_time() {
        let rows = vec![
            row("GET", 2_000),
            row("HGETALL", 30_000),
            row("GET", 4_000),
            row("KEYS", 20_000),
        ];
        let aggs = aggregate_commands(&rows);
        assert_eq!(aggs.len(), 3);
        // Ranked by total time: HGETALL(30ms) > KEYS(20ms) > GET(6ms).
        assert_eq!(aggs[0].command.as_ref(), "HGETALL");
        assert_eq!(aggs[1].command.as_ref(), "KEYS");
        assert_eq!(aggs[2].command.as_ref(), "GET");
        assert_eq!(aggs[2].count, 2);
        assert_eq!(aggs[2].total_us, 6_000);
        assert_eq!(aggs[2].max_us, 4_000);
        let share_sum: f64 = aggs.iter().map(|a| a.share_pct).sum();
        assert!((share_sum - 100.0).abs() < 1e-6);
    }

    #[test]
    fn empty_rows_yield_no_aggregates() {
        assert!(aggregate_commands(&[]).is_empty());
    }

    #[test]
    fn formats_microseconds_as_ms() {
        assert_eq!(format_us_as_ms(12_500), "12.5 ms");
        assert_eq!(format_us_as_ms(400), "0.4 ms");
    }
}

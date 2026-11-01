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

use crate::assets::CustomIconName;
use crate::connection::ServerCommand;
/// Redis Slow Log viewer.
///
/// Displays a table of slow-query log entries fetched from the server's
/// periodic `SLOWLOG GET` refresh cycle. Columns: Timestamp, Duration,
/// Command, Client. Rows are sortable by arrival order (newest first).
/// On Valkey 8.1 the same table shows either of `COMMANDLOG`'s size logs
/// (large requests / large replies, fetched by the panel with a 30s poll)
/// behind a log-kind switch; the Duration column reads Size there.
use crate::connection::{
    CommandLogKind, LatencyEvent, LatencySample, ServerDb, SlowLogEntry, SlowLogSettings, command_log_reset,
    command_logs, config_set, floors, get_server, is_subcommand, latency_history, latency_latest,
    latency_monitor_threshold, latency_reset, script_show, slowlog_settings,
};
use crate::error::Error;
use crate::helpers::saves_files;
use crate::helpers::{SlowlogAction, build_csv, djb2_hash, format_unix_secs, get_mono_font_family, pacing};
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
    dialog_button_props, escalate_dangerous_body, i18n_common, i18n_hotkeys, i18n_monitor, i18n_slowlog_editor,
};
use crate::views::{ChartParams, ValueLabel, format_timestamp_ms, make_line_chart};
use crate::views::{ServerReport, export_to_file, open_server_report_dialog};
use ahash::AHashMap;
use gpui::{
    AnyElement, Edges, Entity, SharedString, Subscription, Task, WeakEntity, Window, div, prelude::*, px, relative,
};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    menu::DropdownMenu,
    table::{DataTable, TableState},
    v_flex,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zedis_core::tally::totals_by_command;
use zedis_ui::{
    CellAction, CellActionProvider, CellRenderer, TextColumn, ZedisDialog, ZedisDivider, ZedisTextTable, fitted_table,
};

// One file per tab / subsystem that has a life of its own.
mod commandlog;
mod latency;
mod table;
mod top_commands;

use table::{SlowLogRow, build_table, correlated_slowlog_count_for_event};
use top_commands::{filter_unit, format_us_as_ms};

/// Which sub-panel the performance view is showing. Slow Log is the
/// default — historically this view was slow-log only, so keeping it
/// as the landing tab avoids surprising users on upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerformanceTab {
    SlowLog,
    TopCommands,
    Latency,
}

/// Width of the toolbar's keyword box — the args it searches are long, so give
/// it room to hold a recognisable fragment of a key.
const KEYWORD_INPUT_WIDTH: f32 = 180.0;

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
    /// Whether the server can show a cached script's source (Valkey's
    /// `SCRIPT SHOW`, `floors::SCRIPT_SHOW`) — read by the table's cell
    /// actions, which have no context of their own, so it is a cell shared
    /// with them and refreshed as the server reports itself.
    script_show_supported: Rc<Cell<bool>>,
    /// Shared table state that owns the [`SlowlogTableDelegate`] and drives rendering.
    table_state: Entity<TableState<ZedisTextTable>>,
    /// The [`ZedisServerState::slow_logs_generation`] the rows were built
    /// from: a heartbeat that brought no new log (all but one a minute)
    /// rebuilds nothing.
    shown_slow_logs: Option<u64>,
    /// Total number of filtered rows currently displayed.
    row_count: usize,
    /// All unfiltered rows from the server.
    all_rows: Vec<SlowLogRow>,
    /// Unique command names extracted from all rows, sorted alphabetically.
    available_commands: Vec<SharedString>,
    /// Currently selected commands for filtering. Empty means show all.
    selected_commands: HashSet<SharedString>,
    /// Minimum measure filter — milliseconds in the slow log, bytes in a
    /// size log (the box takes KB). 0 means no filter.
    min_amount: u64,
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

    /// Which log the SlowLog / Top Commands tabs show: the slow log every
    /// server has (from the heartbeat), or one of Valkey 8.1's COMMANDLOG
    /// size logs, fetched by the panel itself.
    log_kind: CommandLogKind,
    /// The size log's entries (`COMMANDLOG GET`), newest first — the slow
    /// log's live on the server state.
    commandlog_entries: Vec<SlowLogEntry>,
    commandlog_loading: bool,
    _commandlog_task: Option<Task<()>>,
    /// 30-second poll while a size log is shown; dropped on the switch back
    /// to the slow log.
    _commandlog_poll_task: Option<Task<()>>,

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
    /// What the server records — `slowlog-log-slower-than` and
    /// `slowlog-max-len` — read best-effort with each slow log and printed
    /// above the table. The Latency tab says its threshold; this tab used to
    /// say nothing about why its table was empty or short (#238). `None`
    /// until read, and when the server would not say.
    slowlog_settings: Option<SlowLogSettings>,
    _slowlog_settings_task: Option<Task<()>>,

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
        let all_rows = Self::build_all_rows(server_state.read(cx).slow_logs(), CommandLogKind::Slow, &[]);
        let shown_slow_logs = Some(server_state.read(cx).slow_logs_generation());
        let available_commands = Self::extract_commands(&all_rows);
        let filtered = all_rows.clone();
        let row_count = filtered.len();
        let editor_weak = cx.entity().downgrade();
        // Read by the table's cell actions, which have no context of their
        // own; set as the server reports itself (the subscription below).
        let script_show_supported = Rc::new(Cell::new(false));
        let table_state = cx.new(|cx| {
            TableState::new(
                build_table(
                    editor_weak,
                    CommandLogKind::Slow,
                    script_show_supported.clone(),
                    window,
                    cx,
                ),
                window,
                cx,
            )
        });
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
                    this.min_amount = text.trim().parse::<u64>().unwrap_or(0) * filter_unit(this.log_kind);
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
        subscriptions.push(
            cx.subscribe_in(&server_state, window, |this, _state, event, window, cx| {
                // The server's word on `SCRIPT SHOW` arrives with its version.
                this.script_show_supported
                    .set(this.server_state.read(cx).supports(floors::SCRIPT_SHOW));
                // A new server starts on its slow log — the size log shown was
                // the previous server's — and its own settings are read.
                if matches!(event, ServerEvent::ServerSelected(_)) {
                    this.set_log_kind(CommandLogKind::Slow, window, cx);
                    this.fetch_slowlog_settings(cx);
                }
                // A size log is the panel's own fetch, not the heartbeat's.
                if !this.log_kind.is_slow() {
                    return;
                }
                if matches!(
                    event,
                    ServerEvent::ServerRedisInfoUpdated | ServerEvent::ServerSelected(_)
                ) {
                    // Skip the rebuild — every row formatted, per master —
                    // when the log is the one already shown.
                    let generation = this.server_state.read(cx).slow_logs_generation();
                    if this.shown_slow_logs == Some(generation) {
                        return;
                    }
                    this.shown_slow_logs = Some(generation);
                    // Use the current latency_events snapshot so chips appear
                    // immediately when slowlog refreshes after latency was
                    // already populated.
                    let new_rows = this.rows_from_current_log(cx);
                    this.replace_rows(new_rows, cx);
                    // A setting changed elsewhere (the Config panel, redis-cli)
                    // is picked up with the next sample.
                    this.fetch_slowlog_settings(cx);
                }
            }),
        );

        let mut editor = Self {
            server_state,
            script_show_supported,
            table_state,
            shown_slow_logs,
            row_count,
            all_rows,
            available_commands,
            selected_commands: HashSet::new(),
            min_amount: 0,
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
            log_kind: CommandLogKind::Slow,
            commandlog_entries: Vec::new(),
            commandlog_loading: false,
            _commandlog_task: None,
            _commandlog_poll_task: None,
            window_filter: None,
            window_filter_label: None,
            slowlog_settings: None,
            _slowlog_settings_task: None,
            _subscriptions: subscriptions,
        };
        editor.fetch_slowlog_settings(cx);
        editor
    }

    /// Read the two settings behind the slow log, best-effort: a refusal
    /// (`CONFIG` hidden by a managed cloud, a user without `@admin`) leaves
    /// them unknown and the line above the table unsaid.
    fn fetch_slowlog_settings(&mut self, cx: &mut gpui::Context<Self>) {
        let state = self.server_state.read(cx);
        if state.server_id().is_empty() {
            return;
        }
        let at = state.at();
        self._slowlog_settings_task = Some(cx.spawn(async move |handle, cx| {
            let settings = cx.background_spawn(async move { slowlog_settings(&at).await }).await;
            let _ = handle.update(cx, |this, cx| {
                this.slowlog_settings = settings.ok().filter(|settings| !settings.is_unknown());
                cx.notify();
            });
        }));
    }

    /// What the server records, said above the Slow Log table: the threshold
    /// and the length kept — or that logging is off, which is why the table
    /// is empty — with the way to the Config panel beside it (#238). Nothing
    /// for a size log (its thresholds are in its empty state) or where the
    /// settings could not be read.
    fn render_slowlog_settings_line(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        if !self.log_kind.is_slow() {
            return None;
        }
        let settings = self.slowlog_settings?;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut color = cx.theme().muted_foreground;
        let mut parts: Vec<String> = Vec::new();
        if settings.is_disabled() {
            parts.push(i18n_slowlog_editor(cx, "settings_disabled").to_string());
            color = cx.theme().yellow;
        } else if settings.logs_every_command() {
            parts.push(i18n_slowlog_editor(cx, "settings_every_command").to_string());
        } else if let Some(us) = settings.slower_than_us {
            let threshold = format_us_as_ms(u64::try_from(us).unwrap_or(0));
            parts.push(
                rust_i18n::t!(
                    "slowlog_editor.settings_slower_than",
                    threshold = threshold,
                    locale = locale
                )
                .to_string(),
            );
        }
        if let Some(max) = settings.max_len {
            parts.push(rust_i18n::t!("slowlog_editor.settings_max_len", max = max, locale = locale).to_string());
        }
        if parts.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .mx_3()
                .mt_2()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(
                    Label::new(SharedString::from(parts.join(" · ")))
                        .text_xs()
                        .text_color(color),
                )
                .child(
                    Button::new("slowlog-open-config")
                        .ghost()
                        .xsmall()
                        .label(i18n_slowlog_editor(cx, "settings_open_config"))
                        .on_click(|_, _w, cx| {
                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                store.update(cx, |state, cx| state.go_to_view(ServerView::Config, cx));
                            });
                        }),
                )
                .into_any_element(),
        )
    }

    /// The empty state of the shown log: the slow log explains the periodic
    /// refresh, a size log its thresholds — or "Loading…" while its first
    /// fetch is out.
    fn empty_message(&self, cx: &gpui::App) -> SharedString {
        if self.log_kind.is_slow() {
            i18n_slowlog_editor(cx, "no_slowlogs")
        } else if self.commandlog_loading {
            i18n_common(cx, "loading")
        } else {
            i18n_slowlog_editor(cx, "no_commandlogs")
        }
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
        let has_filter = !self.selected_commands.is_empty() || self.min_amount > 0 || !self.keyword.is_empty();
        let empty_message = self.empty_message(cx);

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
            // Toolbar. One row where it fits; where it does not, the
            // actions drop to a second row instead of running off the edge
            // (at 900px Export and Reset could not be reached).
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .px_4()
                    .py(px(6.))
                    .min_h(px(40.))
                    .gap_x_4()
                    .gap_y_2()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .justify_between()
                    .child(
                        h_flex()
                            .flex_none()
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
                            )
                            .when(
                                matches!(self.current_tab, PerformanceTab::SlowLog | PerformanceTab::TopCommands),
                                |this| {
                                    let state = self.server_state.read(cx);
                                    let supports_monitor = state.supports_view(ServerView::Monitor);
                                    let supports_hotkeys = state.supports_view(ServerView::Hotkeys);
                                    if !supports_monitor && !supports_hotkeys {
                                        return this;
                                    }
                                    this.child(
                                        Label::new(i18n_common(cx, "see_also"))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .when(supports_monitor, |this| {
                                        this.child(
                                            Button::new("slowlog-goto-monitor")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(CustomIconName::Activity))
                                                .label(i18n_monitor(cx, "title"))
                                                .on_click(|_, _w, cx| {
                                                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                                        store.update(cx, |state, cx| {
                                                            state.go_to_view(ServerView::Monitor, cx);
                                                        });
                                                    });
                                                }),
                                        )
                                    })
                                    .when(supports_hotkeys, |this| {
                                        this.child(
                                            Button::new("slowlog-goto-hotkeys")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(CustomIconName::Flame))
                                                .label(i18n_hotkeys(cx, "title"))
                                                .on_click(|_, _w, cx| {
                                                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                                        store.update(cx, |state, cx| {
                                                            state.go_to_view(ServerView::Hotkeys, cx);
                                                        });
                                                    });
                                                }),
                                        )
                                    })
                                },
                            ),
                    )
                    .child(div().flex_none().child(self.render_toolbar_actions(cx))),
            )
            // Body — slowlog table or latency panel depending on active tab.
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(self.current_tab == PerformanceTab::SlowLog, |this| {
                        let window_pill = self.window_filter_label.clone();
                        let settings_line = self.render_slowlog_settings_line(cx);
                        this
                            // What the server records, whether or not it has
                            // recorded anything yet.
                            .children(settings_line)
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
                                        Label::new(empty_message.clone()).text_color(cx.theme().muted_foreground),
                                    ),
                                )
                            })
                            .when(!is_empty, |this| {
                                this.child(fitted_table(
                                    &self.table_state,
                                    DataTable::new(&self.table_state)
                                        .stripe(true)
                                        .bordered(false)
                                        .scrollbar_visible(true, true),
                                ))
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
    format_unix_secs(ts).unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests {
    use super::top_commands::aggregate_commands;
    use super::{CommandLogKind, SlowLogEntry, SlowLogRow, format_us_as_ms};
    use std::time::Duration;

    fn row(command: &str, duration_us: u64) -> SlowLogRow {
        SlowLogRow {
            timestamp: "2026-08-02 10:00:00".into(),
            raw_timestamp: 0,
            amount_text: "--".into(),
            amount: duration_us / 1000,
            amount_fine: duration_us,
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
        assert_eq!(aggs[2].total, 6_000);
        assert_eq!(aggs[2].max, 4_000);
        let share_sum: f64 = aggs.iter().map(|a| a.share_pct).sum();
        assert!((share_sum - 100.0).abs() < 1e-6);
    }

    /// A container's subcommand is part of the command whatever version
    /// the server is: `CONFIG GET` is the command and `maxmemory` its
    /// argument. The set this was asked of was built at version 0.0.0, which
    /// every command postdates, so it was empty and the column said `CONFIG`.
    #[test]
    fn a_subcommand_is_part_of_the_command() {
        crate::init_embedded_commands();
        let entry = |args: &[&str]| SlowLogEntry {
            id: 1,
            timestamp: 1,
            duration: Duration::from_micros(10),
            amount: 10,
            args: args.iter().map(|arg| arg.to_string()).collect(),
            client_addr: None,
            client_name: None,
        };
        let row = SlowLogRow::from_entry(&entry(&["config", "get", "maxmemory"]), CommandLogKind::Slow);
        assert_eq!((row.command.as_ref(), row.args.as_ref()), ("CONFIG GET", "maxmemory"));
        // A key that happens to spell a subcommand is still a key.
        let row = SlowLogRow::from_entry(&entry(&["GET", "set"]), CommandLogKind::Slow);
        assert_eq!((row.command.as_ref(), row.args.as_ref()), ("GET", "set"));
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

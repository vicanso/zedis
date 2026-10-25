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
use crate::connection::{Capability, MonitorFeeds, ServerDb, get_server, open_monitor_feeds, zedis_client_addrs};
use crate::error::Error;
use crate::helpers::channel;
use crate::helpers::{
    MonitorAction, build_csv, format_clock, get_mono_font_family, group_thousands, pacing, saves_files,
};
use crate::states::{
    ConnectionErrorKind, GlobalEvent, NotificationAction, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState,
    back_to_editor_tooltip, content_area_width, escalate_dangerous_body, i18n_common, i18n_monitor, i18n_status_bar,
    update_app_state_and_save,
};
/// Redis MONITOR live viewer.
///
/// Opens dedicated (non-cached) connections to each master node, sends
/// the `MONITOR` command, and streams the output in real time into a
/// scrollable table.  Supports keyword and command-type filtering, and
/// leaves out what Zedis's own connections send unless asked (#241).
/// The buffer is capped at `MAX_RECORDS` entries.
use crate::views::{BridgeMonitor, bridge_monitor, confirm_dangerous_command, export_to_file, open_key_in_editor};
use gpui::{AnyElement, App, Div, Entity, Hsla, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonGroup},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::DropdownMenu,
    table::{DataTable, TableState},
    tooltip::Tooltip,
    v_flex,
};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;
use tracing::error;
use web_time::Instant;
use zedis_core::tally::{MonitorTally, Tally};
use zedis_ui::{CellAction, CellActionProvider, TextColumn, ZedisDialog, ZedisTextTable, fitted_table};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Maximum number of monitor records to keep in memory.
const MAX_RECORDS: usize = 10_000;
const KEYWORD_INPUT_WIDTH: f32 = 200.0;
/// Sliding window for the events/sec badge.
const RATE_WINDOW_SECS: f64 = 5.0;
/// Sustained event rate (over the sliding window) above which MONITOR is
/// stopped automatically. MONITOR's cost is paid on the server's command
/// thread, so a runaway stream is disconnected outright — pausing the UI
/// alone would keep the server formatting every command for us.
const AUTO_STOP_EVENTS_PER_SEC: f64 = 5000.0;
/// How long a sample may run, in seconds: what the Sample menu offers.
const SAMPLE_SECONDS: [u64; 3] = [5, 10, 30];
/// Rows each list of a sample's summary shows.
const SUMMARY_ROWS: usize = 15;

// ── Parsed MONITOR line ──────────────────────────────────────────────

/// A single parsed MONITOR output line.
///
/// Redis MONITOR format:
///   `+1339518083.107412 [0 127.0.0.1:60866] "keys" "*"`
#[derive(Clone, Debug)]
struct MonitorEntry {
    timestamp: SharedString,
    node: SharedString,
    db: SharedString,
    client: SharedString,
    command: SharedString,
    args: SharedString,
    /// The first argument as the server printed it: the key, for most
    /// commands — what a sample groups by prefix.
    key: Option<SharedString>,
}

/// Parse a raw MONITOR output line into a `MonitorEntry`.
///
/// Format: `+<unix_ts> [<db> <client_addr>] "<cmd>" "<arg1>" ...`
fn parse_monitor_line(line: &str, node_label: &str) -> Option<MonitorEntry> {
    // The RESP '+' prefix is already stripped by the redis library.
    // Skip preamble lines like "OK".
    let line = line.strip_prefix('+').unwrap_or(line);
    if !line.contains('[') {
        return None;
    }

    // Split at '[' to get timestamp and the rest. The client block ends at
    // the `]` before the first quoted word, not the first `]`: an IPv6
    // client is written `[::1]:60866`, which cut the address in two.
    let (ts_part, rest) = line.split_once('[')?;
    let (meta, cmd_part) = match rest.find("] \"") {
        Some(end) => (&rest[..end], &rest[end + 1..]),
        None => rest.split_once(']')?,
    };

    // Parse timestamp
    let ts_str = ts_part.trim();
    let timestamp: SharedString = if let Ok(secs) = ts_str.parse::<f64>() {
        let dt = chrono::DateTime::from_timestamp(secs as i64, ((secs.fract()) * 1_000_000_000.0) as u32);
        dt.map(|d| format_clock(&d, true))
            .unwrap_or_else(|| ts_str.to_string())
            .into()
    } else {
        ts_str.to_string().into()
    };

    // Parse db and client from meta: "0 127.0.0.1:60866"
    let meta = meta.trim();
    let (db, client) = meta.split_once(' ').unwrap_or((meta, ""));

    // Parse command and args from the remainder: " "keys" "*""
    let cmd_part = cmd_part.trim();
    let mut parts: Vec<&str> = Vec::new();
    let mut in_quote = false;
    let mut start = 0;
    let bytes = cmd_part.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Inside a word the server escapes a quote (and a backslash)
            // with a backslash: `\"` is part of the word, not its end —
            // taking it as one shifted every later argument a column.
            b'\\' if in_quote => i += 1,
            b'"' if in_quote => {
                parts.push(&cmd_part[start..i]);
                in_quote = false;
            }
            b'"' => {
                in_quote = true;
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    let (command, args) = if parts.is_empty() {
        (cmd_part.to_string(), String::new())
    } else {
        let cmd = parts[0].to_uppercase();
        let args = parts[1..].join(" ");
        (cmd, args)
    };
    let key = parts.get(1).map(|key| SharedString::from(key.to_string()));

    Some(MonitorEntry {
        timestamp,
        node: node_label.to_string().into(),
        db: db.to_string().into(),
        client: client.to_string().into(),
        command: command.into(),
        args: args.into(),
        key,
    })
}

// ── Table columns ────────────────────────────────────────────────────

const COL_TIMESTAMP: &str = "timestamp";
const COL_NODE: &str = "node";
const COL_DB: &str = "db";
const COL_CLIENT: &str = "client";
const COL_COMMAND: &str = "command";
const COL_ARGS: &str = "args";
const COLUMN_KEYS: [&str; 6] = [COL_TIMESTAMP, COL_NODE, COL_DB, COL_CLIENT, COL_COMMAND, COL_ARGS];
/// Index of the args column in [`COLUMN_KEYS`] — the cell with the
/// open-first-arg-as-key jump.
const ARGS_COLUMN: usize = 5;

impl MonitorEntry {
    fn cells(&self) -> Vec<SharedString> {
        vec![
            self.timestamp.clone(),
            self.node.clone(),
            self.db.clone(),
            self.client.clone(),
            self.command.clone(),
            self.args.clone(),
        ]
    }
}

/// The MONITOR grid: six text columns, newest row first, capped at
/// [`MAX_RECORDS`], with a hover jump from an args cell to its key.
fn build_table(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &App) -> ZedisTextTable {
    let content_width = content_area_width(window, cx);

    // Cells are `[label ..flex_1..][copy button ..flex_none..]`, and the copy
    // button only appears on hover — it takes ~36px out of the label's box, so
    // text that fits at rest is clipped the moment the pointer lands on the
    // row. These budget for it, on top of the 20px of side padding:
    //   timestamp: "22:31:05.123" — 12 mono chars (~101px)
    //   node:      "192.168.1.10:31545" — the master's host:port, ~18 (~151px)
    //   client:    "127.0.0.1:60866" — ~15 (~126px)
    let ts_width = 190.;
    let node_width = 240.;
    let db_width = 80.;
    let client_width = 220.;
    let cmd_width = 120.;
    // The arguments have what the others leave, and no less than this.
    let args_width = 200.;
    let widths = [ts_width, node_width, db_width, client_width, cmd_width, args_width];

    let columns = COLUMN_KEYS
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(ix, (&key, width))| {
            let column = TextColumn::new(key, i18n_monitor(cx, key), width).sortable();
            if ix == ARGS_COLUMN { column.fills() } else { column }
        })
        .collect();

    // Args cell: the first argument is the key for most commands — offer a
    // hover jump straight to it in the editor (heuristic; args with spaces
    // are ambiguous, so only the first token is used).
    let open_key_tooltip = i18n_common(cx, "open_key_tooltip");
    let jump: CellActionProvider = Rc::new(move |col_ix, cells| {
        let action = || -> Option<CellAction> {
            if col_ix != ARGS_COLUMN {
                return None;
            }
            let key: SharedString = cells
                .get(ARGS_COLUMN)?
                .split(' ')
                .next()
                .filter(|s| !s.is_empty())?
                .to_string()
                .into();
            let server_state = server_state.clone();
            Some(CellAction {
                icon: IconName::Search,
                tooltip: open_key_tooltip.clone(),
                on_click: Rc::new(move |_window, cx| open_key_in_editor(&server_state, key.clone(), cx)),
            })
        };
        action().into_iter().collect()
    });

    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .max_rows(MAX_RECORDS)
        // The timestamp is never what someone searches for.
        .filter_columns(&[COL_NODE, COL_DB, COL_CLIENT, COL_COMMAND, COL_ARGS])
        .cell_action(jump)
        .fitted_to(content_width)
}

// ── Main view ────────────────────────────────────────────────────────

pub struct ZedisMonitor {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<ZedisTextTable>>,
    keyword_state: Entity<InputState>,
    monitoring: bool,
    /// Drop inbound batches while true (the MONITOR connections stay up).
    paused: bool,
    /// Set when the stream was cut by the rate guard; shown as a badge
    /// until the next start.
    auto_stopped: bool,
    /// Timestamps of recent ingests for the events/sec badge.
    rate_ticks: VecDeque<Instant>,
    row_count: usize,
    monitor_tasks: Vec<Task<()>>,
    /// A timed sample in progress (the Sample menu): it counts every line it
    /// sees, by client, command and key prefix — all of them, where the
    /// table keeps the last `MAX_RECORDS`.
    sampling: Option<Sampling>,
    /// The last finished sample, until another starts or the server changes.
    sample: Option<SampleResult>,
    /// Show the sample's summary rather than the lines.
    show_summary: bool,
    /// Show the lines Zedis's own connections send. Off by default — on a
    /// quiet server the heartbeat's `INFO` every two seconds was most of the
    /// feed (#241) — and kept in the app state across visits.
    show_own: bool,
    /// Zedis's own connections on each master, as `(node, addr)`, which is
    /// how a `MONITOR` line names its client; read before the first line and
    /// again every `MONITOR_OWN_CLIENTS_REFRESH` while the feed runs.
    own_clients: HashSet<(SharedString, SharedString)>,
    _subscriptions: Vec<Subscription>,
}

/// A sample being taken: its counts, how long it was asked to run, and the
/// timer that ends it (dropped, and so cancelled, when it ends early).
struct Sampling {
    tally: MonitorTally,
    seconds: u64,
    started: Instant,
    _timer: Task<()>,
}

/// What a finished sample counted, and over how long.
struct SampleResult {
    tally: MonitorTally,
    elapsed: Duration,
    /// Ended before its time — Stop, the rate guard, a dropped link.
    cut_short: bool,
}

/// A share of the sample, in percent.
fn share_of(count: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    format!("{:.1}%", count as f64 * 100.0 / total as f64)
}

/// One row of a summary list: the count and its share, the name after them.
fn summary_row(count: String, share: String, muted: Hsla) -> Div {
    h_flex()
        .w_full()
        .gap_3()
        .child(div().w(px(72.)).flex_none().text_right().child(count))
        .child(div().w(px(56.)).flex_none().text_right().text_color(muted).child(share))
}

impl ZedisMonitor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let delegate = build_table(server_state.clone(), window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        subscriptions.push(cx.subscribe(&server_state, |this, _, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.handle_stop(cx);
                // Another server's sample says nothing about this one.
                this.sample = None;
                this.show_summary = false;
                this.table_state.update(cx, |state, _| state.delegate_mut().clear());
                this.row_count = 0;
                this.auto_stopped = false;
                this.rate_ticks.clear();
                cx.notify();
            }
        }));

        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });

        subscriptions.push(cx.subscribe(&keyword_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.handle_filter(cx);
            }
        }));

        Self {
            server_state,
            table_state,
            keyword_state,
            monitoring: false,
            paused: false,
            auto_stopped: false,
            rate_ticks: VecDeque::new(),
            row_count: 0,
            monitor_tasks: Vec::new(),
            sampling: None,
            sample: None,
            show_summary: false,
            show_own: cx.global::<ZedisGlobalStore>().read(cx).monitor_show_own_commands(),
            own_clients: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The latest reading of which clients are ours.
    fn set_own_clients(&mut self, own: HashSet<(String, String)>) {
        self.own_clients = own.into_iter().map(|(node, addr)| (node.into(), addr.into())).collect();
    }

    /// A line from one of Zedis's own connections.
    fn is_own(&self, entry: &MonitorEntry) -> bool {
        self.own_clients.contains(&(entry.node.clone(), entry.client.clone()))
    }

    /// The toggle: what it says is kept for the next visit.
    fn set_show_own(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_own = show;
        update_app_state_and_save(cx, "monitor_show_own_commands", move |state, _| {
            state.set_monitor_show_own_commands(show)
        });
        cx.notify();
    }

    fn events_per_sec(&self) -> f64 {
        let now = Instant::now();
        let window = std::time::Duration::from_secs_f64(RATE_WINDOW_SECS);
        let n = self
            .rate_ticks
            .iter()
            .filter(|t| now.duration_since(**t) <= window)
            .count();
        n as f64 / RATE_WINDOW_SECS
    }

    /// Record `n` freshly-ingested entries and answer whether the
    /// sustained rate has crossed the auto-stop threshold. Ticks older
    /// than twice the window are trimmed so the deque stays bounded.
    fn record_rate(&mut self, n: usize) -> bool {
        let now = Instant::now();
        for _ in 0..n {
            self.rate_ticks.push_back(now);
        }
        let window = std::time::Duration::from_secs_f64(RATE_WINDOW_SECS * 2.0);
        while self.rate_ticks.front().is_some_and(|t| now.duration_since(*t) > window) {
            self.rate_ticks.pop_front();
        }
        self.events_per_sec() > AUTO_STOP_EVENTS_PER_SEC
    }

    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        let keyword = self.keyword_state.read(cx).value().to_string();
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().set_filter(&keyword);
        });
        self.row_count = self.table_state.read(cx).delegate().visible_len();
        cx.notify();
    }

    /// Start, but confirm first on a high-risk (production-tagged) server.
    ///
    /// MONITOR is not the free read it looks like: the server formats *every*
    /// command it executes into a string and pushes it to each MONITOR client,
    /// on its single command thread — Redis's own docs measure a >50% throughput
    /// drop from one attached client. On a cluster we attach to every master, so
    /// each node pays it. Cheap servers start straight away; production gets one
    /// sentence explaining the cost before anything is attached.
    fn confirm_start(&mut self, sample_seconds: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        if self.monitoring || !self.server_state.read(cx).can(Capability::Observe) {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        // The bridge's answer first, where it has one: a refusal said here,
        // where the bridge's would only be "failed"; its question stands in
        // for the warning below, with the same weight, and its answer is
        // what lets `MONITOR` start there.
        let bridge = {
            let state = self.server_state.read(cx);
            bridge_monitor(state.strict_readonly(), state.write_gate())
        };
        if bridge == BridgeMonitor::Refused {
            let message = i18n_monitor(cx, "needs_full_account");
            cx.global::<ZedisGlobalStore>().clone().update(cx, |_, cx| {
                cx.emit(GlobalEvent::Notification(NotificationAction::new_warning(message)));
            });
            return;
        }
        if let BridgeMonitor::Ask(kind) = bridge
            && let Ok(server) = get_server(&server_id)
        {
            let db = self.server_state.read(cx).db();
            let entity = cx.entity();
            let answer = server.name.clone();
            confirm_dangerous_command(&server, db, &kind, Some("MONITOR"), window, cx, move |_window, cx| {
                entity.update(cx, |this, cx| {
                    this.handle_start(sample_seconds, Some(answer.clone()), cx)
                });
            });
            return;
        }
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        if !high_risk {
            self.handle_start(sample_seconds, None, cx);
            return;
        }

        let body = escalate_dangerous_body(cx, &server_id, i18n_monitor(cx, "start_confirm_prompt"));
        let button_props = DialogButtonProps::default()
            .cancel_text(i18n_common(cx, "cancel"))
            .ok_text(i18n_monitor(cx, "start"));
        let entity = cx.entity();
        ZedisDialog::new_alert(i18n_monitor(cx, "start_confirm_title"), body)
            .button_props(button_props)
            .on_ok(move |_, window, cx| {
                entity.update(cx, |this, cx| this.handle_start(sample_seconds, None, cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Attach MONITOR. With `sample_seconds`, it is a sample: the table starts
    /// empty, every line is counted, and the stream ends by itself when the
    /// time is up — then the summary is shown. `confirm` is the answer to
    /// the bridge's question ([`bridge_monitor_question`]), when it asked.
    fn handle_start(&mut self, sample_seconds: Option<u64>, confirm: Option<String>, cx: &mut Context<Self>) {
        // Observe is a read-class capability (allowed on read-only
        // connections); routed through the matrix so a stricter future mode
        // can turn observation off in one place.
        if self.monitoring || !self.server_state.read(cx).can(Capability::Observe) {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();

        self.monitoring = true;
        self.paused = false;
        self.auto_stopped = false;
        self.rate_ticks.clear();
        if let Some(seconds) = sample_seconds {
            self.handle_clear(cx);
            self.sample = None;
            self.show_summary = false;
            let timer = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(seconds)).await;
                let _ = this.update(cx, |this, cx| this.handle_stop(cx));
            });
            self.sampling = Some(Sampling {
                tally: MonitorTally::default(),
                seconds,
                started: Instant::now(),
                _timer: timer,
            });
        }
        cx.notify();

        // Our connections come and go — a pooled one expires after five idle
        // minutes and is dialed again on another port — so which clients are
        // ours is read again while the feed runs; dropped with it on Stop.
        let refresh_at = ServerDb::new(server_id.clone(), db);
        let refresh_entity = cx.entity().downgrade();
        let refresh = cx.spawn(async move |_handle, cx| {
            loop {
                cx.background_executor()
                    .timer(pacing::MONITOR_OWN_CLIENTS_REFRESH)
                    .await;
                match refresh_entity.update(cx, |this: &mut ZedisMonitor, _| this.paused) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => break,
                }
                let at = refresh_at.clone();
                let Ok(own) = cx.background_spawn(async move { zedis_client_addrs(&at).await }).await else {
                    continue;
                };
                if refresh_entity
                    .update(cx, |this: &mut ZedisMonitor, _| this.set_own_clients(own))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.monitor_tasks.push(refresh);

        let table_state = self.table_state.clone();
        let entity = cx.entity().downgrade();
        let (tx, rx) = channel::unbounded::<MonitorEntry>();

        let task = cx.spawn(async move |_handle, cx| {
            // One dedicated MONITOR connection per master, opened off the UI
            // thread. Which nodes failed comes back with the feeds, so a
            // failure is surfaced — it used to be opened inside the reader
            // task, where a failed node vanished silently, and on a
            // standalone that meant MONITOR appeared to do nothing at all.
            let at = ServerDb::new(server_id, db);
            let at = match confirm {
                Some(answer) => at.confirmed(answer),
                None => at,
            };
            // Which clients are ours, before the first line: the filter has
            // nothing to go on until this answers. A refusal leaves the set
            // empty and the feed unfiltered.
            let own_at = at.clone();
            let own = cx
                .background_spawn(async move { zedis_client_addrs(&own_at).await })
                .await
                .unwrap_or_default();
            let _ = entity.update(cx, |this: &mut ZedisMonitor, _| this.set_own_clients(own));
            let opened: Result<MonitorFeeds> = cx
                .background_spawn(async move { Ok(open_monitor_feeds(&at).await?) })
                .await;

            let MonitorFeeds { feeds, failures } = match opened {
                Ok(opened) => opened,
                Err(e) => {
                    let kind = e.connection_kind();
                    let _ = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                        this.monitoring = false;
                        if !this.note_feature_error(&e, cx) {
                            this.notify_monitor_failed(kind, cx);
                        }
                        cx.notify();
                    });
                    return;
                }
            };

            let mut fail_kind: Option<ConnectionErrorKind> = None;
            let mut fail_error: Option<Error> = None;
            for (node, e) in failures {
                error!(error = %e, node = %node, "failed to start MONITOR");
                fail_kind.get_or_insert(e.connection_kind());
                fail_error.get_or_insert(Error::Connection { source: e });
            }

            // One background reader per feed, each sending parsed entries
            // into the shared channel.
            let mut bg_tasks = Vec::new();
            for mut feed in feeds {
                let tx = tx.clone();
                let bg = cx.background_spawn(async move {
                    let node_label = feed.node().to_string();
                    while let Some(line) = feed.next_line().await {
                        if let Some(entry) = parse_monitor_line(&line, &node_label)
                            && tx.send(entry).await.is_err()
                        {
                            break;
                        }
                    }
                });
                bg_tasks.push(bg);
            }
            // Drop the original sender so rx completes when all bg senders drop
            drop(tx);

            // Surface any node that couldn't start; stop entirely if none did.
            if let Some(kind) = fail_kind {
                let any_started = !bg_tasks.is_empty();
                let _ = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                    // MONITOR renamed away or ACL-denied: the feature matrix
                    // takes it from here (one notice, then the placeholder).
                    let explained = fail_error
                        .as_ref()
                        .is_some_and(|error| this.note_feature_error(error, cx));
                    if !explained {
                        this.notify_monitor_failed(kind, cx);
                    }
                    if !any_started {
                        this.monitoring = false;
                    }
                    cx.notify();
                });
                if !any_started {
                    return;
                }
            }

            // Read entries from channel in batches to avoid per-message UI refreshes.
            // After the first recv().await wakes us, drain all pending entries
            // via try_recv (up to BATCH_LIMIT) before issuing a single cx.notify().
            const BATCH_LIMIT: usize = 200;
            while let Ok(first) = rx.recv().await {
                let mut batch = Vec::with_capacity(BATCH_LIMIT);
                batch.push(first);
                while batch.len() < BATCH_LIMIT {
                    match rx.try_recv() {
                        Ok(entry) => batch.push(entry),
                        Err(_) => break,
                    }
                }

                let result = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                    // Paused: drain the channel but drop the batch, so the
                    // background streams never back up an unbounded queue.
                    if this.paused {
                        return false;
                    }
                    // The rate guard counts every line the server sent —
                    // ours cost it the same — so it is counted before the
                    // filter; the table, the sample and the export see only
                    // what is left.
                    let n = batch.len();
                    if !this.show_own {
                        batch.retain(|entry| !this.is_own(entry));
                    }
                    if let Some(sampling) = this.sampling.as_mut() {
                        let separator = this.server_state.read(cx).key_separator().to_string();
                        for entry in &batch {
                            sampling
                                .tally
                                .add(&entry.client, &entry.command, entry.key.as_deref(), &separator);
                        }
                    }
                    // Through the keyword the table already holds (the input
                    // sets it on every change), rather than setting it again
                    // per batch, which re-filtered the whole buffer each time.
                    table_state.update(cx, |state, _| {
                        state
                            .delegate_mut()
                            .push_front_batch(batch.into_iter().map(|entry| entry.cells()));
                    });
                    this.row_count = table_state.read(cx).delegate().visible_len();
                    let auto_stop = this.record_rate(n);
                    if auto_stop {
                        this.auto_stopped = true;
                        this.notify_auto_stopped(cx);
                    }
                    cx.notify();
                    auto_stop
                });
                match result {
                    // Rate guard tripped — fall through to the shared
                    // cleanup below, which drops every MONITOR connection.
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => break,
                }
            }

            // All streams ended, rate guard tripped, or entity dropped
            let _ = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                this.monitoring = false;
                this.paused = false;
                this.end_sampling();
                cx.notify();
            });

            // Dropping bg_tasks cancels any remaining background monitor streams
            drop(bg_tasks);
        });
        self.monitor_tasks.push(task);
    }

    fn handle_stop(&mut self, cx: &mut Context<Self>) {
        self.monitor_tasks.clear();
        self.monitoring = false;
        self.paused = false;
        self.end_sampling();
        cx.notify();
    }

    /// Close a sample in progress into its result, and show the summary.
    fn end_sampling(&mut self) {
        let Some(sampling) = self.sampling.take() else {
            return;
        };
        let elapsed = sampling.started.elapsed();
        self.sample = Some(SampleResult {
            tally: sampling.tally,
            elapsed,
            // A little short of the time is the timer's own scheduling.
            cut_short: elapsed + Duration::from_millis(500) < Duration::from_secs(sampling.seconds),
        });
        self.show_summary = true;
    }

    /// Keep the MONITOR connections attached but drop inbound batches.
    /// Note the server still pays the MONITOR cost while paused — the
    /// toggle only freezes the table; Stop is the one that relieves the
    /// server.
    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if !self.monitoring {
            return;
        }
        self.paused = !self.paused;
        if self.paused {
            self.rate_ticks.clear();
        }
        cx.notify();
    }

    /// Hands a start failure to the server's feature matrix; `true` when it
    /// was a NOPERM / unknown-command reply the matrix now explains (so no
    /// raw toast is needed).
    fn note_feature_error(&self, error: &Error, cx: &mut Context<Self>) -> bool {
        self.server_state
            .update(cx, |state, cx| state.note_command_error(error, cx))
    }

    /// Toast that MONITOR couldn't start on one or more nodes, naming the
    /// reason so a silently-dead monitor doesn't just look like an idle,
    /// empty stream.
    fn notify_monitor_failed(&self, kind: ConnectionErrorKind, cx: &mut Context<Self>) {
        let reason = i18n_status_bar(cx, kind.reason_key());
        let msg: SharedString = format!("{}: {}", i18n_monitor(cx, "monitor_failed"), reason).into();
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_error(msg)));
        });
    }

    /// Toast that the rate guard cut the stream, naming the threshold so
    /// the sudden stop doesn't read as a crash.
    fn notify_auto_stopped(&self, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let msg: SharedString = rust_i18n::t!(
            "monitor.auto_stop_notice",
            rate = format!("{AUTO_STOP_EVENTS_PER_SEC:.0}"),
            locale = locale
        )
        .to_string()
        .into();
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_warning(msg)));
        });
    }

    /// Snapshot of the rows the table is currently showing (filtered rows
    /// when a keyword filter is active, the whole ring buffer otherwise).
    fn visible_rows_snapshot(&self, cx: &Context<Self>) -> Vec<Vec<SharedString>> {
        self.table_state.read(cx).delegate().visible_rows()
    }

    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible_rows_snapshot(cx);
        if rows.is_empty() {
            return;
        }
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|cells| cells.iter().map(|c| c.to_string()).collect())
            .collect();
        let csv = build_csv(&COLUMN_KEYS, &data);
        self.save_export(csv.into_bytes(), "monitor.csv", true, cx);
    }

    fn export_json(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible_rows_snapshot(cx);
        if rows.is_empty() {
            return;
        }
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|cells| {
                let object: serde_json::Map<String, serde_json::Value> = COLUMN_KEYS
                    .iter()
                    .zip(cells)
                    .map(|(key, cell)| (key.to_string(), serde_json::Value::String(cell.to_string())))
                    .collect();
                serde_json::Value::Object(object)
            })
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Array(values)).unwrap_or_default();
        self.save_export(json.into_bytes(), "monitor.json", false, cx);
    }

    /// Shared save flow for the exports — mirrors the slow-log panel.
    fn save_export(&mut self, bytes: Vec<u8>, suggested: &'static str, is_csv: bool, cx: &mut Context<Self>) {
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

    /// Show the lines a summary row stands for: its name in the keyword
    /// box — a prefix without its `*`, which the filter would take literally.
    fn show_lines_for(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let term = name.strip_suffix('*').unwrap_or(&name).to_string();
        self.keyword_state
            .update(cx, |state, cx| state.set_value(term, window, cx));
        self.handle_filter(cx);
        self.show_summary = false;
        cx.notify();
    }

    /// The finished sample: how many commands over how long, then who sent
    /// them, what they were and which keys they touched, the most first.
    fn render_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(sample) = &self.sample else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let total = sample.tally.total();
        let seconds = sample.elapsed.as_secs_f64().max(0.001);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let header = rust_i18n::t!(
            "monitor.summary_header",
            total = group_thousands(total),
            secs = format!("{seconds:.1}"),
            rate = group_thousands((total as f64 / seconds).round() as u64),
            locale = locale
        )
        .to_string();
        let filter_tooltip = i18n_monitor(cx, "summary_show_lines");
        let others = i18n_monitor(cx, "summary_others");
        let column = |id: &'static str, title: SharedString, tally: &Tally| -> AnyElement {
            let mut rows: Vec<AnyElement> = tally
                .top(SUMMARY_ROWS)
                .into_iter()
                .enumerate()
                .map(|(ix, (name, count))| {
                    let name = SharedString::from(name);
                    let target = name.clone();
                    summary_row(group_thousands(count), share_of(count, total), muted)
                        .child(
                            div()
                                .id((id, ix))
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .cursor_pointer()
                                .hover(|style| style.underline())
                                .tooltip({
                                    let tip = filter_tooltip.clone();
                                    move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show_lines_for(target.clone(), window, cx)
                                }))
                                .child(name),
                        )
                        .into_any_element()
                })
                .collect();
            if tally.other() > 0 {
                rows.push(
                    summary_row(group_thousands(tally.other()), share_of(tally.other(), total), muted)
                        .child(Label::new(others.clone()).text_color(muted))
                        .into_any_element(),
                );
            }
            v_flex()
                .flex_1()
                .min_w(px(260.))
                .gap_1()
                .child(Label::new(title).font_semibold().pb_1())
                .children(rows)
                .into_any_element()
        };
        let lists = h_flex()
            .w_full()
            .items_start()
            .flex_wrap()
            .gap_6()
            .child(column(
                "summary-client",
                i18n_monitor(cx, "summary_clients"),
                &sample.tally.clients,
            ))
            .child(column(
                "summary-command",
                i18n_monitor(cx, "summary_commands"),
                &sample.tally.commands,
            ))
            .child(column(
                "summary-prefix",
                i18n_monitor(cx, "summary_prefixes"),
                &sample.tally.prefixes,
            ));
        div()
            .id("monitor-summary")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .text_sm()
                    .child(Label::new(header).font_semibold())
                    .when(sample.cut_short, |this| {
                        this.child(
                            Label::new(i18n_monitor(cx, "summary_cut_short"))
                                .text_xs()
                                .text_color(cx.theme().warning),
                        )
                    })
                    .child(
                        Label::new(i18n_monitor(cx, "summary_no_duration"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .map(|this| {
                        if total == 0 {
                            this.child(Label::new(i18n_monitor(cx, "summary_empty")).text_color(muted))
                        } else {
                            this.child(lists)
                        }
                    }),
            )
            .into_any_element()
    }

    fn handle_clear(&mut self, cx: &mut Context<Self>) {
        self.table_state.update(cx, |state, _| state.delegate_mut().clear());
        self.row_count = 0;
        cx.notify();
    }
}

impl Render for ZedisMonitor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.row_count == 0 && !self.monitoring;
        let total = self.table_state.read(cx).delegate().total_len();
        let count_label = if self.row_count == total {
            format!("({})", total)
        } else {
            format!("({}/{})", self.row_count, total)
        };
        let paused = self.paused;
        let sampling = self.sampling.is_some();
        let sampling_label = self.sampling.as_ref().map(|sampling| {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            SharedString::from(
                rust_i18n::t!("monitor.sampling_badge", secs = sampling.seconds, locale = locale).to_string(),
            )
        });
        let show_summary = self.show_summary && self.sample.is_some();
        let rate = self.events_per_sec();
        let rate_label: Option<SharedString> = if self.monitoring && !paused && rate > 0.05 {
            Some(format!("{rate:.1}/s").into())
        } else {
            None
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            // Monospace cascades to the command stream + filter/toolbar text.
            .font_family(get_mono_font_family())
            // Toolbar. One row where it fits; where it does not, the
            // controls drop to a second row instead of running off the edge.
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py(px(6.))
                    .min_h(px(40.))
                    .gap_x_4()
                    .gap_y_2()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("monitor-back")
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
                            .child(Icon::new(CustomIconName::Activity))
                            .child(Label::new(i18n_monitor(cx, "title")).text_color(cx.theme().foreground))
                            .child(
                                Label::new(count_label)
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            )
                            .when(self.monitoring && !paused && !sampling, |this| {
                                this.child(
                                    Label::new(i18n_monitor(cx, "monitoring"))
                                        .text_color(cx.theme().green)
                                        .text_sm(),
                                )
                            })
                            .when_some(sampling_label, |this, label| {
                                this.child(Label::new(label).text_color(cx.theme().green).text_sm())
                            })
                            .when(self.sample.is_some() && !sampling, |this| {
                                this.child(
                                    ButtonGroup::new("monitor-view-switch")
                                        .compact()
                                        .small()
                                        .outline()
                                        .child(
                                            Button::new("monitor-view-summary")
                                                .label(i18n_monitor(cx, "view_summary"))
                                                .selected(show_summary),
                                        )
                                        .child(
                                            Button::new("monitor-view-lines")
                                                .label(i18n_monitor(cx, "view_lines"))
                                                .selected(!show_summary),
                                        )
                                        .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                                            if let Some(ix) = clicks.first() {
                                                this.show_summary = *ix == 0;
                                                cx.notify();
                                            }
                                        })),
                                )
                            })
                            .when_some(rate_label, |this, r| {
                                // Warn as the stream approaches the auto-stop
                                // threshold so the cut-off isn't a surprise.
                                this.child(Label::new(r).text_xs().text_color(
                                    if rate > AUTO_STOP_EVENTS_PER_SEC * 0.5 {
                                        cx.theme().warning
                                    } else {
                                        cx.theme().muted_foreground
                                    },
                                ))
                            })
                            .when(paused, |this| {
                                this.child(
                                    Label::new(i18n_monitor(cx, "paused_badge"))
                                        .text_xs()
                                        .text_color(cx.theme().yellow),
                                )
                            })
                            .when(self.auto_stopped && !self.monitoring, |this| {
                                this.child(
                                    Label::new(i18n_monitor(cx, "auto_stopped_badge"))
                                        .text_xs()
                                        .text_color(cx.theme().warning),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .items_center()
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .cleanable(true)
                                    .small(),
                            )
                            // The other filter: an unticked box is how the
                            // feed says it is filtered.
                            .child(
                                Checkbox::new("monitor-show-own")
                                    .small()
                                    .label(i18n_monitor(cx, "show_own"))
                                    .tooltip(i18n_monitor(cx, "show_own_tooltip"))
                                    .checked(self.show_own)
                                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                        this.set_show_own(*checked, cx);
                                    })),
                            )
                            .when(!self.monitoring, |this| {
                                this.child(
                                    Button::new("start-monitor")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::Zap))
                                        .label(i18n_monitor(cx, "start"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_start(None, window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("sample-monitor")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::Clock3))
                                        .label(i18n_monitor(cx, "sample"))
                                        .tooltip(i18n_monitor(cx, "sample_tooltip"))
                                        .dropdown_menu(move |menu, _window, cx| {
                                            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                                            SAMPLE_SECONDS.iter().fold(menu, |menu, seconds| {
                                                let label = rust_i18n::t!(
                                                    "monitor.sample_secs",
                                                    secs = seconds,
                                                    locale = locale
                                                )
                                                .to_string();
                                                menu.menu(label, Box::new(MonitorAction::Sample(*seconds)))
                                            })
                                        }),
                                )
                            })
                            .when(self.monitoring && !sampling, |this| {
                                this.child(
                                    Button::new("pause-monitor")
                                        .outline()
                                        .small()
                                        .label(if paused {
                                            i18n_monitor(cx, "resume")
                                        } else {
                                            i18n_monitor(cx, "pause")
                                        })
                                        .tooltip(i18n_monitor(cx, "pause_tooltip"))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.toggle_pause(cx);
                                        })),
                                )
                            })
                            .when(self.monitoring, |this| {
                                this.child(
                                    Button::new("stop-monitor")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::X))
                                        .label(i18n_monitor(cx, "stop"))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.handle_stop(cx);
                                        })),
                                )
                            })
                            .when(saves_files() && total > 0, |this| {
                                this.child(
                                    Button::new("monitor-export")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::Download))
                                        .label(i18n_common(cx, "export"))
                                        .dropdown_menu(move |menu, _window, cx| {
                                            menu.menu(i18n_common(cx, "export_csv"), Box::new(MonitorAction::ExportCsv))
                                                .menu(
                                                    i18n_common(cx, "export_json"),
                                                    Box::new(MonitorAction::ExportJson),
                                                )
                                        }),
                                )
                            })
                            .child(
                                Button::new("clear-monitor")
                                    .outline()
                                    .small()
                                    .icon(Icon::new(CustomIconName::Eraser))
                                    .label(i18n_monitor(cx, "clear"))
                                    .disabled(total == 0)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.handle_clear(cx);
                                    })),
                            ),
                    ),
            )
            // Table body
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(show_summary, |this| this.child(self.render_summary(cx)))
                    .when(is_empty && !show_summary, |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Label::new(i18n_monitor(cx, "no_data")).text_color(cx.theme().muted_foreground)),
                        )
                    })
                    .when(!is_empty && !show_summary, |this| {
                        this.child(fitted_table(
                            &self.table_state,
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        ))
                    }),
            )
            // The toolbar Export dropdown dispatches these; handle them
            // here on the panel root (same pattern as the slow-log panel).
            .on_action(cx.listener(|this, event: &MonitorAction, window, cx| match event {
                MonitorAction::ExportCsv => this.export_csv(cx),
                MonitorAction::ExportJson => this.export_json(cx),
                MonitorAction::Sample(seconds) => this.confirm_start(Some(*seconds), window, cx),
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_monitor_line;

    #[test]
    fn a_line_splits_into_its_columns() {
        let entry = parse_monitor_line(r#"1339518083.107412 [0 127.0.0.1:60866] "keys" "*""#, "n").expect("a line");
        assert_eq!((entry.db.as_ref(), entry.client.as_ref()), ("0", "127.0.0.1:60866"));
        assert_eq!((entry.command.as_ref(), entry.args.as_ref()), ("KEYS", "*"));
    }

    /// A sample groups by the first argument, as the server printed it; a
    /// command without one has none.
    #[test]
    fn the_first_argument_is_kept_for_a_sample() {
        let entry = parse_monitor_line(r#"1.0 [0 10.0.0.1:5000] "hgetall" "user:profile:7""#, "n").expect("a line");
        assert_eq!(entry.key.as_deref(), Some("user:profile:7"));
        let entry = parse_monitor_line(r#"1.0 [0 10.0.0.1:5000] "ping""#, "n").expect("a line");
        assert_eq!(entry.key, None);
    }

    /// An IPv6 client brings brackets of its own, and a quote inside a word
    /// is escaped: neither may move a column.
    #[test]
    fn an_ipv6_client_and_an_escaped_quote_stay_in_their_columns() {
        let entry = parse_monitor_line(r#"1339518083.1 [3 [::1]:60866] "set" "k" "a\"b" "c""#, "n").expect("a line");
        assert_eq!((entry.db.as_ref(), entry.client.as_ref()), ("3", "[::1]:60866"));
        assert_eq!(entry.command.as_ref(), "SET");
        assert_eq!(entry.args.as_ref(), r#"k a\"b c"#);
    }
}

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

//! The Slow Log table: row model, latency correlation chips, script source
//! lookup (`SCRIPT SHOW`), and row filtering.

use super::top_commands::{format_bytes, format_us_as_ms};
use super::*;

/// A single row in the slowlog table, pre-formatted for display.
#[derive(Clone, Debug)]
pub(super) struct SlowLogRow {
    pub(super) timestamp: SharedString,
    /// Unix-seconds timestamp kept alongside the display string so the
    /// correlation chip and time-window filter can compare against
    /// `LatencyEvent::timestamp` without re-parsing the formatted text.
    pub(super) raw_timestamp: i64,
    /// The entry's measure as shown: "12ms" in the slow log, "2.0 kB" in a
    /// COMMANDLOG size log.
    pub(super) amount_text: SharedString,
    /// The measure for filtering and sorting: milliseconds in the slow log,
    /// bytes in a size log.
    pub(super) amount: u64,
    /// The measure for the per-command aggregation: microseconds — SLOWLOG's
    /// native unit, so sub-millisecond entries don't all collapse to zero —
    /// or, again, bytes.
    pub(super) amount_fine: u64,
    /// The Redis command name (args[0]), e.g. "GET", "HSET".
    pub(super) command: SharedString,
    /// The arguments following the command (args[1..]), space-joined.
    pub(super) args: SharedString,
    pub(super) client: SharedString,
    /// Latency event whose timestamp falls within
    /// [`CORRELATION_WINDOW_SECS`] of this slow-log entry, if any.
    /// Populated by `build_all_rows` so the table delegate can render
    /// the chip in O(1) per row without re-scanning the events list.
    /// `Some((event_name, delta_seconds))`.
    pub(super) correlated_event: Option<(SharedString, i64)>,
}

impl SlowLogRow {
    /// Converts a raw [`SlowLogEntry`] from the server into a display-ready row.
    ///
    /// - `timestamp` is formatted as local time (`YYYY-MM-DD HH:MM:SS`).
    /// - the measure is formatted per log: `"12ms"` in the slow log, `"2.0 kB"`
    ///   in a size log.
    /// - `command` / `args` are split by checking whether the first two tokens
    ///   form a known two-word command (e.g. `"CONFIG GET"`, `"SLOWLOG GET"`).
    ///   If so, both tokens become the command; otherwise only the first token is
    ///   used. All tokens are upper-cased for consistent display.
    /// - `client` combines the peer address with the optional connection name.
    pub(super) fn from_entry(entry: &SlowLogEntry, kind: CommandLogKind) -> Self {
        let timestamp = format_unix_secs(entry.timestamp).unwrap_or_default();
        let raw_timestamp = entry.timestamp;

        let (amount, amount_fine, amount_text) = if kind.is_slow() {
            let ms = entry.duration.as_millis() as u64;
            let us = entry.duration.as_micros() as u64;
            // Under a second in milliseconds with a decimal, as the totals
            // are: whole milliseconds put every sub-millisecond entry at 0s.
            let text = if ms < 1000 {
                format_us_as_ms(us)
            } else {
                humantime::format_duration(Duration::from_millis(ms)).to_string()
            };
            (ms, us, text)
        } else {
            (entry.amount, entry.amount, format_bytes(entry.amount))
        };

        // Check whether the first two tokens form a known two-word command
        // (e.g. "CONFIG GET", "SLOWLOG GET") before splitting.
        // The set this was asked of used to be built from `list_commands`
        // at version 0.0.0, which every command in the table postdates, so
        // it was empty and `CONFIG GET` read as `CONFIG` with `GET …`.
        let (command, args) = if entry.args.len() >= 2 {
            if is_subcommand(&entry.args[0], &entry.args[1]) {
                // Two-word command: treat both tokens as the command name.
                let candidate = format!("{} {}", entry.args[0], entry.args[1]).to_uppercase();
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
            amount_text: amount_text.into(),
            amount,
            amount_fine,
            command: command.into(),
            args: args.into(),
            client: client.into(),
            correlated_event: None,
        }
    }

    pub(super) fn cells(&self) -> Vec<SharedString> {
        let (event, delta) = match &self.correlated_event {
            Some((event, delta)) => (event.clone(), SharedString::from(delta.to_string())),
            None => (SharedString::default(), SharedString::default()),
        };
        vec![
            self.timestamp.clone(),
            self.amount_text.clone(),
            self.command.clone(),
            self.args.clone(),
            self.client.clone(),
            SharedString::default(),
            self.amount.to_string().into(),
            event,
            delta,
        ]
    }
}

/// Time window used to associate a slow-log entry with a Latency event,
/// in **seconds**. Five seconds is wide enough to capture a fork that
/// completes just before/after the slow command lands, but tight enough
/// that unrelated background events on a busy server don't all match.
pub(super) const CORRELATION_WINDOW_SECS: i64 = 5;

/// Pick the Latency event whose `timestamp` is closest to `slow_ts`,
/// provided it falls within `±CORRELATION_WINDOW_SECS`. Returns
/// `(event_name, signed_delta_seconds)` so the chip can show direction
/// ("fork +1s" vs "fork -2s"). `None` when no event qualifies.
///
/// Events list is expected to be small (Redis caps LATENCY LATEST at a
/// handful of event types), so a linear scan is fine — no need for a
/// sorted index.
pub(super) fn correlated_event_for_slowlog(slow_ts: i64, events: &[LatencyEvent]) -> Option<(SharedString, i64)> {
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
pub(super) fn correlated_slowlog_count_for_event(event_ts: i64, rows: &[SlowLogRow]) -> usize {
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
const COMMAND_COLUMN: usize = 2;
const ARGS_COLUMN: usize = 3;
const CORRELATED_COLUMN: usize = 5;
/// Payload cells after the six columns: the raw duration the duration
/// column sorts by, and the correlated latency event's name and delta.
const CELL_DURATION_MS: usize = 6;
const CELL_EVENT: usize = 7;
const CELL_EVENT_DELTA: usize = 8;

/// The slow-log grid. Column widths come from the viewport: the "args"
/// column takes all remaining space after the fixed-width columns.
pub(super) fn build_table(
    editor: WeakEntity<ZedisSlowlogEditor>,
    kind: CommandLogKind,
    script_show_supported: Rc<Cell<bool>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
    let content_width = content_area_width(window, cx);
    // Each fixed column is its content plus ~36px of side padding, and what
    // they do not take is the arguments' — the column a slow command is read
    // from, which at the old widths showed 16 characters of it:
    //   timestamp: "2026-07-14 22:31:05" — 19 mono chars
    //   client:    "192.168.1.10:53166" — sized for the address only. The
    //              client *name* is appended after it ("… (zedis:v0.5.5)") and
    //              is allowed to ellipsize: it repeats across every row of the
    //              same client, whereas the address is what tells them apart.
    // Duration and the event column are as wide as their *headers* need
    // (the title, and for a sortable column its arrows).
    let timestamp_width = 180.;
    let duration_width = 112.;
    let command_width = 110.;
    let client_width = 200.;
    // The chip is "<event> +Ns"; a long event name ("active-defrag-cycle")
    // ellipsizes. Fixed instead of stretchy so the chip stays compact next
    // to client info.
    let correlated_width = 136.;
    // `args` is the filling column: it has what the others leave, and no
    // less than this — a narrow window scrolls the table sideways instead
    // of collapsing the column to nothing.
    let args_width = 200.;
    let widths = [
        timestamp_width,
        duration_width,
        command_width,
        args_width,
        client_width,
        correlated_width,
    ];
    let columns = SLOWLOG_COLUMNS
        .iter()
        .zip(widths)
        .map(|(&key, width)| {
            // The measure column is Duration in the slow log, Size in a
            // COMMANDLOG size log.
            let title_key = if key == COLUMN_DURATION && !kind.is_slow() {
                "size"
            } else {
                key
            };
            let column = TextColumn::new(key, i18n_slowlog_editor(cx, title_key), width);
            match key {
                // The duration column shows "12ms" but sorts by the raw value.
                COLUMN_DURATION => column.sort_by_cell(CELL_DURATION_MS),
                COLUMN_TIMESTAMP | COLUMN_COMMAND | COLUMN_CLIENT => column.sortable(),
                COLUMN_ARGS => column.fills(),
                _ => column,
            }
        })
        .collect();
    let chip_editor = editor.clone();
    let chip: CellRenderer = Rc::new(move |row_ix, col_ix, cells, _window, cx| {
        (col_ix == CORRELATED_COLUMN).then(|| render_correlation_chip(row_ix, cells, chip_editor.clone(), cx))
    });
    // An `EVALSHA` names a script by its digest and nothing else; where the
    // server can show the source (Valkey's `SCRIPT SHOW`), the arguments
    // cell grows a button that does.
    let show_script_tooltip = i18n_slowlog_editor(cx, "show_script_tooltip");
    let show_script: CellActionProvider = Rc::new(move |col_ix, cells| {
        if col_ix != ARGS_COLUMN || !script_show_supported.get() {
            return Vec::new();
        }
        let Some(sha) = evalsha_digest(cells) else {
            return Vec::new();
        };
        let editor = editor.clone();
        vec![CellAction {
            icon: IconName::Search,
            tooltip: show_script_tooltip.clone(),
            on_click: Rc::new(move |window, cx| open_script_source(editor.clone(), sha.clone(), window, cx)),
        }]
    });
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .cell_render(chip)
        .cell_action(show_script)
        .fitted_to(content_width)
}

/// The digest an `EVALSHA` / `EVALSHA_RO` row names — its first argument,
/// forty hex characters — or `None` for any other row.
fn evalsha_digest(cells: &[SharedString]) -> Option<String> {
    let command = cells.get(COMMAND_COLUMN)?;
    if !command.eq_ignore_ascii_case("EVALSHA") && !command.eq_ignore_ascii_case("EVALSHA_RO") {
        return None;
    }
    let sha = cells.get(ARGS_COLUMN)?.split_whitespace().next()?;
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_ascii_lowercase())
}

/// What the script dialog knows: the digest, and the source once fetched.
struct ScriptSourceView {
    sha: String,
    /// `None` while fetching; then the source, `Ok(None)` for a digest the
    /// cache no longer holds, or why the fetch failed.
    source: Option<Result<Option<String>, String>>,
}

impl Render for ScriptSourceView {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let body: AnyElement = match &self.source {
            None => h_flex()
                .gap_2()
                .items_center()
                .child(Spinner::new().small())
                .child(Label::new(i18n_slowlog_editor(cx, "script_loading")).text_color(muted))
                .into_any_element(),
            Some(Ok(Some(source))) => div()
                .font_family(get_mono_font_family())
                .text_sm()
                .child(SharedString::from(source.clone()))
                .into_any_element(),
            Some(Ok(None)) => Label::new(i18n_slowlog_editor(cx, "script_missing"))
                .text_color(muted)
                .into_any_element(),
            Some(Err(error)) => Label::new(i18n_slowlog_editor(cx, "script_failed").replace("%{error}", error))
                .text_color(cx.theme().danger)
                .into_any_element(),
        };
        v_flex()
            .gap_2()
            .child(
                Label::new(SharedString::from(self.sha.clone()))
                    .text_xs()
                    .text_color(muted)
                    .font_family(get_mono_font_family()),
            )
            .child(body)
    }
}

/// Open the dialog for `sha` and fetch its source into it. The body is a
/// view so the fetch can land in it after the dialog is up.
fn open_script_source(editor: WeakEntity<ZedisSlowlogEditor>, sha: String, window: &mut Window, cx: &mut gpui::App) {
    let Some(editor) = editor.upgrade() else {
        return;
    };
    let at = editor.read(cx).server_state.read(cx).at();
    let view = cx.new(|_| ScriptSourceView {
        sha: sha.clone(),
        source: None,
    });
    let body = view.clone();
    ZedisDialog::new(i18n_slowlog_editor(cx, "script_source_title").replace("%{sha}", &sha[..8]))
        .child(move || body.clone())
        .ok_text(i18n_common(cx, "close"))
        .on_ok(|_, _, _| true)
        .w(px(640.))
        .open(window, cx);
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_spawn(async move { script_show(&at, &sha).await })
            .await
            .map_err(|e| e.to_string());
        view.update(cx, |view, cx| {
            view.source = Some(fetched);
            cx.notify();
        });
    })
    .detach();
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

impl ZedisSlowlogEditor {
    /// Rebuild `all_rows` from the current server state, re-decorating
    /// each row's `correlated_event` against the latest `latency_events`.
    /// Then re-apply filters and push the result into the table state.
    /// Single source of truth for "the inputs that drive the chip just
    /// changed".
    pub(super) fn rebuild_rows_with_correlations(&mut self, cx: &mut gpui::Context<Self>) {
        let new_rows = self.rows_from_current_log(cx);
        self.replace_rows(new_rows, cx);
    }

    /// The rows of the log the panel shows: the slow log from the server
    /// state (with the Latency correlation), or the fetched size log.
    pub(super) fn rows_from_current_log(&self, cx: &gpui::App) -> Vec<SlowLogRow> {
        if self.log_kind.is_slow() {
            Self::build_all_rows(
                self.server_state.read(cx).slow_logs(),
                self.log_kind,
                &self.latency_events,
            )
        } else {
            Self::build_all_rows(&self.commandlog_entries, self.log_kind, &[])
        }
    }

    /// Swap in a fresh row set; the command list, the selection and the
    /// filtered table follow.
    pub(super) fn replace_rows(&mut self, rows: Vec<SlowLogRow>, cx: &mut gpui::Context<Self>) {
        self.all_rows = rows;
        self.available_commands = Self::extract_commands(&self.all_rows);
        self.selected_commands.retain(|c| self.available_commands.contains(c));
        self.apply_filters(cx);
    }

    /// Switch to the SlowLog tab and narrow the table to commands that
    /// fired within `±CORRELATION_WINDOW_SECS` of `event_ts`. Records
    /// `window_filter_label` so the user sees *why* the table is
    /// suddenly short, with a one-click "Clear" pill to drop it.
    pub(super) fn jump_to_slowlog_window(
        &mut self,
        event_name: SharedString,
        event_ts: i64,
        cx: &mut gpui::Context<Self>,
    ) {
        if event_ts <= 0 {
            return;
        }
        self.set_tab(PerformanceTab::SlowLog, cx);
        let lo = event_ts - CORRELATION_WINDOW_SECS;
        let hi = event_ts + CORRELATION_WINDOW_SECS;
        self.window_filter = Some((lo, hi));
        let when = format_unix_secs(event_ts).unwrap_or_else(|| event_ts.to_string());
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

    /// Clear the time-window filter set by `jump_to_slowlog_window` so
    /// the full slow-log table is visible again. Called by the "Clear"
    /// pill above the table.
    pub(super) fn clear_window_filter(&mut self, cx: &mut gpui::Context<Self>) {
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
    pub(super) fn render_window_pill(&self, label: SharedString, cx: &mut gpui::Context<Self>) -> impl IntoElement {
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

    /// Converts a log's entries into display rows, decorating each row with
    /// the closest Latency event in `±CORRELATION_WINDOW_SECS` (if any) so
    /// the chip column has data to render — the slow log's caller passes
    /// the events, a size log's passes none.
    pub(super) fn build_all_rows(
        entries: &[SlowLogEntry],
        kind: CommandLogKind,
        latency_events: &[LatencyEvent],
    ) -> Vec<SlowLogRow> {
        entries
            .iter()
            .map(|entry| {
                let mut row = SlowLogRow::from_entry(entry, kind);
                row.correlated_event = correlated_event_for_slowlog(row.raw_timestamp, latency_events);
                row
            })
            .collect()
    }

    /// Extracts unique command names from rows, sorted alphabetically.
    pub(super) fn extract_commands(rows: &[SlowLogRow]) -> Vec<SharedString> {
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
    pub(super) fn filter_rows(&self) -> Vec<SlowLogRow> {
        self.all_rows
            .iter()
            .filter(|row| {
                if !self.selected_commands.is_empty() && !self.selected_commands.contains(&row.command) {
                    return false;
                }
                if self.min_amount > 0 && row.amount < self.min_amount {
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
    pub(super) fn apply_filters(&mut self, cx: &mut gpui::Context<Self>) {
        let filtered = self.filter_rows();
        self.row_count = filtered.len();
        self.table_state.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_rows(filtered.iter().map(SlowLogRow::cells).collect());
        });
        cx.notify();
    }

    /// Toggles a command in the selected set.
    pub(super) fn toggle_command(&mut self, command: SharedString, cx: &mut gpui::Context<Self>) {
        if self.selected_commands.contains(&command) {
            self.selected_commands.remove(&command);
        } else {
            self.selected_commands.insert(command);
        }
        self.apply_filters(cx);
    }
}

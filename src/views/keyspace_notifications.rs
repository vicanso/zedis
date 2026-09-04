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

//! Keyspace Notifications subscriber.
//!
//! Subscribes to both `__keyspace@*__:*` and `__keyevent@*__:*` on the
//! connected server so the operator can answer "which client just
//! touched key X" without leaving the GUI. Same background-task pattern
//! as the generic Pub/Sub editor, with:
//!
//! 1. Channel-name parsing into `(db, key, event, source)`.
//! 2. Post-subscription filters (event chips, key substring, DB, source).
//! 3. Config banner + Enable presets for `notify-keyspace-events`.
//! 4. Pause (drop inbound), export filtered rows as CSV, rate hint.

use crate::connection::{Capability, get_connection_manager, get_server};
use crate::error::Error;
use crate::helpers::{build_csv, get_mono_font_family, now_clock};
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
    dialog_button_props, i18n_common, i18n_keyspace_notifications,
};
use crate::views::unavailable_chip;
use crate::views::{export_to_file, open_key_in_editor};
use ahash::AHashSet;
use futures::StreamExt;
use gpui::{App, Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    notification::Notification,
    table::{DataTable, TableState},
    tooltip::Tooltip,
    v_flex,
};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;
use tracing::error;
use zedis_ui::{
    CellRenderer, CellStyle, CellStyleProvider, RowPredicate, TextColumn, ZedisDialog, ZedisTextTable, help_popover,
};

const KEYSPACE_PATTERN: &str = "__keyspace@*__:*";
const KEYEVENT_PATTERN: &str = "__keyevent@*__:*";
/// Default Enable preset: K=keyspace, E=keyevent, A=all event classes.
const ENABLE_FLAGS_AKE: &str = "AKE"; // spellchecker:disable-line
/// Alias form also accepted by Redis.
const ENABLE_FLAGS_KEA: &str = "KEA";
/// String commands only (keyspace + keyevent + string class).
const ENABLE_FLAGS_STRING: &str = "KEg$";
const RING_BUFFER_CAPACITY: usize = 1000;
const BATCH_LIMIT: usize = 200;
/// Sliding window for events/sec badge.
const RATE_WINDOW_SECS: f64 = 5.0;

#[derive(Clone, Debug)]
struct NotificationRow {
    timestamp: SharedString,
    db: u32,
    key: SharedString,
    event: SharedString,
    source: NotificationSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationSource {
    Keyspace,
    Keyevent,
}

impl NotificationSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Keyspace => "keyspace",
            Self::Keyevent => "keyevent",
        }
    }
}

/// Which channel families to keep after parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum SourceFilter {
    #[default]
    Both,
    KeyeventOnly,
    KeyspaceOnly,
}

/// Built-in chips (order ≈ frequency). Dynamic events from the live
/// stream are appended after these.
const DEFAULT_EVENT_CHIPS: &[&str] = &[
    "set",
    "del",
    "expire",
    "expired",
    "evicted",
    "rename_from",
    "rename_to",
    "hset",
    "hdel",
    "sadd",
    "srem",
    "lpush",
    "rpush",
    "zadd",
    "zrem",
    "xadd",
];

const COL_TIME: &str = "col_time";
const COL_DB: &str = "col_db";
const COL_KEY: &str = "col_key";
const COL_EVENT: &str = "col_event";
const COL_SOURCE: &str = "col_source";

const KEYSPACE_COLUMNS: [&str; 5] = [COL_TIME, COL_DB, COL_KEY, COL_EVENT, COL_SOURCE];
const TIME_COLUMN: usize = 0;
const DB_COLUMN: usize = 1;
const KEY_COLUMN: usize = 2;
const EVENT_COLUMN: usize = 3;
const SOURCE_COLUMN: usize = 4;

impl NotificationRow {
    fn cells(&self) -> Vec<SharedString> {
        vec![
            self.timestamp.clone(),
            self.db.to_string().into(),
            self.key.clone(),
            self.event.clone(),
            self.source.as_str().into(),
        ]
    }
}

/// The event grid: newest first, capped at [`RING_BUFFER_CAPACITY`]. The
/// time and source columns are muted, the event is coloured by what it
/// did to the key, and the key cell opens the key in the editor.
fn build_table(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &gpui::App) -> ZedisTextTable {
    let content_width = content_area_width(window, cx);

    let time_width = 160.;
    let db_width = 60.;
    let event_width = 120.;
    let source_width = 90.;
    let remaining = content_width.as_f32() - time_width - db_width - event_width - source_width - 10.;
    let key_width = remaining.max(200.);
    let widths = [time_width, db_width, key_width, event_width, source_width];

    let columns = KEYSPACE_COLUMNS
        .iter()
        .zip(widths)
        .map(|(&key, width)| TextColumn::new(key, i18n_keyspace_notifications(cx, key), width))
        .collect();

    let style: CellStyleProvider = Rc::new(|col_ix, cells, cx| {
        let theme = cx.theme();
        let color = match col_ix {
            TIME_COLUMN | SOURCE_COLUMN => theme.muted_foreground,
            EVENT_COLUMN => match cells.get(EVENT_COLUMN).map(|e| e.as_ref()) {
                Some("del" | "expired" | "evicted") => theme.red,
                Some("set" | "hset" | "sadd" | "zadd" | "lpush" | "rpush" | "xadd") => theme.green,
                Some("expire") => theme.yellow,
                _ => theme.foreground,
            },
            _ => theme.foreground,
        };
        CellStyle {
            color: Some(color),
            ..Default::default()
        }
    });

    // The key cell is itself the link: click anywhere on it to open the key.
    let open_key: CellRenderer = Rc::new(move |row_ix, col_ix, cells, _window, cx| {
        if col_ix != KEY_COLUMN {
            return None;
        }
        let key = cells.get(KEY_COLUMN)?.clone();
        let server_state = server_state.clone();
        let tooltip = i18n_common(cx, "open_key_tooltip");
        let foreground = cx.theme().foreground;
        Some(
            div()
                .size_full()
                .paddings(Edges {
                    top: px(2.),
                    bottom: px(2.),
                    left: px(10.),
                    right: px(10.),
                })
                .child(
                    div()
                        .id(("ks-open-key", row_ix))
                        .cursor_pointer()
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(move |_, _, cx: &mut App| {
                            open_key_in_editor(&server_state, key.clone(), cx);
                        })
                        .child(
                            Label::new(cells[KEY_COLUMN].clone())
                                .text_color(foreground)
                                .text_sm()
                                .text_ellipsis(),
                        ),
                )
                .into_any_element(),
        )
    });

    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .max_rows(RING_BUFFER_CAPACITY)
        .cell_style(style)
        .cell_render(open_key)
}

/// The structured filter as a row predicate over the cells — `None` when
/// nothing is narrowed, so the table shows every row.
fn row_predicate(
    selected_events: &Option<AHashSet<String>>,
    key_filter: &str,
    selected_dbs: &Option<AHashSet<u32>>,
    source_filter: SourceFilter,
) -> Option<RowPredicate> {
    if selected_events.is_none()
        && key_filter.is_empty()
        && selected_dbs.is_none()
        && source_filter == SourceFilter::Both
    {
        return None;
    }
    let events = selected_events.clone();
    let key_filter = key_filter.to_string();
    let dbs = selected_dbs.clone();
    Some(Rc::new(move |cells: &[SharedString]| {
        let cell = |ix: usize| cells.get(ix).map(|c| c.as_ref()).unwrap_or("");
        if let Some(set) = &events
            && !set.contains(cell(EVENT_COLUMN))
        {
            return false;
        }
        if !key_filter.is_empty() && !cell(KEY_COLUMN).contains(key_filter.as_str()) {
            return false;
        }
        if let Some(set) = &dbs
            && !cell(DB_COLUMN).parse::<u32>().is_ok_and(|db| set.contains(&db))
        {
            return false;
        }
        match source_filter {
            SourceFilter::Both => true,
            SourceFilter::KeyeventOnly => cell(SOURCE_COLUMN) == NotificationSource::Keyevent.as_str(),
            SourceFilter::KeyspaceOnly => cell(SOURCE_COLUMN) == NotificationSource::Keyspace.as_str(),
        }
    }))
}

pub struct ZedisKeyspaceNotifications {
    server_state: Entity<ZedisServerState>,
    title: SharedString,
    table_state: Entity<TableState<ZedisTextTable>>,
    row_count: usize,
    total_count: usize,
    subscribe_task: Option<Task<()>>,
    subscribing: bool,
    /// Drop inbound messages while true (subscription stays up).
    paused: bool,
    notify_flags: SharedString,
    notify_flags_fetched: bool,
    flags_error: Option<SharedString>,
    subscribe_error: Option<SharedString>,
    selected_events: Option<AHashSet<String>>,
    selected_dbs: Option<AHashSet<u32>>,
    source_filter: SourceFilter,
    key_filter: String,
    key_filter_input: Entity<InputState>,
    /// Timestamps of recent ingests for events/sec.
    rate_ticks: VecDeque<Instant>,
    pending_notification: Option<Notification>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyspaceNotifications {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id();
        let name = get_server(server_id)
            .map(|s| s.name)
            .unwrap_or_else(|_| "--".to_string());
        let title: SharedString = name.into();

        let key_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_keyspace_notifications(cx, "key_pattern_placeholder"))
        });

        let table_state = cx.new(|cx| TableState::new(build_table(server_state.clone(), window, cx), window, cx));

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe_in(&key_filter_input, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    this.key_filter = state.read(cx).value().to_string();
                    this.apply_filter_now(cx);
                }
            }),
        );
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| {
            if let ServerEvent::ServerSelected(_) = event {
                this.stop_subscribe(cx);
                this.clear_rows(cx);
                this.notify_flags = SharedString::default();
                this.notify_flags_fetched = false;
                this.flags_error = None;
                this.subscribe_error = None;
                this.selected_events = None;
                this.selected_dbs = None;
                this.source_filter = SourceFilter::Both;
                this.paused = false;
                this.rate_ticks.clear();
                let name = get_server(this.server_state.read(cx).server_id())
                    .map(|s| s.name)
                    .unwrap_or_else(|_| "--".to_string());
                this.title = name.into();
                this.refresh_notify_flags(cx);
                cx.notify();
            }
        }));

        Self {
            server_state,
            title,
            table_state,
            row_count: 0,
            total_count: 0,
            subscribe_task: None,
            subscribing: false,
            paused: false,
            notify_flags: SharedString::default(),
            notify_flags_fetched: false,
            flags_error: None,
            subscribe_error: None,
            selected_events: None,
            selected_dbs: None,
            source_filter: SourceFilter::Both,
            key_filter: String::new(),
            key_filter_input,
            rate_ticks: VecDeque::new(),
            pending_notification: None,
            _subscriptions: subscriptions,
        }
    }

    fn is_subscribed(&self) -> bool {
        self.subscribe_task.is_some()
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

    fn start_subscribe(&mut self, cx: &mut Context<Self>) {
        if self.is_subscribed() || self.subscribing {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        self.subscribing = true;
        self.subscribe_error = None;
        cx.notify();

        let entity = cx.entity().downgrade();
        let server_id_for_task = server_id.clone();

        self.refresh_notify_flags(cx);

        self.subscribe_task = Some(cx.spawn(async move |_handle, cx| {
            let connect: Result<_, Error> = cx
                .background_spawn(async move {
                    let mut pubsub = get_connection_manager()
                        .get_pubsub_connection(&server_id_for_task)
                        .await?;
                    let patterns: Vec<&str> = vec![KEYSPACE_PATTERN, KEYEVENT_PATTERN];
                    pubsub
                        .psubscribe(patterns)
                        .await
                        .map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Ok(pubsub)
                })
                .await;

            let mut pubsub = match connect {
                Ok(pubsub) => pubsub,
                Err(e) => {
                    error!(error = %e, "Keyspace notifications subscribe failed");
                    let msg = e.to_string();
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        this.subscribe_task = None;
                        this.subscribe_error = Some(msg.clone().into());
                        this.pending_notification = Some(Notification::error(format!(
                            "{}: {msg}",
                            i18n_keyspace_notifications(cx, "subscribe_failed")
                        )));
                        cx.notify();
                    });
                    return;
                }
            };

            if entity
                .update(cx, |this, cx| {
                    this.subscribing = false;
                    this.subscribe_error = None;
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let (tx, rx) = smol::channel::unbounded::<NotificationRow>();
            let reader = cx.background_spawn(async move {
                let mut stream = pubsub.on_message();
                while let Some(msg) = stream.next().await {
                    let channel: String = msg.get_channel_name().to_string();
                    let payload = msg.get_payload_bytes();
                    if let Some(row) = parse_notification(&channel, payload)
                        && tx.send(row).await.is_err()
                    {
                        break;
                    }
                }
            });

            while let Ok(first) = rx.recv().await {
                let mut batch = Vec::with_capacity(BATCH_LIMIT);
                batch.push(first);
                while batch.len() < BATCH_LIMIT {
                    match rx.try_recv() {
                        Ok(row) => batch.push(row),
                        Err(_) => break,
                    }
                }
                if entity.update(cx, |this, cx| this.ingest_batch(batch, cx)).is_err() {
                    break;
                }
            }

            drop(reader);
        }));
    }

    fn stop_subscribe(&mut self, cx: &mut Context<Self>) {
        if self.subscribe_task.take().is_some() {
            self.subscribing = false;
            self.paused = false;
            cx.notify();
        }
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if !self.is_subscribed() {
            return;
        }
        self.paused = !self.paused;
        cx.notify();
    }

    fn ingest_batch(&mut self, batch: Vec<NotificationRow>, cx: &mut Context<Self>) {
        if self.paused {
            return;
        }
        let selected = self.selected_events.clone();
        let key_filter = self.key_filter.clone();
        let selected_dbs = self.selected_dbs.clone();
        let source_filter = self.source_filter;
        let n = batch.len();
        self.table_state.update(cx, |state, _| {
            let delegate = state.delegate_mut();
            for row in batch {
                delegate.push_front(row.cells());
            }
            delegate.set_row_filter(row_predicate(&selected, &key_filter, &selected_dbs, source_filter));
        });
        let now = Instant::now();
        for _ in 0..n {
            self.rate_ticks.push_back(now);
        }
        // Trim old ticks.
        let window = std::time::Duration::from_secs_f64(RATE_WINDOW_SECS * 2.0);
        while self.rate_ticks.front().is_some_and(|t| now.duration_since(*t) > window) {
            self.rate_ticks.pop_front();
        }
        self.refresh_counts(cx);
        cx.notify();
    }

    fn clear_rows(&mut self, cx: &mut Context<Self>) {
        if self.total_count == 0 {
            return;
        }
        self.table_state.update(cx, |state, _| state.delegate_mut().clear());
        self.row_count = 0;
        self.total_count = 0;
        self.rate_ticks.clear();
        cx.notify();
    }

    fn refresh_counts(&mut self, cx: &App) {
        let d = self.table_state.read(cx).delegate();
        self.row_count = d.visible_len();
        self.total_count = d.total_len();
    }

    fn apply_filter_now(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_events.clone();
        let key_filter = self.key_filter.clone();
        let selected_dbs = self.selected_dbs.clone();
        let source_filter = self.source_filter;
        self.table_state.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_row_filter(row_predicate(&selected, &key_filter, &selected_dbs, source_filter));
        });
        self.refresh_counts(cx);
        cx.notify();
    }

    fn refresh_notify_flags(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.notify_flags_fetched = true;
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let pair: Vec<String> = redis::cmd("CONFIG")
                    .arg("GET")
                    .arg("notify-keyspace-events")
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                Ok::<_, Error>(pair.get(1).cloned().unwrap_or_default())
            });
            let result = task.await;
            let _ = entity.update(cx, |this, cx| {
                match result {
                    Ok(flags) => {
                        this.notify_flags = flags.into();
                        this.flags_error = None;
                    }
                    Err(e) => this.flags_error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn enable_notifications(&mut self, flags: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.server_state.read(cx).can(Capability::ConfigWrite) {
            self.pending_notification = Some(Notification::error(i18n_keyspace_notifications(cx, "enable_readonly")));
            cx.notify();
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        let flags = flags.to_string();
        if high_risk {
            self.open_enable_confirm(&flags, window, cx);
        } else {
            self.run_enable(&flags, cx);
        }
    }

    fn open_enable_confirm(&mut self, flags: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = i18n_keyspace_notifications(cx, "enable_confirm_title");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let body: SharedString = rust_i18n::t!(
            "keyspace_notifications.enable_confirm_body",
            flags = flags,
            locale = locale
        )
        .to_string()
        .into();
        let editor = cx.entity().downgrade();
        let flags = flags.to_string();
        ZedisDialog::new_alert(title, body.to_string())
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |this, cx| this.run_enable(&flags, cx));
                }
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn run_enable(&mut self, flags: &str, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        let entity = cx.entity().downgrade();
        let flags = flags.to_string();
        cx.spawn(async move |_handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("notify-keyspace-events")
                    .arg(&flags)
                    .query_async::<()>(&mut conn)
                    .await
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                Ok::<_, Error>(())
            });
            let result = task.await;
            let _ = entity.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.pending_notification =
                        Some(Notification::success(i18n_keyspace_notifications(cx, "enable_ok")));
                    this.refresh_notify_flags(cx);
                }
                Err(e) => {
                    this.pending_notification = Some(Notification::error(e.to_string()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_event_chip(&mut self, event: &str, cx: &mut Context<Self>) {
        let set = self.selected_events.get_or_insert_with(AHashSet::new);
        if !set.insert(event.to_string()) {
            set.remove(event);
        }
        // Empty explicit set → treat as "show nothing" is confusing; if empty
        // after un-click last chip, reset to All.
        if set.is_empty() {
            self.selected_events = None;
        }
        self.apply_filter_now(cx);
    }

    fn reset_event_filter(&mut self, cx: &mut Context<Self>) {
        if self.selected_events.take().is_some() {
            self.apply_filter_now(cx);
        }
    }

    fn toggle_db_chip(&mut self, db: u32, cx: &mut Context<Self>) {
        let set = self.selected_dbs.get_or_insert_with(AHashSet::new);
        if !set.insert(db) {
            set.remove(&db);
        }
        if set.is_empty() {
            self.selected_dbs = None;
        }
        self.apply_filter_now(cx);
    }

    fn reset_db_filter(&mut self, cx: &mut Context<Self>) {
        if self.selected_dbs.take().is_some() {
            self.apply_filter_now(cx);
        }
    }

    fn set_source_filter(&mut self, filter: SourceFilter, cx: &mut Context<Self>) {
        if self.source_filter != filter {
            self.source_filter = filter;
            self.apply_filter_now(cx);
        }
    }

    /// Default chips first, then any extra event names seen in the buffer.
    fn event_chip_list(&self, cx: &App) -> Vec<String> {
        let mut seen: AHashSet<String> = DEFAULT_EVENT_CHIPS.iter().map(|s| (*s).to_string()).collect();
        let mut out: Vec<String> = DEFAULT_EVENT_CHIPS.iter().map(|s| (*s).to_string()).collect();
        let extra: AHashSet<String> = self
            .table_state
            .read(cx)
            .delegate()
            .all_rows()
            .iter()
            .filter_map(|cells| cells.get(EVENT_COLUMN))
            .map(|e| e.to_string())
            .filter(|e| !seen.contains(e.as_str()))
            .collect();
        let mut extra_sorted: Vec<String> = extra.into_iter().collect();
        extra_sorted.sort();
        for e in extra_sorted {
            if seen.insert(e.clone()) {
                out.push(e);
            }
        }
        out
    }

    fn db_chip_list(&self, cx: &App) -> Vec<u32> {
        let mut dbs: Vec<u32> = self
            .table_state
            .read(cx)
            .delegate()
            .all_rows()
            .iter()
            .filter_map(|cells| cells.get(DB_COLUMN)?.parse::<u32>().ok())
            .collect::<AHashSet<_>>()
            .into_iter()
            .collect();
        dbs.sort_unstable();
        dbs
    }

    /// Export the visible rows (filtered when a filter is active) as a
    /// CSV file — same `build_csv` + `export_to_file` flow as the
    /// slow-log panel, instead of the old clipboard-only copy.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let rows = self.table_state.read(cx).delegate().visible_rows();
        if rows.is_empty() {
            return;
        }
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|cells| cells.iter().map(|c| c.to_string()).collect())
            .collect();
        let csv = build_csv(&["time", "db", "key", "event", "source"], &data);
        let server_state = self.server_state.clone();
        let success = i18n_common(cx, "csv_exported");
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(
            cx,
            server_state,
            csv.into_bytes(),
            "keyspace-events.csv",
            success,
            error,
        );
    }

    // ── Render helpers ────────────────────────────────────────────────

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let subscribed = self.is_subscribed();
        let subscribing = self.subscribing;
        let paused = self.paused;
        let visible = self.row_count;
        let total = self.total_count;

        let action_btn = if subscribed {
            Button::new("ksn-stop")
                .outline()
                .small()
                .label(i18n_keyspace_notifications(cx, "stop"))
                .icon(IconName::CircleX)
                .on_click(cx.listener(|this, _, _w, cx| this.stop_subscribe(cx)))
        } else {
            Button::new("ksn-subscribe")
                .primary()
                .small()
                .loading(subscribing)
                .disabled(subscribing)
                .label(i18n_keyspace_notifications(cx, "subscribe"))
                .icon(IconName::Play)
                .on_click(cx.listener(|this, _, _w, cx| this.start_subscribe(cx)))
        };

        let count_label: SharedString = if total == 0 {
            SharedString::default()
        } else if visible == total {
            format!("({total}/{RING_BUFFER_CAPACITY})").into()
        } else {
            format!("({visible}/{total} · cap {RING_BUFFER_CAPACITY})").into()
        };

        let rate = self.events_per_sec();
        let rate_label: Option<SharedString> = if subscribed && rate > 0.05 {
            Some(format!("{rate:.1}/s").into())
        } else {
            None
        };

        h_flex()
            .w_full()
            .h(px(40.))
            .px_4()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("ksn-back")
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
                    .child(Label::new(i18n_keyspace_notifications(cx, "title")).font_semibold())
                    .child(help_popover(
                        "keyspace-notifications-help",
                        i18n_keyspace_notifications(cx, "help"),
                    ))
                    .child(Label::new(self.title.clone()).text_color(theme.muted_foreground))
                    .child(Label::new(count_label).text_xs().text_color(theme.muted_foreground))
                    .when_some(rate_label, |this, r| {
                        this.child(Label::new(r).text_xs().text_color(if rate > 50.0 {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        }))
                    })
                    .when(paused, |this| {
                        this.child(
                            Label::new(i18n_keyspace_notifications(cx, "paused_badge"))
                                .text_xs()
                                .text_color(theme.yellow),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(subscribed, |this| {
                        this.child(
                            Button::new("ksn-pause")
                                .outline()
                                .small()
                                .label(if paused {
                                    i18n_keyspace_notifications(cx, "resume")
                                } else {
                                    i18n_keyspace_notifications(cx, "pause")
                                })
                                .tooltip(i18n_keyspace_notifications(cx, "pause_tooltip"))
                                .on_click(cx.listener(|this, _, _w, cx| this.toggle_pause(cx))),
                        )
                    })
                    .child(
                        Button::new("ksn-export")
                            .ghost()
                            .small()
                            .label(i18n_keyspace_notifications(cx, "export"))
                            .tooltip(i18n_keyspace_notifications(cx, "export_tooltip"))
                            .disabled(total == 0)
                            .on_click(cx.listener(|this, _, _w, cx| this.export_csv(cx))),
                    )
                    .child(action_btn)
                    .child(
                        Button::new("ksn-clear")
                            .ghost()
                            .small()
                            .label(i18n_keyspace_notifications(cx, "clear"))
                            .disabled(total == 0)
                            .on_click(cx.listener(|this, _, _w, cx| this.clear_rows(cx))),
                    ),
            )
    }

    fn render_flags_status(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let theme = cx.theme();
        if let Some(err) = &self.flags_error {
            return Some(
                div()
                    .mx_4()
                    .my_1()
                    .px_3()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(theme.danger.opacity(0.1))
                    .child(Label::new(err.clone()).text_xs().text_color(theme.danger))
                    .into_any_element(),
            );
        }
        if self.notify_flags.is_empty() {
            return None;
        }
        let flags = self.notify_flags.clone();
        Some(
            h_flex()
                .mx_4()
                .my_1()
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(
                    Label::new(i18n_keyspace_notifications(cx, "flags_label"))
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    Label::new(flags)
                        .text_xs()
                        .font_family(get_mono_font_family())
                        .text_color(theme.green),
                )
                .child(div().flex_1())
                .child(
                    Button::new("ksn-flags-refresh")
                        .ghost()
                        .xsmall()
                        .label(i18n_keyspace_notifications(cx, "flags_refresh"))
                        .on_click(cx.listener(|this, _, _w, cx| {
                            this.notify_flags_fetched = false;
                            this.refresh_notify_flags(cx);
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_config_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let theme = cx.theme();
        if self.flags_error.is_some() {
            return None;
        }
        if !self.notify_flags.is_empty() {
            return None;
        }
        let can_write = self.server_state.read(cx).can(Capability::ConfigWrite);
        let config_block = self.server_state.read(cx).blocked_by(Capability::ConfigWrite);
        Some(
            div()
                .mx_4()
                .my_2()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.warning)
                .bg(theme.warning.opacity(0.1))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(Icon::new(IconName::Info).text_color(theme.warning))
                        .child(
                            Label::new(i18n_keyspace_notifications(cx, "banner_disabled"))
                                .text_sm()
                                .text_color(theme.warning)
                                .flex_1(),
                        )
                        .when(can_write, |this| {
                            this.child(
                                Button::new("ksn-enable-ake")
                                    .primary()
                                    .small()
                                    .label(i18n_keyspace_notifications(cx, "enable_ake"))
                                    .tooltip(i18n_keyspace_notifications(cx, "enable_ake_tooltip"))
                                    .on_click(
                                        cx.listener(|this, _, w, cx| {
                                            this.enable_notifications(ENABLE_FLAGS_AKE, w, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("ksn-enable-kea")
                                    .outline()
                                    .small()
                                    .label(i18n_keyspace_notifications(cx, "enable_kea"))
                                    .tooltip(i18n_keyspace_notifications(cx, "enable_kea_tooltip"))
                                    .on_click(
                                        cx.listener(|this, _, w, cx| {
                                            this.enable_notifications(ENABLE_FLAGS_KEA, w, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("ksn-enable-str")
                                    .outline()
                                    .small()
                                    .label(i18n_keyspace_notifications(cx, "enable_string"))
                                    .tooltip(i18n_keyspace_notifications(cx, "enable_string_tooltip"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.enable_notifications(ENABLE_FLAGS_STRING, w, cx)
                                    })),
                            )
                        })
                        .when(!can_write, |this| match config_block {
                            // The server, not the access mode, forbids it.
                            Some((command, status)) => this.child(unavailable_chip(cx, command, status)),
                            None => this.child(
                                Label::new(i18n_keyspace_notifications(cx, "enable_readonly"))
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            ),
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_subscribe_error(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let err = self.subscribe_error.as_ref()?;
        let theme = cx.theme();
        Some(
            div()
                .mx_4()
                .my_1()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.danger.opacity(0.12))
                .child(Label::new(err.clone()).text_xs().text_color(theme.danger))
                .into_any_element(),
        )
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let event_chips = self.event_chip_list(cx);
        let db_chips = self.db_chip_list(cx);

        let chips: Vec<gpui::AnyElement> = event_chips
            .iter()
            .enumerate()
            .map(|(i, event)| {
                let event_owned = event.clone();
                // Only highlight chips when an *explicit* filter is active.
                let active = match &self.selected_events {
                    None => false,
                    Some(set) => set.contains(event.as_str()),
                };
                let mut btn = Button::new(("ksn-chip", i as u32)).xsmall().label(event.as_str());
                btn = if active { btn.primary() } else { btn.outline() };
                btn.on_click(cx.listener(move |this, _, _w, cx| {
                    this.toggle_event_chip(&event_owned, cx);
                }))
                .into_any_element()
            })
            .collect();

        let db_btns: Vec<gpui::AnyElement> = db_chips
            .iter()
            .enumerate()
            .map(|(i, db)| {
                let db = *db;
                let active = match &self.selected_dbs {
                    None => false,
                    Some(set) => set.contains(&db),
                };
                let mut btn = Button::new(("ksn-db", i as u32)).xsmall().label(db.to_string());
                btn = if active { btn.primary() } else { btn.outline() };
                btn.on_click(cx.listener(move |this, _, _w, cx| this.toggle_db_chip(db, cx)))
                    .into_any_element()
            })
            .collect();

        let source_filter = self.source_filter;

        v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        Label::new(i18n_keyspace_notifications(cx, "filter_event_types"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child({
                        let active = self.selected_events.is_none();
                        let mut btn = Button::new("ksn-chip-all")
                            .xsmall()
                            .label(i18n_keyspace_notifications(cx, "filter_event_all"));
                        btn = if active { btn.primary() } else { btn.outline() };
                        btn.on_click(cx.listener(|this, _, _w, cx| this.reset_event_filter(cx)))
                    })
                    .children(chips),
            )
            .when(!db_chips.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(
                            Label::new(i18n_keyspace_notifications(cx, "filter_db"))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        )
                        .child({
                            let active = self.selected_dbs.is_none();
                            let mut btn = Button::new("ksn-db-all")
                                .xsmall()
                                .label(i18n_keyspace_notifications(cx, "filter_event_all"));
                            btn = if active { btn.primary() } else { btn.outline() };
                            btn.on_click(cx.listener(|this, _, _w, cx| this.reset_db_filter(cx)))
                        })
                        .children(db_btns),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        Label::new(i18n_keyspace_notifications(cx, "filter_source"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child({
                        let mut btn = Button::new("ksn-src-both")
                            .xsmall()
                            .label(i18n_keyspace_notifications(cx, "source_both"));
                        btn = if matches!(source_filter, SourceFilter::Both) {
                            btn.primary()
                        } else {
                            btn.outline()
                        };
                        btn.on_click(cx.listener(|this, _, _w, cx| this.set_source_filter(SourceFilter::Both, cx)))
                    })
                    .child({
                        let mut btn = Button::new("ksn-src-ke")
                            .xsmall()
                            .label(i18n_keyspace_notifications(cx, "source_keyevent"));
                        btn = if matches!(source_filter, SourceFilter::KeyeventOnly) {
                            btn.primary()
                        } else {
                            btn.outline()
                        };
                        btn.on_click(
                            cx.listener(|this, _, _w, cx| this.set_source_filter(SourceFilter::KeyeventOnly, cx)),
                        )
                    })
                    .child({
                        let mut btn = Button::new("ksn-src-ks")
                            .xsmall()
                            .label(i18n_keyspace_notifications(cx, "source_keyspace"));
                        btn = if matches!(source_filter, SourceFilter::KeyspaceOnly) {
                            btn.primary()
                        } else {
                            btn.outline()
                        };
                        btn.on_click(
                            cx.listener(|this, _, _w, cx| this.set_source_filter(SourceFilter::KeyspaceOnly, cx)),
                        )
                    })
                    .child(div().w(px(12.)))
                    .child(
                        Label::new(i18n_keyspace_notifications(cx, "filter_key_pattern"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().w(px(220.)).child(Input::new(&self.key_filter_input).small())),
            )
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let msg = if !self.is_subscribed() && !self.subscribing {
            if self.notify_flags.is_empty() && self.flags_error.is_none() {
                i18n_keyspace_notifications(cx, "empty_enable_or_subscribe")
            } else {
                i18n_keyspace_notifications(cx, "empty_subscribe")
            }
        } else if self.paused {
            i18n_keyspace_notifications(cx, "empty_paused")
        } else if self.selected_events.is_some()
            || self.selected_dbs.is_some()
            || !self.key_filter.is_empty()
            || !matches!(self.source_filter, SourceFilter::Both)
        {
            i18n_keyspace_notifications(cx, "empty_filtered")
        } else {
            i18n_keyspace_notifications(cx, "empty_waiting")
        };
        div()
            .flex_1()
            .w_full()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .max_w(px(420.))
                    .child(Icon::new(IconName::Inbox).text_color(muted))
                    .child(
                        Label::new(msg)
                            .text_sm()
                            .text_color(muted)
                            .text_center()
                            .whitespace_normal(),
                    ),
            )
            .into_any_element()
    }

    fn render_message_table(&self) -> impl IntoElement {
        div().flex_1().w_full().min_h_0().child(
            DataTable::new(&self.table_state)
                .stripe(true)
                .bordered(false)
                .scrollbar_visible(true, true),
        )
    }
}

impl Render for ZedisKeyspaceNotifications {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        if !self.notify_flags_fetched {
            self.refresh_notify_flags(cx);
        }

        let show_empty = self.row_count == 0;

        let mut body = v_flex().size_full().font_family(get_mono_font_family());
        body = body.child(self.render_header(cx));
        if let Some(banner) = self.render_config_banner(cx) {
            body = body.child(banner);
        }
        if let Some(status) = self.render_flags_status(cx) {
            body = body.child(status);
        }
        if let Some(err) = self.render_subscribe_error(cx) {
            body = body.child(err);
        }
        body = body.child(self.render_filter_bar(cx));
        if show_empty {
            body = body.child(self.render_empty_state(cx));
        } else {
            body = body.child(self.render_message_table());
        }
        body.into_any_element()
    }
}

fn parse_notification(channel: &str, payload: &[u8]) -> Option<NotificationRow> {
    let inner = channel.strip_prefix("__")?;
    let (kind, rest) = inner.split_once('@')?;
    let (db_str, after_db) = rest.split_once("__:")?;
    let db: u32 = db_str.parse().ok()?;
    if after_db.is_empty() {
        return None;
    }
    let payload_str = String::from_utf8_lossy(payload).into_owned();
    let timestamp = now_clock(true).into();

    let (key, event, source) = match kind {
        "keyspace" => (after_db.to_string(), payload_str, NotificationSource::Keyspace),
        "keyevent" => (payload_str, after_db.to_string(), NotificationSource::Keyevent),
        _ => return None,
    };

    Some(NotificationRow {
        timestamp,
        db,
        key: key.into(),
        event: event.to_lowercase().into(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{NotificationSource, parse_notification};

    #[test]
    fn parses_keyspace_channel() {
        let row = parse_notification("__keyspace@0__:user:42", b"del").expect("parse");
        assert_eq!(row.db, 0);
        assert_eq!(row.key.as_ref(), "user:42");
        assert_eq!(row.event.as_ref(), "del");
        assert_eq!(row.source, NotificationSource::Keyspace);
    }

    #[test]
    fn parses_keyevent_channel_swaps_key_and_event() {
        let row = parse_notification("__keyevent@3__:expire", b"session:abc").expect("parse");
        assert_eq!(row.db, 3);
        assert_eq!(row.key.as_ref(), "session:abc");
        assert_eq!(row.event.as_ref(), "expire");
        assert_eq!(row.source, NotificationSource::Keyevent);
    }
}

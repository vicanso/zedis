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
use crate::connection::{Capability, RedisServer, get_connection_manager, get_server, open_monitor_connection};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{
    ConnectionErrorKind, GlobalEvent, NotificationAction, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState,
    back_to_editor_tooltip, content_area_width, escalate_dangerous_body, i18n_common, i18n_monitor, i18n_status_bar,
};
/// Redis MONITOR live viewer.
///
/// Opens dedicated (non-cached) connections to each master node, sends
/// the `MONITOR` command, and streams the output in real time into a
/// scrollable table.  Supports keyword and command-type filtering.
/// The buffer is capped at `MAX_RECORDS` entries.
use crate::views::open_key_in_editor;
use chrono::Local;
use futures::StreamExt;
use gpui::{App, ClipboardItem, Edges, Entity, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::VecDeque;
use tracing::error;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Maximum number of monitor records to keep in memory.
const MAX_RECORDS: usize = 10_000;
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

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

    // Split at '[' to get timestamp and the rest
    let (ts_part, rest) = line.split_once('[')?;
    let (meta, cmd_part) = rest.split_once(']')?;

    // Parse timestamp
    let ts_str = ts_part.trim();
    let timestamp: SharedString = if let Ok(secs) = ts_str.parse::<f64>() {
        let dt = chrono::DateTime::from_timestamp(secs as i64, ((secs.fract()) * 1_000_000_000.0) as u32);
        dt.map(|d| d.with_timezone(&Local).format("%H:%M:%S%.3f").to_string())
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
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            if in_quote {
                parts.push(&cmd_part[start..i]);
                in_quote = false;
            } else {
                in_quote = true;
                start = i + 1;
            }
        }
    }

    let (command, args) = if parts.is_empty() {
        (cmd_part.to_string(), String::new())
    } else {
        let cmd = parts[0].to_uppercase();
        let args = parts[1..].join(" ");
        (cmd, args)
    };

    Some(MonitorEntry {
        timestamp,
        node: node_label.to_string().into(),
        db: db.to_string().into(),
        client: client.to_string().into(),
        command: command.into(),
        args: args.into(),
    })
}

/// Case-insensitive substring search. Uses a zero-allocation ASCII fast
/// path (covers virtually all Redis MONITOR output) with a fallback for
/// non-ASCII content.
fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if haystack.is_ascii() {
        haystack
            .as_bytes()
            .windows(needle_lower.len())
            .any(|w| w.eq_ignore_ascii_case(needle_lower.as_bytes()))
    } else {
        haystack.to_lowercase().contains(needle_lower)
    }
}

// ── Table delegate ───────────────────────────────────────────────────

const COL_TIMESTAMP: &str = "timestamp";
const COL_NODE: &str = "node";
const COL_DB: &str = "db";
const COL_CLIENT: &str = "client";
const COL_COMMAND: &str = "command";
const COL_ARGS: &str = "args";

struct MonitorTableDelegate {
    /// For the args column's open-first-arg-as-key jump.
    server_state: Entity<ZedisServerState>,
    /// All entries (unfiltered).
    all_rows: VecDeque<MonitorEntry>,
    /// Visible rows when a keyword filter is active.
    filtered_rows: Vec<MonitorEntry>,
    /// Whether a keyword filter is currently active.
    is_filtered: bool,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
}

impl MonitorTableDelegate {
    fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &gpui::App) -> Self {
        let content_width = content_area_width(window, cx);

        // Cells are `[label ..flex_1..][copy button ..flex_none..]`, and the copy
        // button only appears on hover — it takes ~36px out of the label's box, so
        // text that fits at rest is clipped the moment the pointer lands on the
        // row. These budget for it, on top of the 20px of side padding:
        //   timestamp: "22:31:05.123" — 12 mono chars (~101px)
        //   node:      "10.51.168.20:31545" — the master's host:port, ~18 (~151px)
        //   client:    "127.0.0.1:60866" — ~15 (~126px)
        let ts_width = 190.;
        let node_width = 240.;
        let db_width = 80.;
        let client_width = 220.;
        let cmd_width = 120.;
        let remaining = content_width.as_f32() - ts_width - node_width - db_width - client_width - cmd_width - 10.;
        let args_width = remaining.max(200.);

        let make_paddings = || {
            Some(Edges {
                top: px(2.),
                bottom: px(2.),
                left: px(10.),
                right: px(10.),
            })
        };

        let column_keys = vec![COL_TIMESTAMP, COL_NODE, COL_DB, COL_CLIENT, COL_COMMAND, COL_ARGS];
        let widths = [ts_width, node_width, db_width, client_width, cmd_width, args_width];

        let columns = column_keys
            .iter()
            .zip(widths.iter())
            .map(|(&key, &width)| {
                Column::new(key, SharedString::default())
                    .width(width)
                    .map(|mut col| {
                        col.paddings = make_paddings();
                        col
                    })
                    .sortable()
            })
            .collect();

        Self {
            server_state,
            all_rows: VecDeque::new(),
            filtered_rows: Vec::new(),
            is_filtered: false,
            columns,
            column_keys,
        }
    }

    fn apply_filter(&mut self, keyword: &str) {
        if keyword.is_empty() {
            self.is_filtered = false;
            self.filtered_rows.clear();
        } else {
            self.is_filtered = true;
            let kw = keyword.to_lowercase();
            self.filtered_rows = self
                .all_rows
                .iter()
                .filter(|e| {
                    contains_ignore_case(&e.command, &kw)
                        || contains_ignore_case(&e.args, &kw)
                        || contains_ignore_case(&e.client, &kw)
                        || contains_ignore_case(&e.node, &kw)
                        || contains_ignore_case(&e.db, &kw)
                })
                .cloned()
                .collect();
        }
    }

    fn visible_row(&self, index: usize) -> Option<&MonitorEntry> {
        if self.is_filtered {
            self.filtered_rows.get(index)
        } else {
            self.all_rows.get(index)
        }
    }

    fn visible_count(&self) -> usize {
        if self.is_filtered {
            self.filtered_rows.len()
        } else {
            self.all_rows.len()
        }
    }
}

impl Clone for MonitorTableDelegate {
    fn clone(&self) -> Self {
        Self {
            server_state: self.server_state.clone(),
            all_rows: self.all_rows.clone(),
            filtered_rows: self.filtered_rows.clone(),
            is_filtered: self.is_filtered,
            columns: self.columns.clone(),
            column_keys: self.column_keys.clone(),
        }
    }
}

impl TableDelegate for MonitorTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.visible_count()
    }

    fn column(&self, index: usize, _cx: &App) -> Column {
        self.columns[index].clone()
    }

    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        let key = self.column_keys[col_ix];
        let cmp = |a: &MonitorEntry, b: &MonitorEntry| -> std::cmp::Ordering {
            match key {
                COL_TIMESTAMP => a.timestamp.cmp(&b.timestamp),
                COL_NODE => a.node.cmp(&b.node),
                COL_DB => a.db.cmp(&b.db),
                COL_CLIENT => a.client.cmp(&b.client),
                COL_COMMAND => a.command.cmp(&b.command),
                COL_ARGS => a.args.cmp(&b.args),
                _ => std::cmp::Ordering::Equal,
            }
        };
        if self.is_filtered {
            match sort {
                ColumnSort::Ascending => self.filtered_rows.sort_by(cmp),
                _ => self.filtered_rows.sort_by(|a, b| cmp(b, a)),
            }
        } else {
            let mut rows: Vec<MonitorEntry> = self.all_rows.drain(..).collect();
            match sort {
                ColumnSort::Ascending => rows.sort_by(cmp),
                _ => rows.sort_by(|a, b| cmp(b, a)),
            }
            self.all_rows = rows.into();
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let name = i18n_monitor(cx, self.column_keys[col_ix]);
        // h_flex (items_center) matches render_td, so header text is
        // vertically centered like the cells.
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(name)
                    .text_align(column.align)
                    .text_color(cx.theme().muted_foreground)
                    .text_sm()
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let col_key = self.column_keys[col_ix];

        let value: SharedString = if let Some(row) = self.visible_row(row_ix) {
            match col_key {
                COL_TIMESTAMP => row.timestamp.clone(),
                COL_NODE => row.node.clone(),
                COL_DB => row.db.clone(),
                COL_CLIENT => row.client.clone(),
                COL_COMMAND => row.command.clone(),
                COL_ARGS => row.args.clone(),
                _ => "--".into(),
            }
        } else {
            "--".into()
        };

        let group_name: SharedString = format!("monitor-td-{}-{}", row_ix, col_ix).into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        // Args cell: the first argument is the key for most commands — offer a
        // hover jump straight to it in the editor (heuristic; args with spaces
        // are ambiguous, so only the first token is used).
        let jump_key: Option<SharedString> = if col_key == COL_ARGS {
            self.visible_row(row_ix).and_then(|row| {
                row.args
                    .split(' ')
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(|s| SharedString::from(s.to_string()))
            })
        } else {
            None
        };
        let jump_server_state = self.server_state.clone();
        let open_key_tooltip = i18n_common(cx, "open_key_tooltip");
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
                    .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                    .when_some(jump_key, |this, key| {
                        this.child(
                            Button::new(("open-key-cell", row_ix * 100 + col_ix))
                                .ghost()
                                .icon(IconName::Search)
                                .tooltip(open_key_tooltip.clone())
                                .on_click(move |_, _, cx: &mut gpui::App| {
                                    open_key_in_editor(&jump_server_state, key.clone(), cx);
                                }),
                        )
                    })
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
            .into_any_element()
    }

    fn has_more(&self, _cx: &App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _window: &mut Window, _cx: &mut Context<TableState<Self>>) {}
}

// ── Main view ────────────────────────────────────────────────────────

pub struct ZedisMonitor {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<MonitorTableDelegate>>,
    keyword_state: Entity<InputState>,
    monitoring: bool,
    row_count: usize,
    monitor_tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisMonitor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let delegate = MonitorTableDelegate::new(server_state.clone(), window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        subscriptions.push(cx.subscribe(&server_state, |this, _, event, cx| {
            if matches!(event, ServerEvent::ServerSelected(_)) {
                this.handle_stop(cx);
                this.table_state.update(cx, |state, _| {
                    state.delegate_mut().all_rows.clear();
                    state.delegate_mut().filtered_rows.clear();
                    state.delegate_mut().is_filtered = false;
                });
                this.row_count = 0;
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
            row_count: 0,
            monitor_tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        let keyword = self.keyword_state.read(cx).value().to_string();
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().apply_filter(&keyword);
        });
        self.row_count = self.table_state.read(cx).delegate().visible_count();
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
    fn confirm_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.monitoring || !self.server_state.read(cx).can(Capability::Observe) {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        let high_risk = get_server(&server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
        if !high_risk {
            self.handle_start(cx);
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
                entity.update(cx, |this, cx| this.handle_start(cx));
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn handle_start(&mut self, cx: &mut Context<Self>) {
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
        cx.notify();

        let table_state = self.table_state.clone();
        let keyword_state = self.keyword_state.clone();
        let entity = cx.entity().downgrade();
        let (tx, rx) = smol::channel::unbounded::<MonitorEntry>();

        let task = cx.spawn(async move |_handle, cx| {
            // Get master node addresses
            let servers: Result<Vec<RedisServer>> = cx
                .background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_id, db).await?;
                    Ok(client.master_servers())
                })
                .await;

            let servers = match servers {
                Ok(servers) => servers,
                Err(e) => {
                    let kind = e.connection_kind();
                    let _ = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                        this.monitoring = false;
                        this.notify_monitor_failed(kind, cx);
                        cx.notify();
                    });
                    return;
                }
            };

            // Spawn one background monitor stream per master node.
            // Each sends parsed entries into the shared channel.
            // Open each node's dedicated MONITOR connection on the foreground
            // task so a failure can be surfaced. The old code opened inside
            // background_spawn, where a failed node vanished silently — on a
            // standalone that meant MONITOR appeared to do nothing at all.
            let mut bg_tasks = Vec::new();
            let mut fail_kind: Option<ConnectionErrorKind> = None;
            for server in servers {
                let node_label = format!("{}:{}", server.host, server.port);
                let monitor = match open_monitor_connection(&server).await {
                    Ok(m) => m,
                    Err(e) => {
                        error!(error = %e, node = %node_label, "failed to start MONITOR");
                        fail_kind.get_or_insert(e.connection_kind());
                        continue;
                    }
                };
                let tx = tx.clone();
                let bg = cx.background_spawn(async move {
                    let mut stream = monitor.into_on_message::<String>();
                    while let Some(line) = stream.next().await {
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
                    this.notify_monitor_failed(kind, cx);
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
                    let keyword = keyword_state.read(cx).value().to_string();
                    table_state.update(cx, |state, _| {
                        let delegate = state.delegate_mut();
                        for entry in batch {
                            delegate.all_rows.push_front(entry);
                        }
                        while delegate.all_rows.len() > MAX_RECORDS {
                            delegate.all_rows.pop_back();
                        }
                        delegate.apply_filter(&keyword);
                    });
                    this.row_count = table_state.read(cx).delegate().visible_count();
                    cx.notify();
                });
                if result.is_err() {
                    break;
                }
            }

            // All streams ended or entity dropped
            let _ = entity.update(cx, |this: &mut ZedisMonitor, cx| {
                this.monitoring = false;
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
        cx.notify();
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

    fn handle_clear(&mut self, cx: &mut Context<Self>) {
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().all_rows.clear();
            state.delegate_mut().filtered_rows.clear();
            state.delegate_mut().is_filtered = false;
        });
        self.row_count = 0;
        cx.notify();
    }
}

impl Render for ZedisMonitor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.row_count == 0 && !self.monitoring;
        let total = self.table_state.read(cx).delegate().all_rows.len();
        let count_label = if self.row_count == total {
            format!("({})", total)
        } else {
            format!("({}/{})", self.row_count, total)
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            // Monospace cascades to the command stream + filter/toolbar text.
            .font_family(get_mono_font_family())
            // Toolbar
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .h(px(40.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
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
                            .when(self.monitoring, |this| {
                                this.child(
                                    Label::new(i18n_monitor(cx, "monitoring"))
                                        .text_color(cx.theme().green)
                                        .text_sm(),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .cleanable(true)
                                    .small(),
                            )
                            .when(!self.monitoring, |this| {
                                this.child(
                                    Button::new("start-monitor")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::Zap))
                                        .label(i18n_monitor(cx, "start"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_start(window, cx);
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
                    .when(is_empty, |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Label::new(i18n_monitor(cx, "no_data")).text_color(cx.theme().muted_foreground)),
                        )
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

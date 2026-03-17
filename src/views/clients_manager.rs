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

/// Redis Client Management viewer.
///
/// Displays a sortable table of connected clients fetched via `CLIENT LIST`.
/// Supports sorting by IP, connected time, and idle time, and allows
/// killing individual client connections via `CLIENT KILL ID`.
use crate::assets::CustomIconName;
use crate::connection::{RedisServer, get_connection_manager, open_single_connection};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::format_duration;
use crate::states::{
    ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_clients_manager, i18n_common,
};
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use redis::cmd;
use rust_i18n::t;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Callback type for killing a client: (id, addr, node).
type KillCallback = Arc<dyn Fn(SharedString, SharedString, RedisServer) + Send + Sync + 'static>;

/// A single parsed client from `CLIENT LIST` output.
#[derive(Clone, Debug)]
struct ClientRow {
    id: SharedString,
    addr: SharedString,
    name: SharedString,
    /// Connection age in seconds.
    age: u64,
    age_display: SharedString,
    /// Idle time in seconds.
    idle: u64,
    idle_display: SharedString,
    db: SharedString,
    flags: SharedString,
    command: SharedString,
    /// The node this client is connected to (for targeted CLIENT KILL).
    node: RedisServer,
}

/// Parses the raw `CLIENT LIST` output (one line per client) into rows.
fn parse_client_list(raw: &str, node: &RedisServer) -> Vec<ClientRow> {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut id = String::new();
            let mut addr = String::new();
            let mut name = String::new();
            let mut age: u64 = 0;
            let mut idle: u64 = 0;
            let mut db = String::new();
            let mut flags = String::new();
            let mut command = String::new();

            for part in line.split_whitespace() {
                if let Some((key, value)) = part.split_once('=') {
                    match key {
                        "id" => id = value.to_string(),
                        "addr" => addr = value.to_string(),
                        "name" => name = value.to_string(),
                        "age" => age = value.parse().unwrap_or(0),
                        "idle" => idle = value.parse().unwrap_or(0),
                        "db" => db = value.to_string(),
                        "flags" => flags = value.to_string(),
                        "cmd" => command = value.to_string(),
                        _ => {}
                    }
                }
            }

            if id.is_empty() {
                return None;
            }

            Some(ClientRow {
                id: id.into(),
                addr: addr.into(),
                name: name.into(),
                age,
                age_display: format_duration(Duration::from_secs(age)).into(),
                idle,
                idle_display: format_duration(Duration::from_secs(idle)).into(),
                db: db.into(),
                flags: flags.into(),
                command: command.into(),
                node: node.clone(),
            })
        })
        .collect()
}

const COLUMN_ID: &str = "id";
const COLUMN_ADDR: &str = "addr";
const COLUMN_NAME: &str = "name";
const COLUMN_AGE: &str = "age";
const COLUMN_IDLE: &str = "idle";
const COLUMN_DB: &str = "db";
const COLUMN_FLAGS: &str = "flags";
const COLUMN_CMD: &str = "cmd";
const COLUMN_ACTION: &str = "action";

struct ClientsTableDelegate {
    /// All rows (unfiltered).
    all_rows: Vec<ClientRow>,
    /// Visible rows after filtering.
    rows: Vec<ClientRow>,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
    /// Callback for killing a client by (ID, addr, node).
    kill_callback: Option<KillCallback>,
    readonly: bool,
}

impl ClientsTableDelegate {
    fn new(rows: Vec<ClientRow>, readonly: bool, window: &mut Window, _cx: &mut gpui::App) -> Self {
        let window_width = window.viewport_size().width;
        let content_width = window_width - SIDEBAR_WIDTH;
        let id_width = 100.;
        let name_width = 150.;
        let age_width = 110.;
        let idle_width = 110.;
        let db_width = 100.;
        let flags_width = 80.;
        let addr_width = 200.;
        let action_width = if readonly { 0. } else { 60. };
        let remaining_width = content_width.as_f32()
            - id_width
            - name_width
            - age_width
            - idle_width
            - db_width
            - flags_width
            - action_width
            - addr_width
            - 10.;
        let cmd_width = remaining_width;

        let make_paddings = || {
            Some(Edges {
                top: px(2.),
                bottom: px(2.),
                left: px(10.),
                right: px(10.),
            })
        };

        let mut column_keys: Vec<&'static str> = vec![
            COLUMN_ID,
            COLUMN_ADDR,
            COLUMN_NAME,
            COLUMN_AGE,
            COLUMN_IDLE,
            COLUMN_DB,
            COLUMN_FLAGS,
            COLUMN_CMD,
        ];
        let mut widths = vec![
            id_width,
            addr_width,
            name_width,
            age_width,
            idle_width,
            db_width,
            flags_width,
            cmd_width,
        ];
        if !readonly {
            column_keys.push(COLUMN_ACTION);
            widths.push(action_width);
        }
        let sortable_cols = [
            COLUMN_ID,
            COLUMN_ADDR,
            COLUMN_AGE,
            COLUMN_IDLE,
            COLUMN_DB,
            COLUMN_FLAGS,
            COLUMN_CMD,
        ];

        let columns = column_keys
            .iter()
            .zip(widths.iter())
            .map(|(&key, &width)| {
                let mut column = Column::new(key, SharedString::default()).width(width).map(|mut col| {
                    col.paddings = make_paddings();
                    col
                });
                if sortable_cols.contains(&key) {
                    column = column.sortable();
                }
                column
            })
            .collect();

        Self {
            all_rows: rows.clone(),
            rows,
            columns,
            column_keys,
            kill_callback: None,
            readonly,
        }
    }

    /// Apply client-side filter.
    ///
    /// - `keyword` — fuzzy match on addr, name, id, db, flags, cmd
    /// - `min_idle` — filter clients idle for at least N seconds
    /// - `min_age`  — filter clients connected for at least N seconds
    fn apply_filter(&mut self, keyword: &str, min_idle: Option<u64>, min_age: Option<u64>) {
        if keyword.is_empty() && min_idle.is_none() && min_age.is_none() {
            self.rows = self.all_rows.clone();
            return;
        }

        let kw = keyword.to_lowercase();
        self.rows = self
            .all_rows
            .iter()
            .filter(|row| {
                if let Some(n) = min_idle
                    && row.idle < n
                {
                    return false;
                }
                if let Some(n) = min_age
                    && row.age < n
                {
                    return false;
                }
                if kw.is_empty() {
                    return true;
                }
                row.addr.to_lowercase().contains(&kw)
                    || row.name.to_lowercase().contains(&kw)
                    || row.id.to_lowercase().contains(&kw)
                    || row.db.to_lowercase().contains(&kw)
                    || row.flags.to_lowercase().contains(&kw)
                    || row.command.to_lowercase().contains(&kw)
            })
            .cloned()
            .collect();
    }
}

impl Clone for ClientsTableDelegate {
    fn clone(&self) -> Self {
        Self {
            all_rows: self.all_rows.clone(),
            rows: self.rows.clone(),
            columns: self.columns.clone(),
            column_keys: self.column_keys.clone(),
            kill_callback: self.kill_callback.clone(),
            readonly: self.readonly,
        }
    }
}

impl TableDelegate for ClientsTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _cx: &gpui::App) -> Column {
        self.columns[index].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) {
        let col = &self.columns[col_ix];
        match col.key.as_ref() {
            COLUMN_ID => match sort {
                ColumnSort::Ascending => self
                    .rows
                    .sort_by(|a, b| a.id.parse::<u64>().unwrap_or(0).cmp(&b.id.parse::<u64>().unwrap_or(0))),
                _ => self
                    .rows
                    .sort_by(|a, b| b.id.parse::<u64>().unwrap_or(0).cmp(&a.id.parse::<u64>().unwrap_or(0))),
            },
            COLUMN_ADDR => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.addr.cmp(&b.addr)),
                _ => self.rows.sort_by(|a, b| b.addr.cmp(&a.addr)),
            },
            COLUMN_AGE => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.age.cmp(&b.age)),
                _ => self.rows.sort_by(|a, b| b.age.cmp(&a.age)),
            },
            COLUMN_IDLE => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.idle.cmp(&b.idle)),
                _ => self.rows.sort_by(|a, b| b.idle.cmp(&a.idle)),
            },
            COLUMN_DB => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.db.cmp(&b.db)),
                _ => self.rows.sort_by(|a, b| b.db.cmp(&a.db)),
            },
            COLUMN_FLAGS => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.flags.cmp(&b.flags)),
                _ => self.rows.sort_by(|a, b| b.flags.cmp(&a.flags)),
            },
            COLUMN_CMD => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.command.cmp(&b.command)),
                _ => self.rows.sort_by(|a, b| b.command.cmp(&a.command)),
            },
            _ => {}
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let name = i18n_clients_manager(cx, self.column_keys[col_ix]);
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

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let col_key = self.column_keys[col_ix];

        // Action column: render kill button (skip for replica connections flagged S or M)
        if col_key == COLUMN_ACTION {
            let Some(row) = self.rows.get(row_ix) else {
                return div().into_any_element();
            };
            if row.flags.contains('S') || row.flags.contains('M') {
                return div().into_any_element();
            }
            let client_id = row.id.clone();
            let client_addr = row.addr.clone();
            let client_node = row.node.clone();
            let kill_callback = self.kill_callback.clone();
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
            let title = i18n_clients_manager(cx, "kill_confirm_title");
            let prompt = t!(
                "clients_manager.kill_confirm_prompt",
                addr = client_addr.as_ref(),
                id = client_id.as_ref(),
                locale = locale
            )
            .to_string();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Button::new(("kill-client", row_ix))
                        .icon(Icon::new(CustomIconName::FileXCorner))
                        .xsmall()
                        .tooltip(i18n_clients_manager(cx, "kill_tooltip"))
                        .on_click(move |_, window, cx: &mut gpui::App| {
                            let kill_callback = kill_callback.clone();
                            let client_id = client_id.clone();
                            let client_addr = client_addr.clone();
                            let client_node = client_node.clone();
                            ZedisDialog::new_alert(title.clone(), prompt.clone())
                                .button_props(dialog_button_props(cx))
                                .on_ok(move |_, window, cx| {
                                    if let Some(ref cb) = kill_callback {
                                        cb(client_id.clone(), client_addr.clone(), client_node.clone());
                                    }
                                    window.close_dialog(cx);
                                    true
                                })
                                .open(window, cx);
                        }),
                )
                .into_any_element();
        }

        // Flags column: render S as HardDrive icon, N as Laptop icon
        if col_key == COLUMN_FLAGS {
            let Some(row) = self.rows.get(row_ix) else {
                return div().into_any_element();
            };
            let flags = &row.flags;
            let content = if flags.contains('S') {
                Icon::new(CustomIconName::HardDrive).into_any_element()
            } else if flags.contains('N') {
                Icon::new(CustomIconName::Laptop).into_any_element()
            } else {
                Label::new(flags.clone()).into_any_element()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .when_some(column.paddings, |this, paddings| this.paddings(paddings))
                .child(content)
                .into_any_element();
        }

        let value: SharedString = if let Some(row) = self.rows.get(row_ix) {
            match col_key {
                COLUMN_ID => row.id.clone(),
                COLUMN_ADDR => row.addr.clone(),
                COLUMN_NAME => row.name.clone(),
                COLUMN_AGE => row.age_display.clone(),
                COLUMN_IDLE => row.idle_display.clone(),
                COLUMN_DB => row.db.clone(),
                COLUMN_CMD => row.command.clone(),
                _ => "--".into(),
            }
        } else {
            "--".into()
        };

        let group_name: SharedString = format!("clients-td-{}-{}", row_ix, col_ix).into();
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
            .into_any_element()
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }

    fn load_more_threshold(&self) -> usize {
        0
    }

    fn load_more(&mut self, _window: &mut Window, _cx: &mut gpui::Context<TableState<Self>>) {}
}

const KEYWORD_INPUT_WIDTH: f32 = 200.0;

pub struct ZedisClientsManager {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<ClientsTableDelegate>>,
    keyword_state: Entity<InputState>,
    idle_state: Entity<InputState>,
    age_state: Entity<InputState>,
    row_count: usize,
    _fetch_task: Option<Task<()>>,
    _kill_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisClientsManager {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let readonly = server_state.read(cx).readonly();
        let delegate = ClientsTableDelegate::new(vec![], readonly, window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        subscriptions.push(cx.subscribe(&server_state, {
            let table_state = table_state.clone();
            move |this, _state, event, cx| {
                if matches!(event, ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated) {
                    this.fetch_clients(table_state.clone(), cx);
                }
            }
        }));

        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });
        let idle_state = cx.new(|cx| InputState::new(window, cx).clean_on_escape().placeholder("idle>=s"));
        let age_state = cx.new(|cx| InputState::new(window, cx).clean_on_escape().placeholder("age>=s"));

        for state in [&keyword_state, &idle_state, &age_state] {
            subscriptions.push(cx.subscribe(state, |this, _, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.handle_filter(cx);
                }
            }));
        }

        let mut this = Self {
            server_state,
            table_state: table_state.clone(),
            keyword_state,
            idle_state,
            age_state,
            row_count: 0,
            _fetch_task: None,
            _kill_task: None,
            _subscriptions: subscriptions,
        };

        this.fetch_clients(table_state, cx);
        this
    }

    fn filter_params(&self, cx: &gpui::Context<Self>) -> (String, Option<u64>, Option<u64>) {
        let keyword = self.keyword_state.read(cx).value().to_string();
        let min_idle = self.idle_state.read(cx).value().parse::<u64>().ok();
        let min_age = self.age_state.read(cx).value().parse::<u64>().ok();
        (keyword, min_idle, min_age)
    }

    fn handle_filter(&mut self, cx: &mut gpui::Context<Self>) {
        let (keyword, min_idle, min_age) = self.filter_params(cx);
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().apply_filter(&keyword, min_idle, min_age);
        });
        self.row_count = self.table_state.read(cx).delegate().rows.len();
        cx.notify();
    }

    fn fetch_clients(&mut self, table_state: Entity<TableState<ClientsTableDelegate>>, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        let readonly = self.server_state.read(cx).readonly();

        self._fetch_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let (addrs, results): (Vec<RedisServer>, Vec<String>) = client
                    .query_async_masters(vec![cmd("CLIENT").arg("LIST").clone()])
                    .await?;
                let mut all_rows = Vec::new();
                for (node, raw) in addrs.iter().zip(results.iter()) {
                    all_rows.extend(parse_client_list(raw, node));
                }
                all_rows.sort_by(|a, b| b.age.cmp(&a.age));
                Ok::<Vec<ClientRow>, Error>(all_rows)
            });

            let result: Result<Vec<ClientRow>> = task.await;
            let _ = handle.update(cx, move |this, cx| {
                match result {
                    Ok(rows) => {
                        let (keyword, min_idle, min_age) = this.filter_params(cx);
                        table_state.update(cx, |state, _| {
                            state.delegate_mut().all_rows = rows;
                            state.delegate_mut().readonly = readonly;
                            state.delegate_mut().apply_filter(&keyword, min_idle, min_age);
                        });
                        this.row_count = table_state.read(cx).delegate().rows.len();
                        this.setup_kill_callback(cx);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to fetch client list");
                    }
                }
                cx.notify();
            });
        }));
    }

    fn setup_kill_callback(&mut self, cx: &mut gpui::Context<Self>) {
        let server_state = self.server_state.clone();
        let table_state = self.table_state.clone();

        let (tx, rx) = smol::channel::unbounded::<(SharedString, SharedString, RedisServer)>();

        self.table_state.update(cx, |state, _| {
            state.delegate_mut().kill_callback = Some(Arc::new(move |id, addr, node| {
                let _ = tx.send_blocking((id, addr, node));
            }));
        });

        self._kill_task = Some(cx.spawn(async move |handle, cx| {
            while let Ok((client_id, _client_addr, node)) = rx.recv().await {
                let db = server_state.update(cx, |state, _| state.db());

                let id_clone = client_id.clone();
                let task = cx.background_spawn(async move {
                    let mut conn = open_single_connection(&node, db, true).await?;
                    let _: String = cmd("CLIENT")
                        .arg("KILL")
                        .arg("ID")
                        .arg(id_clone.as_ref())
                        .query_async(&mut conn)
                        .await?;
                    Ok::<(), Error>(())
                });

                let result = task.await;
                let locale_table_state = table_state.clone();
                let _ = handle.update(cx, move |this, cx| {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                    match result {
                        Ok(()) => {
                            let msg = t!(
                                "clients_manager.kill_success",
                                addr = client_id.as_ref(),
                                locale = locale
                            );
                            this.server_state.update(cx, |state, cx| {
                                state.emit_success_notification(msg.into(), "CLIENT KILL".into(), cx);
                            });
                            this.fetch_clients(locale_table_state, cx);
                        }
                        Err(e) => {
                            let msg = t!("clients_manager.kill_failed", error = e.to_string(), locale = locale);
                            this.server_state.update(cx, |state, cx| {
                                state.emit_error_notification(msg.into(), cx);
                            });
                        }
                    }
                });
            }
        }));
    }
}

impl gpui::Render for ZedisClientsManager {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let is_empty = self.row_count == 0;
        let total = self.table_state.read(cx).delegate().all_rows.len();
        let count_label = if self.row_count == total {
            format!("({})", total)
        } else {
            format!("({}/{})", self.row_count, total)
        };

        v_flex()
            .size_full()
            .overflow_hidden()
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
                            .child(Icon::new(CustomIconName::AudioWaveform))
                            .child(Label::new(i18n_clients_manager(cx, "title")).text_color(cx.theme().foreground))
                            .child(
                                Label::new(count_label)
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            ),
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
                            .child(Input::new(&self.idle_state).w(px(80.)).cleanable(true).small())
                            .child(Input::new(&self.age_state).w(px(80.)).cleanable(true).small())
                            .child(
                                Button::new("filter-clients")
                                    .outline()
                                    .small()
                                    .icon(IconName::Search)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.handle_filter(cx);
                                    })),
                            )
                            .child(
                                Button::new("refresh-clients")
                                    .outline()
                                    .small()
                                    .icon(Icon::new(CustomIconName::RotateCw))
                                    .tooltip(i18n_clients_manager(cx, "refresh_tooltip"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        let table_state = this.table_state.clone();
                                        this.fetch_clients(table_state, cx);
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
                        this.child(div().size_full().flex().items_center().justify_center().child(
                            Label::new(i18n_clients_manager(cx, "no_clients")).text_color(cx.theme().muted_foreground),
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

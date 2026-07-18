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
use crate::connection::{Capability, RedisServer, get_connection_manager, open_single_connection};
use crate::error::Error;
use crate::helpers::{format_duration, get_mono_font_family};
/// Redis Client Management viewer.
///
/// Displays a sortable table of connected clients fetched via `CLIENT LIST`.
/// Supports sorting by IP, connected time, and idle time, and allows
/// killing individual client connections via `CLIENT KILL ID`.
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width,
    dialog_button_props, escalate_dangerous_body, i18n_clients_manager, i18n_common,
};
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    select::{Select, SelectEvent, SelectItem, SelectState},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    tooltip::Tooltip,
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

/// One entry of the client-type filter. `flag` is the letter `CLIENT LIST`
/// reports in its `flags=` field, so filtering is a plain `contains`:
/// `N` normal · `S` replica · `M` master · `O` monitor · `P` pub/sub ·
/// `b` blocked. `None` is the unfiltered default.
#[derive(Clone, Debug)]
struct FlagOption {
    label: SharedString,
    flag: Option<char>,
}

impl SelectItem for FlagOption {
    type Value = Option<char>;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.flag
    }
}

/// The filter's entries, in menu order. `i18n` labels are resolved by the caller.
const FLAG_FILTERS: [(&str, Option<char>); 7] = [
    ("flag_all", None),
    ("flag_normal", Some('N')),
    ("flag_replica", Some('S')),
    ("flag_master", Some('M')),
    ("flag_monitor", Some('O')),
    ("flag_pubsub", Some('P')),
    ("flag_blocked", Some('b')),
];

/// The action column holds exactly one xsmall icon button (kill), centered — so
/// it is sized once and never flexes. 60px also lets the "Action" header sit
/// unclipped, provided the column drops the 10px side paddings the text columns
/// use (the button is centered, so it needs none).
const ACTION_COLUMN_WIDTH: f32 = 80.0;

/// What Redis puts in `cmd=` for a connection that has never run a command —
/// the literal four letters, not an absent field (see `catClientInfoString`).
const REDIS_NO_COMMAND: &str = "NULL";

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
    /// Active server id, used to escalate the kill-confirm wording on
    /// high-risk (PROD-tagged) servers. Set when the client list loads.
    server_id: String,
}

impl ClientsTableDelegate {
    fn new(rows: Vec<ClientRow>, readonly: bool, window: &mut Window, cx: &mut gpui::App) -> Self {
        let content_width = content_area_width(window, cx);
        let id_width = 120.;
        // Cells are `[label ..flex_1..][copy button ..flex_none..]`, and the copy
        // button only appears on hover — so it steals ~28px from the label's box
        // and clips text that fitted perfectly at rest. Both widths budget for it.
        //
        // addr must fit the longest IPv4 form, "255.255.255.255:65535" — 21 mono
        // chars (~176px) plus 20px of padding.
        //
        // name is deliberately the smallest column that still identifies the
        // client ("zedis…" is enough to tell ours apart); most clients set no
        // name at all, so every pixel spent here is taken from `cmd`, which is
        // what you actually read when diagnosing. Longer names ellipsize and can
        // be read via the copy button.
        let name_width = 120.;
        let addr_width = 240.;
        let age_width = 110.;
        let idle_width = 110.;
        let db_width = 100.;
        let flags_width = 80.;
        // CLIENT KILL is gated by the capability matrix.
        let can_kill = Capability::KillClient.allowed(readonly);
        let action_width = if can_kill { ACTION_COLUMN_WIDTH } else { 0. };
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
        // `cmd` is the flexible column, but a narrow window must not drive it to
        // zero (or negative) — the table scrolls horizontally instead.
        let cmd_width = remaining_width.max(160.);

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
        if can_kill {
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
                    // The action column's cell is a centered button and its header
                    // is a single word — the text columns' side paddings would only
                    // eat 20 of its 60px and clip the header.
                    if key != COLUMN_ACTION {
                        col.paddings = make_paddings();
                    }
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
            server_id: String::new(),
        }
    }

    /// Apply client-side filter.
    ///
    /// - `keyword` — fuzzy match on addr, name, id, db, flags, cmd
    /// - `min_idle` — filter clients idle for at least N seconds
    /// - `min_age`  — filter clients connected for at least N seconds
    /// - `flag`     — client type, as its `CLIENT LIST` flag letter (see
    ///   [`FLAG_FILTERS`]); `None` keeps every type
    fn apply_filter(&mut self, keyword: &str, min_idle: Option<u64>, min_age: Option<u64>, flag: Option<char>) {
        if keyword.is_empty() && min_idle.is_none() && min_age.is_none() && flag.is_none() {
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
                // Client type: the `flags=` field is a set of letters, so a
                // membership test is the whole rule.
                if let Some(flag) = flag
                    && !row.flags.contains(flag)
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
            server_id: self.server_id.clone(),
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
                ColumnSort::Ascending => self.rows.sort_by_key(|a| a.id.parse::<u64>().unwrap_or(0)),
                _ => self
                    .rows
                    .sort_by_key(|b| std::cmp::Reverse(b.id.parse::<u64>().unwrap_or(0))),
            },
            COLUMN_ADDR => match sort {
                ColumnSort::Ascending => self.rows.sort_by(|a, b| a.addr.cmp(&b.addr)),
                _ => self.rows.sort_by(|a, b| b.addr.cmp(&a.addr)),
            },
            COLUMN_AGE => match sort {
                ColumnSort::Ascending => self.rows.sort_by_key(|a| a.age),
                _ => self.rows.sort_by_key(|b| std::cmp::Reverse(b.age)),
            },
            COLUMN_IDLE => match sort {
                ColumnSort::Ascending => self.rows.sort_by_key(|a| a.idle),
                _ => self.rows.sort_by_key(|b| std::cmp::Reverse(b.idle)),
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
        // h_flex (items_center) matches render_td, so header text is
        // vertically centered like the cells.
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(name)
                    .text_align(column.align)
                    .text_color(cx.theme().primary)
                    .text_sm()
                    .flex_1(),
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
            let prompt = escalate_dangerous_body(cx, &self.server_id, prompt);
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

        // `cmd` is the *last command run*, and Redis writes the literal string
        // "NULL" for a connection that has never run one. Printed as-is it reads
        // like a command actually named NULL, so show a muted placeholder and put
        // the meaning in a tooltip. There is nothing worth copying either, so this
        // cell drops the copy button.
        if col_key == COLUMN_CMD
            && let Some(row) = self.rows.get(row_ix)
            && row.command == REDIS_NO_COMMAND
        {
            let tooltip = i18n_clients_manager(cx, "cmd_none_tooltip");
            return h_flex()
                .id(("cmd-none", row_ix))
                .size_full()
                .when_some(column.paddings, |this, paddings| this.paddings(paddings))
                .child(Label::new("—").text_color(cx.theme().muted_foreground))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
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
/// Wide enough for the longest localized client-type label.
const FLAG_SELECT_WIDTH: f32 = 120.0;

pub struct ZedisClientsManager {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<ClientsTableDelegate>>,
    keyword_state: Entity<InputState>,
    idle_state: Entity<InputState>,
    age_state: Entity<InputState>,
    /// Client-type filter (`CLIENT LIST` flag letter) — see [`FLAG_FILTERS`].
    flag_state: Entity<SelectState<Vec<FlagOption>>>,
    row_count: usize,
    /// Set when `CLIENT LIST` fails, so the empty body shows the error
    /// instead of a misleading "no clients" message.
    error: Option<SharedString>,
    _fetch_task: Option<Task<()>>,
    _kill_task: Option<Task<()>>,
    /// One-shot batch kill over the current filtered rows; aggregates the
    /// outcome into a single notification + refresh (unlike the per-row
    /// channel, which notifies and refetches per kill).
    _batch_kill_task: Option<Task<()>>,
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

        let flag_options = FLAG_FILTERS
            .iter()
            .map(|(key, flag)| FlagOption {
                label: i18n_clients_manager(cx, key),
                flag: *flag,
            })
            .collect::<Vec<_>>();
        // Index 0 is "all" — the unfiltered default.
        let flag_state = cx.new(|cx| SelectState::new(flag_options, Some(IndexPath::new(0)), window, cx));
        subscriptions.push(cx.subscribe_in(
            &flag_state,
            window,
            |this, _state, event: &SelectEvent<Vec<FlagOption>>, _window, cx| match event {
                // Re-filter as soon as a type is picked; no need to press the
                // search button like the free-text fields require.
                SelectEvent::Confirm(_) => this.handle_filter(cx),
            },
        ));

        let mut this = Self {
            server_state,
            table_state: table_state.clone(),
            keyword_state,
            idle_state,
            age_state,
            flag_state,
            row_count: 0,
            error: None,
            _fetch_task: None,
            _kill_task: None,
            _batch_kill_task: None,
            _subscriptions: subscriptions,
        };

        this.fetch_clients(table_state, cx);
        this
    }

    fn filter_params(&self, cx: &gpui::Context<Self>) -> (String, Option<u64>, Option<u64>, Option<char>) {
        let keyword = self.keyword_state.read(cx).value().to_string();
        let min_idle = self.idle_state.read(cx).value().parse::<u64>().ok();
        let min_age = self.age_state.read(cx).value().parse::<u64>().ok();
        let flag = self.flag_state.read(cx).selected_value().copied().flatten();
        (keyword, min_idle, min_age, flag)
    }

    fn handle_filter(&mut self, cx: &mut gpui::Context<Self>) {
        let (keyword, min_idle, min_age, flag) = self.filter_params(cx);
        self.table_state.update(cx, |state, _| {
            state.delegate_mut().apply_filter(&keyword, min_idle, min_age, flag);
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
        let server_id_for_delegate = server_id.clone();

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
                all_rows.sort_by_key(|b| std::cmp::Reverse(b.age));
                Ok::<Vec<ClientRow>, Error>(all_rows)
            });

            let result: Result<Vec<ClientRow>> = task.await;
            let _ = handle.update(cx, move |this, cx| {
                match result {
                    Ok(rows) => {
                        let (keyword, min_idle, min_age, flag) = this.filter_params(cx);
                        table_state.update(cx, |state, _| {
                            state.delegate_mut().all_rows = rows;
                            state.delegate_mut().readonly = readonly;
                            state.delegate_mut().server_id = server_id_for_delegate;
                            state.delegate_mut().apply_filter(&keyword, min_idle, min_age, flag);
                        });
                        this.row_count = table_state.read(cx).delegate().rows.len();
                        this.error = None;
                        this.setup_kill_callback(cx);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to fetch client list");
                        this.error = Some(e.to_string().into());
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

    /// Confirm-and-kill every *currently filtered* client (replica links
    /// flagged S/M are skipped, same rule as the per-row button). The confirm
    /// prompt names the exact count and escalates on production-tagged servers.
    fn handle_batch_kill(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // Defense in depth — the button is hidden without KillClient.
        if !self.server_state.read(cx).can(Capability::KillClient) {
            return;
        }
        let targets: Vec<(SharedString, RedisServer)> = self
            .table_state
            .read(cx)
            .delegate()
            .rows
            .iter()
            .filter(|row| !row.flags.contains('S') && !row.flags.contains('M'))
            .map(|row| (row.id.clone(), row.node.clone()))
            .collect();
        if targets.is_empty() {
            return;
        }

        let server_id = self.server_state.read(cx).server_id().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_clients_manager(cx, "batch_kill_confirm_title");
        let prompt = t!(
            "clients_manager.batch_kill_confirm_prompt",
            count = targets.len(),
            locale = locale
        )
        .to_string();
        let prompt = escalate_dangerous_body(cx, &server_id, prompt);
        let entity = cx.entity().clone();

        ZedisDialog::new_alert(title, prompt)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let targets = targets.clone();
                entity.update(cx, |this, cx| {
                    this.batch_kill(targets, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Kill the given clients one by one (`CLIENT KILL ID`, each on its own
    /// node), then surface a single aggregated notification and refresh once.
    fn batch_kill(&mut self, targets: Vec<(SharedString, RedisServer)>, cx: &mut gpui::Context<Self>) {
        let db = self.server_state.read(cx).db();
        let table_state = self.table_state.clone();

        self._batch_kill_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut ok = 0usize;
                let mut failed = 0usize;
                for (id, node) in targets {
                    let result = async {
                        let mut conn = open_single_connection(&node, db, true).await?;
                        let _: String = cmd("CLIENT")
                            .arg("KILL")
                            .arg("ID")
                            .arg(id.as_ref())
                            .query_async(&mut conn)
                            .await?;
                        Ok::<(), Error>(())
                    }
                    .await;
                    match result {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            error!(error = %e, id = id.as_ref(), "batch client kill fail");
                            failed += 1;
                        }
                    }
                }
                (ok, failed)
            });

            let (ok, failed) = task.await;
            let _ = handle.update(cx, move |this, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                if failed == 0 {
                    let msg = t!("clients_manager.batch_kill_success", count = ok, locale = locale);
                    this.server_state.update(cx, |state, cx| {
                        state.emit_success_notification(msg.into(), "CLIENT KILL".into(), cx);
                    });
                } else {
                    let msg = t!(
                        "clients_manager.batch_kill_partial",
                        ok = ok,
                        failed = failed,
                        locale = locale
                    );
                    this.server_state.update(cx, |state, cx| {
                        state.emit_error_notification(msg.into(), cx);
                    });
                }
                this.fetch_clients(table_state, cx);
            });
        }));
    }
}

impl gpui::Render for ZedisClientsManager {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let is_empty = self.row_count == 0;
        // On a fetch error, show the error (red) in the empty body instead of
        // the misleading "no clients" message.
        let (empty_text, empty_color) = if let Some(err) = &self.error {
            (err.clone(), cx.theme().red)
        } else {
            (i18n_clients_manager(cx, "no_clients"), cx.theme().muted_foreground)
        };
        let total = self.table_state.read(cx).delegate().all_rows.len();
        let readonly = self.table_state.read(cx).delegate().readonly;
        // Batch kill targets = the filtered rows minus replica links (S/M),
        // mirroring the per-row kill button's rule.
        let killable = self
            .table_state
            .read(cx)
            .delegate()
            .rows
            .iter()
            .filter(|row| !row.flags.contains('S') && !row.flags.contains('M'))
            .count();
        let count_label = if self.row_count == total {
            format!("({})", total)
        } else {
            format!("({}/{})", self.row_count, total)
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            // Monospace cascades to client addresses, IDs and last-command text.
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
                            .items_center()
                            .child(
                                Button::new("clients-back")
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
                            // Keyword leads: it is the widest control and the one
                            // reached for first. The type filter is a secondary
                            // facet, so it follows.
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .cleanable(true)
                                    .small(),
                            )
                            // Client type (CLIENT LIST flag) — applies on pick, so
                            // "show me just the normal clients" is one click.
                            //
                            // Wrapped in a fixed-width box: `Select`'s outer element
                            // is `size_full`, so on its own it stretches to fill the
                            // toolbar (its own `.w()` only refines the inner input),
                            // shoving everything after it to the right.
                            .child(
                                div()
                                    .w(px(FLAG_SELECT_WIDTH))
                                    .flex_none()
                                    .child(Select::new(&self.flag_state).small()),
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
                            .when(Capability::KillClient.allowed(readonly), |this| {
                                this.child(
                                    Button::new("batch-kill-clients")
                                        .outline()
                                        .small()
                                        .icon(Icon::new(CustomIconName::FileXCorner))
                                        .tooltip(i18n_clients_manager(cx, "batch_kill_tooltip"))
                                        .disabled(killable == 0)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.handle_batch_kill(window, cx);
                                        })),
                                )
                            })
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
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Label::new(empty_text).text_color(empty_color)),
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

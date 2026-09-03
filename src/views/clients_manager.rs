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
use crate::views::unavailable_chip;
use gpui::{Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    select::{Select, SelectEvent, SelectItem, SelectState},
    table::{DataTable, TableState},
    tooltip::Tooltip,
    v_flex,
};
use redis::cmd;
use rust_i18n::t;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;
use zedis_ui::{CellRenderer, CellStyle, CellStyleProvider, RowPredicate, TextColumn, ZedisDialog, ZedisTextTable};

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

const CLIENT_COLUMNS: [&str; 8] = [
    COLUMN_ID,
    COLUMN_ADDR,
    COLUMN_NAME,
    COLUMN_AGE,
    COLUMN_IDLE,
    COLUMN_DB,
    COLUMN_FLAGS,
    COLUMN_CMD,
];
const ID_COLUMN: usize = 0;
const FLAGS_COLUMN: usize = 6;
const CMD_COLUMN: usize = 7;
/// The kill-button column, present only when `CLIENT KILL` is usable.
const ACTION_COLUMN: usize = 8;
/// Payload cells: raw age and idle seconds, behind the humanised columns,
/// for sorting and the `>=` filters.
const CELL_AGE: usize = 9;
const CELL_IDLE: usize = 10;

impl ClientRow {
    fn cells(&self) -> Vec<SharedString> {
        vec![
            self.id.clone(),
            self.addr.clone(),
            self.name.clone(),
            self.age_display.clone(),
            self.idle_display.clone(),
            self.db.clone(),
            self.flags.clone(),
            self.command.clone(),
            SharedString::default(),
            self.age.to_string().into(),
            self.idle.to_string().into(),
        ]
    }
}

/// A replica link (`S`) or the master's link on a replica (`M`): never a
/// kill target.
fn is_replica_link(flags: &str) -> bool {
    flags.contains('S') || flags.contains('M')
}

/// What the per-row kill button needs beyond the row: whether kills are
/// allowed at all, the server (for the PROD-escalated wording), the
/// callback, and the node each client id was listed from. Shared between
/// the view, which refreshes it with every `CLIENT LIST`, and the table's
/// cell renderer.
#[derive(Default)]
struct KillContext {
    readonly: bool,
    server_id: String,
    callback: Option<KillCallback>,
    nodes: HashMap<String, RedisServer>,
}

/// The client grid: eight text columns plus, when the user may kill
/// clients, an action column with the per-row button.
fn build_table(
    readonly: bool,
    kill: Rc<RefCell<KillContext>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> ZedisTextTable {
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
    let widths = [
        id_width,
        addr_width,
        name_width,
        age_width,
        idle_width,
        db_width,
        flags_width,
        cmd_width,
    ];
    let mut columns: Vec<TextColumn> = CLIENT_COLUMNS
        .iter()
        .zip(widths)
        .map(|(&key, width)| {
            let column = TextColumn::new(key, i18n_clients_manager(cx, key), width);
            match key {
                COLUMN_ID => column.sortable().numeric(),
                COLUMN_AGE => column.sort_by_cell(CELL_AGE),
                COLUMN_IDLE => column.sort_by_cell(CELL_IDLE),
                COLUMN_ADDR | COLUMN_DB | COLUMN_FLAGS | COLUMN_CMD => column.sortable(),
                _ => column,
            }
        })
        .collect();
    if can_kill {
        // The action column's cell is a centered button and its header is a
        // single word — the text columns' side paddings would only eat 20 of
        // its 80px and clip the header.
        columns.push(TextColumn::new(COLUMN_ACTION, i18n_clients_manager(cx, COLUMN_ACTION), action_width).unpadded());
    }

    // Flags column: a replica link shows as a drive, a normal client as a
    // laptop, anything else as its letters.
    let style: CellStyleProvider = Rc::new(|col_ix, cells, _cx| {
        if col_ix != FLAGS_COLUMN {
            return CellStyle::default();
        }
        let flags = cells.get(FLAGS_COLUMN).map(|f| f.as_ref()).unwrap_or("");
        let icon = if flags.contains('S') {
            Some(Icon::new(CustomIconName::HardDrive))
        } else if flags.contains('N') {
            Some(Icon::new(CustomIconName::Laptop))
        } else {
            None
        };
        CellStyle {
            icon_only: icon.is_some(),
            icon,
            color: None,
        }
    });

    let render: CellRenderer = Rc::new(move |row_ix, col_ix, cells, _window, cx| {
        let cell = |ix: usize| cells.get(ix).cloned().unwrap_or_default();
        // `cmd` is the *last command run*, and Redis writes the literal
        // string "NULL" for a connection that has never run one. Printed
        // as-is it reads like a command actually named NULL, so show a muted
        // placeholder and put the meaning in a tooltip. There is nothing
        // worth copying either, so this cell drops the copy button.
        if col_ix == CMD_COLUMN && cell(CMD_COLUMN).as_ref() == REDIS_NO_COMMAND {
            let tooltip = i18n_clients_manager(cx, "cmd_none_tooltip");
            return Some(
                h_flex()
                    .id(("cmd-none", row_ix))
                    .size_full()
                    .paddings(Edges {
                        top: px(2.),
                        bottom: px(2.),
                        left: px(10.),
                        right: px(10.),
                    })
                    .child(Label::new("—").text_color(cx.theme().muted_foreground))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .into_any_element(),
            );
        }
        if col_ix != ACTION_COLUMN {
            return None;
        }
        // Action column: the kill button, except for replica links.
        if is_replica_link(cell(FLAGS_COLUMN).as_ref()) {
            return Some(div().into_any_element());
        }
        let client_id = cell(ID_COLUMN);
        let client_addr = cell(1);
        let (kill_callback, client_node, server_id) = {
            let ctx = kill.borrow();
            (
                ctx.callback.clone(),
                ctx.nodes.get(client_id.as_ref()).cloned(),
                ctx.server_id.clone(),
            )
        };
        let client_node = client_node?;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let title = i18n_clients_manager(cx, "kill_confirm_title");
        let prompt = t!(
            "clients_manager.kill_confirm_prompt",
            addr = client_addr.as_ref(),
            id = client_id.as_ref(),
            locale = locale
        )
        .to_string();
        let prompt = escalate_dangerous_body(cx, &server_id, prompt);
        Some(
            div()
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
                .into_any_element(),
        )
    });

    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .filter_columns(&[COLUMN_ADDR, COLUMN_NAME, COLUMN_ID, COLUMN_DB, COLUMN_FLAGS, COLUMN_CMD])
        .cell_style(style)
        .cell_render(render)
}

/// The structured part of the filter — `min_idle` / `min_age` in seconds
/// and the client-type flag letter (see [`FLAG_FILTERS`]) — as a row
/// predicate; `None` when none of them is set.
fn row_predicate(min_idle: Option<u64>, min_age: Option<u64>, flag: Option<char>) -> Option<RowPredicate> {
    if min_idle.is_none() && min_age.is_none() && flag.is_none() {
        return None;
    }
    Some(Rc::new(move |cells: &[SharedString]| {
        let secs = |ix: usize| cells.get(ix).and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
        if let Some(n) = min_idle
            && secs(CELL_IDLE) < n
        {
            return false;
        }
        if let Some(n) = min_age
            && secs(CELL_AGE) < n
        {
            return false;
        }
        // Client type: the `flags=` field is a set of letters, so a
        // membership test is the whole rule.
        if let Some(flag) = flag
            && !cells.get(FLAGS_COLUMN).is_some_and(|f| f.contains(flag))
        {
            return false;
        }
        true
    }))
}

const KEYWORD_INPUT_WIDTH: f32 = 200.0;
/// Wide enough for the longest localized client-type label.
const FLAG_SELECT_WIDTH: f32 = 120.0;

pub struct ZedisClientsManager {
    server_state: Entity<ZedisServerState>,
    table_state: Entity<TableState<ZedisTextTable>>,
    /// Shared with the table's kill button — see [`KillContext`].
    kill: Rc<RefCell<KillContext>>,
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
    _unpause_task: Option<Task<()>>,
    /// One-shot batch kill over the current filtered rows; aggregates the
    /// outcome into a single notification + refresh (unlike the per-row
    /// channel, which notifies and refetches per kill).
    _batch_kill_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisClientsManager {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        // The delegate's "readonly" is really "no CLIENT KILL": read-only
        // mode or a server where KILL is missing / denied both hide the
        // per-row button.
        let readonly = !server_state.read(cx).can(Capability::KillClient);
        let kill = Rc::new(RefCell::new(KillContext {
            readonly,
            ..Default::default()
        }));
        let table_state = cx.new(|cx| TableState::new(build_table(readonly, kill.clone(), window, cx), window, cx));

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
            _unpause_task: None,
            _batch_kill_task: None,
            kill,
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
            let delegate = state.delegate_mut();
            delegate.set_filter(&keyword);
            delegate.set_row_filter(row_predicate(min_idle, min_age, flag));
        });
        self.row_count = self.table_state.read(cx).delegate().visible_len();
        cx.notify();
    }

    fn fetch_clients(&mut self, table_state: Entity<TableState<ZedisTextTable>>, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        let readonly = !self.server_state.read(cx).can(Capability::KillClient);
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
                        {
                            let mut kill = this.kill.borrow_mut();
                            kill.readonly = readonly;
                            kill.server_id = server_id_for_delegate;
                            kill.nodes = rows.iter().map(|r| (r.id.to_string(), r.node.clone())).collect();
                        }
                        table_state.update(cx, |state, _| {
                            let delegate = state.delegate_mut();
                            delegate.set_rows(rows.iter().map(ClientRow::cells).collect());
                            delegate.set_filter(&keyword);
                            delegate.set_row_filter(row_predicate(min_idle, min_age, flag));
                        });
                        this.row_count = table_state.read(cx).delegate().visible_len();
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

    /// The visible rows minus replica links, each with the node it was
    /// listed from — the batch-kill set.
    fn killable_targets(&self, cx: &gpui::Context<Self>) -> Vec<(SharedString, RedisServer)> {
        let kill = self.kill.borrow();
        self.table_state
            .read(cx)
            .delegate()
            .visible_rows()
            .iter()
            .filter(|cells| !cells.get(FLAGS_COLUMN).is_some_and(|f| is_replica_link(f)))
            .filter_map(|cells| {
                let id = cells.get(ID_COLUMN)?;
                let node = kill.nodes.get(id.as_ref())?;
                Some((id.clone(), node.clone()))
            })
            .collect()
    }

    fn setup_kill_callback(&mut self, cx: &mut gpui::Context<Self>) {
        let server_state = self.server_state.clone();
        let table_state = self.table_state.clone();

        let (tx, rx) = smol::channel::unbounded::<(SharedString, SharedString, RedisServer)>();

        self.kill.borrow_mut().callback = Some(Arc::new(move |id, addr, node| {
            let _ = tx.send_blocking((id, addr, node));
        }));

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

    /// `CLIENT UNPAUSE` on every master — lifts a `CLIENT PAUSE` left
    /// behind by a failover script or a stray terminal command, the state
    /// where the whole server appears hung. Idempotent and harmless when
    /// nothing is paused, so no confirm dialog.
    fn handle_unpause(&mut self, cx: &mut gpui::Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        let server_state = self.server_state.clone();
        self._unpause_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let (_, replies): (_, Vec<String>) = client
                    .query_async_masters(vec![cmd("CLIENT").arg("UNPAUSE").clone()])
                    .await?;
                let _ = replies;
                Ok::<(), Error>(())
            });
            let result = task.await;
            let _ = handle.update(cx, move |_this, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                match result {
                    Ok(()) => {
                        let msg = t!("clients_manager.unpause_success", locale = locale);
                        server_state.update(cx, |state, cx| {
                            state.emit_success_notification(msg.to_string().into(), "CLIENT UNPAUSE".into(), cx);
                        });
                    }
                    Err(e) => {
                        let msg = t!("clients_manager.unpause_failed", error = e.to_string(), locale = locale);
                        server_state.update(cx, |state, cx| {
                            state.emit_error_notification(msg.to_string().into(), cx);
                        });
                    }
                }
                cx.notify();
            });
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
        let targets = self.killable_targets(cx);
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
        let total = self.table_state.read(cx).delegate().total_len();
        let readonly = self.kill.borrow().readonly;
        // Batch kill targets = the filtered rows minus replica links (S/M),
        // mirroring the per-row kill button's rule.
        let killable = self.killable_targets(cx).len();
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
                            // Explain a missing kill button when the server
                            // (not the access mode) is what takes it away.
                            .when_some(
                                self.server_state.read(cx).blocked_by(Capability::KillClient),
                                |this, (command, status)| this.child(unavailable_chip(cx, command, status)),
                            )
                            .when(!readonly, |this| {
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
                                .child(
                                    Button::new("unpause-clients")
                                        .outline()
                                        .small()
                                        .icon(IconName::Play)
                                        .tooltip(i18n_clients_manager(cx, "unpause_tooltip"))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.handle_unpause(cx);
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

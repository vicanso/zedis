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

use crate::states::{command_status_label, i18n_features, i18n_trash, i18n_troubleshoot};
use crate::{
    assets::CustomIconName,
    connection::{
        CommandStatus, DangerKind, KillTarget, RedisClientDescription, ServerCommand, ServerFeatures, ServerFlavor,
        WRITE_UNLOCK_SECS, get_server,
    },
    constants::STATUS_BAR_HEIGHT,
    helpers::{
        client_counts, format_lag_bytes, get_mono_font_family, group_thousands, kills_runaway_scripts, pacing,
        resolve_tag_chip, saves_files, with_hot_key,
    },
    states::{
        ConnectionErrorKind, ConnectionHealth, ErrorMessage, RedisKeySpaceStats, ReplicaInfo, SOFT_WRAP_MAX_BYTES,
        ServerEvent, ServerToolsAction, ServerView, ViewMode, ZedisGlobalStore, ZedisServerState, content_area_width,
        get_session_option, i18n_acl, i18n_common, i18n_config_editor, i18n_functions, i18n_hotkeys, i18n_key_tree,
        i18n_keyspace_notifications, i18n_lua_scripts, i18n_monitor, i18n_persistence, i18n_search, i18n_server_info,
        i18n_server_load, i18n_sidebar, i18n_status_bar, i18n_timeseries, i18n_topology, i18n_value_search,
        save_session_option,
    },
    views::confirm_dangerous_command,
};
use gpui::{
    Anchor, App, Entity, Hsla, MouseButton, Pixels, SharedString, Subscription, Task, TextAlign, Window, div,
    prelude::*, px, rgb,
};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};
use rust_i18n::t;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tracing::{debug, info};
use zedis_ui::ZedisDivider;

mod editor_controls;
mod server_status;
mod telemetry;
mod tools_menu;

use editor_controls::VIEWER_MODES;
pub(crate) use server_status::product_line;
use server_status::{format_nodes, format_nodes_description};
use telemetry::format_latency;
use tools_menu::ToolsMenuGates;

/// The key count, "listed/total": the keys the tree lists of the ones the
/// database holds.
#[inline]
fn format_size(dbsize: Option<u64>, listed: usize) -> SharedString {
    if let Some(dbsize) = dbsize {
        format!("{}/{}", group_thousands(listed as u64), group_thousands(dbsize))
    } else {
        "--".to_string()
    }
    .into()
}

/// The design's recessive status-bar text color (t2): `#878d97` dark /
/// `#686d76` light. gpui-component's `Label` always paints `theme.foreground`
/// (it does not inherit the parent's `text_color`), so each value label sets
/// this explicitly to read as muted instead of bright.
fn status_text_color(is_dark: bool) -> Hsla {
    if is_dark {
        rgb(0x878d97).into()
    } else {
        rgb(0x686d76).into()
    }
}

// --- Local State ---

/// Make `db` the database select's committed choice, unless it already is.
///
/// `set_items` swaps the rows and nothing else. A choice made while there
/// were none — the first build, the moment after a server switch — stays
/// empty, and the trigger still reads "DB: n" off the list's cursor; Esc then
/// puts that cursor back on the (empty) committed choice and the trigger goes
/// blank. Not re-applied when it already matches: the rows are rebuilt
/// whenever a key count changes, and that would pull the cursor back under a
/// user moving it through the open menu.
fn commit_selected_db(
    state: &mut SelectState<Vec<DbInfo>>,
    db: usize,
    window: &mut Window,
    cx: &mut Context<SelectState<Vec<DbInfo>>>,
) {
    if state.selected_value() != Some(&db) {
        state.set_selected_index(Some(IndexPath::new(db)), window, cx);
    }
}

/// The content width from which the bar names its counts ("0 / 2 clients"
/// rather than "0 / 2" beside an icon), and the wider one it needs while the
/// editor's own controls (Soft Wrap, the format, the Viewer) share the bar.
const UNITS_MIN_WIDTH: f32 = 1020.;
const UNITS_MIN_WIDTH_WITH_EDITOR: f32 = 1260.;
/// The content width from which the editor's three controls are drawn as
/// they are; below it they fold into one menu button. The default window
/// (1200, so 1020 of content) cut "Viewer: Auto" at the window's edge.
const EDITOR_CONTROLS_MIN_WIDTH: f32 = 1120.;
/// The content width from which the link chip says "Connected" beside its
/// icon, with the editor's controls (folded) on the bar and without them.
const LINK_LABEL_MIN_WIDTH_WITH_EDITOR: f32 = 860.;
const LINK_LABEL_MIN_WIDTH: f32 = 810.;

/// What the bar has room for, from the most to the least. A bar that has to
/// stay one line gives things up in this order, and whole: first the words
/// beside its counts, then the editor's controls fold into one button, then
/// the link chip lets its word go and keeps its icon — never half of
/// something cut by the window's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum BarFit {
    Full,
    NoUnits,
    FoldedEditorControls,
    LinkIconOnly,
}

impl BarFit {
    fn for_width(width: f32, editor_controls: bool) -> Self {
        if editor_controls {
            if width >= UNITS_MIN_WIDTH_WITH_EDITOR {
                BarFit::Full
            } else if width >= EDITOR_CONTROLS_MIN_WIDTH {
                BarFit::NoUnits
            } else if width >= LINK_LABEL_MIN_WIDTH_WITH_EDITOR {
                BarFit::FoldedEditorControls
            } else {
                BarFit::LinkIconOnly
            }
        } else if width >= UNITS_MIN_WIDTH {
            BarFit::Full
        } else if width >= LINK_LABEL_MIN_WIDTH {
            BarFit::NoUnits
        } else {
            BarFit::LinkIconOnly
        }
    }
}

#[derive(Default)]
struct StatusBarServerState {
    server_id: SharedString,
    size: SharedString,
    latency: (SharedString, Hsla),
    /// Live-connection health for the status dot, mirrored from
    /// `ZedisServerState::connection_health()` each heartbeat / health change.
    health: ConnectionHealth,
    /// Mirrors `ZedisServerState::last_connection_error()` — names the reason
    /// in the offline tooltip (auth / perms / timeout / network / tunnel).
    last_connection_error: ConnectionErrorKind,
    used_memory: SharedString,
    clients: SharedString,
    nodes: SharedString,
    scan_finished: bool,
    soft_wrap: bool,
    nodes_description: SharedString,
    slow_log_tips: SharedString,
    tag: SharedString,
    /// Stored tag color *preset key* (e.g. `magenta`), resolved to
    /// mode-aware chip colors at render time.
    tag_color_key: Option<String>,
    /// Mirrors `ZedisServerState::supports_search()` — gates the Search
    /// item in the Tools dropdown so servers without the RediSearch
    /// module don't get a dead-end menu entry.
    supports_search: bool,
    /// Mirrors `ZedisServerState::supports_acl()` — Redis 5.x and
    /// earlier don't expose the `ACL` command family, so hide the
    /// menu item entirely there.
    supports_acl: bool,
    /// Mirrors `ZedisServerState::supports_functions()` — `FUNCTION`
    /// commands are Redis 7+.
    supports_functions: bool,
    /// Mirrors `ZedisServerState::supports_topology()` — the Topology panel
    /// is hidden only while the server type is still unknown.
    supports_topology: bool,
    /// True when `INFO` reports `role:slave` — the connection points at a
    /// replica, so writes will bounce with `-READONLY`. Drives the quiet
    /// "Replica" badge next to the environment tag.
    is_replica: bool,
    /// Mirrors `ZedisServerState::features()` — the probed command matrix.
    /// Drives the "Limited" badge, disabled Tools entries and metric chips.
    features: Arc<ServerFeatures>,
    /// Number of currently active threshold alerts on this server.
    active_alert_count: usize,
}

/// DB-dropdown row geometry, shared by the row renderer and the menu-width
/// calculation so they can't drift apart. The rows render at `Size::Small`
/// (⇒ `text_sm` = 14px) in the bar's JetBrains Mono (advance 0.6em), so a
/// character is 14 × 0.6 = 8.4px.
const DB_MENU_CHAR_W: f32 = 8.4;
/// Fixed label column ("DB: 15" = 6 mono chars) so the counts line up.
const DB_LABEL_COL_W: f32 = 52.;

#[derive(Debug, Clone)]
struct DbInfo {
    label: SharedString,
    db: usize,
    /// Key count from the heartbeat's `INFO keyspace` (0 = empty or not
    /// yet sampled) — rendered as a muted figure beside the label so the
    /// dropdown answers "which db has the keys?" without switching.
    keys: u64,
}

impl SelectItem for DbInfo {
    type Value = usize;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.db
    }
    // Dropdown row only — the trigger falls back to `title()` and stays
    // a compact "DB: n".
    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap_3()
            // Fixed label column so the counts line up (mono font cascades
            // from the status bar).
            .child(div().min_w(px(DB_LABEL_COL_W)).child(self.label.clone()))
            .when(self.keys > 0, |this| {
                this.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(group_thousands(self.keys))),
                )
            })
    }
}

/// Map the `INFO keyspace` section (`"db0"` → stats) onto a dense per-db
/// key-count vec of length `databases` (missing dbs are empty ⇒ 0).
fn keyspace_key_counts(keyspace: &HashMap<String, RedisKeySpaceStats>, databases: usize) -> Vec<u64> {
    let mut counts = vec![0u64; databases];
    for (name, stats) in keyspace {
        if let Some(db) = name.strip_prefix("db").and_then(|s| s.parse::<usize>().ok())
            && db < counts.len()
        {
            counts[db] = stats.keys;
        }
    }
    counts
}

/// Local state for the status bar to cache formatted strings and colors.
/// This prevents re-calculating strings on every render frame.
#[derive(Default)]
struct StatusBarState {
    server_state: StatusBarServerState,
    data_format: Option<SharedString>,
    error: Option<ErrorMessage>,
}

pub struct ZedisStatusBar {
    state: StatusBarState,

    viewer_mode_state: Entity<SelectState<SearchableVec<SharedString>>>,
    db_state: Entity<SelectState<Vec<DbInfo>>>,
    /// The database list is open — from the press on its trigger until the
    /// select reports a choice or a dismissal. The trigger's tooltip is
    /// held back meanwhile: it opened over the bottom rows of the list.
    db_menu_open: bool,
    should_reset_viewer_mode: Option<bool>,
    should_reset_db: Option<bool>,
    should_rebuild_db_items: Option<usize>,
    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    databases: usize,
    /// Latest per-db key counts (index = db) from `INFO keyspace`; a change
    /// triggers a db-item rebuild so the dropdown shows fresh counts.
    db_key_counts: Vec<u64>,
    readonly: bool,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize state from the current server state
        // Read only necessary fields to avoid cloning the entire state if it's large

        let mut subscriptions = vec![];
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            match event {
                ServerEvent::ServerSelected(server_id) => {
                    this.reset(server_id.clone());
                }
                ServerEvent::ServerRedisInfoUpdated => {
                    this.fill_state(server_state, cx);
                }
                ServerEvent::ConnectionHealthChanged => {
                    // Heartbeat reported a link transition (e.g. a failed PING).
                    // The Ok path already refreshes health via fill_state; this
                    // arm covers the Err path, which emits no info update.
                    let s = server_state.read(cx);
                    this.state.server_state.health = s.connection_health();
                    this.state.server_state.last_connection_error = s.last_connection_error();
                    this.state.server_state.active_alert_count = s.active_alerts().len();
                }
                ServerEvent::ServerInfoUpdated => {
                    this.readonly = server_state.read(cx).readonly();
                    let databases = server_state.read(cx).databases();
                    if this.databases != databases {
                        this.databases = databases;
                        this.should_rebuild_db_items = Some(databases);
                    }
                    server_state.update(cx, |state, cx| {
                        state.refresh_redis_info(cx);
                    });
                }
                ServerEvent::KeyScanStarted => {
                    this.state.server_state.scan_finished = false;
                }
                ServerEvent::KeyScanFinished | ServerEvent::KeyScanPaged | ServerEvent::KeysListed => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.listed_count());
                    this.state.server_state.scan_finished = state.scan_completed();
                }
                ServerEvent::ErrorOccurred(error) => {
                    debug!(
                        message = error.message.as_str(),
                        category = error.category.as_str(),
                        "error occurred"
                    );
                    this.state.error = Some(error.clone());
                }
                ServerEvent::TaskStarted(task) => {
                    // Background heartbeat (RefreshRedisInfo) fires every
                    // 2s (less while backing off) and never changes anything visible from
                    // this handler — return early so it doesn't trigger
                    // an empty re-render. Other tasks clear any stale
                    // error chip.
                    if task.is_background_refresh() {
                        return;
                    }
                    this.state.error = None;
                }
                ServerEvent::ValueLoaded => {
                    let state = server_state.read(cx);
                    this.should_reset_viewer_mode = Some(true);
                    if let Some(value) = state.value().and_then(|item| item.bytes_value()) {
                        let mut format = value.format.as_str().to_string();
                        if let Some(mime) = &value.mime {
                            format = format!("{}({})", format, mime);
                        }
                        this.state.data_format = Some(format.into());
                    } else {
                        this.state.data_format = None;
                    }
                }
                _ => {
                    return;
                }
            }
            cx.notify();
        }));
        let viewer_mode_state = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(VIEWER_MODES.iter().map(|mode| mode.as_str().into()).collect::<Vec<_>>()),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });

        subscriptions.push(cx.subscribe_in(
            &viewer_mode_state,
            window,
            |view, _state, event: &SelectEvent<SearchableVec<SharedString>>, _window, cx| match event {
                SelectEvent::Confirm(value) => {
                    if let Some(selected_value) = value {
                        view.server_state.update(cx, |state, cx| {
                            state.update_bytes_value_view_mode(selected_value.clone(), cx);
                        });
                    }
                }
            },
        ));

        let db_state = cx.new(|cx| SelectState::new(vec![], Some(IndexPath::new(0)), window, cx));
        subscriptions.push(cx.subscribe_in(
            &db_state,
            window,
            |view, _state, event: &SelectEvent<Vec<DbInfo>>, _window, cx| match event {
                SelectEvent::Confirm(value) => {
                    // A choice or a dismissal: either way the list is closed.
                    view.db_menu_open = false;
                    cx.notify();
                    let Some(db) = *value else {
                        return;
                    };
                    let server_id = view.server_state.read(cx).server_id().to_string();
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.set_selected_server((server_id, db), cx);
                        });
                    });
                }
            },
        ));

        let readonly = server_state.read(cx).readonly();
        let databases = server_state.read(cx).databases();

        let mut this = Self {
            databases,
            db_key_counts: Vec::new(),
            should_rebuild_db_items: Some(databases),
            heartbeat_task: None,
            viewer_mode_state,
            db_state,
            db_menu_open: false,
            should_reset_db: None,
            server_state: server_state.clone(),
            _subscriptions: subscriptions,
            should_reset_viewer_mode: None,
            state: StatusBarState { ..Default::default() },
            readonly,
        };
        this.fill_state(server_state.clone(), cx);
        this.start_heartbeat(server_state, cx);

        info!("Creating new status bar view");
        this
    }
    fn reset(&mut self, server_id: SharedString) {
        if self.state.server_state.server_id != server_id {
            self.state.server_state = StatusBarServerState::default();
            self.state.server_state.server_id = server_id.clone();
        } else {
            self.state.server_state.size = SharedString::default();
        }
        // Refresh the cached tag chip whenever the selection changes.
        if !server_id.is_empty()
            && let Ok(server) = get_server(server_id.as_ref())
        {
            self.state.server_state.tag = server.tag_label().unwrap_or_default().to_string().into();
            self.state.server_state.tag_color_key = server.tag_color.clone();
        } else {
            self.state.server_state.tag = SharedString::default();
            self.state.server_state.tag_color_key = None;
        }
        self.should_reset_db = Some(true);
        // Drop the previous server's key counts right away — the new
        // server's arrive with its first heartbeat INFO.
        if !self.db_key_counts.is_empty() {
            self.db_key_counts.clear();
            self.should_rebuild_db_items = Some(self.databases);
        }
        self.state.data_format = None;
        self.state.error = None;
    }
    fn fill_state(&mut self, server_state: Entity<ZedisServerState>, cx: &Context<Self>) {
        let state = server_state.read(cx);
        let Some(redis_info) = state.redis_info() else {
            return;
        };
        let clients = if redis_info.metrics.connected_clients == 0 {
            "--".to_string()
        } else {
            client_counts(redis_info.metrics.connected_clients, redis_info.metrics.blocked_clients)
        };
        let used_memory = if redis_info.metrics.used_memory == 0 {
            "--".to_string()
        } else {
            humansize::format_size(
                redis_info.metrics.used_memory,
                humansize::FormatSizeOptions::default().decimal_places(0),
            )
        };

        let slow_log_tips = format!(
            "{} / {}",
            group_thousands(state.last_slow_log_count() as u64),
            group_thousands(state.slow_logs().len() as u64)
        )
        .into();
        let tag = self.state.server_state.tag.clone();
        let tag_color_key = self.state.server_state.tag_color_key.clone();
        let supports_search = state.supports_search();
        let supports_acl = state.supports_acl();
        let supports_functions = state.supports_functions();
        let supports_topology = state.supports_topology();
        self.state.server_state = StatusBarServerState {
            server_id: state.server_id().to_string().into(),
            size: format_size(state.dbsize(), state.listed_count()),
            latency: format_latency(Some(Duration::from_millis(redis_info.metrics.latency_ms)), cx),
            health: state.connection_health(),
            last_connection_error: state.last_connection_error(),
            used_memory: used_memory.into(),
            clients: clients.into(),
            nodes: {
                // Node discovery only lists replicas for Cluster; on plain
                // master-replica (and Sentinel) splice in the count the
                // heartbeat's `INFO replication` reports, so the chip
                // agrees with the tooltip's replica lines.
                let (masters, discovered_replicas) = state.nodes();
                let replica_count = if discovered_replicas == 0 {
                    redis_info.replicas.len()
                } else {
                    discovered_replicas
                };
                format_nodes((masters, replica_count))
            },
            scan_finished: state.scan_completed(),
            slow_log_tips,
            soft_wrap: state.soft_wrap(),
            nodes_description: {
                // The server-status tooltip also names what the probe found
                // unusable — the quiet place for it (the Tools menu has the
                // full matrix).
                let mut text = format_nodes_description(
                    state.nodes_description().clone(),
                    redis_info.replicas.as_slice(),
                    state.version(),
                    cx,
                )
                .to_string();
                let unusable = state.features().unusable();
                if !unusable.is_empty() {
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    text.push_str("\n\n");
                    text.push_str(&t!(
                        "servers.diag_capabilities_limited",
                        count = unusable.len(),
                        locale = &locale
                    ));
                    for (command, status) in unusable {
                        text.push('\n');
                        text.push_str(&command_status_label(cx, command, status));
                    }
                }
                text.into()
            },
            tag,
            tag_color_key,
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
            is_replica: redis_info.meta.role == "slave",
            features: state.features(),
            active_alert_count: state.active_alerts().len(),
        };
        // Per-db key counts for the DB dropdown (#116). Rebuild the select
        // items only on an actual change so the 2s heartbeat doesn't churn
        // the dropdown state.
        let db_key_counts = keyspace_key_counts(&redis_info.keyspace, self.databases);
        if self.db_key_counts != db_key_counts {
            self.db_key_counts = db_key_counts;
            self.should_rebuild_db_items = Some(self.databases);
        }
    }
    /// Start the heartbeat task. 2-second cadence keeps the chips
    /// (latency, used memory, connected clients, replication lag)
    /// snappy — matches the Metrics panel heartbeat for consistency.
    /// The CPU baseline is dominated by other render paths anyway,
    /// so this interval doesn't move the needle either direction.
    /// The tick is only the metronome: `refresh_redis_info` decides
    /// whether an attempt goes out (one in flight at a time, exponential
    /// backoff while the server is unreachable, 30s in a background tab).
    fn start_heartbeat(&mut self, server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) {
        self.heartbeat_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(pacing::HEARTBEAT_INTERVAL).await;
                server_state.update(cx, |state, cx| {
                    state.refresh_redis_info(cx);
                });
            }
        }));
    }

    /// Whether the bar is wide enough to say what its counts are. Three
    /// bare "n / m" pairs beside three icons were only readable to someone
    /// who already knew them; the words go away again where they would push
    /// the rest of the bar out of the window.
    fn shows_units(&self, window: &Window, cx: &App) -> bool {
        self.bar_fit(window, cx) == BarFit::Full
    }

    /// What the bar has room for at the content's current width.
    fn bar_fit(&self, window: &Window, cx: &App) -> BarFit {
        BarFit::for_width(content_area_width(window, cx).as_f32(), self.show_editor_controls(cx))
    }

    /// Render the error message
    fn render_errors(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = &self.state.error else {
            return h_flex().flex_1();
        };
        // Full text in tooltip — the middle slot truncates when the bar is
        // crowded, so hover still surfaces the complete error.
        let full = data.message.clone();
        let tip = full.clone();
        h_flex().flex_1().min_w_0().child(
            div()
                .id("zedis-status-bar-error")
                .w_full()
                .min_w_0()
                .mr_2()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(
                    Label::new(full)
                        .w_full()
                        .text_xs()
                        .text_color(cx.theme().red)
                        .text_align(TextAlign::Right)
                        .truncate(),
                ),
        )
    }
}

impl Render for ZedisStatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug!("render status bar view");
        if self.state.server_state.server_id.is_empty() {
            return h_flex();
        }
        if let Some(true) = self.should_reset_viewer_mode.take() {
            self.viewer_mode_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(0)), window, cx);
            });
        }
        if let Some(true) = self.should_reset_db.take() {
            let db = cx
                .global::<ZedisGlobalStore>()
                .read(cx)
                .selected_server()
                .map(|(_, db)| *db)
                .unwrap_or_default();
            self.db_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(db)), window, cx);
            });
        }
        if let Some(databases) = self.should_rebuild_db_items.take() {
            let db_items = (0..databases)
                .map(|db| DbInfo {
                    label: format!("DB: {}", db).into(),
                    db,
                    keys: self.db_key_counts.get(db).copied().unwrap_or(0),
                })
                .collect::<Vec<_>>();
            let db = cx
                .global::<ZedisGlobalStore>()
                .read(cx)
                .selected_server()
                .map(|(_, db)| *db)
                .unwrap_or_default();
            self.db_state.update(cx, |state, cx| {
                state.set_items(db_items, window, cx);
                commit_selected_db(state, db, window, cx);
            });
        }
        let status_text = status_text_color(cx.theme().is_dark());
        let editor_controls = self.show_editor_controls(cx);
        let folded = self.bar_fit(window, cx) >= BarFit::FoldedEditorControls;
        h_flex()
            .items_center()
            .w_full()
            .h(STATUS_BAR_HEIGHT)
            .text_sm()
            // Monospace for the whole bar — cascades to every child label /
            // button / select text (icons are SVGs, unaffected).
            .font_family(get_mono_font_family())
            .py_1p5()
            .px_4()
            .gap_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_color(status_text)
            // Left: connection context (env tag · DB · readonly · tools | keyspace).
            .child(self.render_server_status(window, cx))
            // Middle: flexible spacer; also right-aligns any error text and
            // pushes the telemetry cluster to the far right (matches the design).
            .child(self.render_errors(window, cx))
            // Right: telemetry (Connected · metrics) + editor settings.
            .child(
                ZedisDivider::new()
                    .child(self.render_telemetry(window, cx))
                    .when(editor_controls && !folded, |this| {
                        this.child(self.render_editor_settings(window, cx))
                    })
                    .when(editor_controls && !folded, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .child(self.render_data_format(window, cx))
                                .child(self.render_viewer_mode(window, cx)),
                        )
                    })
                    .when(editor_controls && folded, |this| {
                        this.child(self.render_folded_editor_controls(cx))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AnyWindowHandle, ElementId, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};

    fn open_bar(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ZedisServerState>) {
        let server_state = cx.new(|_| ZedisServerState::default());
        let handle = cx.open_window(size(px(1400.), px(200.)), |window, cx| {
            let view = cx.new(|cx| ZedisStatusBar::new(server_state.clone(), window, cx));
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        server_state.update(cx, |state, cx| state.select("scratch".into(), 0, cx));
        cx.run_until_parked();
        (handle.into(), server_state)
    }

    /// What the open Tools menu lists, top to bottom: its own entries, and
    /// those of the group whose submenu is open.
    fn menu_labels(window: &Window) -> (Vec<String>, Vec<String>) {
        let menu = ElementId::Name("popup-menu".into());
        let submenu = ElementId::Name("submenu".into());
        let mut entries: Vec<(bool, f32, String)> = window
            .elements()
            .into_iter()
            .filter(|element| element.path().contains(&menu))
            .filter_map(|element| {
                let label = element.label()?.to_string();
                Some((
                    element.path().contains(&submenu),
                    f32::from(element.bounds().origin.y),
                    label,
                ))
            })
            .collect();
        entries.sort_by(|a, b| a.1.total_cmp(&b.1));
        let pick = |inner: bool| entries.iter().filter(|e| e.0 == inner).map(|e| e.2.clone()).collect();
        (pick(false), pick(true))
    }

    /// Opens the Tools menu, walks its groups with the arrow key and closes
    /// it again: the menu's own entries, and each group's in turn.
    fn walk_tools_menu(cx: &mut TestAppContext, handle: AnyWindowHandle) -> (Vec<String>, Vec<Vec<String>>) {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("zedis-status-bar-tools", cx);
        })
        .expect("the window is open");
        cx.run_until_parked();
        let mut top = Vec::new();
        let mut groups = Vec::new();
        for _ in 0..4 {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let (entries, group) = menu_labels(window);
                top = entries;
                groups.push(group);
                window.press("down", cx);
            })
            .expect("the window is open");
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .expect("the window is open");
        cx.run_until_parked();
        (top, groups)
    }

    /// The bar for an open server: its context on the left (the terminal,
    /// the read-only switch, the Tools menu, more keys), the link and the
    /// four chips that open their panels on the right. No Kill button while
    /// the link is not down with `BUSY`, and none of the editor's controls
    /// off the editor.
    #[gpui::test]
    fn the_bar_draws_the_server_s_context_and_its_telemetry(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, _server_state) = open_bar(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "zedis-status-bar-server-terminal",
                "zedis-status-bar-server-toggle-readonly",
                "zedis-status-bar-tools",
                "zedis-status-bar-scan-more",
                "zedis-status-bar-conn-health",
                "zedis-status-bar-server-metrics",
                "zedis-status-bar-server-memory-analysis",
                "zedis-status-bar-clients",
                "zedis-status-bar-server-slow-logs",
            ] {
                assert!(window.try_find(id).is_some(), "{id} is drawn");
            }
            for id in [
                "zedis-status-bar-kill-script",
                "soft-wrap",
                "data-format",
                "zedis-status-bar-view",
            ] {
                assert!(window.try_find(id).is_none(), "{id} is not drawn");
            }
        })
        .expect("the window is open");
    }

    /// The Tools menu: four groups by purpose and the two reports beside
    /// them, each group's panels in its order — and on a read-only
    /// connection the writes stay listed, with the reason they are off.
    #[gpui::test]
    fn the_tools_menu_groups_the_panels_and_says_why_a_write_is_off(cx: &mut TestAppContext) {
        init_ui(cx);
        let (handle, server_state) = open_bar(cx);
        let (top, groups) = walk_tools_menu(cx, handle);
        assert_eq!(
            top,
            [
                "Observability",
                "Query & Scripting",
                "Keys & Data",
                "Administration",
                "Server capabilities…",
                "Troubleshooting Report…"
            ]
        );
        assert_eq!(
            groups,
            [
                vec![
                    "Live Monitor",
                    "Command Stats",
                    "Hot Keys",
                    "Keyspace Notifications",
                    "Time Series Explorer",
                    "Pubsub Mode",
                    "Server Info (INFO)"
                ],
                vec!["Value Search", "Lua Scripts"],
                vec![
                    "Recycle Bin…",
                    "Import Keys…",
                    "Export Loaded Keys…",
                    "Compare with Another Server…"
                ],
                vec![
                    "Config Management",
                    "Persistence",
                    "Flush Current Database…",
                    "Flush All Databases…"
                ],
            ]
        );

        server_state.update(cx, |state, cx| state.toggle_readonly(cx));
        cx.run_until_parked();
        let (_, groups) = walk_tools_menu(cx, handle);
        assert_eq!(
            groups[2],
            [
                "Recycle Bin…",
                "Import Keys…  ·  Disabled in read-only mode",
                "Export Loaded Keys…",
                "Compare with Another Server…"
            ]
        );
        assert_eq!(
            groups[3],
            [
                "Config Management",
                "Persistence",
                "Flush Current Database…  ·  Disabled in read-only mode",
                "Flush All Databases…  ·  Disabled in read-only mode"
            ]
        );
    }

    fn db_rows(count: usize) -> Vec<DbInfo> {
        (0..count)
            .map(|db| DbInfo {
                label: format!("DB: {db}").into(),
                db,
                keys: 0,
            })
            .collect()
    }

    /// The order the status bar builds its database select in: created
    /// empty, told which database is open before the rows arrive, then given
    /// them. Esc puts the list's cursor back on the committed choice, so an
    /// empty one blanked the trigger.
    #[gpui::test]
    fn the_open_database_is_committed_once_the_rows_arrive(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let select = cx.new(|cx| SelectState::new(Vec::<DbInfo>::new(), Some(IndexPath::new(0)), window, cx));
            select.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(2)), window, cx);
                state.set_items(db_rows(4), window, cx);
                assert_eq!(state.selected_value(), None, "set_items commits nothing by itself");

                commit_selected_db(state, 2, window, cx);
                assert_eq!(state.selected_value(), Some(&2));
                assert_eq!(state.selected_index(cx), Some(IndexPath::new(2)));

                // New key counts rebuild the rows; the choice stands.
                state.set_items(db_rows(4), window, cx);
                commit_selected_db(state, 2, window, cx);
                assert_eq!(state.selected_value(), Some(&2));
            });
        });
    }

    #[test]
    fn format_size_groups_both_counts() {
        assert_eq!(format_size(Some(500_000), 10_535).as_ref(), "10,535/500,000");
        assert_eq!(format_size(None, 10_535).as_ref(), "--");
    }

    #[test]
    fn keyspace_key_counts_maps_dense_and_ignores_out_of_range() {
        let mut keyspace = HashMap::new();
        keyspace.insert(
            "db0".to_string(),
            RedisKeySpaceStats {
                keys: 42,
                ..Default::default()
            },
        );
        keyspace.insert(
            "db15".to_string(),
            RedisKeySpaceStats {
                keys: 7,
                ..Default::default()
            },
        );
        // Out of range for the configured db count — must not panic or land.
        keyspace.insert(
            "db99".to_string(),
            RedisKeySpaceStats {
                keys: 1,
                ..Default::default()
            },
        );
        // Not a keyspace line shape — ignored.
        keyspace.insert("dbx".to_string(), RedisKeySpaceStats::default());

        let counts = keyspace_key_counts(&keyspace, 16);
        assert_eq!(counts.len(), 16);
        assert_eq!(counts[0], 42);
        assert_eq!(counts[15], 7);
        // Every db INFO didn't mention is empty.
        assert!(counts[1..15].iter().all(|&c| c == 0));

        assert!(keyspace_key_counts(&keyspace, 0).is_empty());
    }

    /// The bar gives things up one at a time as the content narrows, in one
    /// order, and later with the editor's controls on it than without. The
    /// default window (1020 of content) used to cut "Viewer: Auto" at its
    /// edge; there the editor's controls are folded into one button.
    #[test]
    fn the_bar_gives_things_up_in_order_as_it_narrows() {
        let editor = |width: f32| BarFit::for_width(width, true);
        let page = |width: f32| BarFit::for_width(width, false);
        assert_eq!(editor(1400.), BarFit::Full);
        assert_eq!(editor(1200.), BarFit::NoUnits);
        assert_eq!(editor(1020.), BarFit::FoldedEditorControls);
        assert_eq!(editor(780.), BarFit::LinkIconOnly);
        assert_eq!(page(1020.), BarFit::Full);
        assert_eq!(page(900.), BarFit::NoUnits);
        assert_eq!(page(780.), BarFit::LinkIconOnly);
        // Narrower never shows more.
        let mut last = BarFit::Full;
        for width in (600..1500).rev().step_by(10) {
            let fit = editor(width as f32);
            assert!(fit >= last, "{width}");
            last = fit;
        }
    }
}

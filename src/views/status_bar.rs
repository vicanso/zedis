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

use crate::states::{command_status_label, i18n_features};
// The recycle bin's menu entry is the desktop's: the browser keeps no bin.
#[cfg(not(target_family = "wasm"))]
use crate::states::i18n_trash;
use crate::{
    assets::CustomIconName,
    connection::{
        CommandStatus, DangerKind, KillTarget, RedisClientDescription, ServerCommand, ServerFeatures,
        WRITE_UNLOCK_SECS, get_server,
    },
    constants::STATUS_BAR_HEIGHT,
    helpers::{format_lag_bytes, get_mono_font_family, group_thousands, pacing, resolve_tag_chip, with_hot_key},
    states::{
        ConnectionErrorKind, ConnectionHealth, ErrorMessage, RedisKeySpaceStats, ReplicaInfo, SOFT_WRAP_MAX_BYTES,
        ServerEvent, ServerTask, ServerToolsAction, ServerView, ViewMode, ZedisGlobalStore, ZedisServerState,
        content_area_width, get_session_option, i18n_acl, i18n_common, i18n_config_editor, i18n_functions,
        i18n_hotkeys, i18n_key_tree, i18n_keyspace_notifications, i18n_lua_scripts, i18n_monitor, i18n_persistence,
        i18n_search, i18n_server_info, i18n_server_load, i18n_sidebar, i18n_status_bar, i18n_timeseries, i18n_topology,
        i18n_value_search, save_session_option,
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
    menu::{DropdownMenu, PopupMenu},
    tooltip::Tooltip,
};
use rust_i18n::t;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tracing::{debug, info};
use zedis_ui::ZedisDivider;

/// Fixed mono width for the latency label (value left-aligned, padding
/// trailing): 5 chars covers "999ms" / "1.23s" / "--". The heartbeat
/// refreshes the value every 2s and the
/// telemetry cluster is right-anchored, so a "9ms" ⇄ "10ms" flip would resize
/// the (now native, center-justified) latency button and shove the Connected
/// chip beside it on each beat; padding the value to a fixed mono width keeps
/// the button width constant instead.
const LATENCY_LABEL_CHARS: usize = 5;

/// Formats the database size and scan count string "count/total".
#[inline]
fn format_size(dbsize: Option<u64>, scan_count: usize) -> SharedString {
    if let Some(dbsize) = dbsize {
        format!("{}/{}", group_thousands(scan_count as u64), group_thousands(dbsize))
    } else {
        "--".to_string()
    }
    .into()
}
/// The green of a healthy link — the latency figure and the "Connected"
/// label. One value could not serve both themes: `#69b083` is 7.7:1 on the
/// dark bar and 2.6:1 on the light one, so light takes a darker green
/// (`#1a7f37`, 5.1:1 on white).
#[inline]
fn healthy_color(dark: bool) -> Hsla {
    rgb(if dark { 0x69b083 } else { 0x1a7f37 }).into()
}
/// Formats the latency string and determines the color based on the delay.
#[inline]
fn format_latency(latency: Option<Duration>, cx: &Context<ZedisStatusBar>) -> (SharedString, Hsla) {
    let Some(latency) = latency else {
        return ("--".into(), cx.theme().primary);
    };
    let ms = latency.as_millis();
    let theme = cx.theme();
    let color = match ms {
        // Healthy latency uses the same green as the "Connected" label.
        0..50 => healthy_color(theme.is_dark()),
        50..500 => theme.yellow,
        _ => theme.red,
    };
    let label = if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", ms as f64 / 1000.0)
    };
    (label.into(), color)
}

/// Formats the node count only (`masters / replicas`). Redis version lives
/// in the nodes tooltip so the bar stays short.
#[inline]
fn format_nodes(nodes: (usize, usize)) -> SharedString {
    format!("{} / {}", nodes.0, nodes.1).into()
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

/// Build the multi-line tooltip body. `replicas` is the dynamic per-replica
/// state from the most recent `INFO replication` heartbeat — used to splice
/// `lag X / Ys` onto the matching topology line. Falls back gracefully when
/// the lag map has nothing for a given replica address (e.g. a fail node).
///
/// `version` is shown here (not in the bar label) so the compact
/// `masters / replicas` chip stays short.
#[inline]
fn format_nodes_description(
    description: Arc<RedisClientDescription>,
    replicas: &[ReplicaInfo],
    version: &str,
    cx: &Context<ZedisStatusBar>,
) -> SharedString {
    let t = i18n_sidebar(cx, "server_type");
    let master_nodes = i18n_sidebar(cx, "master_nodes");
    let slave_nodes = i18n_sidebar(cx, "slave_nodes");
    let modules_label = i18n_sidebar(cx, "modules");
    let topology_label = i18n_sidebar(cx, "topology");
    let mut messages = Vec::with_capacity(6);

    // One product line: `Valkey 9.0.3` / `Redis 8.6.1`. The version was
    // read from `valkey_version` when the server is Valkey, so the old
    // "Valkey: Yes" + "Redis version: 9.0.3" pair labeled a Valkey version
    // as Redis; the product name needs no translation and says both facts
    // at once.
    if !version.is_empty() {
        let flavor = if description.is_valkey { "Valkey" } else { "Redis" };
        messages.push(format!("{flavor} {version}"));
    } else if description.is_valkey {
        messages.push("Valkey".to_string());
    }
    messages.push(format!("{t}: {}", description.server_type.as_str()));
    if description.topology.is_empty() {
        // Fallback for standalone or any client without grouped topology data.
        messages.push(format!("{master_nodes}: {}", description.master_nodes));
        if !replicas.is_empty() {
            // Plain (non-Sentinel) master-replica: node discovery only lists
            // the configured node, but the heartbeat's `INFO replication`
            // still reports the attached replicas — render them here with
            // the same lag/state detail the topology branch shows, instead
            // of silently dropping them.
            let mut lines: Vec<String> = vec![format!("{slave_nodes}:")];
            for lag in replicas {
                let mut line = format!(
                    "  ↳ {} replica  lag {} / {}s",
                    lag.addr,
                    format_lag_bytes(lag.lag_bytes),
                    lag.lag_seconds
                );
                // Surface non-steady states (wait_bgsave / send_bulk / etc.).
                if !lag.state.is_empty() && lag.state.as_ref() != "online" {
                    line.push_str("  ");
                    line.push_str(lag.state.as_ref());
                }
                lines.push(line);
            }
            messages.push(lines.join("\n"));
        } else if !description.slave_nodes.is_empty() {
            messages.push(format!("{slave_nodes}: {}", description.slave_nodes));
        }
    } else {
        let mut lines: Vec<String> = vec![format!("{topology_label}:")];
        for tm in description.topology.iter() {
            let mut header = format!("{} {} master", tm.master.role_marker, tm.master.addr);
            if !tm.master.annotation.is_empty() {
                header.push_str("  ");
                header.push_str(tm.master.annotation.as_ref());
            }
            lines.push(header);
            for r in &tm.replicas {
                let mut line = format!("  {} {} replica", r.role_marker, r.addr);
                if let Some(lag) = replicas.iter().find(|i| i.addr == r.addr) {
                    line.push_str(&format!(
                        "  lag {} / {}s",
                        format_lag_bytes(lag.lag_bytes),
                        lag.lag_seconds
                    ));
                    // Surface non-steady states (wait_bgsave / send_bulk / etc.).
                    // "online" is the silent default and would just be noise.
                    if !lag.state.is_empty() && lag.state.as_ref() != "online" {
                        line.push_str("  ");
                        line.push_str(lag.state.as_ref());
                    }
                }
                lines.push(line);
            }
        }
        messages.push(lines.join("\n"));
    }
    if !description.modules.is_empty() {
        messages.push(format!("{modules_label}: {}", description.modules));
    }
    messages.join("\n").into()
}

// --- Local State ---

/// Everything that decides which Tools-menu entries are enabled: the
/// module / version / server-type gates, the access mode (Import Keys and
/// FLUSH need writes), and the probed command matrix.
struct ToolsMenuGates {
    supports_search: bool,
    supports_acl: bool,
    supports_functions: bool,
    supports_topology: bool,
    readonly: bool,
    features: Arc<ServerFeatures>,
}

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

/// One Tools-menu entry, built when the menu opens and moved into the
/// submenu that draws it.
struct ToolEntry {
    label: SharedString,
    icon: Icon,
    action: ServerToolsAction,
    disabled: bool,
}

/// Inputs for a clickable status-bar metric chip (icon + value → tool page).
struct MetricChip {
    id: &'static str,
    icon: CustomIconName,
    label: SharedString,
    label_color: Hsla,
    icon_color: Hsla,
    tooltip: SharedString,
    view: ServerView,
    /// Set when the destination panel can't work on this server: the chip is
    /// disabled and this replaces the tooltip.
    disabled_reason: Option<SharedString>,
    /// What the number counts — "clients", "slow" — and the colour to say
    /// it in, when the bar has room for it ([`ZedisStatusBar::shows_units`]).
    unit: Option<(SharedString, Hsla)>,
}

/// The content width from which the bar names its counts ("0 / 2 clients"
/// rather than "0 / 2" beside an icon), and the wider one it needs while the
/// editor's own controls (Soft Wrap, the format, the Viewer) share the bar.
const UNITS_MIN_WIDTH: f32 = 1020.;
const UNITS_MIN_WIDTH_WITH_EDITOR: f32 = 1260.;

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
                ServerEvent::KeyScanFinished => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
                    this.state.server_state.scan_finished = true;
                }
                ServerEvent::KeyScanPaged => {
                    let state = server_state.read(cx);
                    this.state.server_state.size = format_size(state.dbsize(), state.scan_count());
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
                    if *task == ServerTask::RefreshRedisInfo {
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
                SearchableVec::new(vec![
                    ViewMode::Auto.as_str().into(),
                    ViewMode::Plain.as_str().into(),
                    ViewMode::Hex.as_str().into(),
                ]),
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
            format!(
                "{} / {}",
                group_thousands(redis_info.metrics.blocked_clients),
                group_thousands(redis_info.metrics.connected_clients)
            )
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
            size: format_size(state.dbsize(), state.scan_count()),
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
    /// Build the "Tools" dropdown: four submenus by purpose — observability,
    /// query & scripting, keys & data, administration — then Topology and the
    /// capability matrix, so the top level stays eight rows however many
    /// panels the server can open. Items dispatch [`ServerToolsAction`],
    /// handled centrally on the `Zedis` root; a submenu inherits the
    /// dropdown's action context, so its items reach the same handlers.
    ///
    /// What the server does not have — a module that is not loaded, a
    /// version below the feature's floor, a command it answers `unknown
    /// command` to — is left out rather than greyed: it is nothing this user
    /// can change, the ⌘K palette leaves it out the same way, and Server
    /// capabilities lists every command with its verdict. What can change
    /// stays in the menu, disabled with the reason: a command this ACL user
    /// is denied, and a write on a read-only connection. A submenu with
    /// nothing left in it is left out too.
    fn render_tools_menu(
        this: PopupMenu,
        gates: &ToolsMenuGates,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let ToolsMenuGates {
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
            readonly,
            features,
        } = gates;
        let features: &ServerFeatures = features;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let tool = |label: SharedString,
                    icon: Icon,
                    action: ServerToolsAction,
                    required: &[ServerCommand],
                    writes: bool|
         -> Option<ToolEntry> {
            let statuses: Vec<(ServerCommand, CommandStatus)> = required
                .iter()
                .map(|command| (*command, features.status(*command)))
                .filter(|(_, status)| !status.is_usable())
                .collect();
            if statuses.iter().any(|(_, status)| *status == CommandStatus::Missing) {
                return None;
            }
            // Read-only wins the wording: it is the reason the user can undo.
            let reason = if writes && *readonly {
                Some(i18n_common(cx, "disable_in_readonly"))
            } else {
                statuses.first().map(|(command, _)| {
                    t!("features.menu_suffix", command = command.label(), locale = &locale)
                        .to_string()
                        .into()
                })
            };
            Some(ToolEntry {
                label: match &reason {
                    Some(reason) => format!("{label}  ·  {reason}").into(),
                    None => label,
                },
                icon,
                action,
                disabled: reason.is_some(),
            })
        };

        let observability: Vec<ToolEntry> = [
            tool(
                i18n_monitor(cx, "title"),
                Icon::new(CustomIconName::Radar),
                ServerToolsAction::Monitor,
                ServerView::Monitor.required_commands(),
                false,
            ),
            tool(
                i18n_server_load(cx, "title"),
                Icon::new(CustomIconName::Zap),
                ServerToolsAction::ServerLoad,
                ServerView::ServerLoad.required_commands(),
                false,
            ),
            // HOTKEYS tracking — Redis 8.6; absent from the menu elsewhere.
            tool(
                i18n_hotkeys(cx, "title"),
                Icon::new(CustomIconName::Flame),
                ServerToolsAction::Hotkeys,
                ServerView::Hotkeys.required_commands(),
                false,
            ),
            // `notify-keyspace-events` may be empty: the panel offers a
            // one-click Enable. Its hard dependency is SUBSCRIBE.
            tool(
                i18n_keyspace_notifications(cx, "title"),
                Icon::new(CustomIconName::AudioWaveform),
                ServerToolsAction::KeyspaceNotifications,
                ServerView::KeyspaceNotifications.required_commands(),
                false,
            ),
            // RedisTimeSeries only.
            tool(
                i18n_timeseries(cx, "explorer_title"),
                Icon::new(CustomIconName::Activity),
                ServerToolsAction::TimeSeriesExplorer,
                ServerView::TimeSeriesExplorer.required_commands(),
                false,
            ),
            // Pub/Sub is the editor suite's channel mode, mirrored here next
            // to its observability siblings.
            tool(
                i18n_key_tree(cx, "pubsub_mode"),
                Icon::new(CustomIconName::Rss),
                ServerToolsAction::PubsubMode,
                &[],
                false,
            ),
            tool(
                i18n_server_info(cx, "title"),
                Icon::new(IconName::Info),
                ServerToolsAction::ServerInfo,
                ServerView::ServerInfo.required_commands(),
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();

        let scripting: Vec<ToolEntry> = [
            tool(
                i18n_value_search(cx, "title"),
                Icon::new(IconName::Search),
                ServerToolsAction::ValueSearch,
                ServerView::ValueSearch.required_commands(),
                false,
            ),
            // RediSearch / valkey-search, when the module is loaded.
            supports_search
                .then(|| {
                    tool(
                        i18n_search(cx, "title"),
                        Icon::new(IconName::Search),
                        ServerToolsAction::Search,
                        ServerView::Search.required_commands(),
                        false,
                    )
                })
                .flatten(),
            // `FUNCTION`, Redis 7.0+.
            supports_functions
                .then(|| {
                    tool(
                        i18n_functions(cx, "title"),
                        Icon::new(IconName::Asterisk),
                        ServerToolsAction::Functions,
                        ServerView::Functions.required_commands(),
                        false,
                    )
                })
                .flatten(),
            // The saved scripts are local, so the library opens everywhere;
            // running one needs EVAL and degrades inside.
            tool(
                i18n_lua_scripts(cx, "title"),
                Icon::new(IconName::SquareTerminal),
                ServerToolsAction::LuaScripts,
                &[],
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();

        let mut data: Vec<ToolEntry> = Vec::new();
        // The local recycle bin — a dialog, and not in the browser, which
        // keeps no bin (`ZedisAppState::soft_delete`).
        #[cfg(not(target_family = "wasm"))]
        data.extend(tool(
            i18n_trash(cx, "menu"),
            Icon::new(CustomIconName::FileXCorner),
            ServerToolsAction::Trash,
            &[],
            false,
        ));
        data.extend(
            [
                // RESTORE: disabled, not hidden, on a read-only connection so
                // the user knows where it lives.
                tool(
                    i18n_status_bar(cx, "import_keys_menu"),
                    Icon::new(CustomIconName::Upload),
                    ServerToolsAction::ImportKeys,
                    &[],
                    true,
                ),
                tool(
                    i18n_status_bar(cx, "export_keys_menu"),
                    Icon::new(CustomIconName::Download),
                    ServerToolsAction::ExportKeys,
                    &[],
                    false,
                ),
                tool(
                    i18n_status_bar(cx, "compare_keys_menu"),
                    Icon::new(CustomIconName::GitCompareArrows),
                    ServerToolsAction::CompareKeys,
                    &[],
                    false,
                ),
            ]
            .into_iter()
            .flatten(),
        );

        let mut admin: Vec<ToolEntry> = [
            // CONFIG GET is the one managed clouds most often take away.
            tool(
                i18n_config_editor(cx, "title"),
                Icon::new(IconName::Settings),
                ServerToolsAction::Config,
                ServerView::Config.required_commands(),
                false,
            ),
            // ACL, Redis 6.0+.
            supports_acl
                .then(|| {
                    tool(
                        i18n_acl(cx, "title"),
                        Icon::new(IconName::CircleUser),
                        ServerToolsAction::Acl,
                        ServerView::Acl.required_commands(),
                        false,
                    )
                })
                .flatten(),
            tool(
                i18n_persistence(cx, "title"),
                Icon::new(CustomIconName::HardDrive),
                ServerToolsAction::Persistence,
                ServerView::Persistence.required_commands(),
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        // FLUSHDB / FLUSHALL go through the same destructive-command confirm
        // a typed one hits in the terminal: the menu adds an entry point, not
        // a second policy. Kept apart from the panels below a divider.
        let flush: Vec<ToolEntry> = [
            tool(
                i18n_status_bar(cx, "flush_db_menu"),
                Icon::new(CustomIconName::Eraser),
                ServerToolsAction::FlushDb,
                &[ServerCommand::FlushDb],
                true,
            ),
            tool(
                i18n_status_bar(cx, "flush_all_menu"),
                Icon::new(CustomIconName::DatabaseZap),
                ServerToolsAction::FlushAll,
                &[ServerCommand::FlushAll],
                true,
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let flush_after = (!admin.is_empty() && !flush.is_empty()).then_some(admin.len());
        admin.extend(flush);

        let groups = [
            (
                Icon::new(IconName::ChartPie),
                i18n_status_bar(cx, "group_observability"),
                observability,
                None,
            ),
            (
                Icon::new(CustomIconName::Braces),
                i18n_status_bar(cx, "group_scripting"),
                scripting,
                None,
            ),
            (
                Icon::new(CustomIconName::Database),
                i18n_status_bar(cx, "group_data"),
                data,
                None,
            ),
            (
                Icon::new(IconName::Settings),
                i18n_status_bar(cx, "group_admin"),
                admin,
                flush_after,
            ),
        ];
        let topology_label = i18n_topology(cx, "title");
        // The probed command matrix: a count suffix here rather than a badge
        // in the bar.
        let unusable = features.unusable().len();
        let capabilities_label: SharedString = if unusable == 0 {
            format!("{}…", i18n_features(cx, "dialog_title")).into()
        } else {
            format!(
                "{}…  ·  {}",
                i18n_features(cx, "dialog_title"),
                t!("status_bar.capabilities_limited", count = unusable, locale = &locale)
            )
            .into()
        };

        let mut menu = this;
        for (icon, label, entries, separator_at) in groups {
            if entries.is_empty() {
                continue;
            }
            menu = menu.submenu_with_icon(Some(icon), label, window, cx, move |mut submenu, _, _| {
                for (index, entry) in entries.iter().enumerate() {
                    if separator_at == Some(index) {
                        submenu = submenu.separator();
                    }
                    submenu = submenu.menu_with_icon_and_disabled(
                        entry.label.clone(),
                        entry.icon.clone(),
                        Box::new(entry.action),
                        entry.disabled,
                    );
                }
                submenu
            });
        }
        // The page adapts to Cluster / Sentinel / Standalone; absent only
        // while the server type is unknown.
        if *supports_topology {
            menu = menu.menu_with_icon(
                topology_label,
                Icon::new(CustomIconName::Network),
                Box::new(ServerToolsAction::Topology),
            );
        }
        menu.menu_with_icon(
            capabilities_label,
            Icon::new(CustomIconName::ListCheck),
            Box::new(ServerToolsAction::Capabilities),
        )
    }
    /// Render the server status
    /// DB-dropdown menu width, hugging the current content: the default
    /// (`Length::Auto`) follows the narrow "DB N ▾" trigger and clips the
    /// key counts, while any fixed width leaves blank space when counts
    /// are small or absent. Sized from the widest count in the current
    /// snapshot instead.
    fn db_menu_width(&self) -> Pixels {
        // px_2 row padding (16) + label column + gap to the check icon (4)
        // + xsmall check icon (~14) + rounding slack (4).
        let mut width = 16. + DB_LABEL_COL_W + 4. + 14. + 4.;
        let max_keys = self.db_key_counts.iter().copied().max().unwrap_or(0);
        if max_keys > 0 {
            // gap_3 (12) + the widest grouped count at the mono advance.
            width += 12. + group_thousands(max_keys).len() as f32 * DB_MENU_CHAR_W;
        }
        px(width)
    }

    fn render_server_status(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nodes_unit = self
            .shows_units(window, cx)
            .then(|| (i18n_status_bar(cx, "unit_nodes"), cx.theme().muted_foreground));
        let server_state = &self.state.server_state;
        let is_completed = server_state.scan_finished;
        // Read live, not from the snapshot: a scan round starts on this
        // button's own press, which is no server event.
        let scanning = self.server_state.read(cx).scanning();
        let nodes_description = server_state.nodes_description.clone();
        let terminal_tooltip = with_hot_key("terminal", &i18n_status_bar(cx, "toggle_terminal_tooltip"));
        let readonly_tooltip = i18n_status_bar(cx, "toggle_readonly_tooltip");
        let tag_text = server_state.tag.clone();
        let tag_chip = resolve_tag_chip(server_state.tag_color_key.as_deref(), cx.theme().is_dark());
        let is_replica = server_state.is_replica;
        let supports_search = server_state.supports_search;
        let supports_acl = server_state.supports_acl;
        let supports_functions = server_state.supports_functions;
        let supports_topology = server_state.supports_topology;
        let readonly = self.readonly;
        let features = server_state.features.clone();
        let gates = ToolsMenuGates {
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
            readonly,
            features,
        };
        let status_text = status_text_color(cx.theme().is_dark());
        ZedisDivider::new()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .when(!tag_text.is_empty() && tag_chip.is_some(), |this| {
                        let (bg, fg) = tag_chip.unwrap_or_else(|| (gpui::black(), gpui::white()));
                        this.child(
                            div()
                                .px_1p5()
                                .rounded_sm()
                                .bg(bg)
                                .child(Label::new(tag_text).text_xs().text_color(fg)),
                        )
                    })
                    // Quiet outline badge when the connection points at a
                    // replica (`role:slave`) — without it the only signal is a
                    // bare `-READONLY` error on the first write.
                    .when(is_replica, |this| {
                        let replica_tooltip = i18n_status_bar(cx, "replica_badge_tooltip");
                        this.child(
                            div()
                                .id("zedis-status-bar-replica-badge")
                                .px_1p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().warning)
                                .tooltip(move |window, cx| Tooltip::new(replica_tooltip.clone()).build(window, cx))
                                .child(
                                    Label::new(i18n_status_bar(cx, "replica_badge"))
                                        .text_xs()
                                        .text_color(cx.theme().warning),
                                ),
                        )
                    })
                    .child(
                        Button::new("zedis-status-bar-server-terminal")
                            .ghost()
                            .small()
                            .tooltip(terminal_tooltip)
                            .icon(Icon::new(IconName::SquareTerminal).text_color(status_text))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.toggle_terminal(cx);
                                });
                            })),
                    )
                    .when(self.databases > 1, |this| {
                        // `appearance(false)`: drop the bordered input chrome so it
                        // reads as plain "DB N ▾" text that inherits the muted
                        // status-bar color (matches the design), not a bright box.
                        // The tooltip names what the per-row numbers are — the
                        // dropdown shows bare counts with no unit, and on a
                        // multi-master setup they are sums across nodes.
                        let db_tooltip = i18n_status_bar(cx, "db_tooltip");
                        let menu_open = self.db_menu_open;
                        this.child(
                            div()
                                .id("zedis-status-bar-db-select")
                                .when(!menu_open, |this| {
                                    this.tooltip(move |window, cx| Tooltip::new(db_tooltip.clone()).build(window, cx))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _window, cx| {
                                        this.db_menu_open = true;
                                        cx.notify();
                                    }),
                                )
                                .child(
                                    Select::new(&self.db_state)
                                        .small()
                                        .appearance(false)
                                        .menu_width(self.db_menu_width()),
                                ),
                        )
                    })
                    // Clients paused — by this app (remembered), or by anyone
                    // where the server says so (Valkey 8.1+): the state in
                    // which every write hangs and nothing else on screen
                    // explains why. The click lifts it.
                    .when_some(self.server_state.read(cx).client_pause(), |this, pause| {
                        let seconds = pause.remaining.as_secs().to_string();
                        let label = i18n_status_bar(cx, "paused_chip")
                            .replace("%{actions}", &pause.actions)
                            .replace("%{seconds}", &seconds);
                        let tooltip: SharedString = if pause.from_server {
                            i18n_status_bar(cx, "paused_tooltip_server")
                                .replace("%{actions}", &pause.actions)
                                .replace("%{reason}", pause.reason.as_deref().unwrap_or("-"))
                                .replace("%{seconds}", &seconds)
                                .into()
                        } else {
                            i18n_status_bar(cx, "paused_tooltip_local")
                                .replace("%{actions}", &pause.actions)
                                .replace("%{seconds}", &seconds)
                                .into()
                        };
                        this.child(
                            Button::new("zedis-status-bar-client-pause")
                                .warning()
                                .small()
                                .compact()
                                .icon(Icon::new(CustomIconName::Clock3))
                                .label(label)
                                .tooltip(tooltip)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.server_state.update(cx, |state, cx| state.unpause_clients(cx));
                                })),
                        )
                    })
                    .child({
                        // A locked entry's lock is a window, not a switch (ADR
                        // 14): closed, the button asks the lock's question and
                        // opens it for a while; open, it shows what is left and
                        // closes it. Any other entry keeps the plain toggle.
                        let (windowed, remaining) = {
                            let state = self.server_state.read(cx);
                            // Not for an account or ACL user that may not
                            // write at all: there is no window to open.
                            (
                                state.write_locked() && !state.strict_readonly(),
                                state.unlock_remaining(),
                            )
                        };
                        let minutes_left = remaining.map(|left| left.as_secs().div_ceil(60));
                        let tooltip: SharedString = match (windowed, minutes_left) {
                            (true, Some(minutes)) => i18n_status_bar(cx, "unlocked_tooltip")
                                .replace("%{minutes}", &minutes.to_string())
                                .into(),
                            (true, None) => i18n_status_bar(cx, "write_locked_tooltip")
                                .replace("%{minutes}", &(WRITE_UNLOCK_SECS / 60).to_string())
                                .into(),
                            (false, _) => readonly_tooltip,
                        };
                        Button::new("zedis-status-bar-server-toggle-readonly")
                            .ghost()
                            .small()
                            .tooltip(tooltip)
                            .when(self.readonly, |this| {
                                this.icon(Icon::new(CustomIconName::Lock).text_color(status_text))
                            })
                            .when(!self.readonly, |this| {
                                this.icon(Icon::new(CustomIconName::LockOpen).text_color(status_text))
                            })
                            .when_some(minutes_left.filter(|_| windowed), |this, minutes| {
                                this.label(
                                    i18n_status_bar(cx, "unlocked_chip").replace("%{minutes}", &minutes.to_string()),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if !windowed {
                                    this.server_state.update(cx, |state, cx| state.toggle_readonly(cx));
                                    return;
                                }
                                if minutes_left.is_some() {
                                    this.server_state.update(cx, |state, cx| state.lock_writes(cx));
                                    return;
                                }
                                let Ok(server) = get_server(this.server_state.read(cx).server_id()) else {
                                    return;
                                };
                                let server_state = this.server_state.clone();
                                confirm_dangerous_command(
                                    &server,
                                    server_state.read(cx).db(),
                                    &DangerKind::WriteLocked,
                                    None,
                                    window,
                                    cx,
                                    move |_, cx| {
                                        server_state.update(cx, |state, cx| state.unlock_writes(cx));
                                    },
                                );
                            }))
                    })
                    .child(
                        Button::new("zedis-status-bar-tools")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Menu).text_color(status_text))
                            .tooltip(i18n_status_bar(cx, "tools_tooltip"))
                            // Status bar sits at the bottom, so open the menu
                            // upward (its bottom edge anchored to the button).
                            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |this, window, cx| {
                                Self::render_tools_menu(this, &gates, window, cx)
                            }),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("zedis-status-bar-scan-more")
                            .ghost()
                            .small()
                            .disabled(is_completed)
                            .loading(scanning && !is_completed)
                            .tooltip(if is_completed {
                                i18n_status_bar(cx, "scan_completed")
                            } else {
                                i18n_status_bar(cx, "scan_more_keys")
                            })
                            .mr_1()
                            .icon(Icon::new(CustomIconName::ChevronsDown).text_color(status_text))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.scan_next(cx);
                                });
                                cx.notify();
                            })),
                    )
                    .child(Label::new(server_state.size.clone()).text_color(status_text).mr_2())
                    .child(
                        div()
                            .child(
                                h_flex()
                                    .items_center()
                                    .child(Icon::new(CustomIconName::Network).text_color(status_text).mr_1())
                                    .child(Label::new(server_state.nodes.clone()).text_color(status_text))
                                    .when_some(nodes_unit, |this, (unit, color)| {
                                        this.child(Label::new(unit).text_xs().text_color(color).ml_1())
                                    }),
                            )
                            .id("zedis-servers")
                            .tooltip(move |window, cx| Tooltip::new(nodes_description.clone()).build(window, cx)),
                    ),
            )
    }
    /// Render the right-hand telemetry cluster: a "Connected" health indicator
    /// (with one-click reconnect when the link is down) followed by the live
    /// metric chips — latency / memory / clients / slow log.
    fn render_telemetry(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let shows_units = self.shows_units(window, cx);
        let unit_color = cx.theme().muted_foreground;
        let unit = |key: &str| shows_units.then(|| (i18n_status_bar(cx, key), unit_color));
        // Health icon + label. A chain link (connected) vs a broken one (link
        // down) carries the state in the *shape*, not just the color — a bare
        // colored dot was too easy to miss. Colors mirror the latency palette so
        // the cluster reads as one health unit.
        //
        // `Unknown` (still connecting) keeps the intact link at the muted color:
        // nothing is broken yet, so a red-flavoured "unlink" would over-alarm.
        let (health_color, health_icon, health_label) = match server_state.health {
            ConnectionHealth::Connected => (
                healthy_color(cx.theme().is_dark()),
                CustomIconName::Link,
                i18n_status_bar(cx, "conn_connected"),
            ),
            ConnectionHealth::Reconnecting => (
                cx.theme().yellow,
                CustomIconName::Unlink,
                i18n_status_bar(cx, "conn_reconnecting"),
            ),
            ConnectionHealth::Offline => (
                cx.theme().red,
                CustomIconName::Unlink,
                i18n_status_bar(cx, "conn_offline"),
            ),
            ConnectionHealth::Unknown => (
                cx.theme().muted_foreground,
                CustomIconName::Link,
                i18n_status_bar(cx, "conn_connecting"),
            ),
        };
        // When the link is down the icon doubles as a one-click reconnect
        // affordance. The heartbeat alone leaves `server_status` Idle, so a
        // plain re-select would no-op — `reconnect()` forces the reload.
        let is_link_down = matches!(
            server_state.health,
            ConnectionHealth::Offline | ConnectionHealth::Reconnecting
        );
        // Connected → the icon doubles as a one-click *disconnect*; when down it's
        // a *reconnect*. `Unknown` (still connecting) stays inert.
        let is_connected = server_state.health == ConnectionHealth::Connected;
        let icon_clickable = is_link_down || is_connected;
        let health_tooltip = if is_link_down {
            let hint = i18n_status_bar(cx, "conn_reconnect_hint");
            let reason = i18n_status_bar(cx, server_state.last_connection_error.reason_key());
            format!("{reason} · {hint}").into()
        } else if is_connected {
            format!("{} · {}", health_label, i18n_status_bar(cx, "conn_disconnect_hint")).into()
        } else {
            health_label.clone()
        };
        // When the link is down the cached latency is stale and misleading
        // (a green "5ms" beside a red dot), so blank it to a muted "--".
        // Chips whose panel can't work on this server are disabled with the
        // reason as tooltip (computed up front: the closure borrows `cx`).
        let features = server_state.features.clone();
        let block = |view: ServerView| -> Option<SharedString> {
            features
                .first_unusable(view.required_commands())
                .map(|(command, status)| command_status_label(cx, command, status))
        };
        // The memory and clients chips show numbers that are `INFO`'s. For
        // a user who may not run it they read "--", and the tooltip says why
        // where it would have named the number.
        let info_reason = features
            .first_unusable(&[ServerCommand::Info])
            .map(|(command, status)| command_status_label(cx, command, status));
        let stat_hint = |key: &str| info_reason.clone().unwrap_or_else(|| i18n_status_bar(cx, key));
        let chip_blocks = [
            (ServerView::Metrics, block(ServerView::Metrics)),
            (ServerView::MemoryAnalysis, block(ServerView::MemoryAnalysis)),
            (ServerView::Clients, block(ServerView::Clients)),
            (ServerView::Slowlog, block(ServerView::Slowlog)),
        ];
        let chip_block = |view: ServerView| -> Option<SharedString> {
            chip_blocks
                .iter()
                .find(|(v, _)| *v == view)
                .and_then(|(_, reason)| reason.clone())
        };
        let (latency_text, latency_color) = if server_state.health == ConnectionHealth::Offline {
            (SharedString::from("--"), cx.theme().muted_foreground)
        } else {
            server_state.latency.clone()
        };
        // Left-aligned, fixed mono width: the padding trails the value so the
        // button width stays constant (no resize jogging the Connected chip each
        // beat) while the number sits at the left edge of its slot.
        let latency_text: SharedString =
            format!("{:<width$}", latency_text.as_ref(), width = LATENCY_LABEL_CHARS).into();
        let status_text = status_text_color(cx.theme().is_dark());
        ZedisDivider::new()
            .child(
                // One native ghost button covering icon + label, so the whole
                // "Connected" chip is the click target (disconnect when up /
                // reconnect when down) with the same hover background as the
                // metric chips and the left-side buttons — the label used to sit
                // outside the button and wasn't clickable. The icon keeps its own
                // health color (`.text_color` colors only the label); inert while
                // still connecting.
                Button::new("zedis-status-bar-conn-health")
                    .ghost()
                    .small()
                    // Dense status-bar row: small's default px_3 leaves too much
                    // air between adjacent chips; compact → px_1p5.
                    .compact()
                    .disabled(!icon_clickable)
                    .icon(Icon::new(health_icon).mr_1().text_color(health_color))
                    .label(health_label)
                    .text_color(status_text)
                    .tooltip(health_tooltip)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.server_state.update(cx, |state, cx| {
                            if is_connected {
                                state.disconnect(cx);
                            } else {
                                state.reconnect(cx);
                            }
                        });
                    })),
            )
            // The link is down because a script has the server: reconnecting
            // cannot help, the kill can — offered right where the reason is
            // shown, on a connection a BUSY server still answers.
            .when(
                cfg!(not(target_family = "wasm"))
                    && is_link_down
                    && server_state.last_connection_error == ConnectionErrorKind::Busy,
                |this| {
                    this.child(
                        Button::new("zedis-status-bar-kill-script")
                            .danger()
                            .small()
                            .compact()
                            .label(i18n_common(cx, "kill_script_button"))
                            .tooltip(i18n_common(cx, "kill_script_tooltip"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state
                                    .update(cx, |state, cx| state.kill_running_script(KillTarget::Script, cx));
                            })),
                    )
                },
            )
            .child(
                h_flex()
                    .items_center()
                    // Chip internal padding already separates them; gap_1 is
                    // enough once buttons are compact (gap_2 looked sparse).
                    .gap_1()
                    // Whole chip is clickable (icon + value), with a tooltip that
                    // names both the metric and the destination page.
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Metrics),
                        id: "zedis-status-bar-server-metrics",
                        unit: None,
                        icon: CustomIconName::Activity,
                        label: latency_text,
                        label_color: latency_color,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            i18n_status_bar(cx, "metric_latency_hint"),
                            i18n_status_bar(cx, "toggle_metrics_tooltip")
                        )),
                        view: ServerView::Metrics,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::MemoryAnalysis),
                        id: "zedis-status-bar-server-memory-analysis",
                        unit: None,
                        icon: CustomIconName::MemoryStick,
                        label: server_state.used_memory.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            stat_hint("metric_memory_hint"),
                            i18n_status_bar(cx, "toggle_memory_analysis_tooltip")
                        )),
                        view: ServerView::MemoryAnalysis,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Clients),
                        id: "zedis-status-bar-clients",
                        unit: unit("unit_clients"),
                        icon: CustomIconName::AudioWaveform,
                        label: server_state.clients.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            stat_hint("clients_stat_tooltip"),
                            i18n_status_bar(cx, "toggle_clients_tooltip")
                        )),
                        view: ServerView::Clients,
                    }))
                    .child(Self::metric_chip(MetricChip {
                        disabled_reason: chip_block(ServerView::Slowlog),
                        id: "zedis-status-bar-server-slow-logs",
                        unit: unit("unit_slow"),
                        icon: CustomIconName::Snail,
                        label: server_state.slow_log_tips.clone(),
                        label_color: status_text,
                        icon_color: status_text,
                        tooltip: SharedString::from(format!(
                            "{} · {}",
                            i18n_status_bar(cx, "slowlog_stat_tooltip"),
                            i18n_status_bar(cx, "toggle_slowlog_tooltip")
                        )),
                        view: ServerView::Slowlog,
                    })),
            )
    }

    /// Icon + value chip that opens a tool page on click. A native ghost
    /// `Button` (not a hand-rolled div) so the whole chip — icon *and* value —
    /// is the hit target and gets the same hover-background treatment as the
    /// terminal / readonly / tools buttons on the left, rather than only
    /// flipping the cursor. The icon keeps its own color (muted, or the
    /// latency palette); `.text_color` colors just the value label.
    fn metric_chip(chip: MetricChip) -> impl IntoElement {
        let MetricChip {
            id,
            icon,
            label,
            label_color,
            icon_color,
            tooltip,
            view,
            disabled_reason,
            unit,
        } = chip;
        // A disabled `Button` still shows its tooltip, so the reason the
        // panel is unavailable replaces the destination hint.
        let disabled = disabled_reason.is_some();
        let tooltip = disabled_reason.unwrap_or(tooltip);
        Button::new(id)
            .ghost()
            .small()
            // Status-bar metric chips sit edge-to-edge; compact halves the
            // horizontal padding (px_3 → px_1p5) so the cluster reads dense.
            .compact()
            .icon(Icon::new(icon).mr_1().text_color(icon_color))
            .label(label)
            .text_color(label_color)
            .when_some(unit, |this, (unit, color)| {
                this.child(Label::new(unit).text_xs().text_color(color).ml_1())
            })
            .disabled(disabled)
            .tooltip(tooltip)
            .on_click(move |_, _window, cx| {
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.toggle_view(view, cx);
                });
            })
    }

    /// Whether the bar is wide enough to say what its counts are. Three
    /// bare "n / m" pairs beside three icons were only readable to someone
    /// who already knew them; the words go away again where they would push
    /// the rest of the bar out of the window.
    fn shows_units(&self, window: &Window, cx: &App) -> bool {
        let needed = if self.show_editor_controls(cx) {
            UNITS_MIN_WIDTH_WITH_EDITOR
        } else {
            UNITS_MIN_WIDTH
        };
        content_area_width(window, cx).as_f32() >= needed
    }
    fn render_editor_settings(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let wrap_fits = self
            .server_state
            .read(cx)
            .value()
            .and_then(|value| value.bytes_value())
            .is_none_or(|bytes| bytes.soft_wrap_fits());
        // Custom variant (transparent bg, muted foreground) so the label + check
        // read in the same recessive status-bar color as everything else — a
        // plain `.ghost()` paints them at the brighter `secondary_foreground`.
        let status_text = status_text_color(cx.theme().is_dark());
        Button::new("soft-wrap")
            .ghost()
            .text_color(status_text)
            .xsmall()
            // A value too large to wrap is shown unwrapped whatever the
            // preference: the button says so instead of showing a tick for
            // something that is not happening.
            .when(server_state.soft_wrap && wrap_fits, |this| this.icon(IconName::Check))
            .disabled(!wrap_fits)
            .tooltip(if wrap_fits {
                i18n_status_bar(cx, "soft_wrap_tooltip")
            } else {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                t!(
                    "status_bar.soft_wrap_capped_tooltip",
                    size = humansize::format_size(SOFT_WRAP_MAX_BYTES, humansize::DECIMAL),
                    locale = locale
                )
                .to_string()
                .into()
            })
            .label(i18n_status_bar(cx, "soft_wrap"))
            .on_click(cx.listener(|this, _, _window, cx| {
                let soft_wrap = !this.state.server_state.soft_wrap;
                this.state.server_state.soft_wrap = soft_wrap;
                this.server_state.update(cx, |state, cx| {
                    state.set_soft_wrap(soft_wrap, cx);
                });
                cx.notify();

                let server_id = this.state.server_state.server_id.clone();
                if let Ok(mut option) = get_session_option(server_id.as_str()) {
                    option.soft_wrap = Some(soft_wrap);
                    save_session_option(server_id.as_str(), option, cx);
                }
            }))
    }
    fn render_data_format(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data_format) = self.state.data_format.clone() else {
            return h_flex().into_any_element();
        };
        Button::new("data-format")
            .ghost()
            .disabled(true)
            .text_color(cx.theme().primary)
            .tooltip(i18n_status_bar(cx, "data_format_tooltip"))
            .icon(Icon::new(CustomIconName::Binary))
            .label(data_format)
            .into_any_element()
    }
    fn render_viewer_mode(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.data_format.is_none() {
            return h_flex();
        };
        let label = i18n_status_bar(cx, "viewer");
        h_flex()
            .child(Label::new(label).mr_1())
            .child(Select::new(&self.viewer_mode_state).appearance(false))
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

    /// Soft Wrap, the format label and the Viewer picker belong to the
    /// string/bytes value editor — hidden on tool pages and when no string
    /// value is selected. (The last two used to stay behind on Metrics or
    /// Config once a string had been opened, a control for a page that was
    /// not on screen.)
    fn show_editor_controls(&self, cx: &App) -> bool {
        if self.state.data_format.is_none() {
            return false;
        }
        matches!(
            cx.global::<ZedisGlobalStore>().read(cx).route(),
            crate::states::Route::Server {
                view: ServerView::Editor,
                ..
            }
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
                    .when(self.show_editor_controls(cx), |this| {
                        this.child(self.render_editor_settings(window, cx))
                    })
                    .when(self.show_editor_controls(cx), |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .child(self.render_data_format(window, cx))
                                .child(self.render_viewer_mode(window, cx)),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

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
}

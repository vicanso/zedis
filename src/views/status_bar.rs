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

use crate::{
    assets::CustomIconName,
    connection::{RedisClientDescription, get_server},
    constants::STATUS_BAR_HEIGHT,
    helpers::{UpdateAction, humanize_keystroke, resolve_tag_chip},
    states::{
        ConnectionErrorKind, ConnectionHealth, ErrorMessage, GlobalEvent, ReplicaInfo, Route, ServerEvent, ServerTask,
        ServerToolsAction, ViewMode, ZedisGlobalStore, ZedisServerState, get_session_option, i18n_server_load,
        i18n_sidebar, i18n_status_bar, i18n_topology, i18n_value_search, save_session_option,
    },
};
use gpui::{Anchor, Entity, Hsla, SharedString, Subscription, Task, TextAlign, Window, div, prelude::*, px};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu},
    tooltip::Tooltip,
};
use std::{sync::Arc, time::Duration};
use tracing::{debug, info};
use zedis_ui::ZedisDivider;

/// Formats the database size and scan count string "count/total".
#[inline]
fn format_size(dbsize: Option<u64>, scan_count: usize) -> SharedString {
    if let Some(dbsize) = dbsize {
        format!("{scan_count}/{dbsize}")
    } else {
        "--".to_string()
    }
    .into()
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
        0..50 => theme.green,
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

/// Formats the node count and version information.
#[inline]
fn format_nodes(nodes: (usize, usize), version: &str) -> SharedString {
    format!("{} / {} (v{})", nodes.0, nodes.1, version).into()
}

/// Compact human form for replication lag in bytes.
/// Drops the unit when zero so healthy replicas don't carry "0 B" noise.
fn format_lag_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0".into();
    }
    humansize::format_size(bytes as u64, humansize::FormatSizeOptions::default().decimal_places(1))
}

/// Build the multi-line tooltip body. `replicas` is the dynamic per-replica
/// state from the most recent `INFO replication` heartbeat — used to splice
/// `lag X / Ys` onto the matching topology line. Falls back gracefully when
/// the lag map has nothing for a given replica address (e.g. a fail node).
#[inline]
fn format_nodes_description(
    description: Arc<RedisClientDescription>,
    replicas: &[ReplicaInfo],
    cx: &Context<ZedisStatusBar>,
) -> SharedString {
    let t = i18n_sidebar(cx, "server_type");
    let master_nodes = i18n_sidebar(cx, "master_nodes");
    let slave_nodes = i18n_sidebar(cx, "slave_nodes");
    let modules_label = i18n_sidebar(cx, "modules");
    let topology_label = i18n_sidebar(cx, "topology");
    let mut messages = Vec::with_capacity(5);

    if description.is_valkey {
        messages.push(format!("Valkey: {}", i18n_sidebar(cx, "yes")));
    }
    messages.push(format!("{t}: {}", description.server_type.as_str()));
    if description.topology.is_empty() {
        // Fallback for standalone or any client without grouped topology data.
        messages.push(format!("{master_nodes}: {}", description.master_nodes));
        if !description.slave_nodes.is_empty() {
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
    /// only has content for Cluster / Sentinel, so it's hidden on Standalone.
    supports_topology: bool,
}

#[derive(Debug, Clone)]
struct DbInfo {
    label: SharedString,
    db: usize,
}

impl SelectItem for DbInfo {
    type Value = usize;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.db
    }
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
    should_reset_viewer_mode: Option<bool>,
    should_reset_db: Option<bool>,
    should_rebuild_db_items: Option<usize>,
    server_state: Entity<ZedisServerState>,
    heartbeat_task: Option<Task<()>>,
    databases: usize,
    readonly: bool,
    _subscriptions: Vec<Subscription>,
}
impl ZedisStatusBar {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize state from the current server state
        // Read only necessary fields to avoid cloning the entire state if it's large

        let mut subscriptions = vec![];
        // Re-render when an update becomes available / is cleared so the update
        // chip appears/disappears promptly. The state lives in the global store
        // (see `render_server_status`), so this just nudges a redraw.
        let global_state = cx.global::<ZedisGlobalStore>().state();
        subscriptions.push(cx.subscribe(&global_state, |_this, _store, event, cx| {
            if matches!(
                event,
                GlobalEvent::UpdateAvailable | GlobalEvent::UpdateDownloadProgress
            ) {
                cx.notify();
            }
        }));
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
                    // Background heartbeat (RefreshRedisInfo) fires once
                    // per 5s and never changes anything visible from
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
            should_rebuild_db_items: Some(databases),
            heartbeat_task: None,
            viewer_mode_state,
            db_state,
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
                redis_info.metrics.blocked_clients, redis_info.metrics.connected_clients
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

        let slow_log_tips = format!("{} / {}", state.last_slow_log_count(), state.slow_logs().len()).into();
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
            nodes: format_nodes(state.nodes(), state.version()),
            scan_finished: state.scan_completed(),
            slow_log_tips,
            soft_wrap: state.soft_wrap(),
            nodes_description: format_nodes_description(
                state.nodes_description().clone(),
                redis_info.replicas.as_slice(),
                cx,
            ),
            tag,
            tag_color_key,
            supports_search,
            supports_acl,
            supports_functions,
            supports_topology,
        };
    }
    /// Start the heartbeat task. 2-second cadence keeps the chips
    /// (latency, used memory, connected clients, replication lag)
    /// snappy — matches the Metrics panel heartbeat for consistency.
    /// The CPU baseline is dominated by other render paths anyway,
    /// so this interval doesn't move the needle either direction.
    fn start_heartbeat(&mut self, server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) {
        self.heartbeat_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                server_state.update(cx, |state, cx| {
                    state.refresh_redis_info(cx);
                });
            }
        }));
    }
    /// Build the "Tools" dropdown that gathers server-scoped navigation
    /// actions (Monitor / Config / ACL / Search). Items dispatch
    /// [`ServerToolsAction`] which is handled centrally in `main.rs`,
    /// so the dropdown does not need per-item `on_click` listeners.
    ///
    /// `supports_search` / `supports_acl` / `supports_functions` gate
    /// per-server-capability menu entries. Monitor and Config work on
    /// all Redis versions so they stay unconditional. Capability-gated
    /// entries stay *visible but disabled* with a why-suffix (e.g.
    /// "module not loaded" / "requires Redis ≥ 7.0") so the feature is
    /// discoverable and the user knows what to enable. `supports_topology`
    /// is the exception: a cluster topology isn't a feature you can turn
    /// on, so that group is still hidden on Standalone.
    fn render_tools_menu(
        this: PopupMenu,
        supports_search: bool,
        supports_acl: bool,
        supports_functions: bool,
        supports_topology: bool,
        cx: &gpui::App,
    ) -> PopupMenu {
        // The tool list has grown, so it's split into titled,
        // separator-delimited sections. Each group is anchored by an
        // always-available item (Monitor / Lua / Config+Persistence /
        // Topology), so the capability-gated entries can drop out without ever
        // leaving a dangling separator or an empty group heading. `label()`
        // renders a dimmed, non-clickable section header.

        // ── Observability ──
        let mut menu = this
            .label(i18n_status_bar(cx, "group_observability"))
            .menu_element_with_icon(
                Icon::new(CustomIconName::Radar),
                Box::new(ServerToolsAction::Monitor),
                move |_window, cx| Label::new(i18n_status_bar(cx, "toggle_monitor_tooltip")),
            )
            .menu_element_with_icon(
                Icon::new(CustomIconName::Zap),
                Box::new(ServerToolsAction::ServerLoad),
                move |_window, cx| Label::new(i18n_server_load(cx, "title")),
            )
            // Keyspace Notifications relies on `notify-keyspace-events`
            // (since 2.8) — no capability gate; an empty config surfaces a
            // one-click Enable banner inside the panel.
            .menu_element_with_icon(
                Icon::new(CustomIconName::AudioWaveform),
                Box::new(ServerToolsAction::KeyspaceNotifications),
                move |_window, cx| Label::new(i18n_status_bar(cx, "toggle_keyspace_notifications_tooltip")),
            );

        // ── Query & Scripting ──
        menu = menu.separator().label(i18n_status_bar(cx, "group_scripting"));
        // Search keys by value content — works on any Redis (no module), so it
        // anchors this group.
        menu = menu.menu_element_with_icon(
            Icon::new(IconName::Search),
            Box::new(ServerToolsAction::ValueSearch),
            move |_window, cx| Label::new(i18n_value_search(cx, "title")),
        );
        // RediSearch (module `search`). Shown disabled with a "module not
        // loaded" suffix when the module is absent, so the feature stays
        // discoverable and the user knows what to enable.
        let search_label: SharedString = if supports_search {
            i18n_status_bar(cx, "toggle_search_tooltip")
        } else {
            format!(
                "{}  ·  {}",
                i18n_status_bar(cx, "toggle_search_tooltip"),
                i18n_status_bar(cx, "module_not_loaded")
            )
            .into()
        };
        menu = menu.menu_with_icon_and_disabled(
            search_label,
            Icon::new(IconName::Search),
            Box::new(ServerToolsAction::Search),
            !supports_search,
        );
        // Functions (`FUNCTION`, Redis 7.0+). Version-gated rather than a
        // module, so the suffix points at the required Redis version.
        let functions_label: SharedString = if supports_functions {
            i18n_status_bar(cx, "toggle_functions_tooltip")
        } else {
            format!(
                "{}  ·  {}",
                i18n_status_bar(cx, "toggle_functions_tooltip"),
                i18n_status_bar(cx, "requires_redis_7")
            )
            .into()
        };
        menu = menu.menu_with_icon_and_disabled(
            functions_label,
            Icon::new(IconName::Asterisk),
            Box::new(ServerToolsAction::Functions),
            !supports_functions,
        );
        // Lua script library uses EVAL/EVALSHA (since 2.6) — always available,
        // so it anchors this group.
        menu = menu.menu_element_with_icon(
            Icon::new(IconName::SquareTerminal),
            Box::new(ServerToolsAction::LuaScripts),
            move |_window, cx| Label::new(i18n_status_bar(cx, "toggle_lua_scripts_tooltip")),
        );

        // ── Administration ──
        menu = menu.separator().label(i18n_status_bar(cx, "group_admin"));
        menu = menu.menu_element_with_icon(
            Icon::new(IconName::Settings),
            Box::new(ServerToolsAction::Config),
            move |_window, cx| Label::new(i18n_status_bar(cx, "toggle_config_tooltip")),
        );
        // ACL (Redis 6.0+). Version-gated; suffix points at the required
        // Redis version when unavailable.
        let acl_label: SharedString = if supports_acl {
            i18n_status_bar(cx, "toggle_acl_tooltip")
        } else {
            format!(
                "{}  ·  {}",
                i18n_status_bar(cx, "toggle_acl_tooltip"),
                i18n_status_bar(cx, "requires_redis_6")
            )
            .into()
        };
        menu = menu.menu_with_icon_and_disabled(
            acl_label,
            Icon::new(IconName::CircleUser),
            Box::new(ServerToolsAction::Acl),
            !supports_acl,
        );
        // Persistence (BGSAVE / BGREWRITEAOF) is pre-2.0 — always offered.
        menu = menu.menu_element_with_icon(
            Icon::new(CustomIconName::HardDrive),
            Box::new(ServerToolsAction::Persistence),
            move |_window, cx| Label::new(i18n_status_bar(cx, "toggle_persistence_tooltip")),
        );

        // ── Cluster ── (multi-node only; on Standalone the Topology panel is
        // just a placeholder, so the whole group — separator and heading
        // included — is hidden).
        if supports_topology {
            menu = menu.separator().label(i18n_status_bar(cx, "group_cluster"));
            // Topology adapts to Cluster / Sentinel. Re-uses `topology.title`
            // as the label to avoid an extra 8-locale tooltip key.
            menu = menu.menu_element_with_icon(
                Icon::new(CustomIconName::Network),
                Box::new(ServerToolsAction::Topology),
                move |_window, cx| Label::new(i18n_topology(cx, "title")),
            );
        }

        menu
    }
    /// Render the server status
    fn render_server_status(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        let is_completed = server_state.scan_finished;
        let nodes_description = server_state.nodes_description.clone();
        let terminal_tooltip = format!(
            "{} ({})",
            i18n_status_bar(cx, "toggle_terminal_tooltip"),
            humanize_keystroke("cmd-j")
        );
        let readonly_tooltip = i18n_status_bar(cx, "toggle_readonly_tooltip");
        let tag_text = server_state.tag.clone();
        let tag_chip = resolve_tag_chip(server_state.tag_color_key.as_deref(), cx.theme().is_dark());
        let supports_search = server_state.supports_search;
        let supports_acl = server_state.supports_acl;
        let supports_functions = server_state.supports_functions;
        let supports_topology = server_state.supports_topology;
        // Live-connection dot beside the latency chip. Colors mirror the
        // latency palette next to it (green/yellow/red) so the row reads as one
        // health cluster; muted = no heartbeat result yet.
        let (health_color, health_label) = match server_state.health {
            ConnectionHealth::Connected => (cx.theme().green, i18n_status_bar(cx, "conn_connected")),
            ConnectionHealth::Reconnecting => (cx.theme().yellow, i18n_status_bar(cx, "conn_reconnecting")),
            ConnectionHealth::Offline => (cx.theme().red, i18n_status_bar(cx, "conn_offline")),
            ConnectionHealth::Unknown => (cx.theme().muted_foreground, i18n_status_bar(cx, "conn_connecting")),
        };
        // When the link is down the dot doubles as a one-click reconnect
        // affordance. The heartbeat alone leaves `server_status` Idle, so a
        // plain re-select would no-op — `reconnect()` forces the reload.
        let is_link_down = matches!(
            server_state.health,
            ConnectionHealth::Offline | ConnectionHealth::Reconnecting
        );
        let health_tooltip = if is_link_down {
            // Name the failure ("Connection timed out · click to reconnect")
            // when we classified it; fall back to a bare "Offline" otherwise.
            let hint = i18n_status_bar(cx, "conn_reconnect_hint");
            let reason = i18n_status_bar(cx, server_state.last_connection_error.reason_key());
            format!("{reason} · {hint}").into()
        } else {
            health_label
        };
        // When the link is down the cached latency is stale and misleading
        // (a green "5ms" beside a red dot), so blank it to a muted "--".
        let (latency_text, latency_color) = if server_state.health == ConnectionHealth::Offline {
            (SharedString::from("--"), cx.theme().muted_foreground)
        } else {
            server_state.latency.clone()
        };
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
                    .child(
                        Button::new("zedis-status-bar-server-terminal")
                            .ghost()
                            .small()
                            .tooltip(terminal_tooltip)
                            .icon(IconName::SquareTerminal)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.toggle_terminal(cx);
                                });
                            })),
                    )
                    .when(self.databases > 1, |this| {
                        this.child(Select::new(&self.db_state).mt_1().small())
                    })
                    .child(
                        Button::new("zedis-status-bar-server-toggle-readonly")
                            .ghost()
                            .small()
                            .tooltip(readonly_tooltip)
                            .when(self.readonly, |this| this.icon(Icon::new(CustomIconName::Lock)))
                            .when(!self.readonly, |this| this.icon(Icon::new(CustomIconName::LockOpen)))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.toggle_readonly(cx);
                                });
                            })),
                    )
                    .child(
                        Button::new("zedis-status-bar-tools")
                            .ghost()
                            .small()
                            .icon(IconName::Menu)
                            .tooltip(i18n_status_bar(cx, "tools_tooltip"))
                            // Status bar sits at the bottom, so open the menu
                            // upward (its bottom edge anchored to the button).
                            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |this, _, cx| {
                                Self::render_tools_menu(
                                    this,
                                    supports_search,
                                    supports_acl,
                                    supports_functions,
                                    supports_topology,
                                    cx,
                                )
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
                            .tooltip(if is_completed {
                                i18n_status_bar(cx, "scan_completed")
                            } else {
                                i18n_status_bar(cx, "scan_more_keys")
                            })
                            .mr_1()
                            .icon(CustomIconName::ChevronsDown)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.server_state.update(cx, |state, cx| {
                                    state.scan_next(cx);
                                });
                            })),
                    )
                    .child(Label::new(server_state.size.clone()).mr_2())
                    .child(
                        div()
                            .child(
                                h_flex()
                                    .child(Icon::new(CustomIconName::Network).text_color(cx.theme().primary).mr_1())
                                    .child(Label::new(server_state.nodes.clone())),
                            )
                            .id("zedis-servers")
                            .tooltip(move |window, cx| Tooltip::new(nodes_description.clone()).build(window, cx)),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("zedis-conn-health")
                                    .size(px(8.))
                                    .rounded_full()
                                    .bg(health_color)
                                    .when(is_link_down, |this| {
                                        this.cursor_pointer().on_click(cx.listener(|this, _, _window, cx| {
                                            this.server_state.update(cx, |state, cx| {
                                                state.reconnect(cx);
                                            });
                                        }))
                                    })
                                    .tooltip(move |window, cx| Tooltip::new(health_tooltip.clone()).build(window, cx)),
                            )
                            .child(
                                Button::new("zedis-status-bar-server-metrics")
                                    .ghost()
                                    .small()
                                    .icon(CustomIconName::Activity)
                                    .tooltip(i18n_status_bar(cx, "toggle_metrics_tooltip"))
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                            state.toggle_route((Route::Metrics, Route::Editor), cx);
                                        });
                                    })),
                            )
                            .child(Label::new(latency_text).text_color(latency_color)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("zedis-status-bar-server-memory-analysis")
                                    .ghost()
                                    .small()
                                    .icon(CustomIconName::MemoryStick)
                                    .tooltip(i18n_status_bar(cx, "toggle_memory_analysis_tooltip"))
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                            state.toggle_route((Route::MemoryAnalysis, Route::Editor), cx);
                                        });
                                    })),
                            )
                            .child(Label::new(server_state.used_memory.clone())),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("zedis-status-bar-clients")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(CustomIconName::AudioWaveform))
                                    .tooltip(i18n_status_bar(cx, "toggle_clients_tooltip"))
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                            state.toggle_route((Route::Clients, Route::Editor), cx);
                                        });
                                    })),
                            )
                            .child(Label::new(server_state.clients.clone())),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("zedis-status-bar-server-slow-logs")
                                    .ghost()
                                    .small()
                                    .icon(CustomIconName::Snail)
                                    .tooltip(i18n_status_bar(cx, "toggle_slowlog_tooltip"))
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                            state.toggle_route((Route::Slowlog, Route::Editor), cx);
                                        });
                                    })),
                            )
                            .child(Label::new(server_state.slow_log_tips.clone())),
                    ),
            )
    }
    fn render_editor_settings(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = &self.state.server_state;
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(server_state.soft_wrap, |this| this.icon(IconName::Check))
            .tooltip(i18n_status_bar(cx, "soft_wrap_tooltip"))
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
        // error message is always on the right
        h_flex().flex_1().child(
            Label::new(data.message.clone())
                .mr_2()
                .w_full()
                .text_xs()
                .text_color(cx.theme().red)
                .text_align(TextAlign::Right),
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
                })
                .collect::<Vec<_>>();
            self.db_state.update(cx, |state, cx| {
                state.set_items(db_items, window, cx);
            });
        }
        // App-global: a newer release awaiting action lights the far-right chip.
        let update_version = cx.global::<ZedisGlobalStore>().read(cx).available_update_version();
        // While downloading, the chip shows the percentage instead of the version.
        let download_progress = cx.global::<ZedisGlobalStore>().read(cx).download_progress();
        h_flex()
            .justify_between()
            .h(STATUS_BAR_HEIGHT)
            .text_sm()
            .py_1p5()
            .px_4()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .child(
                ZedisDivider::new()
                    .child(self.render_server_status(window, cx))
                    .child(self.render_editor_settings(window, cx))
                    .when(self.state.data_format.is_some(), |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .child(self.render_data_format(window, cx))
                                .child(self.render_viewer_mode(window, cx)),
                        )
                    }),
            )
            // Far-right cluster (pushed to the edge by `justify_between`),
            // separated from the stats. The update chip lives here, showing the
            // latest version; click opens the download/skip prompt.
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_errors(window, cx))
                    .when(update_version.is_some(), |this| {
                        let v = update_version.clone().unwrap_or_default();
                        let label = match download_progress {
                            Some(pct) => format!("{pct}%"),
                            None => format!("v{v}"),
                        };
                        this.child(
                            Button::new("zedis-status-bar-update")
                                .ghost()
                                .small()
                                .icon(CustomIconName::Download)
                                .label(label)
                                // No prompt while a download is already running.
                                .disabled(download_progress.is_some())
                                .tooltip(i18n_status_bar(cx, "update_available"))
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(UpdateAction::OpenPrompt), cx);
                                }),
                        )
                    }),
            )
    }
}

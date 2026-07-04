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

use crate::connection::{
    RedisServer, get_server, get_servers, save_servers, set_redis_connection_timeout, set_redis_response_timeout,
};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::{
    UpdateInfo, decrypt, encrypt, get_key_tree_widths, get_or_create_config_dir, is_development, unix_ts,
};
use crate::states::i18n_common;
use chrono::Local;
use gpui::{Action, App, AppContext, Bounds, Context, Entity, EventEmitter, Global, Pixels, SharedString};
use gpui_component::{ThemeMode, dialog::DialogButtonProps};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use sys_locale::get_locale;
use tracing::{error, info, warn};
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level navigation target — the runtime single source of truth for
/// "where am I", including the active connection: a connection-scoped page is
/// only representable together with its `(id, db)`. App-scoped pages
/// (`Home` / `Settings` / `Protos` / `Scripts`) stand alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Route {
    #[default]
    Home,
    Settings,
    Protos,
    Scripts,
    Server {
        id: SharedString,
        db: usize,
        view: ServerView,
    },
}

/// A connection-scoped page, rendered against the active `selected_server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum ServerView {
    #[default]
    Editor,
    Metrics,
    Slowlog,
    MemoryAnalysis,
    Clients,
    Monitor,
    Config,
    Acl,
    Search,
    Functions,
    LuaScripts,
    Persistence,
    KeyspaceNotifications,
    Topology,
    ServerLoad,
    ValueSearch,
}

impl Route {
    /// Stable lowercase name used for persistence (and, later, deep links).
    pub fn as_str(&self) -> &'static str {
        match self {
            Route::Home => "home",
            Route::Settings => "settings",
            Route::Protos => "protos",
            Route::Scripts => "scripts",
            Route::Server { view, .. } => view.as_str(),
        }
    }
    /// Parse an app-level route name (case-insensitive). Connection-scoped
    /// names go through `ServerView::from_name` instead — they can't stand
    /// alone as a `Route` without an `(id, db)`.
    pub fn app_from_name(s: &str) -> Option<Route> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "home" => Route::Home,
            "settings" => Route::Settings,
            "protos" => Route::Protos,
            "scripts" => Route::Scripts,
            _ => return None,
        })
    }
    /// The connection-scoped view, if this is a server route.
    pub fn server_view(&self) -> Option<ServerView> {
        match self {
            Route::Server { view, .. } => Some(*view),
            _ => None,
        }
    }
    /// The `(id, db)` this route renders against, if it is a server route.
    pub fn server(&self) -> Option<(SharedString, usize)> {
        match self {
            Route::Server { id, db, .. } => Some((id.clone(), *db)),
            _ => None,
        }
    }
    pub fn is_server(&self) -> bool {
        matches!(self, Route::Server { .. })
    }
}

impl ServerView {
    /// Stable lowercase name (matches the lowercased legacy variant name).
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerView::Editor => "editor",
            ServerView::Metrics => "metrics",
            ServerView::Slowlog => "slowlog",
            ServerView::MemoryAnalysis => "memoryanalysis",
            ServerView::Clients => "clients",
            ServerView::Monitor => "monitor",
            ServerView::Config => "config",
            ServerView::Acl => "acl",
            ServerView::Search => "search",
            ServerView::Functions => "functions",
            ServerView::LuaScripts => "luascripts",
            ServerView::Persistence => "persistence",
            ServerView::KeyspaceNotifications => "keyspacenotifications",
            ServerView::Topology => "topology",
            ServerView::ServerLoad => "serverload",
            ServerView::ValueSearch => "valuesearch",
        }
    }
    /// Parse a connection-scoped view name (expects an already-lowercased str).
    pub fn from_name(s: &str) -> Option<ServerView> {
        Some(match s {
            "editor" => ServerView::Editor,
            "metrics" => ServerView::Metrics,
            "slowlog" => ServerView::Slowlog,
            "memoryanalysis" => ServerView::MemoryAnalysis,
            "clients" => ServerView::Clients,
            "monitor" => ServerView::Monitor,
            "config" => ServerView::Config,
            "acl" => ServerView::Acl,
            "search" => ServerView::Search,
            "functions" => ServerView::Functions,
            "luascripts" => ServerView::LuaScripts,
            "persistence" => ServerView::Persistence,
            "keyspacenotifications" => ServerView::KeyspaceNotifications,
            "topology" => ServerView::Topology,
            "serverload" => ServerView::ServerLoad,
            "valuesearch" => ServerView::ValueSearch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
}
impl FontSize {
    pub fn to_pixels(self) -> Option<f32> {
        match self {
            FontSize::Small => Some(14.0),
            FontSize::Medium => None,
            FontSize::Large => Some(18.0),
        }
    }
}

/// Theme selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ThemeAction {
    /// Light theme mode
    Light,
    /// Dark theme mode
    Dark,
    /// Follow system theme
    System,
}

/// Apply a named theme from the registry (carries the theme's name, e.g.
/// "Ayu Dark"). Dispatched from the title-bar theme menu.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub struct SelectThemeAction {
    pub name: String,
}

/// Locale/language selection actions for the settings menu
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum LocaleAction {
    /// English language
    En,
    /// Chinese language
    Zh,
    /// Japanese language
    Ja,
    /// Russian language
    Ru,
    /// Portuguese language
    Pt,
    /// German language
    De,
    /// French language
    Fr,
    /// Spanish language
    Es,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum SettingsAction {
    Editor,
    Protos,
    Scripts,
}

/// Server-scoped tools that open a sub-route. Triggered from the status bar
/// "Tools" dropdown so the bar itself does not need a button per route.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ServerToolsAction {
    Monitor,
    Config,
    Acl,
    Search,
    Functions,
    LuaScripts,
    Persistence,
    KeyspaceNotifications,
    Topology,
    ServerLoad,
    ValueSearch,
}

const LIGHT_THEME_MODE: &str = "light";
const DARK_THEME_MODE: &str = "dark";

fn get_or_create_server_config() -> Result<PathBuf> {
    let config_dir = get_or_create_config_dir()?;
    let path = if is_development() {
        config_dir.join("zedis-dev.toml")
    } else {
        config_dir.join("zedis.toml")
    };
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

/// Notification category for user feedback
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Default)]
pub enum NotificationCategory {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// Notification action that can be triggered in the UI
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action, Default)]
pub struct NotificationAction {
    pub title: Option<SharedString>,
    pub category: NotificationCategory,
    pub message: SharedString,
}

impl NotificationAction {
    /// Creates a new info notification
    pub fn new_info(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Info,
            message,
            ..Default::default()
        }
    }

    /// Creates a new success notification
    pub fn new_success(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Success,
            message,
            ..Default::default()
        }
    }

    /// Creates a new warning notification
    pub fn new_warning(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Warning,
            message,
            ..Default::default()
        }
    }

    /// Creates a new error notification
    pub fn new_error(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Error,
            message,
            ..Default::default()
        }
    }

    /// Sets the title for the notification
    pub fn with_title(mut self, title: SharedString) -> Self {
        self.title = Some(title);
        self
    }
}

pub enum GlobalEvent {
    /// A notification has been emitted.
    Notification(NotificationAction),
    /// User selected a different server
    ServerSelected(SharedString, usize),
    /// Server list config has been modified (add/remove/edit).
    ServerListUpdated,
    /// Route has been changed.
    RouteChanged(Route),
    /// Update availability changed (a newer release was found, or the prompt was
    /// cleared). The status bar re-reads `available_update` from the store.
    UpdateAvailable,
    /// Installer download progress changed (0–100, or cleared). The status bar
    /// re-reads `download_progress` to show the percentage on the update chip.
    UpdateDownloadProgress,
}

/// Direction passed to [`ZedisGlobalStore::reorder_server`].
#[derive(Debug, Clone, Copy)]
pub enum ReorderDirection {
    Up,
    Down,
}

/// Cap on remembered per-display window placements (MRU order). Bounds the
/// config size for users who connect to many different monitors over time.
const MAX_WINDOW_PLACEMENTS: usize = 8;

/// A window placement anchored to a specific display, so it survives
/// multi-monitor rearrangement. `bounds` is the window rectangle **relative to
/// that display's origin**; `display_uuid` identifies the display across
/// restarts. Kept per display in [`ZedisAppState::window_placements`]; the
/// absolute [`ZedisAppState::bounds`] is the fallback (see
/// `main.rs::resolve_window_bounds`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub display_uuid: String,
    pub bounds: Bounds<Pixels>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZedisAppState {
    // Runtime route — the single source of truth for "where am I", including
    // the `(id, db)` of connection-scoped views. Reassembled by `try_new` from
    // `route_token` + the `selected_server` snapshot below, so the on-disk
    // format is unchanged from before the (id, db) fold-in.
    #[serde(skip)]
    route: Route,
    /// Persisted flat route name (`"metrics"`, `"home"`, …) — same key and
    /// format the pre-fold config used. Kept in lockstep with `route` by the
    /// navigation core.
    #[serde(rename = "route", default)]
    route_token: String,
    // Runtime-only: a stale "open key X" param shouldn't survive a restart.
    #[serde(skip)]
    query: Option<HashMap<String, String>>,
    locale: Option<String>,
    bounds: Option<Bounds<Pixels>>,
    /// Per-display window placements (origin relative to each display), keyed by
    /// `display_uuid`, most-recently-used first and capped at
    /// `MAX_WINDOW_PLACEMENTS`. Lets each monitor remember its own last position
    /// (e.g. work vs home). `bounds` above is the fallback for old configs / a
    /// display that's gone.
    #[serde(default)]
    window_placements: Vec<WindowPlacement>,
    key_tree_width: Pixels,
    theme: Option<String>,
    /// Selected named theme from the registry (e.g. "Ayu Dark"). Takes
    /// precedence over the `theme` mode; `None` falls back to Light/Dark/System.
    theme_name: Option<String>,
    font_size: Option<FontSize>,
    /// Continuous UI font size (rem px) from the settings slider. Takes
    /// precedence over the legacy `font_size` enum; `None` falls back to it,
    /// then to gpui's 16px default. Additive so old configs migrate silently.
    font_rem_px: Option<f32>,
    max_key_tree_depth: Option<usize>,
    key_separator: Option<String>,
    auto_expand_threshold: Option<usize>,
    key_scan_count: Option<usize>,
    max_truncate_length: Option<usize>,
    redis_connection_timeout: Option<Duration>,
    redis_response_timeout: Option<Duration>,
    /// Last-active connection snapshot. `route` is the runtime truth for the
    /// current view's connection; this survives app-level detours (Settings /
    /// Protos) and restarts, so back-navigation and startup restore know which
    /// server to return to.
    selected_server: Option<(String, usize)>,
    /// Per-server last-viewed database, so connecting reopens the DB the user
    /// left it on instead of always DB 0. Keyed by server id.
    #[serde(default)]
    last_db: HashMap<String, usize>,
    tray_enabled: Option<bool>,
    /// When `true`, the key tree fetches TTL per key during SCAN and shows
    /// a TTL chip next to each leaf. When `false` the TTL pipeline command
    /// is skipped (cheaper scans on large dbs) and no chip is rendered.
    /// Defaults to `true`.
    show_key_tree_ttl: Option<bool>,
    /// Server-page group section keys whose card grid is collapsed.
    /// Keyed by group label (or `"__none__"` for the ungrouped
    /// bucket). A renamed/deleted group simply leaves a harmless
    /// stale string here that no section ever matches.
    collapsed_server_groups: Option<Vec<String>>,
    /// Whether the navigation sidebar is collapsed to an icon-only rail.
    #[serde(default)]
    sidebar_collapsed: Option<bool>,
    /// Base URL of the OpenAI-compatible endpoint used by the AI
    /// memory-analysis feature, e.g. `https://api.openai.com/v1`.
    ai_base_url: Option<String>,
    /// API key for the AI endpoint. Stored **encrypted** (AES-256-GCM,
    /// same scheme as server passwords) — never persisted in plaintext.
    ai_api_key: Option<String>,
    /// Model name passed to the AI endpoint, e.g. `gpt-4o-mini`.
    ai_model: Option<String>,
    /// When `true` (default), check GitHub for a newer release on startup,
    /// throttled to once per day. `false` disables the network check entirely.
    auto_update_check: Option<bool>,
    /// Unix seconds of the last update check, used to throttle the startup
    /// check to once per day.
    last_update_check: Option<i64>,
    /// A version the user chose to skip (e.g. `"0.5.0"`). Suppresses the silent
    /// startup prompt for exactly that version; a manual check ignores it.
    skipped_version: Option<String>,
    /// A newer release found by a check, awaiting the user's action. Runtime
    /// only (never persisted) — it's re-discovered on the next check. Drives
    /// the status-bar update chip; the chip's click opens the prompt for it.
    #[serde(skip)]
    available_update: Option<UpdateInfo>,
    /// Installer download progress (0–100) while an update is downloading;
    /// `None` when idle. Runtime only — drives the status-bar chip percentage.
    #[serde(skip)]
    download_progress: Option<u8>,
    /// True while an update check (network fetch) is in flight. Runtime only —
    /// drives the title-bar update chip's loading spinner.
    #[serde(skip)]
    update_checking: bool,
}

impl EventEmitter<GlobalEvent> for ZedisAppState {}

#[derive(Debug, Clone)]
pub struct ZedisGlobalStore {
    app_state: Entity<ZedisAppState>,
}

impl ZedisGlobalStore {
    pub fn new(app_state: Entity<ZedisAppState>) -> Self {
        Self { app_state }
    }
    pub fn state(&self) -> Entity<ZedisAppState> {
        self.app_state.clone()
    }
    pub fn value(&self, cx: &App) -> ZedisAppState {
        self.app_state.read(cx).clone()
    }
    pub fn update<R, C: AppContext>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut ZedisAppState, &mut Context<ZedisAppState>) -> R,
    ) -> R {
        self.app_state.update(cx, update)
    }
    pub fn read<'a>(&self, cx: &'a App) -> &'a ZedisAppState {
        self.app_state.read(cx)
    }
}

impl Global for ZedisGlobalStore {}

pub fn save_app_state(state: &ZedisAppState) -> Result<()> {
    let path = get_or_create_server_config()?;
    let value = toml::to_string(state)?;
    std::fs::write(path, value)?;
    Ok(())
}

impl ZedisAppState {
    pub fn try_new() -> Result<Self> {
        let path = get_or_create_server_config()?;
        let value = std::fs::read_to_string(path)?;
        let mut state = if value.is_empty() {
            Self::default()
        } else {
            toml::from_str(&value)?
        };
        if state.locale.clone().unwrap_or_default().is_empty() {
            if let Some(locale) = get_locale() {
                // Try to extract the language code from the locale string
                // Handle formats like: "en-US", "zh-CN", "en", "zh", etc.
                let lang = if let Some((lang, _)) = locale.split_once('-') {
                    lang
                } else if let Some((lang, _)) = locale.split_once('_') {
                    // Some systems use underscore: "en_US"
                    lang
                } else {
                    // Already a simple language code like "en" or "zh"
                    locale.as_str()
                };
                state.locale = Some(lang.to_lowercase());
            } else {
                // Fallback to English if locale detection fails
                state.locale = Some("en".to_string());
            }
        }
        // Reassemble the runtime route from the persisted flat token plus the
        // last-connection snapshot; a connection-scoped view whose remembered
        // server is gone (or absent) opens Home instead.
        state.route = match Route::app_from_name(&state.route_token) {
            Some(route) => route,
            None => {
                let view = ServerView::from_name(state.route_token.trim().to_ascii_lowercase().as_str());
                let conn = state.selected_server.as_ref().filter(|(id, _)| get_server(id).is_ok());
                match (view, conn) {
                    (Some(view), Some((id, db))) => Route::Server {
                        id: SharedString::from(id.clone()),
                        db: *db,
                        view,
                    },
                    _ => Route::Home,
                }
            }
        };
        state.route_token = state.route.as_str().to_string();

        if let Some(redis_connection_timeout) = state.redis_connection_timeout {
            set_redis_connection_timeout(redis_connection_timeout);
        }
        if let Some(redis_response_timeout) = state.redis_response_timeout {
            set_redis_response_timeout(redis_response_timeout);
        }

        Ok(state)
    }
    pub fn new() -> Self {
        Self { ..Default::default() }
    }
    pub fn key_tree_width(&self) -> Pixels {
        self.key_tree_width
    }
    pub fn content_width(&self) -> Option<Pixels> {
        let bounds = self.bounds?;
        let width = bounds.size.width.as_f32();
        let (key_tree_width, _, _) = get_key_tree_widths(self.key_tree_width);
        Some((width - SIDEBAR_WIDTH.as_f32() - key_tree_width.as_f32()).into())
    }
    pub fn set_key_tree_width(&mut self, width: Pixels) {
        self.key_tree_width = width;
    }
    pub fn route(&self) -> Route {
        self.route.clone()
    }
    pub fn bounds(&self) -> Option<&Bounds<Pixels>> {
        self.bounds.as_ref()
    }
    /// Persist navigation state in the background so the app reopens on the last
    /// route (only the route lands on disk — `query` is serde-skipped).
    fn persist_nav(&self, cx: &mut Context<Self>) {
        let snapshot = self.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = save_app_state(&snapshot) {
                    error!(error = %e, "failed to persist route");
                }
            })
            .detach();
    }
    fn go_to_with_query(&mut self, route: Route, query: Option<HashMap<String, String>>, cx: &mut Context<Self>) {
        if self.route == route && self.query == query {
            return;
        }
        self.query = query;
        self.apply_route(route, cx);
        cx.notify();
        self.persist_nav(cx);
    }
    /// The single mutation point for `route`: keeps the persisted token and the
    /// last-connection snapshot in lockstep, and emits `RouteChanged` (always)
    /// plus `ServerSelected` (only when the connection actually changed, so
    /// returning to the same server from an app page doesn't reload it).
    fn apply_route(&mut self, route: Route, cx: &mut Context<Self>) {
        self.route_token = route.as_str().to_string();
        self.route = route.clone();
        if let Some((id, db)) = route.server() {
            let changed = self
                .selected_server
                .as_ref()
                .map(|(sid, sdb)| sid.as_str() != id.as_ref() || *sdb != db)
                .unwrap_or(true);
            if changed {
                self.last_db.insert(id.to_string(), db);
                self.selected_server = Some((id.to_string(), db));
                cx.emit(GlobalEvent::ServerSelected(id, db));
            }
        }
        cx.emit(GlobalEvent::RouteChanged(route));
    }
    pub fn go_to(&mut self, route: Route, cx: &mut Context<Self>) {
        self.go_to_with_query(route, None, cx);
    }
    pub fn go_with_query(&mut self, route: Route, query: HashMap<String, String>, cx: &mut Context<Self>) {
        self.go_to_with_query(route, Some(query), cx);
    }
    pub fn get_route_query(&self) -> Option<&HashMap<String, String>> {
        self.query.as_ref()
    }
    /// Switch to a connection-scoped view, keeping the current connection — or,
    /// from an app page (Settings / Protos / …), the last-active one. No-op
    /// (with a warning) when no valid connection is known.
    pub fn go_to_view(&mut self, view: ServerView, cx: &mut Context<Self>) {
        let conn = self.route.server().or_else(|| {
            self.selected_server
                .as_ref()
                .filter(|(id, _)| get_server(id).is_ok())
                .map(|(id, db)| (SharedString::from(id.clone()), *db))
        });
        let Some((id, db)) = conn else {
            warn!(view = view.as_str(), "no active server for view; ignoring");
            return;
        };
        self.go_to(Route::Server { id, db, view }, cx);
    }
    /// Toggle between a server view and the editor (the status-bar chips).
    pub fn toggle_view(&mut self, view: ServerView, cx: &mut Context<Self>) {
        if self.route.server_view() == Some(view) {
            self.go_to_view(ServerView::Editor, cx);
        } else {
            self.go_to_view(view, cx);
        }
    }
    /// Startup activation: (re)apply `route` once the views have subscribed,
    /// always announcing a server route's connection — `try_new` assembled the
    /// route silently, before any subscriber existed.
    pub fn activate(&mut self, route: Route, cx: &mut Context<Self>) {
        self.route_token = route.as_str().to_string();
        self.route = route.clone();
        if let Some((id, db)) = route.server() {
            self.last_db.insert(id.to_string(), db);
            self.selected_server = Some((id.to_string(), db));
            cx.emit(GlobalEvent::ServerSelected(id, db));
        }
        cx.emit(GlobalEvent::RouteChanged(route));
        cx.notify();
        self.persist_nav(cx);
    }
    /// Effective UI font size in rem px: the slider value if set, else the
    /// legacy `font_size` enum's pixels, else `None` (gpui's 16px default).
    pub fn font_rem_px(&self) -> Option<f32> {
        self.font_rem_px
            .or_else(|| self.font_size.and_then(FontSize::to_pixels))
    }
    pub fn set_font_rem_px(&mut self, px: Option<f32>) {
        self.font_rem_px = px;
    }
    pub fn max_key_tree_depth(&self) -> usize {
        self.max_key_tree_depth.unwrap_or(5)
    }
    pub fn set_max_key_tree_depth(&mut self, max_key_tree_depth: usize) {
        if max_key_tree_depth == 0 {
            self.max_key_tree_depth = None;
            return;
        }
        self.max_key_tree_depth = Some(max_key_tree_depth);
    }
    pub fn set_redis_connection_timeout(&mut self, redis_connection_timeout: Option<Duration>) {
        if let Some(redis_connection_timeout) = redis_connection_timeout {
            set_redis_connection_timeout(redis_connection_timeout);
        }
        self.redis_connection_timeout = redis_connection_timeout;
    }
    pub fn set_redis_response_timeout(&mut self, redis_response_timeout: Option<Duration>) {
        if let Some(redis_response_timeout) = redis_response_timeout {
            set_redis_response_timeout(redis_response_timeout);
        }
        self.redis_response_timeout = redis_response_timeout;
    }
    pub fn theme(&self) -> Option<ThemeMode> {
        match self.theme.as_deref() {
            Some(LIGHT_THEME_MODE) => Some(ThemeMode::Light),
            Some(DARK_THEME_MODE) => Some(ThemeMode::Dark),
            _ => None,
        }
    }
    /// The selected named theme, if any (overrides the Light/Dark/System mode).
    pub fn theme_name(&self) -> Option<String> {
        self.theme_name.clone()
    }
    pub fn set_theme_name(&mut self, name: Option<String>) {
        self.theme_name = name;
    }
    pub fn locale(&self) -> &str {
        self.locale.as_deref().unwrap_or("en")
    }

    pub fn set_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.bounds = Some(bounds);
    }
    pub fn window_placements(&self) -> &[WindowPlacement] {
        &self.window_placements
    }
    /// Upsert a per-display placement: drop any prior entry for the same display,
    /// move it to the front (most-recently-used), and keep at most
    /// `MAX_WINDOW_PLACEMENTS` distinct displays.
    pub fn upsert_window_placement(&mut self, placement: WindowPlacement) {
        self.window_placements
            .retain(|p| p.display_uuid != placement.display_uuid);
        self.window_placements.insert(0, placement);
        self.window_placements.truncate(MAX_WINDOW_PLACEMENTS);
    }
    pub fn set_theme(&mut self, theme: Option<ThemeMode>) {
        // Picking a Light/Dark/System mode clears any named theme so the mode
        // actually takes effect (the two are mutually exclusive).
        self.theme_name = None;
        match theme {
            Some(ThemeMode::Light) => {
                self.theme = Some(LIGHT_THEME_MODE.to_string());
            }
            Some(ThemeMode::Dark) => {
                self.theme = Some(DARK_THEME_MODE.to_string());
            }
            _ => {
                self.theme = None;
            }
        }
    }
    pub fn set_locale(&mut self, locale: String) {
        self.locale = Some(locale);
    }
    pub fn key_separator(&self) -> &str {
        self.key_separator.as_deref().unwrap_or(":")
    }
    pub fn set_key_separator(&mut self, key_separator: String) {
        if key_separator.is_empty() {
            self.key_separator = None;
            return;
        }
        self.key_separator = Some(key_separator);
    }
    pub fn max_truncate_length(&self) -> usize {
        self.max_truncate_length.unwrap_or(1000)
    }
    pub fn set_max_truncate_length(&mut self, max_truncate_length: usize) {
        // 0 means "reset to default" (cleared input) — store None.
        if max_truncate_length == 0 {
            self.max_truncate_length = None;
            return;
        }
        self.max_truncate_length = Some(max_truncate_length);
    }
    pub fn redis_connection_timeout(&self) -> String {
        self.redis_connection_timeout
            .map(|timeout| timeout.as_secs().to_string())
            .unwrap_or_default()
    }
    pub fn redis_response_timeout(&self) -> String {
        self.redis_response_timeout
            .map(|timeout| timeout.as_secs().to_string())
            .unwrap_or_default()
    }
    pub fn key_scan_count(&self) -> usize {
        self.key_scan_count.unwrap_or(10_000)
    }
    pub fn set_key_scan_count(&mut self, key_scan_count: usize) {
        // 0 means "reset to default" (cleared input) — store None so the
        // getter's default applies.
        if key_scan_count == 0 {
            self.key_scan_count = None;
            return;
        }
        self.key_scan_count = Some(key_scan_count);
    }
    pub fn auto_expand_threshold(&self) -> usize {
        self.auto_expand_threshold.unwrap_or(100)
    }
    pub fn set_auto_expand_threshold(&mut self, auto_expand_threshold: usize) {
        // 0 means "reset to default" (cleared input) — store None.
        if auto_expand_threshold == 0 {
            self.auto_expand_threshold = None;
            return;
        }
        self.auto_expand_threshold = Some(auto_expand_threshold);
    }
    pub fn tray_enabled(&self) -> bool {
        self.tray_enabled.unwrap_or(true)
    }
    pub fn set_tray_enabled(&mut self, enabled: bool) {
        self.tray_enabled = Some(enabled);
    }
    pub fn show_key_tree_ttl(&self) -> bool {
        self.show_key_tree_ttl.unwrap_or(true)
    }
    pub fn set_show_key_tree_ttl(&mut self, enabled: bool) {
        self.show_key_tree_ttl = Some(enabled);
    }
    /// Whether the navigation sidebar is collapsed to an icon-only rail.
    pub fn sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed.unwrap_or(false)
    }
    pub fn toggle_sidebar_collapsed(&mut self) {
        self.sidebar_collapsed = Some(!self.sidebar_collapsed());
    }
    /// Base URL of the OpenAI-compatible AI endpoint (without trailing
    /// slash normalization — that happens at request time). Empty when
    /// unset.
    pub fn ai_base_url(&self) -> String {
        self.ai_base_url.clone().unwrap_or_default()
    }
    pub fn set_ai_base_url(&mut self, base_url: String) {
        let base_url = base_url.trim().to_string();
        self.ai_base_url = if base_url.is_empty() { None } else { Some(base_url) };
    }
    /// Decrypted API key for the AI endpoint, or empty when unset.
    /// Falls back to the stored value if decryption fails (e.g. a
    /// hand-edited plaintext key in `zedis.toml`).
    pub fn ai_api_key(&self) -> String {
        self.ai_api_key
            .as_ref()
            .map(|cipher| decrypt(cipher).unwrap_or_else(|_| cipher.clone()))
            .unwrap_or_default()
    }
    /// Store the API key, encrypting it before persistence. An empty
    /// value clears it.
    pub fn set_ai_api_key(&mut self, api_key: String) {
        let api_key = api_key.trim();
        self.ai_api_key = if api_key.is_empty() {
            None
        } else {
            Some(encrypt(api_key).unwrap_or_else(|_| api_key.to_string()))
        };
    }
    /// Model name passed to the AI endpoint. Empty when unset.
    pub fn ai_model(&self) -> String {
        self.ai_model.clone().unwrap_or_default()
    }
    pub fn set_ai_model(&mut self, model: String) {
        let model = model.trim().to_string();
        self.ai_model = if model.is_empty() { None } else { Some(model) };
    }
    /// Whether the AI analysis feature has the minimum configuration
    /// (endpoint + key) to be usable.
    pub fn ai_configured(&self) -> bool {
        self.ai_base_url.is_some() && self.ai_api_key.is_some()
    }
    /// Whether the app checks GitHub for a newer release on startup. Defaults
    /// to `true`; the user can disable it in Settings.
    pub fn auto_update_check(&self) -> bool {
        self.auto_update_check.unwrap_or(true)
    }
    pub fn set_auto_update_check(&mut self, enabled: bool) {
        self.auto_update_check = Some(enabled);
    }
    /// Whether a startup update check is due: never run, or more than a day ago.
    pub fn update_check_due(&self) -> bool {
        match self.last_update_check {
            Some(ts) => unix_ts().saturating_sub(ts) >= 24 * 60 * 60,
            None => true,
        }
    }
    /// Record that an update check just ran, resetting the once-per-day throttle.
    pub fn mark_update_checked(&mut self) {
        self.last_update_check = Some(unix_ts());
    }
    /// Whether the user chose to skip this exact version.
    pub fn update_skipped(&self, version: &str) -> bool {
        self.skipped_version.as_deref() == Some(version)
    }
    pub fn set_skipped_version(&mut self, version: String) {
        self.skipped_version = if version.is_empty() { None } else { Some(version) };
    }
    /// Just the version string of the pending update — for the status-bar chip.
    pub fn available_update_version(&self) -> Option<SharedString> {
        self.available_update
            .as_ref()
            .map(|info| SharedString::from(info.version.clone()))
    }
    /// Set (or clear with `None`) the pending update and broadcast it so the
    /// status-bar chip lights up / clears.
    pub fn set_available_update(&mut self, info: Option<UpdateInfo>, cx: &mut Context<Self>) {
        self.available_update = info;
        cx.emit(GlobalEvent::UpdateAvailable);
    }
    /// Current installer download progress (0–100), or `None` when not downloading.
    pub fn download_progress(&self) -> Option<u8> {
        self.download_progress
    }
    /// Set (or clear with `None`) the download progress and broadcast it so the
    /// status-bar chip shows the percentage.
    pub fn set_download_progress(&mut self, progress: Option<u8>, cx: &mut Context<Self>) {
        self.download_progress = progress;
        cx.emit(GlobalEvent::UpdateDownloadProgress);
    }
    /// Whether an update check (network fetch) is currently running.
    pub fn update_checking(&self) -> bool {
        self.update_checking
    }
    /// Set the "checking for updates" flag and broadcast it so the update chip
    /// can show / clear its loading spinner.
    pub fn set_update_checking(&mut self, checking: bool, cx: &mut Context<Self>) {
        self.update_checking = checking;
        cx.emit(GlobalEvent::UpdateAvailable);
    }
    /// Whether the given server-page group section is collapsed.
    pub fn is_server_group_collapsed(&self, key: &str) -> bool {
        self.collapsed_server_groups
            .as_ref()
            .is_some_and(|groups| groups.iter().any(|g| g == key))
    }
    /// Flip the collapsed state of a server-page group section.
    pub fn toggle_server_group_collapsed(&mut self, key: &str) {
        let groups = self.collapsed_server_groups.get_or_insert_with(Vec::new);
        if let Some(pos) = groups.iter().position(|g| g == key) {
            groups.swap_remove(pos);
        } else {
            groups.push(key.to_string());
        }
    }
    pub fn selected_server(&self) -> Option<&(String, usize)> {
        self.selected_server.as_ref()
    }
    /// The DB this server was last viewed on (0 if never). Lets connecting to a
    /// server reopen the database the user left it on instead of always DB 0.
    pub fn last_db_for(&self, server_id: &str) -> usize {
        self.last_db.get(server_id).copied().unwrap_or(0)
    }
    /// Drop the active connection (Home click): clear the snapshot, announce
    /// the empty selection, and route to Home.
    pub fn clear_selected_server(&mut self, cx: &mut Context<Self>) {
        self.selected_server = None;
        cx.emit(GlobalEvent::ServerSelected(SharedString::default(), 0));
        self.go_to(Route::Home, cx);
    }
    /// Connect to a server, landing on the editor — the sidebar / server-card
    /// / palette / tray entry points. The status-bar DB switch, which keeps
    /// the current view, goes through `set_selected_server` instead.
    ///
    /// These entry points mean "(re)connect", not just "navigate", so the
    /// selection is announced *unconditionally* before routing. `apply_route`'s
    /// dedupe compares against the persisted `selected_server` snapshot, which
    /// after a restart can point at this server while nothing is loaded in the
    /// session yet — relying on it alone would skip the `ServerSelected` that
    /// actually loads the connection (the "tray click does nothing" bug).
    pub fn connect_server(&mut self, id: String, db: usize, cx: &mut Context<Self>) {
        self.last_db.insert(id.clone(), db);
        self.selected_server = Some((id.clone(), db));
        cx.emit(GlobalEvent::ServerSelected(id.clone().into(), db));
        self.go_to(
            Route::Server {
                id: id.into(),
                db,
                view: ServerView::Editor,
            },
            cx,
        );
    }
    /// Activate a connection: routes to it, keeping the current server view
    /// when one is active (the status-bar DB switch) and falling back to the
    /// editor otherwise (sidebar / server-card / tray connects). Route
    /// transition, snapshot, `last_db` and persistence all flow through
    /// `apply_route`.
    pub fn set_selected_server(&mut self, selected_server: (String, usize), cx: &mut Context<Self>) {
        let (server_id, db) = selected_server;
        if server_id.is_empty() {
            return self.clear_selected_server(cx);
        }
        let view = self.route.server_view().unwrap_or(ServerView::Editor);
        self.go_to(
            Route::Server {
                id: server_id.into(),
                db,
                view,
            },
            cx,
        );
    }
    pub fn remove_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                servers.retain(|s| s.id != id);
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;
            if let Err(e) = &result {
                error!(error = %e, "Failed to remove server");
            }
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }
    /// Swap the `sort_order` of `server_id` with the adjacent neighbor
    /// in the same `group`, in the requested direction. No-op at the
    /// edge of the group. Persists and broadcasts `ServerListUpdated`
    /// so the grid re-renders in the new order.
    ///
    /// Implementation note: instead of swapping just two sort_order
    /// values, we (a) collect the in-group entries in their current
    /// sorted-on-display order, (b) renumber the entire group as
    /// `0..n` so legacy entries with `sort_order: None` get real
    /// values, then (c) perform the index swap. This guarantees the
    /// operation is observable even on data saved before this field
    /// existed (everyone tied at `None` would otherwise no-op).
    pub fn reorder_server(&mut self, server_id: &str, direction: ReorderDirection, cx: &mut Context<Self>) {
        let server_id = server_id.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut servers = get_servers()?;
                let Some(target_idx) = servers.iter().position(|s| s.id == server_id) else {
                    return Ok::<(), Error>(());
                };
                let group_key = servers[target_idx]
                    .group
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let in_same_group = |s: &RedisServer| {
                    s.group
                        .as_deref()
                        .map(str::trim)
                        .filter(|g| !g.is_empty())
                        .map(String::from)
                        == group_key
                };

                // (a) Collect indices belonging to the target's group,
                // in current display order. `get_servers()` already
                // returns canonical sort order, so this list is
                // monotonic.
                let group_indices: Vec<usize> = servers
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| in_same_group(s))
                    .map(|(i, _)| i)
                    .collect();
                let Some(pos_in_group) = group_indices.iter().position(|&i| i == target_idx) else {
                    return Ok(());
                };

                // (c) Determine the swap partner's position in-group.
                let swap_pos = match direction {
                    ReorderDirection::Up if pos_in_group > 0 => pos_in_group - 1,
                    ReorderDirection::Down if pos_in_group + 1 < group_indices.len() => pos_in_group + 1,
                    _ => return Ok(()), // at edge — nothing to do
                };

                // (b) Renumber 0..n in current order, then write the
                // swapped positions back. This means even legacy data
                // (`sort_order: None`) ends up with a stable, distinct
                // sort_order after the first reorder click.
                let mut new_order: Vec<i64> = (0..group_indices.len() as i64).collect();
                new_order.swap(pos_in_group, swap_pos);
                for (slot, &server_idx) in group_indices.iter().enumerate() {
                    servers[server_idx].sort_order = Some(new_order[slot]);
                }

                save_servers(servers).await?;
                Ok(())
            });
            let _: Result<()> = task.await;
            handle.update(cx, |_this, cx| {
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }

    pub fn upsert_server(&mut self, mut server: RedisServer, cx: &mut Context<Self>) {
        if server.id.is_empty() {
            server.id = Uuid::now_v7().to_string();
        }
        server.updated_at = Some(Local::now().to_string());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if server.name.is_empty() {
                    return Err(Error::Invalid {
                        message: "Server name is required".to_string(),
                    });
                }
                let mut servers = get_servers()?;
                if let Some(existing_server) = servers.iter_mut().find(|s| s.id == server.id) {
                    // Preserve the existing sort_order on update unless
                    // the caller explicitly supplied one (reorder
                    // buttons set it; the edit form leaves it None).
                    if server.sort_order.is_none() {
                        server.sort_order = existing_server.sort_order;
                    }
                    *existing_server = server;
                } else {
                    // New server: append to the tail of its group by
                    // assigning max(sort_order)+1 within that group.
                    if server.sort_order.is_none() {
                        let new_group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
                        let next = servers
                            .iter()
                            .filter(|s| s.group.as_deref().map(str::trim).filter(|g| !g.is_empty()) == new_group)
                            .filter_map(|s| s.sort_order)
                            .max()
                            .map(|m| m + 1)
                            .unwrap_or(0);
                        server.sort_order = Some(next);
                    }
                    servers.push(server);
                }
                save_servers(servers.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;

            handle.update(cx, |_this, cx| {
                if let Err(e) = &result {
                    error!(error = %e, "Failed to upsert server");
                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                        e.to_string().into(),
                    )));
                    return;
                }
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }

    /// Insert or update **multiple** servers in one atomic read-modify-save.
    ///
    /// Calling [`Self::upsert_server`] in a loop races: each call is an
    /// independent detached task that reads the whole list, appends one entry,
    /// and writes the list back — so concurrent saves clobber each other and
    /// only one entry survives. Batching reads the list once and saves once.
    pub fn upsert_servers(&mut self, servers: Vec<RedisServer>, cx: &mut Context<Self>) {
        if servers.is_empty() {
            return;
        }
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut current = get_servers()?;
                for mut server in servers {
                    // Skip nameless entries rather than abort the whole batch.
                    if server.name.is_empty() {
                        continue;
                    }
                    if server.id.is_empty() {
                        server.id = Uuid::now_v7().to_string();
                    }
                    server.updated_at = Some(Local::now().to_string());
                    if let Some(existing) = current.iter_mut().find(|s| s.id == server.id) {
                        if server.sort_order.is_none() {
                            server.sort_order = existing.sort_order;
                        }
                        *existing = server;
                    } else {
                        // Append to the tail of its group; `sort_order` is
                        // computed against the in-progress list so a batch
                        // gets sequential indices.
                        if server.sort_order.is_none() {
                            let new_group = server.group.as_deref().map(str::trim).filter(|s| !s.is_empty());
                            let next = current
                                .iter()
                                .filter(|s| s.group.as_deref().map(str::trim).filter(|g| !g.is_empty()) == new_group)
                                .filter_map(|s| s.sort_order)
                                .max()
                                .map(|m| m + 1)
                                .unwrap_or(0);
                            server.sort_order = Some(next);
                        }
                        current.push(server);
                    }
                }
                save_servers(current.clone()).await?;
                Ok(())
            });
            let result: Result<()> = task.await;

            handle.update(cx, |_this, cx| {
                if let Err(e) = &result {
                    error!(error = %e, "Failed to upsert servers");
                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                        e.to_string().into(),
                    )));
                    return;
                }
                cx.emit(GlobalEvent::ServerListUpdated);
                cx.notify();
            })
        })
        .detach();
    }
}

/// Update app state in background, persist to disk, and refresh UI
///
/// This helper function abstracts the common pattern for updating global state:
/// 1. Apply mutation to app state
/// 2. Save updated state to disk asynchronously
/// 3. Refresh all windows to apply changes
///
/// Used for theme and locale changes to ensure consistency across the app.
///
/// # Arguments
/// * `cx` - Context for spawning async tasks
/// * `action_name` - Human-readable action name for logging
/// * `mutation` - Callback to modify the app state
#[inline]
pub fn update_app_state_and_save<F>(cx: &App, action_name: &'static str, mutation: F)
where
    F: FnOnce(&mut ZedisAppState, &App) + Send + 'static + Clone,
{
    let store = cx.global::<ZedisGlobalStore>().clone();

    cx.spawn(async move |cx| {
        // Step 1: Update global state with the mutation
        let state = store.update(cx, |state, cx| {
            mutation(state, cx);
            state.clone() // Return clone for async persistence
        });

        // Step 2: Persist to disk in background executor
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = save_app_state(&state) {
                    error!(error = %e, action = action_name, "Failed to save state");
                } else {
                    info!(action = action_name, "State saved successfully");
                }
            })
            .await;

        // Step 3: Refresh windows to apply visual changes (theme/locale)
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

pub fn dialog_button_props(cx: &App) -> DialogButtonProps {
    DialogButtonProps::default()
        .cancel_text(i18n_common(cx, "cancel"))
        .ok_text(i18n_common(cx, "delete"))
}

/// Escalate a destructive-action confirm-dialog body for production servers.
///
/// The app's safety convention is that destructive Redis ops escalate their
/// *wording* on a high-risk (PROD-tagged) connection. `dialog_button_props`
/// only sets button labels, so call-sites that build their own
/// `ZedisDialog::new_alert(...)` must run the body through this to actually
/// get the escalation. It mirrors the `high_risk_warning` suffix that
/// `confirm_dangerous_command` appends for `ConfirmStrictness::TypeName`, but
/// works for the many UI actions (key/server delete, XGROUP DESTROY, cluster
/// ops, ACL/FUNCTION delete, ...) that don't map to a CLI `DangerKind`.
/// Returns the body unchanged for non-high-risk servers.
pub fn escalate_dangerous_body(cx: &App, server_id: &str, body: impl Into<SharedString>) -> SharedString {
    let body = body.into();
    let high_risk = get_server(server_id).map(|s| s.is_high_risk_tag()).unwrap_or(false);
    if !high_risk {
        return body;
    }
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let warning = rust_i18n::t!("danger.high_risk_warning", locale = &locale).to_string();
    format!("{body}\n\n{warning}").into()
}

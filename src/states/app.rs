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

use crate::connection::ServerCommand;
#[cfg(not(target_family = "wasm"))]
use crate::connection::{
    DEFAULT_CONNECTION_TIMEOUT, DEFAULT_RESPONSE_TIMEOUT, set_redis_connection_timeout, set_redis_response_timeout,
};
use crate::connection::{
    DEFAULT_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT, MIN_KEY_SCAN_COUNT, RedisServer, ReplyFormat, get_server, get_servers,
    save_servers,
};
use crate::constants::{SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH};
use crate::error::Error;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::set_configured_proxy;
use crate::helpers::{
    ConfigRecovery, DEFAULT_UI_FONT_SIZE, UpdateInfo, get_key_tree_widths, load_config_with_recovery,
};
use crate::helpers::{DEFAULT_DATE_FORMAT, TimeZonePref};
use crate::startup::is_nightly_build;
use crate::states::i18n_common;
use gpui::{Action, App, AppContext, Bounds, Context, Entity, EventEmitter, Global, Pixels, SharedString, Window};
use gpui_kit::component::ThemeMode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use sys_locale::get_locale;
use tracing::{error, warn};
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

// The route types, persistence helpers, update/hint state, and server-list
// operations are their own files; all remain under `states::app` to callers.
mod persistence;
mod route;
mod servers;
mod updates;
use persistence::{app_state_path, seal_secret, timeout_text, unseal_secret};
pub use persistence::{
    content_area_width, dialog_button_props, escalate_dangerous_body, flush_app_state_on_quit, save_app_state,
    update_app_state_and_save, update_app_state_and_save_debounced, update_app_state_and_save_quiet,
    update_app_state_and_save_quiet_debounced,
};
pub use route::{Route, ServerView};

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
    Hotkeys,
    ValueSearch,
    ServerInfo,
    Benchmark,
    Workbench,
    TimeSeriesExplorer,
    /// Opens the local recycle-bin dialog (soft-deleted keys) instead of a
    /// sub-route — handled specially in `main.rs`.
    Trash,
    /// Opens the key dump import window for the current server / db
    /// (RESTORE) — handled specially in `main.rs`, not a sub-route.
    ImportKeys,
    /// Opens the export window for every key currently loaded in the
    /// active connection's tree — the selection-free Tools-menu entry.
    ExportKeys,
    /// Compare the keys under a prefix with another server / db, in a
    /// dedicated window.
    CompareKeys,
    /// Switches the editor area into Pub/Sub (channel) mode — same as the
    /// key tree's "Pubsub Mode" menu item, surfaced in the Tools menu.
    PubsubMode,
    /// `FLUSHDB` on the active connection — opens the destructive-command
    /// confirm first, like every other flush path. Not a sub-route.
    FlushDb,
    /// `FLUSHALL` on the active connection — every database on the instance.
    FlushAll,
    /// The probed command matrix for the active connection (a dialog).
    Capabilities,
    /// A troubleshooting report of the active connection, to copy or save
    /// (a dialog — ADR 19).
    TroubleshootingReport,
}

/// How the editor opens a JSON value: as its text, or as a tree. Each key
/// opens this way; the Text / Tree switch then changes the one on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JsonView {
    #[default]
    Text,
    Tree,
}

impl JsonView {
    pub const ALL: [JsonView; 2] = [JsonView::Text, JsonView::Tree];

    pub fn from_name(name: &str) -> Self {
        match name {
            "tree" => JsonView::Tree,
            _ => JsonView::Text,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            JsonView::Text => "text",
            JsonView::Tree => "tree",
        }
    }
}

const LIGHT_THEME_MODE: &str = "light";
const DARK_THEME_MODE: &str = "dark";

/// Keys for the one-time onboarding hints (`dismissed_hints`). Each hint
/// shows at most once, ever; the key is persisted the first time it fires.
/// Route query key that asks the connections page to open one entry's form;
/// its value is the server id (`ZedisAppState::edit_server`).
pub const EDIT_SERVER_QUERY: &str = "edit";
/// Route query key that asks the connections page to open the add form
/// pre-filled from one entry; its value is the server id
/// (`ZedisAppState::clone_server`).
pub const CLONE_SERVER_QUERY: &str = "clone";
pub const HINT_WELCOME: &str = "welcome";
/// The shortcut / tips card under the editor's "no key selected" screen,
/// once its close button has been used.
pub const HINT_EDITOR_GUIDE: &str = "editor_guide";
pub const HINT_FIRST_CONNECT: &str = "first_connect";
pub const HINT_TOPOLOGY: &str = "topology";
pub const HINT_MEMORY_ANALYSIS: &str = "memory_analysis";

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
    /// Ask the workspace root (`main.rs`) to open the connection in a content
    /// tab. The `bool` forces a fresh tab: `false` re-activates the tab already
    /// bound to `(id, db)` when one exists (reveal-or-open); `true` always
    /// appends a new tab, even alongside an existing one.
    ServerOpenInNewTab(SharedString, usize, bool),
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
    /// In the browser: what other accounts changed was brought in from the
    /// bridge (ADR 17) — the key notes of these servers, the saved scripts,
    /// and the account's own lists (favorites, recent keys, search history)
    /// from another browser. Never sent on the desktop.
    PageDataRefreshed {
        key_notes_of: Vec<String>,
        rows: bool,
        scripts: bool,
        /// The account's proto viewers, saved in another browser.
        protos: bool,
    },
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
    /// The window was maximized on this display; `bounds` then keeps the
    /// last *windowed* rectangle, which is what un-maximizing returns to.
    #[serde(default)]
    pub maximized: bool,
}

/// Persisted scope of the multi-database search palette (which
/// connections a search fans out to). Only ids are stored for the
/// explicit list — names/groups resolve live so renames don't break it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum MultiSearchScope {
    /// Search every open workspace tab's connection. (Alias keeps configs
    /// written by the pre-rename build parsing — a bad variant would fail
    /// the whole `ZedisAppState` deserialize and reset the config.)
    #[default]
    #[serde(alias = "ActiveTab")]
    OpenTabs,
    /// Search every connection in one server group (by group name).
    Group(String),
    /// Search an explicit set of connections (server ids).
    Servers(Vec<String>),
}

/// Persisted to `zedis.toml`. `#[serde(default)]` is the upgrade contract: a
/// file written by any earlier version — or a `.bak` restored by the config
/// recovery — must parse with whatever fields it has, so every new field
/// needs a `Default`, never a required value. `states/fixtures/` holds real
/// old files and `upgrade_fixtures` keeps this honest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// Persisted width of the kv-table entry panel — the right-hand
    /// preview/edit pane in the collection editors (Hash/List/Set/ZSet/
    /// Stream), user-resizable via the split handle. Global: a display
    /// preference, not a per-server setting (same policy as
    /// `key_tree_width`). `None` falls back to half the viewport.
    #[serde(default)]
    kv_edit_panel_width: Option<Pixels>,
    theme: Option<String>,
    /// Selected named theme from the registry (e.g. "Ayu Dark"). Takes
    /// precedence over the `theme` mode; `None` falls back to Light/Dark/System.
    theme_name: Option<String>,
    font_size: Option<FontSize>,
    /// UI font size in rem px from Settings (and ⌘+/⌘−). Takes precedence
    /// over the legacy `font_size` enum; `None` falls back to it, then to
    /// gpui's 16px default. Additive so old configs migrate silently.
    font_rem_px: Option<f32>,
    /// User-chosen UI font family (all non-mono text). Empty/None ⇒ the OS
    /// system UI font. A single family name, never a comma-separated stack.
    ui_font_family: Option<String>,
    /// User-chosen monospace font family (code / tables / terminal). Empty/None
    /// ⇒ the bundled JetBrains Mono.
    mono_font_family: Option<String>,
    max_key_tree_depth: Option<usize>,
    auto_expand_threshold: Option<usize>,
    key_scan_count: Option<usize>,
    /// Multi-database search palette: which connections to search.
    /// Persisted so the palette reopens with the last scope.
    #[serde(default)]
    multi_search_scope: MultiSearchScope,
    /// Multi-database search palette: per-server SCAN result cap.
    multi_search_scan_count: Option<usize>,
    /// Value search: keys examined per search before stopping (`None` ⇒
    /// the 10k default). The panel's other guardrails (value-size and
    /// container-element skip thresholds) stay fixed — they protect the
    /// server, not the search depth.
    value_search_scan_cap: Option<usize>,
    /// Value search: wall-clock budget in seconds (`None` ⇒ 10s).
    value_search_time_budget_secs: Option<u64>,
    /// Value search: matches kept before truncating (`None` ⇒ 500).
    value_search_max_matches: Option<usize>,
    /// One-shot handoff from the multi-database search palette: once the
    /// clicked hit's `(server, db)` connects, the editor selects this key.
    /// Runtime only.
    #[serde(skip)]
    multi_search_pending_key: Option<(String, usize, String)>,
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
    /// Workspace tabs from the last session — every tab's `(server_id, db)`
    /// in strip order (tabs still on Home are skipped). Restored at startup;
    /// `selected_server` decides which of them starts active. Empty for the
    /// everyday single-tab session, so the config stays unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    open_tabs: Vec<(String, usize)>,
    tray_enabled: Option<bool>,
    /// When `true` (default), deleting a single key first stashes its DUMP
    /// payload into the local recycle bin (24h retention) so it can be
    /// restored. `false` deletes permanently, as before.
    soft_delete: Option<bool>,
    /// When `true`, the key tree fetches TTL per key during SCAN and shows
    /// a TTL chip next to each leaf. When `false` the TTL pipeline command
    /// is skipped (cheaper scans on large dbs) and no chip is rendered.
    /// Defaults to `true`.
    show_key_tree_ttl: Option<bool>,
    /// Whether the key tree shows keys as a flat list (`true`) instead of a
    /// nested folder tree (unset / `false`, the default).
    key_tree_flat_view: Option<bool>,
    /// When `true`, the Live Monitor shows the commands Zedis's own
    /// connections send (the heartbeat's `INFO`, the key tree's `SCAN`…)
    /// among everyone else's. Defaults to `false`: on a quiet server they
    /// were most of the feed (#241).
    monitor_show_own_commands: Option<bool>,
    /// When `true`, clicking a server in the sidebar opens it in a workspace
    /// tab (reveal-or-open) instead of switching the current tab in place.
    /// ⌘/Ctrl+click inverts this per click; ⌘⇧+click always duplicates.
    /// Defaults to `false`.
    sidebar_click_new_tab: Option<bool>,
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
    /// How the terminal renders replies (`text` / `table` / `json`, see
    /// `ReplyFormat::as_str`). Unset = text.
    terminal_reply_format: Option<String>,
    /// Outbound HTTP proxy for the app's own requests (update check, AI).
    /// `None`/empty follows the environment / OS system proxy; `"none"`
    /// forces a direct connection; anything else is a proxy URI. Mirrored
    /// into `helpers::proxy` (background HTTP callers have no `cx`).
    http_proxy: Option<String>,
    /// When `true` (default), check GitHub for a newer release on startup,
    /// throttled to [`updates::UPDATE_CHECK_INTERVAL`]. `false` disables the network
    /// check entirely.
    auto_update_check: Option<bool>,
    /// Follow pre-releases and the nightly build as well as stable releases.
    /// Unset = on for a nightly build, off otherwise.
    update_prerelease: Option<bool>,
    /// Zone every rendered timestamp uses: `"local"` (default) or `"utc"`.
    time_zone: Option<String>,
    /// Date + time layout id (`helpers::DATE_FORMATS`); unset = ISO-like.
    date_format: Option<String>,
    /// How a JSON value opens in the editor: `"text"` (default) or `"tree"`
    /// ([`JsonView`]).
    json_view: Option<String>,
    /// The value diff's layout as it was last left: one column (`true`) or
    /// side by side (unset / `false`, the default).
    diff_unified: Option<bool>,
    /// How the server list orders cards inside each group. `None` keeps the
    /// hand-made order the ↑/↓ buttons produce, which stays the default —
    /// a list someone arranged themselves should not be re-sorted for them.
    server_sort: Option<String>,
    /// Unix seconds of the last update check, used to throttle the startup
    /// check to one per [`updates::UPDATE_CHECK_INTERVAL`].
    last_update_check: Option<i64>,
    /// A version the user chose to skip (e.g. `"0.5.0"`). Suppresses the silent
    /// startup prompt for exactly that version; a manual check ignores it.
    skipped_version: Option<String>,
    /// One-time onboarding hints (welcome card, first-connect toast, panel
    /// banners) the user has already seen — see the `HINT_*` constants.
    dismissed_hints: Option<Vec<String>>,
    /// A newer release found by a check, awaiting the user's action. Runtime
    /// only (never persisted) — it's re-discovered on the next check. Drives
    /// the status-bar update chip; the chip's click opens the prompt for it.
    #[serde(skip)]
    available_update: Option<UpdateInfo>,
    /// Installer download progress as `(downloaded, total)` bytes while an
    /// update is downloading; `None` when idle. Runtime only — the update
    /// popover renders the bar plus "6.2 MB / 13.2 MB", the title-bar chip
    /// renders just the percentage (see `download_percent`).
    #[serde(skip)]
    download_progress: Option<(u64, u64)>,
    /// True while an update check (network fetch) is in flight. Runtime only —
    /// drives the title-bar update chip's loading spinner.
    #[serde(skip)]
    update_checking: bool,
    /// Whether the update chip's popover (the notes, Skip, Update and
    /// restart) is open. Runtime only; see [`Self::update_prompt_open`].
    #[serde(skip)]
    update_prompt_open: bool,
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

/// The bundled UI locales — one `locales/<lang>.toml` each.
pub const SUPPORTED_LOCALES: [&str; 8] = ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"];

/// The bundled locale for an OS language tag — `zh-Hans-CN`, `en_US`,
/// `pt-BR`, `de_DE.UTF-8`: the language subtag decides, region and script
/// are ignored, and a language without a bundle falls back to English.
pub fn language_from_system_locale(tag: Option<&str>) -> &'static str {
    let lang = tag
        .unwrap_or_default()
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    SUPPORTED_LOCALES
        .iter()
        .copied()
        .find(|bundled| *bundled == lang)
        .unwrap_or("en")
}

/// The OS language as a bundled locale, read once per process.
fn system_language() -> &'static str {
    static LANGUAGE: OnceLock<&'static str> = OnceLock::new();
    LANGUAGE.get_or_init(|| language_from_system_locale(get_locale().as_deref()))
}

impl ZedisAppState {
    pub fn try_new() -> Result<Self> {
        let path = app_state_path()?;
        // A damaged file is quarantined and the `.bak` restored (or defaults
        // used) — never parsed-as-empty, which the next save would then
        // write over the user's preferences. The recovery is reported in the
        // UI once the window is up (`take_config_recoveries`).
        let loaded = load_config_with_recovery(&path, |text| toml::from_str::<Self>(text).map_err(|e| e.to_string()))?;
        match &loaded.recovery {
            Some(ConfigRecovery::RestoredFromBackup { corrupt_path, .. }) => {
                warn!(corrupt = %corrupt_path.display(), "zedis.toml was unreadable; restored from backup")
            }
            Some(ConfigRecovery::Reset { corrupt_path, .. }) => {
                error!(corrupt = %corrupt_path.display(), "zedis.toml was unreadable and no backup parsed; reset to defaults")
            }
            None => {}
        }
        let mut state = loaded.value.unwrap_or_default();
        // First launch, or a cleared preference: follow the OS language,
        // clamped to a bundled locale. The raw language tag used to be
        // stored verbatim, so an unsupported one ("ko") rendered English
        // through the fallback while the language menu showed nothing.
        if state.locale.as_deref().is_none_or(|l| l.trim().is_empty()) {
            state.locale = Some(system_language().to_string());
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

        // The timeouts govern the dial, which the bridge does on the web.
        #[cfg(not(target_family = "wasm"))]
        {
            // A zero saved by an older build is "not set", not "time out at
            // once" — see `set_redis_connection_timeout`.
            if let Some(redis_connection_timeout) = state.redis_connection_timeout.filter(|t| !t.is_zero()) {
                set_redis_connection_timeout(redis_connection_timeout);
            }
            if let Some(redis_response_timeout) = state.redis_response_timeout.filter(|t| !t.is_zero()) {
                set_redis_response_timeout(redis_response_timeout);
            }
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
        Some((width - self.sidebar_px().as_f32() - key_tree_width.as_f32()).into())
    }
    /// The sidebar's *current* width — the icon rail is far narrower than the
    /// expanded panel, so anything sizing itself against the sidebar has to ask
    /// rather than assume [`SIDEBAR_WIDTH`].
    pub fn sidebar_px(&self) -> Pixels {
        if self.sidebar_collapsed() {
            SIDEBAR_COLLAPSED_WIDTH
        } else {
            SIDEBAR_WIDTH
        }
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
    /// Route with a query the target page reads once: `new` (the tray) and
    /// `edit=<server id>` (the editor's link panel) open the connection form
    /// on the connections page.
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
    /// Effective UI font size in rem px: the Settings value if set, else the
    /// legacy `font_size` enum's pixels, else `None` (falls through to
    /// `Theme::font_size`, which Zedis pins to [`crate::helpers::DEFAULT_UI_FONT_SIZE`]).
    pub fn font_rem_px(&self) -> Option<f32> {
        self.font_rem_px
            .or_else(|| self.font_size.and_then(FontSize::to_pixels))
    }
    pub fn set_font_rem_px(&mut self, px: Option<f32>) {
        // A value equal to the pinned default carries no information — store None
        // so the config stays clean and the rem falls back to
        // DEFAULT_UI_FONT_SIZE via the theme (gpui-component's `Root` sets the rem
        // base from `theme.font_size` each frame). Mirrors the max_key_tree_depth
        // "0 ⇒ None" reset. Epsilon compare since the control steps in whole px
        // and clippy forbids `==` on f32.
        self.font_rem_px = px.filter(|v| (*v - DEFAULT_UI_FONT_SIZE).abs() >= f32::EPSILON);
    }
    /// UI font family, `None` when unset or blank (falls back to system).
    pub fn ui_font_family(&self) -> Option<String> {
        self.ui_font_family.clone().filter(|s| !s.trim().is_empty())
    }
    pub fn set_ui_font_family(&mut self, name: Option<String>) {
        self.ui_font_family = name.filter(|s| !s.trim().is_empty());
    }
    /// Monospace font family, `None` when unset or blank (falls back to
    /// bundled JetBrains Mono).
    pub fn mono_font_family(&self) -> Option<String> {
        self.mono_font_family.clone().filter(|s| !s.trim().is_empty())
    }
    pub fn set_mono_font_family(&mut self, name: Option<String>) {
        self.mono_font_family = name.filter(|s| !s.trim().is_empty());
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
    /// `None` — an empty field, or a zero — means the default. A zero used to
    /// be kept as a timeout of zero, which made every dial fail at once; and
    /// clearing the field stored "not set" while the running process kept
    /// the old value until a restart, so the default is applied here too.
    pub fn set_redis_connection_timeout(&mut self, redis_connection_timeout: Option<Duration>) {
        let redis_connection_timeout = redis_connection_timeout.filter(|t| !t.is_zero());
        #[cfg(not(target_family = "wasm"))]
        set_redis_connection_timeout(redis_connection_timeout.unwrap_or(DEFAULT_CONNECTION_TIMEOUT));
        self.redis_connection_timeout = redis_connection_timeout;
    }
    /// The same for the per-command timeout.
    pub fn set_redis_response_timeout(&mut self, redis_response_timeout: Option<Duration>) {
        let redis_response_timeout = redis_response_timeout.filter(|t| !t.is_zero());
        #[cfg(not(target_family = "wasm"))]
        set_redis_response_timeout(redis_response_timeout.unwrap_or(DEFAULT_RESPONSE_TIMEOUT));
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
    /// The UI language: the saved choice when it is a bundled locale, else
    /// the OS language clamped to one (never a code the app can't render).
    pub fn locale(&self) -> &str {
        match self.locale.as_deref() {
            Some(locale) if SUPPORTED_LOCALES.contains(&locale) => locale,
            _ => system_language(),
        }
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
    pub fn upsert_window_placement(&mut self, mut placement: WindowPlacement) {
        // A maximized window reports the display rectangle as its bounds;
        // keep the windowed rectangle we already had for that display, so a
        // later un-maximize (and the next launch, once un-maximized) lands
        // where the user left the window.
        if placement.maximized
            && let Some(previous) = self
                .window_placements
                .iter()
                .find(|p| p.display_uuid == placement.display_uuid)
        {
            placement.bounds = previous.bounds;
        }
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
        // `None` is the live default, not an empty field. The constant
        // lives in `zedis-connection` but is native-only (wasm has no
        // dialer), so the number is spelled here too.
        timeout_text(self.redis_connection_timeout.unwrap_or(Duration::from_secs(10)))
    }
    pub fn redis_response_timeout(&self) -> String {
        timeout_text(self.redis_response_timeout.unwrap_or(Duration::from_secs(20)))
    }
    pub fn key_scan_count(&self) -> usize {
        self.key_scan_count
            .unwrap_or(DEFAULT_KEY_SCAN_COUNT)
            .clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT)
    }
    /// The count chosen in Settings, `None` while it is left at its default
    /// — which is not one number: a production-tagged server has a smaller
    /// one (`RedisServer::resolve_key_scan_count`).
    pub fn key_scan_count_setting(&self) -> Option<usize> {
        self.key_scan_count
    }
    pub fn multi_search_scope(&self) -> MultiSearchScope {
        self.multi_search_scope.clone()
    }
    pub fn set_multi_search_scope(&mut self, scope: MultiSearchScope) {
        self.multi_search_scope = scope;
    }
    pub fn multi_search_scan_count(&self) -> usize {
        // Per-server result cap. Defaults low: the palette's primary path
        // is the exact lookup; SCAN is discovery, and 10 hits per server
        // is usually enough to spot the right key — raise it (persisted)
        // when it isn't.
        self.multi_search_scan_count.unwrap_or(10)
    }
    pub fn set_multi_search_scan_count(&mut self, count: usize) {
        // 0 = "reset to default" (cleared input); otherwise clamp to a
        // sane per-server range so a typo can't turn SCAN into a flood.
        self.multi_search_scan_count = if count == 0 { None } else { Some(count.clamp(10, 5_000)) };
    }
    /// Arm the "select this key once its server connects" handoff.
    pub fn set_multi_search_pending_key(&mut self, server_id: String, db: usize, key: String) {
        self.multi_search_pending_key = Some((server_id, db, key));
    }
    /// Consume the pending selection if it matches the connection that
    /// just came up; a mismatch leaves it armed (another select is
    /// still in flight) — it's runtime-only state either way.
    pub fn take_multi_search_pending_key(&mut self, server_id: &str, db: usize) -> Option<String> {
        match &self.multi_search_pending_key {
            Some((sid, sdb, _)) if sid == server_id && *sdb == db => {
                self.multi_search_pending_key.take().map(|(_, _, key)| key)
            }
            _ => None,
        }
    }
    pub fn value_search_scan_cap(&self) -> usize {
        self.value_search_scan_cap.unwrap_or(10_000)
    }
    pub fn set_value_search_scan_cap(&mut self, cap: usize) {
        // 0 = "reset to default" (cleared input); clamped so a typo can't
        // turn one search into an unbounded keyspace walk.
        self.value_search_scan_cap = if cap == 0 {
            None
        } else {
            Some(cap.clamp(1_000, 1_000_000))
        };
    }
    pub fn value_search_time_budget_secs(&self) -> u64 {
        self.value_search_time_budget_secs.unwrap_or(10)
    }
    pub fn set_value_search_time_budget_secs(&mut self, secs: u64) {
        self.value_search_time_budget_secs = if secs == 0 { None } else { Some(secs.clamp(3, 300)) };
    }
    pub fn value_search_max_matches(&self) -> usize {
        self.value_search_max_matches.unwrap_or(500)
    }
    pub fn set_value_search_max_matches(&mut self, max: usize) {
        self.value_search_max_matches = if max == 0 { None } else { Some(max.clamp(50, 10_000)) };
    }
    pub fn set_key_scan_count(&mut self, key_scan_count: usize) {
        // 0 means "reset to default" (cleared input) — store None so the
        // getter's default applies. Anything else is clamped so a typo
        // cannot ask for a 5-key SCAN page or the whole keyspace.
        if key_scan_count == 0 {
            self.key_scan_count = None;
            return;
        }
        self.key_scan_count = Some(key_scan_count.clamp(MIN_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT));
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
    pub fn sidebar_click_new_tab(&self) -> bool {
        self.sidebar_click_new_tab.unwrap_or(false)
    }
    pub fn set_sidebar_click_new_tab(&mut self, enabled: bool) {
        self.sidebar_click_new_tab = Some(enabled);
    }
    /// Whether a delete puts the key in the recycle bin first — the local
    /// database's on the desktop, the bridge's in the browser (`trash_store`).
    pub fn soft_delete(&self) -> bool {
        self.soft_delete.unwrap_or(true)
    }
    pub fn set_soft_delete(&mut self, enabled: bool) {
        self.soft_delete = Some(enabled);
    }
    pub fn show_key_tree_ttl(&self) -> bool {
        self.show_key_tree_ttl.unwrap_or(true)
    }
    pub fn set_show_key_tree_ttl(&mut self, enabled: bool) {
        self.show_key_tree_ttl = Some(enabled);
    }
    /// Whether the key tree opens in flat-list mode, as it was last left.
    pub fn key_tree_flat_view(&self) -> bool {
        self.key_tree_flat_view.unwrap_or(false)
    }
    pub fn set_key_tree_flat_view(&mut self, flat: bool) {
        self.key_tree_flat_view = Some(flat);
    }
    pub fn monitor_show_own_commands(&self) -> bool {
        self.monitor_show_own_commands.unwrap_or(false)
    }
    pub fn set_monitor_show_own_commands(&mut self, enabled: bool) {
        self.monitor_show_own_commands = Some(enabled);
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
    /// The persisted proxy setting (decrypted when stored encrypted), or
    /// empty when following the system. Falls back to the stored value if
    /// decryption fails (e.g. a hand-edited plaintext URI in `zedis.toml`).
    /// `zedis.toml` as it would be saved, with the two secrets — the AI API
    /// key and any proxy credentials — replaced, for the diagnostics bundle.
    pub fn redacted_toml(&self) -> Result<String> {
        let mut copy = self.clone();
        copy.ai_api_key = copy.ai_api_key.as_ref().map(|_| "<redacted>".to_string());
        // Stored encrypted, but a `user:pass@` proxy is still a secret.
        copy.http_proxy = copy.http_proxy.as_ref().map(|_| "<redacted>".to_string());
        Ok(toml::to_string(&copy)?)
    }

    pub fn http_proxy(&self) -> String {
        self.http_proxy.as_deref().map(unseal_secret).unwrap_or_default()
    }
    /// Persist the proxy setting and mirror the plaintext into
    /// `helpers::proxy` so the next HTTP request (updater / AI, background
    /// threads without `cx`) picks it up immediately. A URI carrying
    /// credentials (`user:pass@` userinfo) is encrypted like `ai_api_key`;
    /// a credential-free address stays readable in `zedis.toml`.
    pub fn set_http_proxy(&mut self, value: String) {
        let value = value.trim().to_string();
        #[cfg(not(target_family = "wasm"))]
        set_configured_proxy(&value);
        let stored = if value.contains('@') {
            seal_secret(&value)
        } else {
            value.clone()
        };
        self.http_proxy = if value.is_empty() { None } else { Some(stored) };
    }

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
        self.ai_api_key.as_deref().map(unseal_secret).unwrap_or_default()
    }
    /// Store the API key, encrypting it before persistence. An empty
    /// value clears it.
    pub fn set_ai_api_key(&mut self, api_key: String) {
        let api_key = api_key.trim();
        // Sealed only when there is a key to keep: clearing it needs no
        // master key, which on a machine where nothing else asked for one
        // would be the session's first keychain read.
        self.ai_api_key = if api_key.is_empty() {
            None
        } else {
            Some(seal_secret(api_key))
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
    /// Whether the AI features have the minimum configuration to be
    /// usable: an endpoint. The key is optional — a local endpoint (Ollama,
    /// LM Studio, vLLM without auth) takes none, and used to need a fake
    /// one typed in just to pass this check.
    pub fn ai_configured(&self) -> bool {
        self.ai_base_url.is_some()
    }
    /// The terminal's reply rendering; text unless a known name is stored.
    pub fn terminal_reply_format(&self) -> ReplyFormat {
        self.terminal_reply_format
            .as_deref()
            .and_then(ReplyFormat::from_name)
            .unwrap_or_default()
    }
    pub fn set_terminal_reply_format(&mut self, format: ReplyFormat) {
        self.terminal_reply_format = Some(format.as_str().to_string());
    }
    /// Whether the app checks GitHub for a newer release on startup. Defaults
    /// to `true`; the user can disable it in Settings.
    pub fn auto_update_check(&self) -> bool {
        self.auto_update_check.unwrap_or(true)
    }
    pub fn set_auto_update_check(&mut self, enabled: bool) {
        self.auto_update_check = Some(enabled);
    }
    /// Whether the update check also considers pre-releases and the nightly
    /// build. A nightly build defaults to yes — the stable channel would
    /// never offer it anything.
    pub fn update_prerelease(&self) -> bool {
        self.update_prerelease.unwrap_or_else(is_nightly_build)
    }
    pub fn set_update_prerelease(&mut self, enabled: bool) {
        self.update_prerelease = Some(enabled);
    }
    pub fn time_zone(&self) -> TimeZonePref {
        self.time_zone
            .as_deref()
            .map(TimeZonePref::from_name)
            .unwrap_or_default()
    }
    pub fn set_time_zone(&mut self, zone: TimeZonePref) {
        self.time_zone = Some(zone.name().to_string());
    }
    pub fn date_format(&self) -> String {
        self.date_format
            .clone()
            .unwrap_or_else(|| DEFAULT_DATE_FORMAT.to_string())
    }
    pub fn set_date_format(&mut self, id: &str) {
        self.date_format = Some(id.to_string());
    }
    /// How a JSON value opens in the editor.
    pub fn json_view(&self) -> JsonView {
        self.json_view.as_deref().map(JsonView::from_name).unwrap_or_default()
    }
    pub fn set_json_view(&mut self, view: JsonView) {
        self.json_view = Some(view.name().to_string());
    }
    /// Whether the value diff opens as one column, as it was last left.
    pub fn diff_unified(&self) -> bool {
        self.diff_unified.unwrap_or(false)
    }
    pub fn set_diff_unified(&mut self, unified: bool) {
        self.diff_unified = Some(unified);
    }
    /// Server-list order id — see `ServerSort` in the servers view.
    pub fn server_sort(&self) -> String {
        self.server_sort.clone().unwrap_or_else(|| "manual".to_string())
    }
    pub fn set_server_sort(&mut self, id: &str) {
        self.server_sort = Some(id.to_string());
    }
}

/// Navigation / connection-selection logic tests on the headless gpui test
/// platform. These lock in the event contracts that broke in past releases:
/// the tray "Quick Connect does nothing" and "New Connection dialog doesn't
/// open" regressions were both silent logic bugs in this file.
/// Real `zedis.toml` files from earlier releases. Every one must keep
/// parsing — an upgrade that resets a user's preferences is a regression —
/// and values that still exist must survive. Add a fixture whenever a
/// release changes the persisted shape.
#[cfg(test)]
mod upgrade_fixtures {

    #[test]
    fn a_timeout_reads_back_as_what_was_typed() {
        use std::time::Duration;
        assert_eq!(super::timeout_text(Duration::from_secs(10)), "10");
        assert_eq!(super::timeout_text(Duration::from_millis(500)), "500ms");
        assert_eq!(super::timeout_text(Duration::from_millis(1500)), "1s 500ms");
    }

    use super::*;
    use gpui::px;

    const V0_4_0: &str = include_str!("fixtures/zedis-v0.4.0.toml");
    const V0_6_0: &str = include_str!("fixtures/zedis-v0.6.0.toml");

    fn load(fixture: &str) -> ZedisAppState {
        toml::from_str(fixture).expect("an old zedis.toml must keep parsing")
    }

    #[test]
    fn an_empty_file_parses_to_defaults() {
        // The contract behind `#[serde(default)]`: no field may ever be
        // required, or a `.bak` from before that field was added breaks.
        let state: ZedisAppState = toml::from_str("").expect("empty file");
        assert_eq!(state.locale, None);
        assert_eq!(state.key_tree_width, Pixels::ZERO);
        // Settings fields show the effective default, not a blank that
        // looks unset while the dialer still uses 10s / 20s.
        assert_eq!(state.redis_connection_timeout(), "10");
        assert_eq!(state.redis_response_timeout(), "20");
    }

    #[test]
    fn value_search_guardrails_clamp_and_reset() {
        let mut state: ZedisAppState = toml::from_str("").expect("empty file");
        assert_eq!(state.value_search_scan_cap(), 10_000);
        assert_eq!(state.value_search_time_budget_secs(), 10);
        assert_eq!(state.value_search_max_matches(), 500);

        // A typo can't turn a search into an unbounded walk — clamped.
        state.set_value_search_scan_cap(5);
        assert_eq!(state.value_search_scan_cap(), 1_000);
        state.set_value_search_time_budget_secs(9_999);
        assert_eq!(state.value_search_time_budget_secs(), 300);
        state.set_value_search_max_matches(2_000);
        assert_eq!(state.value_search_max_matches(), 2_000);

        // 0 (cleared input) resets to the defaults.
        state.set_value_search_scan_cap(0);
        state.set_value_search_time_budget_secs(0);
        state.set_value_search_max_matches(0);
        assert_eq!(state.value_search_scan_cap(), 10_000);
        assert_eq!(state.value_search_time_budget_secs(), 10);
        assert_eq!(state.value_search_max_matches(), 500);
    }

    #[test]
    fn key_scan_count_clamps_and_resets() {
        let mut state: ZedisAppState = toml::from_str("").expect("empty file");
        assert_eq!(state.key_scan_count(), DEFAULT_KEY_SCAN_COUNT);
        assert_eq!(state.key_scan_count_setting(), None);

        state.set_key_scan_count(5);
        assert_eq!(state.key_scan_count(), MIN_KEY_SCAN_COUNT);
        state.set_key_scan_count(1_000_000);
        assert_eq!(state.key_scan_count(), MAX_KEY_SCAN_COUNT);
        state.set_key_scan_count(5_000);
        assert_eq!(state.key_scan_count(), 5_000);

        state.set_key_scan_count(0);
        assert_eq!(state.key_scan_count(), DEFAULT_KEY_SCAN_COUNT);
        assert_eq!(state.key_scan_count_setting(), None);
    }

    #[test]
    fn v0_4_0_preferences_survive() {
        let state = load(V0_4_0);
        // 0.4 stored the `Route` enum; its unit variant is the token the
        // current loader resolves through `ServerView::from_name`.
        assert_eq!(state.route_token, "Editor");
        assert_eq!(state.locale.as_deref(), Some("zh"));
        assert_eq!(state.theme.as_deref(), Some("dark"));
        assert_eq!(state.font_size, Some(FontSize::Large));
        assert_eq!(state.key_tree_width, px(280.));
        assert_eq!(state.max_key_tree_depth, Some(6));
        assert_eq!(state.key_scan_count, Some(500));
        // The 0.4 file still holds 500; SCAN itself is clamped to the floor.
        assert_eq!(state.key_scan_count(), MIN_KEY_SCAN_COUNT);
        assert_eq!(state.selected_server, Some(("srv-1".to_string(), 2)));
        assert_eq!(state.redis_connection_timeout, Some(Duration::from_secs(10)));
        assert_eq!(state.tray_enabled, Some(true));
        assert_eq!(
            state.collapsed_server_groups.as_deref(),
            Some(&["Team A".to_string()][..])
        );
        let bounds = state.bounds.expect("bounds");
        assert_eq!(bounds.size.width, px(1200.));
        // Fields 0.4 didn't have fall back cleanly.
        assert!(state.window_placements.is_empty());
        assert!(state.last_db.is_empty());
        assert_eq!(state.multi_search_scope, MultiSearchScope::default());
        assert!(!state.key_tree_flat_view());
    }

    #[test]
    fn v0_6_0_preferences_survive() {
        let state = load(V0_6_0);
        assert_eq!(state.route_token, "metrics");
        assert_eq!(state.theme_name.as_deref(), Some("Catppuccin Latte"));
        assert_eq!(state.font_rem_px, Some(15.0));
        assert_eq!(state.ui_font_family.as_deref(), Some("Inter"));
        assert_eq!(state.kv_edit_panel_width, Some(px(420.)));
        assert_eq!(
            state.open_tabs,
            vec![("srv-2".to_string(), 0), ("srv-3".to_string(), 1)]
        );
        assert_eq!(state.last_db.get("srv-3"), Some(&1));
        assert_eq!(state.window_placements.len(), 1);
        assert_eq!(state.soft_delete, Some(true));
        assert_eq!(state.sidebar_collapsed, Some(false));
    }

    #[test]
    fn old_files_round_trip_through_the_current_writer() {
        // What the current version writes back must itself load — the
        // `.bak` a later recovery restores is exactly this output.
        for fixture in [V0_4_0, V0_6_0] {
            let state = load(fixture);
            let written = toml::to_string(&state).expect("serialize");
            let again: ZedisAppState = toml::from_str(&written).expect("re-parse");
            assert_eq!(again.locale, state.locale);
            assert_eq!(again.selected_server, state.selected_server);
            assert_eq!(again.key_tree_width, state.key_tree_width);
            assert_eq!(again.route_token, state.route_token);
        }
    }

    #[test]
    fn key_tree_flat_view_round_trips_through_toml() {
        let mut state = super::ZedisAppState::default();
        assert!(!state.key_tree_flat_view());

        state.set_key_tree_flat_view(true);
        let written = toml::to_string(&state).expect("serialize");
        let again: super::ZedisAppState = toml::from_str(&written).expect("re-parse");
        assert!(again.key_tree_flat_view());

        state.set_key_tree_flat_view(false);
        let written = toml::to_string(&state).expect("serialize");
        let again: super::ZedisAppState = toml::from_str(&written).expect("re-parse");
        assert!(!again.key_tree_flat_view());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::DeepLink;
    // Before any test triggers `persist_nav`: a stray background save must
    // never touch the real `zedis.toml`.
    use crate::test_support::{hold_the_proxy, isolate_config};
    use gpui::{Subscription, TestAppContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use zedis_connection::set_servers_cache;

    /// The two secrets the settings hold are kept encrypted — the proxy only
    /// when it carries credentials — and read back plain. A value that does
    /// not decrypt (typed into `zedis.toml` by hand) is read as it is, and an
    /// empty one clears the setting.
    #[test]
    fn a_secret_setting_is_kept_encrypted_and_read_back_plain() {
        isolate_config();
        // Setting the proxy sets the process's.
        let _proxy = hold_the_proxy();
        let mut state = ZedisAppState::default();

        state.set_ai_api_key("  sk-test  ".to_string());
        assert_ne!(
            state.ai_api_key.as_deref(),
            Some("sk-test"),
            "the key is kept encrypted"
        );
        assert_eq!(state.ai_api_key(), "sk-test");
        state.set_ai_api_key(String::new());
        assert_eq!(state.ai_api_key, None);

        state.set_http_proxy("http://user:secret@proxy:3128".to_string());
        let kept = state.http_proxy.clone().unwrap_or_default();
        assert!(
            !kept.contains("secret"),
            "a proxy's credentials are kept encrypted: {kept}"
        );
        assert_eq!(state.http_proxy(), "http://user:secret@proxy:3128");
        state.set_http_proxy("http://proxy:3128".to_string());
        assert_eq!(
            state.http_proxy.as_deref(),
            Some("http://proxy:3128"),
            "an address alone stays readable"
        );
        assert_eq!(state.http_proxy(), "http://proxy:3128");
        state.set_http_proxy(String::new());
        assert_eq!(state.http_proxy, None);

        state.ai_api_key = Some("typed-by-hand".to_string());
        assert_eq!(state.ai_api_key(), "typed-by-hand");
    }

    /// Recorded `(server_id, db)` of every `ServerSelected` emission.
    type SelectedRecorder = Rc<RefCell<Vec<(String, usize)>>>;

    /// A fresh state entity plus a recorder of every `ServerSelected` it
    /// emits (the event that actually loads a connection).
    fn state_with_recorder(cx: &mut TestAppContext) -> (Entity<ZedisAppState>, SelectedRecorder, Subscription) {
        isolate_config();
        let state = cx.new(|_| ZedisAppState::default());
        let events: SelectedRecorder = Rc::default();
        let recorder = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&state, move |_state, event: &GlobalEvent, _cx| {
                if let GlobalEvent::ServerSelected(id, db) = event {
                    recorder.borrow_mut().push((id.to_string(), *db));
                }
            })
        });
        (state, events, subscription)
    }

    #[gpui::test]
    fn connect_server_always_announces(cx: &mut TestAppContext) {
        let (state, events, _sub) = state_with_recorder(cx);
        // Restart scenario: the persisted snapshot already points at the
        // target server while nothing is loaded in this session. Relying on
        // apply_route's dedupe here was the "tray click does nothing" bug.
        state.update(cx, |state, _| state.selected_server = Some(("srv-1".to_string(), 2)));
        state.update(cx, |state, cx| state.connect_server("srv-1".to_string(), 2, cx));
        assert_eq!(events.borrow().as_slice(), &[("srv-1".to_string(), 2)]);
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_server(), Some(&("srv-1".to_string(), 2)));
            assert_eq!(state.open_db_for("srv-1"), 2);
            assert!(matches!(
                state.route(),
                Route::Server {
                    db: 2,
                    view: ServerView::Editor,
                    ..
                }
            ));
        });
        // Reconnecting the already-active server is still a (re)connect.
        state.update(cx, |state, cx| state.connect_server("srv-1".to_string(), 2, cx));
        assert_eq!(events.borrow().len(), 2);
    }

    #[gpui::test]
    fn navigation_does_not_reannounce_same_connection(cx: &mut TestAppContext) {
        let (state, events, _sub) = state_with_recorder(cx);
        state.update(cx, |state, cx| state.connect_server("srv-1".to_string(), 0, cx));
        assert_eq!(events.borrow().len(), 1);
        // App-level detour and back: same connection → no reload.
        state.update(cx, |state, cx| state.go_to(Route::Settings, cx));
        state.update(cx, |state, cx| {
            state.go_to(
                Route::Server {
                    id: "srv-1".into(),
                    db: 0,
                    view: ServerView::Metrics,
                },
                cx,
            );
        });
        assert_eq!(events.borrow().len(), 1);
        // Switching databases is a different connection → announce.
        state.update(cx, |state, cx| {
            state.go_to(
                Route::Server {
                    id: "srv-1".into(),
                    db: 3,
                    view: ServerView::Metrics,
                },
                cx,
            );
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[("srv-1".to_string(), 0), ("srv-1".to_string(), 3)]
        );
    }

    // Tray flow.
    #[gpui::test]
    fn new_connection_query_survives_clear(cx: &mut TestAppContext) {
        let (state, events, _sub) = state_with_recorder(cx);
        state.update(cx, |state, cx| state.connect_server("srv-1".to_string(), 0, cx));
        // Tray "New Connection": clear first, then route Home with new=true.
        // The reverse order lets clear_selected_server's Home routing wipe
        // the query (the "dialog doesn't open" bug).
        state.update(cx, |state, cx| {
            state.clear_selected_server(cx);
            let mut query = HashMap::new();
            query.insert("new".to_string(), "true".to_string());
            state.go_with_query(Route::Home, query, cx);
        });
        state.read_with(cx, |state, _| {
            assert_eq!(state.route(), Route::Home);
            assert_eq!(state.selected_server(), None);
            assert_eq!(
                state.get_route_query().and_then(|q| q.get("new")).map(String::as_str),
                Some("true")
            );
        });
        // The clear announced the empty selection (sidebar/status bar reset).
        assert_eq!(events.borrow().last(), Some(&(String::new(), 0)));
    }

    #[gpui::test]
    fn activate_always_announces_server_route(cx: &mut TestAppContext) {
        let (state, events, _sub) = state_with_recorder(cx);
        // Startup restore: the snapshot matches the restored route, but the
        // views subscribed after try_new — activate must still announce so
        // the connection actually loads.
        state.update(cx, |state, _| state.selected_server = Some(("srv-1".to_string(), 1)));
        state.update(cx, |state, cx| {
            state.activate(
                Route::Server {
                    id: "srv-1".into(),
                    db: 1,
                    view: ServerView::Editor,
                },
                cx,
            );
        });
        assert_eq!(events.borrow().as_slice(), &[("srv-1".to_string(), 1)]);
    }

    #[gpui::test]
    fn open_deep_link_resolves_server_and_queues_key(cx: &mut TestAppContext) {
        let (state, events, _sub) = state_with_recorder(cx);
        set_servers_cache(vec![RedisServer {
            id: "srv-prod".to_string(),
            name: "Prod Cache".to_string(),
            host: "127.0.0.1".to_string(),
            port: 6379,
            ..Default::default()
        }]);
        let notifications: Rc<RefCell<Vec<String>>> = Rc::default();
        let notif_recorder = notifications.clone();
        let _notif_sub = cx.update(|cx| {
            cx.subscribe(&state, move |_state, event: &GlobalEvent, _cx| {
                if let GlobalEvent::Notification(action) = event {
                    notif_recorder.borrow_mut().push(action.message.to_string());
                }
            })
        });

        // Case-insensitive name match + db + key + view
        state.update(cx, |state, cx| {
            state.open_deep_link(
                DeepLink {
                    server: "prod cache".to_string(),
                    db: Some(4),
                    key: Some("user:42".to_string()),
                    view: Some("editor".to_string()),
                },
                cx,
            );
        });
        assert_eq!(events.borrow().as_slice(), &[("srv-prod".to_string(), 4)]);
        state.update(cx, |state, _| {
            assert_eq!(
                state.take_multi_search_pending_key("srv-prod", 4).as_deref(),
                Some("user:42")
            );
        });

        // Unknown server emits a warning notification without changing selection
        state.update(cx, |state, cx| {
            state.open_deep_link(
                DeepLink {
                    server: "nonexistent".to_string(),
                    db: Some(0),
                    key: None,
                    view: None,
                },
                cx,
            );
        });
        assert_eq!(events.borrow().len(), 1);
        assert_eq!(notifications.borrow().len(), 1);
        assert!(notifications.borrow()[0].contains("nonexistent"));
    }

    /// The state is written asynchronously, so quitting used to be a race the
    /// user could lose. These drive the real shutdown path: `TestAppContext::quit`
    /// runs gpui's quit observers and blocks on their futures, same as a ⌘Q.
    mod quit_flush {
        use super::*;

        fn config_file() -> std::path::PathBuf {
            app_state_path().expect("config path")
        }

        fn file_text() -> String {
            std::fs::read_to_string(config_file()).unwrap_or_default()
        }

        #[gpui::test]
        fn quitting_writes_a_save_that_is_still_waiting_out_its_debounce(cx: &mut TestAppContext) {
            isolate_config();
            let probe = "quit-flush-probe";
            let state = cx.new(|_| ZedisAppState::default());

            cx.update(|cx| {
                cx.set_global(ZedisGlobalStore::new(state.clone()));
                flush_app_state_on_quit(cx);
                update_app_state_and_save_debounced(cx, "test", move |state, _| {
                    state.set_theme_name(Some(probe.to_string()));
                });
            });

            // The mutation lands immediately; the write is parked on the
            // debounce timer, which the test clock never advances.
            cx.run_until_parked();
            assert_eq!(
                state.read_with(cx, |s, _| s.theme_name()),
                Some(probe.to_string()),
                "the mutation itself is not debounced"
            );
            assert!(!file_text().contains(probe), "the write really is still pending");

            cx.quit();
            assert!(
                file_text().contains(probe),
                "quitting must flush the pending state, got: {}",
                file_text()
            );
        }

        #[gpui::test]
        fn quitting_without_a_store_is_not_a_panic(cx: &mut TestAppContext) {
            // The database-recovery window never installs the global store, and
            // quitting from there must still work.
            isolate_config();
            cx.update(|cx| flush_app_state_on_quit(cx));
            cx.quit();
        }
    }
}

#[cfg(test)]
mod system_locale_tests {
    use super::{SUPPORTED_LOCALES, language_from_system_locale};

    #[test]
    fn os_language_tags_map_to_bundled_locales() {
        assert_eq!(language_from_system_locale(Some("zh-Hans-CN")), "zh");
        assert_eq!(language_from_system_locale(Some("zh_TW")), "zh");
        assert_eq!(language_from_system_locale(Some("en-US")), "en");
        assert_eq!(language_from_system_locale(Some("pt-BR")), "pt");
        assert_eq!(language_from_system_locale(Some("de_DE.UTF-8")), "de");
        assert_eq!(language_from_system_locale(Some("JA")), "ja");
        // A language without a bundle, an empty tag, and no tag at all.
        assert_eq!(language_from_system_locale(Some("ko-KR")), "en");
        assert_eq!(language_from_system_locale(Some("")), "en");
        assert_eq!(language_from_system_locale(None), "en");
        for bundled in SUPPORTED_LOCALES {
            assert_eq!(language_from_system_locale(Some(bundled)), bundled);
        }
    }
}

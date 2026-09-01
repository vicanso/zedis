#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::connection::{DangerKind, clear_expired_cache, get_server, get_servers, servers_toml_redacted};
use crate::constants::{SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH};
use crate::db::{
    DbOpenFailure, LuaScriptManager, ProtoManager, ScriptManager, TRASH_RETENTION_MS, init_database, open_failure_kind,
    purge_all_trash, quarantine_database,
};
use crate::helpers::{
    ConfigRecovery, CrashContext, CrashReport, Delivery, DiagnosticsAction, DiagnosticsInput, EditorAction, MemuAction,
    MultiSearchAction, NavAction, PaletteAction, RecentKeysAction, ShortcutsAction, UpdateAction, UpdateInfo,
    WorkspaceTabAction, apply_default_ui_font_size, apply_fonts, download_and_verify, export_diagnostics,
    fetch_latest_release, focus_installer_ui, get_mono_font_family, get_or_create_config_dir, humanize_keystroke,
    init_logger, install_panic_hook, install_update, installer_requires_quit, is_app_store_build, logs_dir,
    new_hot_keys, register_extra_languages, set_configured_proxy, take_config_recoveries, take_pending_crash,
    unix_ts_millis, with_app_identity,
};
use crate::states::{
    GlobalEvent, HINT_WELCOME, LocaleAction, NotificationCategory, Route, SelectThemeAction, ServerToolsAction,
    ServerView, SettingsAction, ThemeAction, WindowPlacement, ZedisAppState, ZedisGlobalStore, flush_app_state_on_quit,
    i18n_common, i18n_crash, i18n_hints, i18n_sidebar, i18n_update, save_app_state, update_app_state_and_save,
    update_app_state_and_save_quiet,
};
use crate::views::{
    DialogCallback, ExportSource, ZedisCommandPalette, ZedisContent, ZedisMultiSearch, ZedisRecentKeysPalette,
    ZedisShortcutsOverlay, ZedisSidebar, ZedisTitleBar, ZedisUpdateDialog, confirm_dangerous_command,
    open_about_window, open_features_dialog, open_migration_export_window, open_migration_import_window,
    open_settings_window, open_trash_dialog,
};
use gpui::{
    Action, App, Bounds, Entity, Menu, MenuItem, MouseButton, Pixels, Point, SharedString, Task, WeakEntity, Window,
    WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px, rems, size,
};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
use gpui::TitlebarOptions;
use gpui_component::{
    ActiveTheme, IconName, Root, Sizable, StyledExt, Theme, ThemeMode, ThemeRegistry, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::ContextMenuExt,
    notification::Notification,
    scroll::ScrollableElement,
    text::{TextView, TextViewStyle},
    v_flex,
};
use rust_i18n::t;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{cell::Cell, rc::Rc, time::Duration};
use sys_locale::get_locale;
use tracing::{error, info, warn};
use zedis_ui::ZedisDialog;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Pointed at the empty `locales_stub/` so the macro embeds no translations at
// compile time; the real `locales/*.toml` stay compressed until the lazy
// backend from `i18n_loader::runtime_backend` inflates a locale on its first
// `t!` lookup. See `src/i18n_loader.rs`.
rust_i18n::i18n!(
    "locales_stub",
    fallback = "en",
    backend = crate::i18n_loader::runtime_backend()
);

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

/// Upper bound on workspace tabs — each tab holds its own `ZedisServerState`
/// (heartbeat, pooled connections, loaded keys), so the cap keeps a runaway
/// tab strip from piling up background Redis traffic.
const MAX_TABS: usize = 8;

mod assets;
mod components;
mod connection;
mod constants;
mod db;
mod error;
mod helpers;
mod i18n_loader;
mod states;
#[cfg(not(target_os = "linux"))]
mod tray;
mod views;

/// One workspace tab: a content column bound to a connection. `server_id`
/// stays empty until a server is selected in this tab.
struct ContentTab {
    server_id: String,
    db: usize,
    content: Entity<ZedisContent>,
}

/// Context-menu actions on a workspace tab (dispatched by the tab strip's
/// right-click menu, handled on the `Zedis` root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
enum TabAction {
    Close(usize),
    CloseOthers(usize),
    CloseRight(usize),
}

/// Drag payload for reordering workspace tabs.
struct DraggedTab {
    from: usize,
}

/// Floating preview shown while a tab is dragged.
struct TabDragPreview {
    title: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .child(Label::new(self.title.clone()).text_sm())
    }
}

pub struct Zedis {
    pending_notification: Option<Notification>,
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
    // views
    sidebar: Entity<ZedisSidebar>,
    /// Workspace tabs (single tab today; the tab bar UI comes later). Only
    /// the active tab's content reacts to global route/server broadcasts.
    tabs: Vec<ContentTab>,
    active_tab: usize,
    /// A cmd+click "open in new tab" request awaiting `render` (which has the
    /// `Window` needed to build a `ZedisContent`).
    pending_new_tab: Option<(String, usize)>,
    command_palette: Entity<ZedisCommandPalette>,
    recent_keys_palette: Entity<ZedisRecentKeysPalette>,
    multi_search: Entity<ZedisMultiSearch>,
    shortcuts_overlay: Entity<ZedisShortcutsOverlay>,
    title_bar: Option<Entity<ZedisTitleBar>>,
    theme_update_task: Option<Task<()>>,
    _clear_expired_cache: Option<Task<()>>,
    /// A newer release found by a check, awaiting its prompt. Consumed in
    /// `render` (which has the `Window` needed to open the dialog).
    pending_update: Option<UpdateInfo>,
    /// The in-flight update check, if any — guards against overlapping checks.
    update_task: Option<Task<()>>,
    /// The in-flight installer download, if any — guards against re-entry.
    download_task: Option<Task<()>>,
    /// The installer is open and this platform needs Zedis gone to finish the
    /// install — prompt to quit. Consumed in `render` (which has the `Window`).
    pending_install_quit: bool,
    /// First launch with nothing configured — show the one-time welcome card.
    /// Consumed in `render` (which has the `Window` needed for the dialog).
    pending_welcome: bool,
    /// Config files that were damaged and recovered (or reset) while loading
    /// at startup — before any window existed to report it. Consumed in
    /// `render`, one notification each.
    pending_config_recoveries: Vec<ConfigRecovery>,
    /// The crash report the previous run left behind, if it ended in a panic.
    /// Consumed in `render` (which has the `Window` needed for the dialog).
    pending_crash: Option<CrashReport>,
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| ZedisSidebar::new(window, cx));
        let content = cx.new(|cx| ZedisContent::new(window, cx));
        let mut tabs = vec![ContentTab {
            server_id: String::new(),
            db: 0,
            content,
        }];
        let mut active_tab = 0;
        // Restore the last session's workspace tabs. Only the strip layout is
        // rebuilt here — a restored tab's connection loads lazily on first
        // activation (its server state stays empty, so its heartbeat is idle),
        // and the startup composer below activates the remembered
        // `selected_server`, which lands on the matching tab.
        let (saved_tabs, selected) = {
            let store = cx.global::<ZedisGlobalStore>().read(cx);
            // An empty id is a persisted Home tab; server tabs are dropped
            // only when their server no longer exists.
            let saved: Vec<(String, usize)> = store
                .open_tabs()
                .iter()
                .filter(|(id, _)| id.is_empty() || get_server(id).is_ok())
                // Re-resolve each tab's DB so a pinned server restores onto
                // its pin, matching the connection the composer below
                // activates — otherwise the strip and the active connection
                // would disagree about the same server.
                .map(|(id, db)| {
                    let db = if id.is_empty() {
                        *db
                    } else {
                        store.open_db_from(id, *db)
                    };
                    (id.clone(), db)
                })
                .collect();
            (saved, store.selected_server().cloned())
        };
        if let Some((id, db)) = saved_tabs.first() {
            tabs[0].server_id = id.clone();
            tabs[0].db = *db;
        }
        for (id, db) in saved_tabs.iter().skip(1) {
            let content = cx.new(|cx| ZedisContent::new(window, cx));
            content.update(cx, |content, cx| content.set_active(false, cx));
            tabs.push(ContentTab {
                server_id: id.clone(),
                db: *db,
                content,
            });
        }
        // Reactivate the tab the user left off on: the remembered selection's
        // tab, or — when the session ended on Home (no selection) — the first
        // Home tab, so a restored Home tab actually hosts the Home route
        // instead of tab 0 showing Home under a server-bound tab.
        let remembered = match &selected {
            Some((id, db)) => tabs.iter().position(|tab| &tab.server_id == id && tab.db == *db),
            None => tabs.iter().position(|tab| tab.server_id.is_empty()),
        };
        if let Some(ix) = remembered
            && ix != 0
        {
            active_tab = ix;
            tabs[0].content.update(cx, |content, cx| content.set_active(false, cx));
            tabs[ix].content.update(cx, |content, cx| content.set_active(true, cx));
        }
        // The palette fuzzy-searches the active connection's loaded keys, so
        // hand it the active tab's shared ServerState entity.
        let server_state = tabs[active_tab].content.read(cx).server_state();
        let command_palette = cx.new(|cx| ZedisCommandPalette::new(server_state.clone(), window, cx));
        let recent_keys_palette = cx.new(|cx| ZedisRecentKeysPalette::new(server_state, window, cx));
        let multi_search = cx.new(|cx| ZedisMultiSearch::new(window, cx));
        let shortcuts_overlay = cx.new(ZedisShortcutsOverlay::new);
        let global_state = cx.global::<ZedisGlobalStore>().state();
        cx.subscribe(&global_state, |this, _server_state, event, cx| {
            match event {
                GlobalEvent::Notification(e) => {
                    let message = e.message.clone();
                    let mut notification = match e.category {
                        NotificationCategory::Info => {
                            info!(message = %message, "info notification");
                            Notification::info(message)
                        }
                        NotificationCategory::Success => {
                            info!(message = %message, "success notification");
                            Notification::success(message)
                        }
                        NotificationCategory::Warning => {
                            info!(message = %message, "warning notification");
                            Notification::warning(message)
                        }
                        NotificationCategory::Error => {
                            error!(message = %message, "error notification");
                            Notification::error(message)
                        }
                    };
                    // An empty title must not reach the toast: it renders as a
                    // blank title row with the message pushed below it.
                    if let Some(title) = e.title.as_ref().filter(|title| !title.is_empty()) {
                        notification = notification.title(title);
                    }
                    this.pending_notification = Some(notification);
                }
                GlobalEvent::ServerSelected(server_id, db) => {
                    // Track the active tab's connection identity. Deliberately
                    // no "jump to an existing tab" here: the contents'
                    // subscriptions run before this one, so by the time this
                    // fired the active tab has already followed the selection.
                    // Normal clicks navigate the current tab (browser-like);
                    // only the explicit open-in-new-tab path below dedupes
                    // onto an existing tab.
                    let tab = &mut this.tabs[this.active_tab];
                    if server_id.is_empty() {
                        tab.server_id.clear();
                        tab.db = 0;
                    } else {
                        tab.server_id = server_id.to_string();
                        tab.db = *db;
                    }
                    this.persist_tabs(cx);
                }
                GlobalEvent::ServerOpenInNewTab(server_id, db, force) => {
                    // `force` (⌘/Ctrl+Shift+click) skips the dedup lookup so a
                    // duplicate tab on the same `(server, db)` can be opened.
                    let existing = (!*force).then(|| {
                        this.tabs
                            .iter()
                            .position(|tab| tab.server_id.as_str() == server_id.as_str() && tab.db == *db)
                    });
                    if let Some(ix) = existing.flatten() {
                        this.activate_tab(ix, None, cx);
                        this.project_active_tab(cx);
                    } else if this.tabs.len() >= MAX_TABS {
                        this.pending_notification = Some(Notification::warning(i18n_common(cx, "tab_limit")));
                    } else {
                        // Creating a `ZedisContent` needs a `Window`; stash the
                        // request and let `render` build the tab.
                        this.pending_new_tab = Some((server_id.to_string(), *db));
                    }
                }
                _ => {}
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_appearance(window, |this, _window, cx| {
            // Only follow the OS appearance on System mode with no named theme
            // active — a named theme should persist across OS appearance changes.
            let follow_system = {
                let store = cx.global::<ZedisGlobalStore>().read(cx);
                store.theme().is_none() && store.theme_name().is_none()
            };
            if follow_system {
                this.theme_update_task = Some(cx.spawn(async move |_this, cx| {
                    cx.update(|cx| {
                        restore_default_themes(cx);
                        Theme::change(cx.window_appearance(), None, cx);
                        apply_default_ui_font_size(cx);
                        cx.refresh_windows();
                    });
                }));
            }
        })
        .detach();
        let clear_expired_cache = Some(cx.spawn(async move |_this, cx| {
            // 30s ticks. The recycle-bin sweep piggybacks on this loop:
            // first run on the first tick (~30s after launch, so a previous
            // session's expired entries don't linger), then hourly — this
            // keeps the 24h retention honest even for a Zedis left running
            // for days. Off-thread: it's a full-table redb scan.
            const TRASH_SWEEP_EVERY_TICKS: u64 = 120;
            let mut tick: u64 = 0;
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                clear_expired_cache();
                if tick.is_multiple_of(TRASH_SWEEP_EVERY_TICKS) {
                    cx.background_spawn(async {
                        match purge_all_trash(unix_ts_millis() - TRASH_RETENTION_MS) {
                            Ok(removed) if removed > 0 => info!(removed, "purged expired trash entries"),
                            Ok(_) => {}
                            Err(e) => error!(error = %e, "trash purge failed"),
                        }
                    })
                    .detach();
                }
                tick += 1;
            }
        }));
        let title_bar = Some(cx.new(|cx| ZedisTitleBar::new(window, cx)));

        Self {
            sidebar,
            save_task: None,
            tabs,
            active_tab,
            pending_new_tab: None,
            command_palette,
            recent_keys_palette,
            multi_search,
            shortcuts_overlay,
            pending_notification: None,
            title_bar,
            theme_update_task: None,
            _clear_expired_cache: clear_expired_cache,
            last_bounds: Bounds::default(),
            pending_update: None,
            update_task: None,
            download_task: None,
            pending_install_quit: false,
            pending_welcome: false,
            pending_config_recoveries: Vec::new(),
            pending_crash: None,
        }
    }

    /// The active tab's content column (what the root lays out and whose
    /// status bar is shown).
    fn active_content(&self) -> Entity<ZedisContent> {
        self.tabs[self.active_tab].content.clone()
    }

    /// Writes the diagnostics bundle (see `helpers::diagnostics`) and reveals
    /// it. The summary is assembled here because only the root knows the
    /// active tab's connection.
    fn export_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let info = os_info::get();
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let mut summary = format!(
            "Zedis diagnostics\nversion: {VERSION} ({GIT_SHA})\nos: {}-{}\narch: {}\nconfig_dir: {}\nlocale: {}\ntheme: {:?} / {:?}\napp_store_build: {}\ntime: {}\n",
            info.os_type(),
            info.version(),
            info.architecture().unwrap_or_default(),
            get_or_create_config_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            store.locale(),
            store.theme(),
            store.theme_name(),
            is_app_store_build(),
            chrono::Local::now().to_rfc3339(),
        );
        let app_config = store.redacted_toml().unwrap_or_else(|e| format!("<unavailable: {e}>"));
        let locale = store.locale().to_string();
        let state = self.active_content().read(cx).server_state();
        let state = state.read(cx);
        if !state.server_id().is_empty() {
            let features = state.features();
            let unusable: Vec<String> = features
                .unusable()
                .iter()
                .map(|(c, s)| format!("{} ({s:?})", c.label()))
                .collect();
            summary.push_str(&format!(
                "\n[active connection]\nserver_id: {}\nredis_version: {}\nserver_type: {}\nflavor: {}\nreadonly: {}\nhealth: {:?}\nlast_error: {:?}\nfeatures_probed: {}\nunusable_commands: {}\n",
                state.server_id(),
                state.version(),
                state.nodes_description().server_type,
                features.flavor.label(),
                state.readonly(),
                state.connection_health(),
                state.last_connection_error(),
                features.probed,
                if unusable.is_empty() {
                    "none".to_string()
                } else {
                    unusable.join(", ")
                },
            ));
        }
        let servers_config = servers_toml_redacted().unwrap_or_else(|e| format!("<unavailable: {e}>"));
        let input = DiagnosticsInput {
            summary,
            app_config,
            servers_config,
        };
        match export_diagnostics(&input) {
            Ok(path) => {
                info!(path = %path.display(), "diagnostics bundle written");
                let message = t!(
                    "sidebar.diagnostics_saved",
                    path = path.display().to_string(),
                    locale = &locale
                );
                window.push_notification(Notification::success(message.to_string()), cx);
                cx.reveal_path(&path);
            }
            Err(e) => {
                error!(error = %e, "diagnostics bundle failed");
                let message = t!("sidebar.diagnostics_failed", error = e.to_string(), locale = &locale);
                window.push_notification(Notification::error(message.to_string()), cx);
            }
        }
    }

    /// Switch the active tab: the outgoing tab stops reacting to global
    /// route/server broadcasts, the incoming one resumes, and the palettes
    /// are rebound to the incoming tab's server state. No-op when `ix` is
    /// already active (or out of range).
    ///
    /// When `window` is provided, defers a focus reclaim onto the incoming
    /// content: a mouse click on the tab pill leaves focus on the strip (a
    /// sibling of the page), so content-local keybinding handlers never see
    /// actions until the user clicks the page. ⌘F is also handled at the
    /// window root as a second line of defense.
    fn activate_tab(&mut self, ix: usize, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if ix == self.active_tab || ix >= self.tabs.len() {
            return;
        }
        self.tabs[self.active_tab]
            .content
            .update(cx, |content, cx| content.set_active(false, cx));
        self.active_tab = ix;
        self.tabs[ix]
            .content
            .update(cx, |content, cx| content.set_active(true, cx));
        self.rebind_palettes(cx);
        if let Some(window) = window {
            // After the click/key event settles (focus may still be on the
            // tab pill), move it onto the page root so Esc/⌘F have a path.
            cx.defer_in(window, |this, window, cx| {
                this.active_content().update(cx, |content, cx| {
                    content.reclaim_focus(window, cx);
                });
            });
        }
        cx.notify();
    }

    /// Point the ⌘K / ⌘P palettes at the active tab's server state so they
    /// search the right connection's keys after a tab switch.
    fn rebind_palettes(&mut self, cx: &mut Context<Self>) {
        let server_state = self.tabs[self.active_tab].content.read(cx).server_state();
        self.command_palette
            .update(cx, |palette, _| palette.set_server_state(server_state.clone()));
        self.recent_keys_palette
            .update(cx, |palette, _| palette.set_server_state(server_state));
    }

    /// Project the active tab's connection into the global store so the
    /// single-selection consumers (sidebar highlight, title bar, tray, status
    /// bar, …) follow the tab switch. Safe to call with an unchanged
    /// selection: `connect_server` re-announces it, and the active content's
    /// `select` is a no-op for the same `(server_id, db)`.
    fn project_active_tab(&mut self, cx: &mut Context<Self>) {
        let (id, db) = {
            let tab = &self.tabs[self.active_tab];
            (tab.server_id.clone(), tab.db)
        };
        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
            if id.is_empty() {
                state.clear_selected_server(cx);
            } else {
                state.connect_server(id, db, cx);
            }
        });
    }

    /// Close a tab, dropping its content entity (which tears down the tab's
    /// subscriptions, server state and connections). The last tab can't be
    /// closed; closing the active tab activates its left neighbor (or the
    /// new last tab) and projects that tab's connection.
    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || ix >= self.tabs.len() {
            return;
        }
        let was_active = ix == self.active_tab;
        self.tabs.remove(ix);
        if self.active_tab > ix {
            self.active_tab -= 1;
        } else if was_active {
            self.active_tab = ix.min(self.tabs.len() - 1);
            self.tabs[self.active_tab]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Close every tab except `ix` (context menu "close others").
    fn close_others(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() || self.tabs.len() <= 1 {
            return;
        }
        let was_active = self.active_tab == ix;
        let keep = self.tabs.remove(ix);
        self.tabs.clear();
        self.tabs.push(keep);
        self.active_tab = 0;
        if !was_active {
            self.tabs[0]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Close every tab to the right of `ix` (context menu "close right").
    fn close_right(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix + 1 >= self.tabs.len() {
            return;
        }
        let active_closed = self.active_tab > ix;
        self.tabs.truncate(ix + 1);
        if active_closed {
            self.active_tab = ix;
            self.tabs[ix]
                .content
                .update(cx, |content, cx| content.set_active(true, cx));
            self.rebind_palettes(cx);
            self.project_active_tab(cx);
        }
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Reorder: move the tab at `from` so it sits at `to` (drag & drop on the
    /// strip). The active index follows its tab.
    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = moved_active_index(self.active_tab, from, to);
        self.persist_tabs(cx);
        cx.notify();
    }

    /// Persist the strip's `(server_id, db)` list so the next launch restores
    /// the same workspace tabs. Home tabs persist as an empty id — dropping
    /// them here made every Home tab vanish on restart.
    fn persist_tabs(&self, cx: &mut Context<Self>) {
        let tabs: Vec<(String, usize)> = self.tabs.iter().map(|tab| (tab.server_id.clone(), tab.db)).collect();
        update_app_state_and_save_quiet(cx, "save_open_tabs", move |state, _| state.set_open_tabs(tabs.clone()));
    }

    /// Kick off a background check for a newer release. A `manual` check always
    /// reports its outcome (up-to-date / failure toast) and ignores a skipped
    /// version; the silent startup check stays quiet unless it finds a fresh,
    /// non-skipped update.
    fn check_for_updates(&mut self, manual: bool, then_prompt: bool, cx: &mut Context<Self>) {
        // App Store builds are updated through the App Store; never self-check or
        // self-download (Apple forbids it). Guards every trigger at once.
        if is_app_store_build() {
            return;
        }
        if self.update_task.is_some() {
            return;
        }
        // Reset the throttle on every attempt so a transient failure doesn't
        // immediately retry on the next launch.
        update_app_state_and_save_quiet(cx, "mark_update_checked", |state, _| state.mark_update_checked());
        // Flag the check so the title-bar chip can show a loading spinner.
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.set_update_checking(true, cx));
        self.update_task = Some(cx.spawn(async move |handle, cx| {
            // `fetch_latest_release` is blocking (ureq) — keep it off the UI thread.
            let result = cx.background_spawn(async move { fetch_latest_release() }).await;
            let _ = handle.update(cx, |this, cx| {
                this.update_task = None;
                // For a chip click we keep the spinner running until the dialog
                // actually opens (cleared in `render` after `open_update_dialog`),
                // so there's no gap between "loading stops" and the prompt
                // appearing. Every other outcome clears it right here.
                let mut opened_prompt = false;
                match result {
                    Ok(Some(info)) => {
                        let skipped = cx.global::<ZedisGlobalStore>().read(cx).update_skipped(&info.version);
                        if manual || !skipped {
                            let version = info.version.clone();
                            // Light the persistent title-bar chip...
                            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                state.set_available_update(Some(info.clone()), cx);
                            });
                            if then_prompt {
                                // Chip click: open the download/skip dialog with
                                // the freshly fetched info instead of toasting.
                                this.pending_update = Some(info);
                                opened_prompt = true;
                            } else {
                                // ...and fire a one-time toast so the user notices
                                // it, spelling out that updating is manual.
                                this.pending_notification = Some(Notification::info(format!(
                                    "{}: v{version}\n{}",
                                    i18n_update(cx, "found"),
                                    i18n_update(cx, "manual_hint")
                                )));
                            }
                        }
                    }
                    Ok(None) => {
                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                            state.set_available_update(None, cx);
                        });
                        if manual {
                            this.pending_notification = Some(Notification::success(i18n_update(cx, "up_to_date")));
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "update check failed");
                        if manual {
                            this.pending_notification = Some(Notification::error(i18n_update(cx, "check_failed")));
                        }
                    }
                }
                // When a prompt is opening, the spinner is cleared in `render`
                // (right after the dialog opens) to avoid a stop-then-wait gap.
                if !opened_prompt {
                    cx.global::<ZedisGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.set_update_checking(false, cx));
                }
                cx.notify();
            });
        }));
    }
    /// Act on the user's "Download" choice. With a verified manifest asset,
    /// download + checksum-verify it in the background and hand it to the OS
    /// installer; without one (API fallback / missing asset) just open the
    /// release page. A failed download falls back to the page too.
    fn start_download(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        let Some(asset) = info.asset.clone() else {
            // No verified asset for this os/arch (manifest missing → API
            // fallback, or no matching build): nothing to download in-app, so
            // hand off to the browser. Logged because it otherwise looks
            // identical to "the Download button did nothing".
            info!(version = %info.version, url = %info.page_url, "update: no asset for this platform, opening release page");
            cx.open_url(&info.page_url);
            return;
        };
        if self.download_task.is_some() {
            info!("update: download already in progress, ignoring");
            return;
        }
        let page_url = info.page_url.clone();
        let version = info.version.clone();
        info!(
            version = %version,
            asset = %asset.name,
            size = asset.size,
            "update: download started"
        );

        // Publish 0% *synchronously*, before any await: connecting (DNS, TLS,
        // the GitHub → CDN redirect) takes a second or two during which no byte
        // has arrived, and the first `on_progress` can only fire after that. If
        // the UI waited for it, the dialog would keep showing the Download
        // button and the chip its version — looking like the click did nothing,
        // which is exactly what made it get clicked twice. Publishing here
        // swaps both to the progress state on the click itself.
        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
            state.set_download_progress(Some((0, asset.size)), cx);
            // A fresh download voids any earlier "installed, restart?" state.
            state.set_update_installed(false, cx);
        });
        cx.notify();

        // Progress is produced on the background thread and ferried to the UI
        // through a channel as `(downloaded, total)` bytes; this foreground
        // drainer publishes it to the global store, which the update dialog
        // (progress bar) and the title-bar chip (percentage) both read.
        let (tx, rx) = smol::channel::unbounded::<(u64, u64)>();
        cx.spawn(async move |_, cx| {
            while let Ok(progress) = rx.recv().await {
                cx.update(|cx| {
                    cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                        state.set_download_progress(Some(progress), cx);
                    });
                });
            }
            // The sender is dropped once the download settles, so the loop ends
            // with every queued tick already applied. Clearing *here* (rather
            // than in the completion handler, which races the drainer) means a
            // late tick can't land after the clear and freeze the chip at a
            // stale percent. This is also what dismisses the dialog.
            cx.update(|cx| {
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.set_download_progress(None, cx);
                });
            });
        })
        .detach();

        let log_name = asset.name.clone();
        self.download_task = Some(cx.spawn(async move |handle, cx| {
            // Networking + checksum are blocking — keep them off the UI thread.
            let result = cx
                .background_spawn(async move {
                    let mut last_pct = u8::MAX;
                    let mut last_logged_decile = u8::MAX;
                    let outcome = download_and_verify(&asset, |done, total| {
                        if total == 0 {
                            return;
                        }
                        // Throttle to integer-percent changes (≤101 updates).
                        let pct = ((done * 100 / total).min(100)) as u8;
                        if pct == last_pct {
                            return;
                        }
                        last_pct = pct;
                        // Log every 10% so a slow or stalled download is
                        // diagnosable from the log alone, without spamming it
                        // with a line per percent.
                        let decile = pct / 10;
                        if decile != last_logged_decile {
                            last_logged_decile = decile;
                            info!(asset = %log_name, pct, done, total, "update: download progress");
                        }
                        let _ = tx.try_send((done, total));
                    })
                    .and_then(|path| install_update(&path));
                    // Drop the sender so the drainer task ends.
                    drop(tx);
                    outcome
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.download_task = None;
                // The progress is cleared by the drainer once it has applied
                // every queued tick (see above) — clearing it here too would
                // race it and could leave the chip stuck at a stale percent.
                match result {
                    // macOS in-place install landed: the bundle on disk is
                    // already the new version. Flag it on the store — the
                    // update dialog (still open, showing the progress bar)
                    // swaps itself to the Restart / Later row instead of
                    // closing. A separate restart dialog is NOT opened
                    // here: the update dialog's deferred self-close targets
                    // the topmost dialog and would eat it.
                    #[cfg(target_os = "macos")]
                    Ok(Delivery::Replaced) => {
                        info!(version = %version, "update: installed in place, restart offered");
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "installed_done")));
                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                            state.set_update_installed(true, cx);
                        });
                    }
                    Ok(Delivery::HandedToOs) => {
                        info!(version = %version, "update: download finished, installer handed to the OS");
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "download_done")));
                        // macOS / Windows: the installer can't replace a running
                        // Zedis, so offer to quit.
                        if installer_requires_quit() {
                            this.pending_install_quit = true;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "update download failed");
                        this.pending_notification = Some(Notification::error(i18n_update(cx, "download_failed")));
                        // Fall back to the release page so the user can still get it.
                        cx.open_url(&page_url);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn persist_window_state(
        &mut self,
        new_bounds: Bounds<Pixels>,
        display: Option<(String, Point<Pixels>)>,
        cx: &mut Context<Self>,
    ) {
        self.last_bounds = new_bounds;
        let store = cx.global::<ZedisGlobalStore>().clone();
        // Anchor the placement to the current display (origin relative to it) so
        // it survives monitor rearrangement; absolute `bounds` stays as fallback.
        let placement = display.map(|(display_uuid, screen_origin)| WindowPlacement {
            display_uuid,
            bounds: new_bounds - screen_origin,
        });
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            // Snapshot *after* the quiet window, never before: `save_app_state`
            // rewrites the whole TOML, so a clone taken up front would restore
            // every field to its pre-wait value. The first render always lands
            // here (`last_bounds` starts at zero), and its stale snapshot used
            // to revert the startup update-check stamp ~500ms after it was
            // written — freezing `last_update_check` and re-checking on every
            // launch. Same reason `apply_and_save` clones after its own wait.
            let value = store.update(cx, move |state, cx| {
                state.set_bounds(new_bounds);
                if let Some(p) = placement {
                    state.upsert_window_placement(p);
                }
                cx.notify();
                state.clone()
            });

            cx.background_spawn(async move {
                if let Err(e) = save_app_state(&value) {
                    error!(error = %e, "save window bounds fail",);
                } else {
                    info!(bounds = ?new_bounds, "save window bounds success");
                }
            })
            .await;
        });
        self.save_task = Some(task);
    }
    /// Toggle the command palette. Driven by a global (focus-
    /// independent) `PaletteAction` handler so `⌘K` works even when
    /// nothing is focused (e.g. right after the palette closed on ESC).
    pub fn toggle_command_palette(&mut self, cx: &mut Context<Self>) {
        self.command_palette.update(cx, |palette, cx| palette.toggle(cx));
    }

    /// Toggle the recent-keys Quick Open palette (⌘P). Global handler so
    /// it works regardless of focus, matching the command palette.
    pub fn toggle_recent_keys_palette(&mut self, cx: &mut Context<Self>) {
        self.recent_keys_palette.update(cx, |palette, cx| palette.toggle(cx));
    }

    /// Toggle the multi-database search palette (⌘⇧F). Global handler so
    /// it works regardless of focus, matching the command palette.
    pub fn toggle_multi_search(&mut self, cx: &mut Context<Self>) {
        self.multi_search.update(cx, |palette, cx| palette.toggle(cx));
    }

    /// Toggle the keyboard-shortcuts overlay. Like the command palette
    /// it is driven by a global (focus-independent) `ShortcutsAction`
    /// handler so `⌘/` works regardless of focus, and so the command
    /// palette can hand off to it via a dispatched action.
    pub fn toggle_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.shortcuts_overlay.update(cx, |overlay, cx| overlay.toggle(cx));
    }

    /// The workspace tab strip — hand-rolled pills instead of
    /// gpui-component's `TabBar`, whose `child(impl Into<Tab>)` API can't
    /// carry a context menu / drag-drop / middle-click. Hidden with a single
    /// tab; with more: click activates, × or middle-click closes, drag
    /// reorders, right-click offers close / close-others / close-right.
    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let home_label = i18n_sidebar(cx, "home");
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let active_bg = foreground.alpha(0.1);
        let muted = cx.theme().muted_foreground;
        let strip = h_flex()
            .w_full()
            .flex_none()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let title: SharedString = if tab.server_id.is_empty() {
                    home_label.clone()
                } else {
                    let name = get_server(&tab.server_id)
                        .map(|server| server.name)
                        .unwrap_or_else(|_| tab.server_id.clone());
                    if tab.db > 0 {
                        format!("{name} [{}]", tab.db).into()
                    } else {
                        name.into()
                    }
                };
                let is_active = ix == self.active_tab;
                // Per-tab jump shortcut (⌘1–8 / Ctrl+1–8): tabs are capped at
                // 8, so every pill gets a hint. Rendered per-platform via
                // `humanize_keystroke`, matching the ⌘/ overlay's symbols.
                // The hint always sits one shade below its title: `muted`
                // under the active tab's full-foreground title, faded muted
                // under an inactive tab's already-muted title.
                let shortcut: SharedString = humanize_keystroke(&format!("cmd-{}", ix + 1)).into();
                let shortcut_color = if is_active { muted } else { muted.alpha(0.6) };
                // `Label` doesn't inherit a parent's `text_color` (its render
                // unconditionally sets the theme foreground before refining
                // with its own style), so the title's active/inactive color
                // must be set on the Label itself.
                let title_color = if is_active { foreground } else { muted };
                let preview_title = title.clone();
                div()
                    .id(("content-tab", ix))
                    .flex_none()
                    .on_drag(DraggedTab { from: ix }, move |_, _, _, cx| {
                        let title = preview_title.clone();
                        cx.new(|_| TabDragPreview { title })
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, _window, cx| {
                        this.move_tab(dragged.from, ix, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _window, cx| {
                            this.close_tab(ix, cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(ix, Some(window), cx);
                        this.project_active_tab(cx);
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .pl_2()
                            .pr_1()
                            .py_0p5()
                            .rounded_md()
                            .cursor_pointer()
                            .when(is_active, |this| this.bg(active_bg))
                            // Container-level color still covers the close
                            // button's icon; the Labels set their own below.
                            .when(!is_active, |this| this.text_color(muted))
                            .child(Label::new(title).text_sm().text_color(title_color).whitespace_nowrap())
                            .child(
                                Label::new(shortcut)
                                    .text_xs()
                                    .text_color(shortcut_color)
                                    .whitespace_nowrap(),
                            )
                            .child(
                                Button::new(("content-tab-close", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        // Closing is not activating — keep the
                                        // click off the pill underneath.
                                        cx.stop_propagation();
                                        this.close_tab(ix, cx);
                                    })),
                            ),
                    )
                    .context_menu(move |menu, _window, cx| {
                        menu.menu(i18n_common(cx, "tab_close"), Box::new(TabAction::Close(ix)))
                            .menu(
                                i18n_common(cx, "tab_close_others"),
                                Box::new(TabAction::CloseOthers(ix)),
                            )
                            .menu(i18n_common(cx, "tab_close_right"), Box::new(TabAction::CloseRight(ix)))
                    })
            }));
        Some(strip)
    }

    fn render_titlebar(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(title_bar) = self.title_bar.as_ref() else {
            return h_flex().into_any_element();
        };
        title_bar.clone().into_any_element()
    }
}

/// Default window bounds: a 1200×750 window centered on the primary display,
/// shrunk to fit if the primary display is small.
fn default_window_bounds(cx: &mut App) -> Bounds<Pixels> {
    let mut window_size = size(px(1200.), px(750.));
    if let Some(display) = cx.primary_display() {
        let ds = display.bounds().size;
        window_size.width = window_size.width.min(ds.width * 0.85);
        window_size.height = window_size.height.min(ds.height * 0.85);
    }
    Bounds::centered(None, window_size, cx)
}

/// Resolve the bounds to open the window at, validating any saved placement
/// against the *current* display layout (monitors may have been unplugged,
/// resized, or rearranged since last run). Priority:
/// 1. the display we were last on, matched by uuid → restore relative origin;
/// 2. otherwise the absolute saved bounds, snapped onto the display they
///    overlap most (covers old configs / a monitor that's now gone);
/// 3. otherwise center on the primary display.
///
/// In all cases the result is clamped so the window fits and its title bar
/// stays reachable.
fn resolve_window_bounds(state: &ZedisAppState, cx: &mut App) -> Bounds<Pixels> {
    // Shrink to fit the display, then keep the origin (title bar) on-screen.
    let clamp_to = |mut b: Bounds<Pixels>, screen: Bounds<Pixels>| -> Bounds<Pixels> {
        b.size = b.size.min(&screen.size);
        let max_x = screen.origin.x + screen.size.width - b.size.width;
        let max_y = screen.origin.y + screen.size.height - b.size.height;
        b.origin.x = b.origin.x.clamp(screen.origin.x, max_x);
        b.origin.y = b.origin.y.clamp(screen.origin.y, max_y);
        b
    };

    // Currently-connected displays keyed by uuid, plus the primary's uuid.
    let displays: Vec<(String, Bounds<Pixels>)> = cx
        .displays()
        .into_iter()
        .filter_map(|d| Some((d.uuid().ok()?.to_string(), d.bounds())))
        .collect();
    let primary_uuid = cx.primary_display().and_then(|d| d.uuid().ok()).map(|u| u.to_string());

    // 1) Restore the saved placement for a currently-connected display — the
    //    primary display first, else the most-recently-used connected one — so
    //    each monitor (work / home) keeps its own remembered position.
    let placement = state
        .window_placements()
        .iter()
        .find(|p| primary_uuid.as_deref() == Some(p.display_uuid.as_str()))
        .or_else(|| {
            state
                .window_placements()
                .iter()
                .find(|p| displays.iter().any(|(uuid, _)| uuid == &p.display_uuid))
        });
    if let Some(p) = placement
        && let Some((_, screen)) = displays.iter().find(|(uuid, _)| uuid == &p.display_uuid)
    {
        return clamp_to(p.bounds + screen.origin, *screen);
    }

    // 2) Fallback: snap the absolute saved bounds onto the display they overlap
    //    most; no overlap (monitor gone / off-screen) -> fall through.
    if let Some(&saved) = state.bounds() {
        let area = |screen: &Bounds<Pixels>| {
            let i = saved.intersect(screen);
            if i.is_empty() {
                0.0
            } else {
                i.size.width.as_f32() * i.size.height.as_f32()
            }
        };
        if let Some((_, screen)) = displays
            .iter()
            .filter(|(_, b)| area(b) > 0.0)
            .max_by(|(_, a), (_, b)| area(a).total_cmp(&area(b)))
        {
            return clamp_to(saved, *screen);
        }
    }

    // 3) Nothing usable → center on the primary display.
    info!("no usable saved window placement; centering on primary display");
    default_window_bounds(cx)
}

/// Apply a registry theme by name (e.g. "Ayu Dark") if present, returning
/// whether it was found. Used at startup and from the title-bar theme menu.
fn apply_named_theme(name: &str, cx: &mut App) -> bool {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        return false;
    };
    Theme::global_mut(cx).apply_config(&config);
    // apply_config resets font_size to stock 16 unless the theme JSON sets it.
    apply_default_ui_font_size(cx);
    cx.refresh_windows();
    true
}

/// Restore the registry's default light/dark configs into the global `Theme`.
/// `apply_config` (used to apply a named theme) overwrites the matching
/// `light_theme`/`dark_theme` slot, so picking Light/Dark/System afterwards
/// would just re-apply that named theme unless the slots are reset first.
fn restore_default_themes(cx: &mut App) {
    // Clone the configs out (owned) so we can override the primary before
    // installing them.
    let (mut light, mut dark) = {
        let registry = ThemeRegistry::global(cx);
        (
            (**registry.default_light_theme()).clone(),
            (**registry.default_dark_theme()).clone(),
        )
    };
    // The stock default themes use a neutral (near-black / near-white) primary,
    // so primary buttons are pure high-contrast fills that invert with the mode.
    // Override it to the Zedis brand blue so primary buttons read as the app's
    // color in both modes; white on this blue clears 4.5:1, so the label stays
    // legible. The hover/active shades are also set explicitly — the default
    // configs pin them to neutrals (a near-white hover in dark), so they must be
    // overridden too or the button flashes white; they step the primary down to
    // 85% / 75% brightness.
    for cfg in [&mut light, &mut dark] {
        cfg.colors.primary = Some("#1f6feb".into());
        cfg.colors.primary_foreground = Some("#ffffff".into());
        cfg.colors.primary_hover = Some("#1a5ec8".into());
        cfg.colors.primary_active = Some("#1753b0".into());
    }
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(light);
    theme.dark_theme = Rc::new(dark);
    // Not applied to the live Theme yet — caller still runs Theme::change.
    // Pin rem base here too so a bare restore (if ever used alone) keeps 14.
    apply_default_ui_font_size(cx);
}

/// Open the "update available" dialog on the main window. **Download** opens the
/// release page in the browser; **Skip this version** records the version so the
/// silent startup check won't prompt for it again. Closing without choosing
/// leaves nothing recorded, so the next daily check prompts again.
/// Compact Markdown styling for release notes: the library defaults size
/// headings up to ~28px, which dwarfs the dialog body; shrink them to a
/// gentle hierarchy (mirrors the memory-analysis AI panel styling).
/// Map the OS appearance to a theme mode when the user hasn't pinned one.
/// `VibrantLight` is macOS's translucent *light* appearance — group it with
/// `Light` so only genuinely dark appearances select the dark theme.
fn theme_mode_for_appearance(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

fn release_notes_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, _base| match level {
            1 => px(18.),
            2 => px(16.),
            3 => px(15.),
            _ => px(14.),
        })
}

/// First-launch onboarding: a one-shot card walking through the three steps to
/// get productive. Only opened when no server is configured yet, and never
/// twice — `HINT_WELCOME` is dismissed the moment startup decides to show it.
/// Localized one-line account of a startup config recovery: which file, what
/// happened, and where the damaged copy was kept (the full path, so the user
/// can hand it over or inspect it).
fn config_recovery_message(recovery: &ConfigRecovery, cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let file = recovery
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let corrupt = recovery.corrupt_path().display().to_string();
    let key = match recovery {
        ConfigRecovery::RestoredFromBackup { .. } => "common.config_restored_from_backup",
        ConfigRecovery::Reset { .. } => "common.config_reset",
    };
    t!(key, file = file, corrupt = corrupt, locale = locale)
        .to_string()
        .into()
}

/// "Zedis closed unexpectedly last time": the panic message and where the full
/// report (with backtrace) was written, plus a one-click way to the folder so it
/// can be attached to an issue.
fn open_crash_dialog(report: &CrashReport, window: &mut Window, cx: &mut App) {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let body = i18n_crash(cx, "body");
    let summary: SharedString = report.summary.clone().into();
    let saved: SharedString = t!(
        "crash.report_saved",
        path = report.path.display().to_string(),
        locale = &locale
    )
    .to_string()
    .into();
    let muted = cx.theme().muted_foreground;
    let mono = get_mono_font_family();
    ZedisDialog::new(i18n_crash(cx, "title"))
        .icon(IconName::TriangleAlert)
        .child(move || {
            v_flex()
                .gap_2()
                .child(body.clone())
                .when(!summary.is_empty(), |this| {
                    this.child(div().font_family(mono.clone()).text_sm().child(summary.clone()))
                })
                .child(div().text_xs().text_color(muted).child(saved.clone()))
        })
        .ok_text(i18n_crash(cx, "open_logs"))
        .cancel_text(i18n_crash(cx, "dismiss"))
        .on_ok(|_, _window, cx| {
            match logs_dir() {
                Some(logs) => cx.open_with_system(&logs),
                None => error!("failed to resolve logs directory"),
            }
            true
        })
        .open(window, cx);
}

fn open_welcome_dialog(window: &mut Window, cx: &mut App) {
    let intro = i18n_hints(cx, "welcome_intro");
    let steps: [SharedString; 3] = [
        i18n_hints(cx, "welcome_step_connect"),
        i18n_hints(cx, "welcome_step_browse"),
        format!(
            "{} ({})",
            i18n_hints(cx, "welcome_step_palette"),
            humanize_keystroke("secondary-k")
        )
        .into(),
    ];
    ZedisDialog::new(i18n_hints(cx, "welcome_title"))
        .icon(IconName::Info)
        .child(move || v_flex().gap_2().child(intro.clone()).children(steps.iter().cloned()))
        .ok_text(i18n_hints(cx, "welcome_ok"))
        .open(window, cx);
}

/// Offered once the installer is open on a platform that can't install over a
/// running Zedis (macOS / Windows — see `installer_requires_quit`). Quitting is
/// the user's call: an editor may hold unsaved changes, and they may simply want
/// to install later.
fn open_install_quit_dialog(window: &mut Window, cx: &mut App) {
    ZedisDialog::new(i18n_update(cx, "quit_to_install_title"))
        .icon(IconName::Info)
        .message(i18n_update(cx, "quit_to_install_body"))
        .ok_text(i18n_update(cx, "quit_to_install_now"))
        .cancel_text(i18n_update(cx, "quit_to_install_later"))
        .on_ok(|_, _window, cx| {
            // The app state is flushed by `flush_app_state_on_quit`, which gpui
            // waits on during shutdown — nothing to do for it here.
            //
            // Quitting hands focus to whatever ran before Zedis, not to the
            // installer — pull its window forward first, or it ends up buried.
            focus_installer_ui();
            info!("update: quitting so the installer can replace the app");
            cx.quit();
            true
        })
        .open(window, cx);
}

fn open_update_dialog(info: UpdateInfo, zedis: WeakEntity<Zedis>, window: &mut Window, cx: &mut App) {
    // The notes area scrolls, so this cap only guards layout work against a
    // pathologically long release body.
    const MAX_NOTES: usize = 5000;
    let title = format!("{} {}", i18n_update(cx, "available_title"), info.version);
    let mut notes = info.notes.clone();
    if notes.chars().count() > MAX_NOTES {
        notes = notes.chars().take(MAX_NOTES).collect::<String>();
        notes.push('…');
    }
    let update_hint = i18n_update(cx, "update_body");
    let version_line = format!("{} → {}", info.current, info.version);
    let skip_version = info.version.clone();
    let download_info = info.clone();
    // Shared flag so the Download path suppresses the skip-on-close below (the
    // dialog's own × still records a skip when the user never started one).
    let downloaded = Rc::new(Cell::new(false));
    let on_download_flag = downloaded.clone();

    // Kick off the download and *leave the dialog open* — `ZedisUpdateDialog`
    // watches the progress in the store and swaps its buttons for the bar.
    let on_download: DialogCallback = Rc::new(move |_window, cx| {
        on_download_flag.set(true);
        // Download + verify + open the installer (or open the release page when
        // there's no verified asset) — see `Zedis::start_download`.
        if let Some(view) = zedis.upgrade() {
            view.update(cx, |this, cx| this.start_download(download_info.clone(), cx));
        }
    });
    let skip = skip_version.clone();
    let on_skip: DialogCallback = Rc::new(move |_window, cx| {
        info!(version = %skip, "update: version skipped by user");
        let version = skip.clone();
        update_app_state_and_save_quiet(cx, "skip_update_version", move |state, _| {
            state.set_skipped_version(version.clone());
        });
        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
            state.set_available_update(None, cx);
        });
    });

    // The action row is a *view* in the dialog footer, not part of the body:
    // the body is the dialog's scroll container, so a long changelog would push
    // the buttons below the fold. As a footer view it stays put and can swap
    // itself for the live progress bar (see `ZedisUpdateDialog`).
    let actions = cx.new(|cx| ZedisUpdateDialog::new(on_download.clone(), on_skip.clone(), cx));
    ZedisDialog::new(title)
        .child(move || {
            let mut body = v_flex()
                .gap_2()
                .child(Label::new(update_hint.clone()))
                .child(Label::new(version_line.clone()));
            // Render the changelog as Markdown (it comes straight from the
            // GitHub release body) inside a capped, scrollable area.
            //
            // Not `max_h`: `Scrollable` copies the caller's size styles onto
            // its wrapper but the inner content keeps them too, and while its
            // forced `h_auto` overrides a fixed `h`, nothing resets `max_h` —
            // so the content itself gets clamped and there is never anything
            // to scroll. A definite `h` viewport scrolls correctly; short
            // bodies render inline so the dialog stays compact.
            if !notes.trim().is_empty() {
                let text = TextView::markdown("update-release-notes", notes.clone()).style(release_notes_style());
                let long_notes = notes.lines().count() > 12 || notes.chars().count() > 800;
                body = body.child(if long_notes {
                    div()
                        .w_full()
                        .h(px(280.))
                        .child(text)
                        .overflow_y_scrollbar()
                        .into_any_element()
                } else {
                    div().w_full().child(text).into_any_element()
                });
            }
            body
        })
        .footer_child(move || actions.clone().into_any_element())
        .w(px(520.))
        .overlay_closable(false)
        .on_close(move |_, _window, cx| {
            // Only dismissing without downloading (the × button) records a skip
            // and clears the chip. The Download path sets the flag above, and
            // the dialog then closes itself once the download settles.
            if !downloaded.get() {
                let version = skip_version.clone();
                update_app_state_and_save_quiet(cx, "skip_update_version", move |state, _| {
                    state.set_skipped_version(version.clone());
                });
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.set_available_update(None, cx);
                });
            }
        })
        .open(window, cx);
}

impl Render for Zedis {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let current_bounds = window.bounds();
        if current_bounds != self.last_bounds {
            // The display the window is currently on, used to anchor the saved
            // placement to it (so it survives multi-monitor rearrangement).
            let display = window
                .display(cx)
                .and_then(|d| Some((d.uuid().ok()?.to_string(), d.bounds().origin)));
            self.persist_window_state(current_bounds, display, cx);
        }
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        for recovery in std::mem::take(&mut self.pending_config_recoveries) {
            let message = config_recovery_message(&recovery, cx);
            // Losing the server list (secrets included) is an error; a
            // successful restore from `.bak` is only worth a warning.
            let notification = match recovery {
                ConfigRecovery::Reset { .. } => Notification::error(message),
                ConfigRecovery::RestoredFromBackup { .. } => Notification::warning(message),
            };
            window.push_notification(notification, cx);
        }
        if let Some(report) = self.pending_crash.take() {
            // Deferred for the same focus reason as the welcome card below.
            window.defer(cx, move |window, cx| open_crash_dialog(&report, window, cx));
        }
        // The installer is up and this platform needs Zedis closed to finish —
        // ask (the update dialog has already dismissed itself by now).
        if std::mem::take(&mut self.pending_install_quit) {
            open_install_quit_dialog(window, cx);
        }
        if std::mem::take(&mut self.pending_welcome) {
            // Defer past this frame: opening the dialog focuses it, but the
            // views built later in this same first frame steal that focus
            // back (the servers page focuses itself in `new` so ⌘F works on
            // arrival) — leaving Esc/Enter dispatched to the page behind the
            // overlay. Deferring makes the dialog the last focus claimant.
            window.defer(cx, open_welcome_dialog);
        }
        if let Some(info) = self.pending_update.take() {
            let weak = cx.entity().downgrade();
            open_update_dialog(info, weak, window, cx);
            // The prompt is on screen now — stop the chip's loading spinner so it
            // spins right up until the dialog appears (no stop-then-wait gap).
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_update_checking(false, cx));
        }
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(font_size);
        }
        if let Some((id, db)) = self.pending_new_tab.take() {
            // Build the new tab's content now that we have the `Window`, make
            // it the active tab, then project the connection — the fresh
            // content (already subscribed) receives the `ServerSelected` and
            // loads the server.
            let content = cx.new(|cx| ZedisContent::new(window, cx));
            self.tabs[self.active_tab]
                .content
                .update(cx, |content, cx| content.set_active(false, cx));
            self.tabs.push(ContentTab {
                server_id: id.clone(),
                db,
                content,
            });
            self.active_tab = self.tabs.len() - 1;
            self.rebind_palettes(cx);
            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                // An empty server id is a Home tab: route to Home instead of
                // connecting (matches the sidebar Home button's new-tab path).
                if id.is_empty() {
                    state.clear_selected_server(cx);
                } else {
                    state.connect_server(id, db, cx);
                }
            });
        }

        // The status bar is the bottom row *of the content column*, not of the
        // window: it starts where the sidebar ends, which is what lets the sidebar
        // run the full height (down to the window's bottom edge) instead of
        // stopping at a full-width bar. Shown only on server routes (mirrors
        // content.rs's route match — Home/Settings/Protos/Scripts have none).
        let route = cx.global::<ZedisGlobalStore>().read(cx).route();
        let show_status_bar = route.is_server();
        let status_bar = self.active_content().read(cx).status_bar();
        // Sidebar collapses to a narrow icon-only rail; the toggle saves state +
        // refreshes windows, so this re-reads the width on the next render.
        let sidebar_width = if cx.global::<ZedisGlobalStore>().read(cx).sidebar_collapsed() {
            SIDEBAR_COLLAPSED_WIDTH
        } else {
            SIDEBAR_WIDTH
        };

        let content = v_flex()
            .id(PKG_NAME)
            // No font_family here: gpui-component's `Root` (which wraps this
            // view) already cascades `theme.font_family` (`.SystemUIFont` by
            // default), so setting it again is redundant — and would override
            // a theme that customizes the UI font.
            .size_full()
            .child(self.render_titlebar(window, cx))
            .child(
                h_flex()
                    .id(PKG_NAME)
                    // Body row takes the remaining height (between title bar and
                    // the full-width status bar below); `min_h_0` lets its
                    // scrollable children shrink instead of forcing overflow.
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .bg(cx.theme().background)
                    .child(div().w(sidebar_width).flex_none().h_full().child(self.sidebar.clone()))
                    // Content column: tab strip on top, then the content itself,
                    // then the status bar. All three are bounded by the column, so
                    // they line up with each other and leave the sidebar its own
                    // full-height strip to the left.
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .children(self.render_tab_bar(cx))
                            // `flex_1` + `min_h_0`: the content takes what's left
                            // after the strip and the status bar, and yields (rather
                            // than overflowing) instead of pushing the bar off-screen.
                            .child(div().flex_1().min_h_0().w_full().child(self.active_content()))
                            .when(show_status_bar, |this| this.child(status_bar)),
                    )
                    .children(dialog_layer)
                    .children(notification_layer),
            )
            // Command palette overlays everything (absolute, full-size
            // when open; zero-footprint when closed).
            .child(self.command_palette.clone())
            // Recent-keys Quick Open (⌘P); same overlay model.
            .child(self.recent_keys_palette.clone())
            .child(self.multi_search.clone())
            // Keyboard-shortcuts reference overlay (⌘/), same overlay
            // model as the palette; rendered last so it stacks on top.
            .child(self.shortcuts_overlay.clone());
        content
            .on_action(cx.listener(|_this, e: &ThemeAction, _window, cx| {
                let action = *e;

                // Convert action to theme mode
                let mode = match action {
                    ThemeAction::Light => Some(ThemeMode::Light),
                    ThemeAction::Dark => Some(ThemeMode::Dark),
                    ThemeAction::System => None, // Follow OS theme
                };

                // Determine actual render mode (resolve System to Light/Dark)
                let render_mode = match mode {
                    Some(m) => m,
                    None => theme_mode_for_appearance(cx.window_appearance()),
                };

                // A previously-applied named theme overwrote the Theme's
                // light/dark slot, so restore the registry defaults first —
                // otherwise switching mode would just re-apply that named theme.
                restore_default_themes(cx);
                // Apply theme immediately for instant visual feedback
                Theme::change(render_mode, None, cx);
                apply_default_ui_font_size(cx);

                // Save preference to disk asynchronously
                update_app_state_and_save(cx, "save_theme", move |state, _cx| {
                    state.set_theme(mode);
                });
            }))
            .on_action(cx.listener(|_this, e: &SelectThemeAction, _window, cx| {
                let name = e.name.clone();
                if apply_named_theme(&name, cx) {
                    update_app_state_and_save(cx, "save_theme_name", move |state, _cx| {
                        state.set_theme_name(Some(name));
                    });
                }
            }))
            // Locale action handler - changes language and saves to disk
            .on_action(cx.listener(|_this, e: &LocaleAction, _window, cx| {
                let locale = match e {
                    LocaleAction::Zh => "zh",
                    LocaleAction::En => "en",
                    LocaleAction::Ja => "ja",
                    LocaleAction::Ru => "ru",
                    LocaleAction::Pt => "pt",
                    LocaleAction::De => "de",
                    LocaleAction::Fr => "fr",
                    LocaleAction::Es => "es",
                };

                // Save locale preference and refresh UI
                update_app_state_and_save(cx, "save_locale", move |state, _cx| {
                    state.set_locale(locale.to_string());
                });
            }))
            .on_action(cx.listener(move |_this, e: &SettingsAction, _window, cx| match e {
                SettingsAction::Editor => open_settings_window(cx),
                SettingsAction::Protos => {
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.go_to(Route::Protos, cx);
                        });
                    });
                }
                SettingsAction::Scripts => {
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.go_to(Route::Scripts, cx);
                        });
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &DiagnosticsAction, window, cx| {
                this.export_diagnostics(window, cx);
            }))
            .on_action(cx.listener(move |_this, e: &ServerToolsAction, window, cx| {
                let target = match e {
                    ServerToolsAction::Monitor => ServerView::Monitor,
                    ServerToolsAction::Config => ServerView::Config,
                    ServerToolsAction::Acl => ServerView::Acl,
                    ServerToolsAction::Search => ServerView::Search,
                    ServerToolsAction::Functions => ServerView::Functions,
                    ServerToolsAction::LuaScripts => ServerView::LuaScripts,
                    ServerToolsAction::Persistence => ServerView::Persistence,
                    ServerToolsAction::KeyspaceNotifications => ServerView::KeyspaceNotifications,
                    ServerToolsAction::Topology => ServerView::Topology,
                    ServerToolsAction::ServerLoad => ServerView::ServerLoad,
                    ServerToolsAction::Hotkeys => ServerView::Hotkeys,
                    ServerToolsAction::ValueSearch => ServerView::ValueSearch,
                    ServerToolsAction::ServerInfo => ServerView::ServerInfo,
                    // FLUSHDB / FLUSHALL: a confirm dialog over whatever view
                    // is active, never a route. The menu entry is already
                    // disabled on a read-only connection and
                    // `flush_database` re-checks the capability, so this arm
                    // only has to raise the dialog.
                    ServerToolsAction::FlushDb | ServerToolsAction::FlushAll => {
                        let all = matches!(e, ServerToolsAction::FlushAll);
                        let Some((server_id, _)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned()
                        else {
                            return;
                        };
                        let Ok(server) = get_server(&server_id) else {
                            return;
                        };
                        let kind = if all { DangerKind::FlushAll } else { DangerKind::FlushDb };
                        let line = if all { "FLUSHALL" } else { "FLUSHDB" };
                        let server_state = _this.active_content().read(cx).server_state();
                        confirm_dangerous_command(&server, &kind, Some(line), window, cx, move |_window, cx| {
                            server_state.update(cx, |state, cx| {
                                state.flush_database(all, cx);
                            });
                        });
                        return;
                    }
                    // A dialog, not a sub-route: keeps whatever view is
                    // active underneath.
                    ServerToolsAction::Trash => {
                        open_trash_dialog(window, cx);
                        return;
                    }
                    // The probed command matrix of the active tab's connection.
                    ServerToolsAction::Capabilities => {
                        let server_state = _this.active_content().read(cx).server_state();
                        open_features_dialog(server_state, window, cx);
                        return;
                    }
                    // Dump import into the active server / db (not a
                    // key-tree prefix). Opens a dedicated window.
                    ServerToolsAction::ImportKeys => {
                        let Some((server_id, db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned()
                        else {
                            return;
                        };
                        let server_name: gpui::SharedString = get_server(&server_id)
                            .map(|s| s.name.into())
                            .unwrap_or_else(|_| server_id.clone().into());
                        open_migration_import_window(server_id.into(), server_name, db, cx);
                        return;
                    }
                    // Pub/Sub lives inside the editor suite (channel mode),
                    // not on a tool route — flip the mode, then land on the
                    // editor view so the panel is visible.
                    ServerToolsAction::PubsubMode => {
                        let server_state = _this.active_content().read(cx).server_state();
                        server_state.update(cx, |state, cx| state.change_channel_mode(cx));
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                        });
                        return;
                    }
                    // Export every key loaded in the active tab's tree (a
                    // SCAN-limited subset, same coverage as the tree itself).
                    ServerToolsAction::ExportKeys => {
                        let Some((server_id, db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned()
                        else {
                            return;
                        };
                        let server_name: gpui::SharedString = get_server(&server_id)
                            .map(|s| s.name.into())
                            .unwrap_or_else(|_| server_id.clone().into());
                        let mut keys: Vec<gpui::SharedString> = _this
                            .active_content()
                            .read(cx)
                            .server_state()
                            .read(cx)
                            .keys()
                            .keys()
                            .cloned()
                            .collect();
                        if keys.is_empty() {
                            return;
                        }
                        keys.sort_unstable();
                        open_migration_export_window(server_id.into(), server_name, db, keys, ExportSource::Loaded, cx);
                        return;
                    }
                };
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        state.toggle_view(target, cx);
                    });
                });
            }))
            // Esc mirrors the tool pages' "back to editor" button. The
            // global keybinding only reaches here when no deeper handler
            // (focused input, open dialog, command palette) claimed the
            // keystroke; no-op on routes without a back affordance.
            .on_action(cx.listener(|this, e: &TabAction, _window, cx| match e {
                TabAction::Close(ix) => this.close_tab(*ix, cx),
                TabAction::CloseOthers(ix) => this.close_others(*ix, cx),
                TabAction::CloseRight(ix) => this.close_right(*ix, cx),
            }))
            // ⌘1–⌘8 / Ctrl+1–8: jump to the Nth workspace tab (1-based key).
            // No-op when that index is not open yet (or already active).
            .on_action(cx.listener(|this, e: &WorkspaceTabAction, window, cx| {
                let WorkspaceTabAction::Select(ix) = *e;
                if ix < this.tabs.len() {
                    this.activate_tab(ix, Some(window), cx);
                    this.project_active_tab(cx);
                }
            }))
            // ⌘W / Ctrl+W closes the active workspace tab while more than one is
            // open. With a single tab (or any non-Close menu action) it
            // propagates to the app-level `MemuAction` handler, which hides the
            // app on macOS / closes the window elsewhere — the red-button
            // behavior. Handled here (not only globally) so the view's tab list
            // is in reach; ⌘W stays bound to the one `MemuAction::Close`.
            .on_action(cx.listener(|this, e: &MemuAction, _window, cx| {
                if matches!(e, MemuAction::Close) && this.tabs.len() > 1 {
                    this.close_tab(this.active_tab, cx);
                } else {
                    cx.propagate();
                }
            }))
            // ⌘F / secondary-f: focus the active page's filter box.
            // Must live on the window root — after a tab switch focus often
            // sits on the tab pill (sibling of content), so handlers only on
            // `ZedisContent` / `ZedisServers` never see the action.
            // The binding carries `!Input`, so while focus is inside any
            // gpui-component `Input` this never fires and a code editor's
            // built-in ⌘F search panel takes the keystroke instead.
            .on_action(cx.listener(|this, e: &EditorAction, window, cx| match e {
                EditorAction::Search => {
                    this.active_content().update(cx, |content, cx| {
                        content.focus_search(window, cx);
                    });
                }
                _ => cx.propagate(),
            }))
            .on_action(cx.listener(|_this, _e: &NavAction, window, cx| {
                // Esc first dismisses an open dialog (the server editor,
                // import/export, a confirm alert, …). This binding lives in the
                // `Workspace` context and consumes Esc without propagating, so
                // when a dialog's own `escape` handler doesn't win (e.g. focus
                // isn't inside the dialog subtree) Esc would otherwise be
                // swallowed here — doing nothing on Home, or worse, navigating
                // the page *behind* the still-open dialog on a Server route.
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                    return;
                }
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        if !matches!(
                            state.route(),
                            Route::Home
                                | Route::Settings
                                | Route::Server {
                                    view: ServerView::Editor,
                                    ..
                                }
                        ) {
                            state.go_to_view(ServerView::Editor, cx);
                        }
                    });
                });
            }))
    }
}

/// Where the active index lands after the tab at `from` is moved to `to`
/// (`Vec::remove` + `insert` semantics): the active tab follows itself, and a
/// tab crossing over it pushes the index one step the other way. Pure so the
/// arithmetic is unit-tested (see `tests` below).
fn moved_active_index(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && to >= active {
        active - 1
    } else if from > active && to <= active {
        active + 1
    } else {
        active
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

/// Shown when the local database can't be opened. Three causes, three
/// remedies: another instance holds the lock (quit it), the file was written by
/// a newer Zedis (update, or rebuild), or the file is damaged (rebuild).
/// "Back up & rebuild" moves the file aside as `zedis.redb.corrupt-<ts>` —
/// nothing is deleted; tags, favorites, history and scripts live in it —
/// creates a fresh one, and hands over to the normal startup (`launch`).
struct DatabaseErrorView {
    failure: DbOpenFailure,
    app_state: ZedisAppState,
    /// Why the last "Back up & rebuild" attempt failed, shown inline.
    rebuild_error: Option<String>,
}

impl DatabaseErrorView {
    fn new(failure: DbOpenFailure, app_state: ZedisAppState) -> Self {
        Self {
            failure,
            app_state,
            rebuild_error: None,
        }
    }

    /// No `ZedisGlobalStore` exists yet on this path, so translate against
    /// the locale straight from the loaded state.
    fn text(&self, key: &str) -> SharedString {
        t!(format!("database.{key}"), locale = self.app_state.locale())
            .to_string()
            .into()
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let quarantined = match quarantine_database() {
            Ok(path) => path,
            Err(e) => {
                error!(error = %e, "could not move the local database aside");
                self.rebuild_error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        warn!(quarantined = %quarantined.display(), "local database moved aside; creating a fresh one");
        if let Err(e) = init_database() {
            error!(error = %e, "rebuilding the local database failed");
            self.rebuild_error = Some(e.to_string());
            cx.notify();
            return;
        }
        init_caches();
        let handle = window.window_handle();
        launch(cx, self.app_state.clone());
        cx.spawn(async move |_this, cx| {
            // Queued behind `launch`'s own spawn, so the main window is open
            // before this one goes: on Linux/Windows the default QuitMode
            // ends the app when the last window closes. The guard keeps the
            // recovery window around rather than quitting if that ever
            // doesn't hold.
            cx.update(|cx| {
                if cx.windows().len() > 1 {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
            });
        })
        .detach();
    }
}

impl Render for DatabaseErrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (body_key, can_rebuild) = match &self.failure {
            DbOpenFailure::Locked => ("locked_body", false),
            DbOpenFailure::SchemaTooNew { .. } => ("schema_too_new_body", true),
            DbOpenFailure::Damaged(_) => ("damaged_body", true),
            DbOpenFailure::Inaccessible(_) => ("inaccessible_body", false),
        };
        let detail: Option<String> = match &self.failure {
            DbOpenFailure::Locked => None,
            DbOpenFailure::SchemaTooNew { found, supported } => Some(format!("schema v{found} > v{supported}")),
            DbOpenFailure::Damaged(message) | DbOpenFailure::Inaccessible(message) => Some(message.clone()),
        };
        let rebuild_error = self
            .rebuild_error
            .as_ref()
            .map(|e| format!("{}: {e}", self.text("rebuild_failed")));
        let (title, body, quit, rebuild) = (
            self.text("title"),
            self.text(body_key),
            self.text("quit"),
            self.text("rebuild"),
        );
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_5()
            .gap_3()
            .child(Label::new(title).font_semibold())
            .child(Label::new(body).whitespace_normal())
            .when_some(detail, |this, detail| {
                this.child(Label::new(detail).text_xs().text_color(muted).whitespace_normal())
            })
            .when_some(rebuild_error, |this, message| {
                this.child(Label::new(message).text_color(danger).whitespace_normal())
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("quit-db-error")
                            .label(quit)
                            .on_click(|_, _window, cx| cx.quit()),
                    )
                    .when(can_rebuild, |this| {
                        this.child(
                            Button::new("rebuild-db")
                                .label(rebuild)
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| this.rebuild(window, cx))),
                        )
                    }),
            )
    }
}

/// Value of a `--flag <value>` / `--flag=<value>` command-line argument.
fn cli_arg_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
        // `strip_prefix('=')` keeps `--db` from matching `--database=x`.
        if let Some(value) = arg.strip_prefix(flag).and_then(|rest| rest.strip_prefix('=')) {
            return Some(value.to_string());
        }
    }
    None
}

/// A parsed `--route <name>` target: app-level routes stand alone, while a
/// server view still needs the `(id, db)` the startup composer resolves.
enum CliRoute {
    App(Route),
    View(ServerView),
}

/// Startup view override: `--route <name>` (`home`, `editor`, `metrics`, …).
/// Together with `--server` / `--db` this is the deep-link MVP behind the
/// screenshot-comparison workflow. Unrecognized names log a warning and are
/// ignored.
fn cli_route_override() -> Option<CliRoute> {
    let raw = cli_arg_value("--route")?;
    if let Some(route) = Route::app_from_name(&raw) {
        return Some(CliRoute::App(route));
    }
    match ServerView::from_name(raw.trim().to_ascii_lowercase().as_str()) {
        Some(view) => Some(CliRoute::View(view)),
        None => {
            warn!(route = %raw, "unrecognized --route value; ignoring");
            None
        }
    }
}

/// Startup connection override: `--server <id|name>`, resolved to a server id
/// — exact id first, then exact name, then case-insensitive name.
fn cli_server_override() -> Option<String> {
    let raw = cli_arg_value("--server")?;
    let Ok(servers) = get_servers() else {
        warn!("server config unavailable; ignoring --server");
        return None;
    };
    let found = servers
        .iter()
        .find(|s| s.id == raw)
        .or_else(|| servers.iter().find(|s| s.name == raw))
        .or_else(|| servers.iter().find(|s| s.name.eq_ignore_ascii_case(&raw)));
    if found.is_none() {
        warn!(server = %raw, "no server matches --server by id or name; ignoring");
    }
    found.map(|s| s.id.clone())
}

/// Startup database override: `--db <n>`.
fn cli_db_override() -> Option<usize> {
    let raw = cli_arg_value("--db")?;
    let db = raw.parse::<usize>().ok();
    if db.is_none() {
        warn!(db = %raw, "invalid --db value; ignoring");
    }
    db
}

/// True when launched with `ZEDIS_SMOKE_TEST=1` — the CI smoke mode: exit 0
/// as soon as the first frame has painted, else the watchdog kills the
/// process with a nonzero code. See the hooks in `main`.
fn is_smoke_test() -> bool {
    std::env::var("ZEDIS_SMOKE_TEST").is_ok_and(|v| v == "1")
}

/// `ZEDIS_SMOKE_GATE=window` relaxes the smoke success signal from "first
/// frame painted" to "main window created and the process survived its
/// first seconds". Headless Linux CI (Xvfb + llvmpipe) never delivers the
/// frame-present signal, so the frame gate can't be a hard gate there —
/// this one still catches the regressions that matter on that platform:
/// missing system libraries, Vulkan / window-creation failures, startup
/// panics (DB, config, theme, fonts).
fn smoke_gate_is_window() -> bool {
    std::env::var("ZEDIS_SMOKE_GATE").is_ok_and(|v| v == "window")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Held for the whole run so the non-blocking file logger keeps flushing.
    let _log_guard = init_logger()?;
    // Crash context first, before anything that can panic: release builds
    // abort on panic, so the report the hook writes is the only trace left.
    let info = os_info::get();
    let os = format!("{}-{}", info.os_type(), info.version());
    let arch = info.architecture().unwrap_or_default().to_string();
    install_panic_hook(CrashContext {
        version: VERSION,
        git_sha: GIT_SHA,
        os: os.clone(),
        arch: arch.clone(),
    });
    let config_dir = if let Ok(dir) = get_or_create_config_dir() {
        dir.to_string_lossy().to_string()
    } else {
        "--".to_string()
    };
    info!(
        version = VERSION,
        git_sha = GIT_SHA,
        os,
        arch,
        config_dir,
        is_app_store_build = is_app_store_build(),
        sys_locale = ?get_locale(),
        "zedis launch"
    );
    if is_smoke_test() {
        // Smoke watchdog: in the 0.4.5/0.4.6 Windows failure mode (hidden
        // window never receives WM_PAINT) no frame ever paints, so the
        // success hook below never fires — turn that hang into a distinct
        // failing exit code instead of a stuck CI job.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(30));
            eprintln!("ZEDIS_SMOKE_TIMEOUT: no frame painted within 30s");
            std::process::exit(2);
        });
    }
    // Register tree-sitter languages we want the code editor to
    // highlight beyond the JSON-only default. Today this is just Lua
    // (Functions / EVAL editors); add others by extending the helper.
    register_extra_languages();
    // Hand the embedded command metadata to the connection crate — it has
    // no access to the app's asset bundle (see command.rs).
    if let Some(file) = assets::Assets::get("commands.json") {
        crate::connection::init_commands_json(file.data.to_vec());
    }
    let app = gpui_platform::application().with_assets(assets::Assets);
    let app_state = ZedisAppState::try_new().unwrap_or_else(|e| {
        error!(error = %e, "zedis.toml could not be loaded; starting with defaults");
        ZedisAppState::new()
    });
    if let Err(e) = get_servers() {
        error!(error = %e, "get servers fail",);
    }
    if let Err(e) = init_database() {
        let failure = open_failure_kind(&e);
        error!(error = %e, failure = ?failure, "init database failed; showing the recovery window");
        // Don't start a half-broken instance (the DB is required for tags /
        // history / proto / script / Lua). The window explains the cause and,
        // for a damaged or too-new file, offers to move it aside and rebuild.
        app.run(move |cx| {
            gpui_component::init(cx);
            // Match the user's chosen mode, or the OS appearance, so the error
            // window isn't a jarring light flash on a dark system.
            let mode = match app_state.theme() {
                Some(m) => m,
                None => theme_mode_for_appearance(cx.window_appearance()),
            };
            Theme::change(mode, None, cx);
            apply_default_ui_font_size(cx);
            cx.activate(true);
            let bounds = Bounds::centered(None, size(px(540.), px(300.)), cx);
            let opened = cx.open_window(
                with_app_identity(WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(440.), px(240.))),
                    ..Default::default()
                }),
                |window, cx| {
                    window.on_window_should_close(cx, |_window, cx| {
                        cx.quit();
                        true
                    });
                    let view = cx.new(|_| DatabaseErrorView::new(failure, app_state));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            if opened.is_err() {
                cx.quit();
            }
        });
        return Ok(());
    }
    init_caches();
    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);
        launch(cx, app_state);
    });
    Ok(())
}

/// Fills the in-memory proto / script / Lua caches from the local database.
/// Runs synchronously before the window opens — they back the proto/script/
/// Lua editor tables, and loading them in a post-window background task (the
/// old placement) raced: an editor restored as the startup route read an
/// empty cache and showed no rows until the view was recreated.
fn init_caches() {
    if let Err(e) = ProtoManager::init() {
        error!(error = %e, "init protos fail");
    }
    if let Err(e) = ScriptManager::init() {
        error!(error = %e, "init script viewers fail");
    }
    if let Err(e) = LuaScriptManager::init() {
        error!(error = %e, "init lua scripts fail");
    }
}

/// The normal startup once the local database is usable: theme, fonts, global
/// store, menus, hot keys, and the main window. Called from `main` directly,
/// or from the database recovery window after a successful rebuild — so it
/// must not assume anything about which windows already exist.
fn launch(cx: &mut App, app_state: ZedisAppState) {
    // Register the bundled JetBrains Mono faces (assets/fonts/*.ttf) so the
    // monospace family (`get_mono_font_family()`) renders real Regular / Bold
    // weights on every platform instead of leaning on whatever the OS ships
    // — see the "Bold needs a concrete font family" gotcha in CLAUDE.md.
    let fonts = ["fonts/JetBrainsMono-Regular.ttf", "fonts/JetBrainsMono-Bold.ttf"]
        .into_iter()
        .filter_map(|p| assets::Assets::get(p).map(|f| f.data))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        error!(error = %e, "failed to register bundled fonts");
    }
    // Register the embedded color themes so they appear in the theme menu.
    assets::register_themes(cx);

    cx.activate(true);
    let window_bounds = resolve_window_bounds(&app_state, cx);
    info!(bounds = ?window_bounds, "resolved window bounds");
    let app_state = cx.new(|_| app_state);
    let app_store = ZedisGlobalStore::new(app_state);
    // A saved named theme wins; otherwise fall back to the Light/Dark/System
    // mode (resolved against the OS appearance by the renderer).
    let saved_theme_name = app_store.read(cx).theme_name();
    let saved_mode = app_store.read(cx).theme();
    let applied = match saved_theme_name {
        Some(name) => apply_named_theme(&name, cx),
        None => false,
    };
    if !applied {
        // Resolve System mode (no saved name/mode) against the OS appearance
        // *before* the window opens, so the very first painted frame already
        // uses the right light/dark theme. Otherwise the default theme shows
        // for a frame and flashes (e.g. white before a dark theme settles).
        let mode = match saved_mode {
            Some(m) => m,
            None => theme_mode_for_appearance(cx.window_appearance()),
        };
        Theme::change(mode, None, cx);
    }
    // Theme::change / apply_named_theme reset font_size to stock 16; pin
    // the app rem base before the first frame (Root reads theme.font_size).
    apply_default_ui_font_size(cx);
    cx.set_global(app_store);
    // From here on every exit path flushes the state on the way out; nothing
    // else needs to remember to.
    flush_app_state_on_quit(cx);
    // Apply the saved font preferences onto the (already-initialized)
    // Theme before the first frame, so the initial paint uses them.
    {
        let (ui_font, mono_font) = {
            let store = cx.global::<ZedisGlobalStore>().read(cx);
            (store.ui_font_family(), store.mono_font_family())
        };
        apply_fonts(cx, ui_font.as_deref(), mono_font.as_deref());
    }
    // Mirror the persisted proxy setting into helpers::proxy before the
    // startup update check fires — its HTTP runs on background threads
    // that can't read the store.
    {
        let proxy = cx.global::<ZedisGlobalStore>().read(cx).http_proxy();
        set_configured_proxy(&proxy);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let tray_enabled = cx.global::<ZedisGlobalStore>().read(cx).tray_enabled();
        if tray_enabled {
            tray::init_tray(cx);
        }
    }
    cx.bind_keys(new_hot_keys());
    cx.on_action(|e: &MemuAction, cx: &mut App| match e {
        MemuAction::Quit => {
            cx.quit();
        }
        MemuAction::About => {
            open_about_window(cx);
        }
        MemuAction::Close => {
            // ⌘W / Ctrl+W mirrors the red close button: on macOS it hides
            // the app (see on_window_should_close); elsewhere it closes the
            // active window (which quits the app when it's the last one).
            #[cfg(target_os = "macos")]
            cx.hide();
            #[cfg(not(target_os = "macos"))]
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, _cx| window.remove_window());
            }
        }
        MemuAction::OpenLogs => match logs_dir() {
            // `logs_dir` creates the directory, so it exists even before any
            // log line has been written.
            Some(logs) => cx.open_with_system(&logs),
            None => error!("failed to resolve logs directory"),
        },
    });
    let mut menu_items = vec![MenuItem::action("About Zedis", MemuAction::About)];
    // App Store builds update via the App Store — hide the manual check.
    if !is_app_store_build() {
        menu_items.push(MenuItem::action("Check for Updates", UpdateAction::Check));
    }
    menu_items.extend([
        MenuItem::action("Open Logs Folder", MemuAction::OpenLogs),
        MenuItem::action("Export Diagnostics…", DiagnosticsAction::Export),
        MenuItem::action("Close Window", MemuAction::Close),
        MenuItem::action("Quit", MemuAction::Quit),
    ]);
    cx.set_menus(vec![Menu {
        name: "Zedis".into(),
        items: menu_items,
        disabled: false,
    }]);

    cx.spawn(async move |cx| {
        cx.open_window(
            with_app_identity(WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                // macOS / Windows: custom-drawn title bar (transparent OS chrome).
                // Linux: server-side decorations show the title from
                // `with_app_identity` ("Zedis") — see issue #106.
                #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
                }),
                // macOS only: create the window hidden and reveal it after
                // the first themed frame (see on_next_frame below) so
                // there's no white flash before the theme paints. Windows
                // must be shown immediately — its frames are driven by
                // WM_PAINT, which hidden windows never receive, so the
                // "reveal on first frame" deadlocks and the window never
                // appears (the 0.4.5/0.4.6 auto-hide bug). Its backing is
                // a black brush, so there's no white flash to hide anyway.
                // Linux too — Wayland can't reliably reveal a window that
                // was never mapped.
                show: cfg!(not(target_os = "macos")),
                window_min_size: Some(size(px(600.), px(400.))),
                ..Default::default()
            }),
            |window, cx| {
                #[cfg(target_os = "macos")]
                window.on_window_should_close(cx, move |_window, cx| {
                    cx.hide();
                    false
                });
                // Reveal the (hidden) window only after the first frame has
                // painted, so the user never sees the default white backing
                // before the themed background. Pairs with `show: false`.
                // macOS only: it paints hidden windows; Windows/Linux never
                // deliver a frame to an unmapped window, so this callback
                // would never fire there (see the `show:` comment above).
                #[cfg(target_os = "macos")]
                window.on_next_frame(|window, _cx| window.activate_window());
                // CI smoke mode: a painted first frame is exactly the
                // signal the 0.4.5/0.4.6 Windows regression killed
                // (hidden windows never get WM_PAINT, so no frame ever
                // comes). Success → exit 0; the watchdog in `main`
                // turns "no frame" into exit 2.
                if is_smoke_test() {
                    // Stage 1: the window exists. Printed on every platform
                    // (useful in the log either way); with the `window` gate
                    // it is also the success signal, after a grace period
                    // long enough for a startup panic to surface.
                    println!("ZEDIS_SMOKE_WINDOW");
                    if smoke_gate_is_window() {
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_secs(5));
                            println!("ZEDIS_SMOKE_OK (window gate)");
                            std::process::exit(0);
                        });
                    }
                    // Stage 2: a painted frame — the real signal wherever a
                    // display exists.
                    window.on_next_frame(|_window, _cx| {
                        println!("ZEDIS_SMOKE_OK");
                        std::process::exit(0);
                    });
                    // Note for CI: under Xvfb + llvmpipe this hook has
                    // never fired — the frame-present signal doesn't
                    // arrive in that environment even with redraws
                    // forced every second (verified 2026-07-17), so the
                    // Linux smoke step runs best-effort until that's
                    // understood upstream. Real displays (macOS /
                    // Windows runners, actual Linux desktops) fire it
                    // normally.
                }
                let zedis_view = cx.new(|cx| Zedis::new(window, cx));
                // Activate the target connection + view now that the views
                // are subscribed. Deep-link launch args (`--server
                // <id|name>`, `--db <n>`, `--route <view>`) override the
                // remembered state piecewise; with no args this restores
                // the last session. The re-select makes the emitted
                // ServerSelected load the connection (content) and
                // highlight its sidebar row.
                {
                    let store = cx.global::<ZedisGlobalStore>().clone();
                    store.update(cx, |state, cx| {
                        let cli_server = cli_server_override();
                        let cli_db = cli_db_override();
                        // Target connection: the CLI server wins; else the
                        // remembered one, validated (it may have been
                        // deleted since the last run).
                        let target: Option<(String, usize)> = match &cli_server {
                            Some(id) => Some((id.clone(), cli_db.unwrap_or_else(|| state.open_db_for(id)))),
                            None => state
                                .selected_server()
                                .cloned()
                                .filter(|(id, _)| get_server(id).is_ok())
                                // A pinned DB wins over the remembered one
                                // here too, so a restart lands where a
                                // sidebar click would; `--db` still wins
                                // over both.
                                .map(|(id, db)| {
                                    let db = cli_db.unwrap_or_else(|| state.open_db_from(&id, db));
                                    (id, db)
                                }),
                        };
                        // Target view: explicit --route wins; a bare
                        // --server implies the editor (a deep link should
                        // land on that server, not whatever page was
                        // persisted); otherwise the restored route's view.
                        let view = match (cli_route_override(), cli_server.is_some()) {
                            (Some(CliRoute::App(route)), _) => {
                                state.activate(route, cx);
                                return;
                            }
                            (Some(CliRoute::View(view)), _) => Some(view),
                            (None, true) => Some(ServerView::Editor),
                            (None, false) => state.route().server_view(),
                        };
                        let route = match (view, target) {
                            (Some(view), Some((id, db))) => Route::Server {
                                id: id.into(),
                                db,
                                view,
                            },
                            (Some(_), None) => {
                                warn!("server view requested but no valid server available; opening Home");
                                Route::Home
                            }
                            // Restored app-level route (or Home).
                            (None, _) => state.route(),
                        };
                        state.activate(route, cx);
                    });
                }
                // Global (focus-independent) ⌘K handler — element
                // `.on_action` is focus-routed and dies when the
                // palette closes and orphans its focus handle.
                let weak_zedis = zedis_view.downgrade();
                cx.on_action(move |_: &PaletteAction, cx: &mut App| {
                    if let Some(view) = weak_zedis.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_command_palette(cx));
                    }
                });
                // Global (focus-independent) ⌘P — recent keys Quick Open.
                let weak_zedis_recent = zedis_view.downgrade();
                cx.on_action(move |_: &RecentKeysAction, cx: &mut App| {
                    if let Some(view) = weak_zedis_recent.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_recent_keys_palette(cx));
                    }
                });
                // Global (focus-independent) ⌘⇧F — multi-database search.
                let weak_zedis_multi_search = zedis_view.downgrade();
                cx.on_action(move |_: &MultiSearchAction, cx: &mut App| {
                    if let Some(view) = weak_zedis_multi_search.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_multi_search(cx));
                    }
                });
                // Global (focus-independent) ⌘/ handler — same
                // rationale as the ⌘K handler above; also the target
                // of the command palette's "Keyboard Shortcuts" row.
                let weak_zedis_shortcuts = zedis_view.downgrade();
                cx.on_action(move |_: &ShortcutsAction, cx: &mut App| {
                    if let Some(view) = weak_zedis_shortcuts.upgrade() {
                        view.update(cx, |zedis, cx| zedis.toggle_shortcuts(cx));
                    }
                });
                // Manual "Check for Updates" (app menu) — focus-independent
                // like the handlers above so it works regardless of focus.
                let weak_zedis_update = zedis_view.downgrade();
                cx.on_action(move |e: &UpdateAction, cx: &mut App| {
                    let Some(view) = weak_zedis_update.upgrade() else {
                        return;
                    };
                    match e {
                        UpdateAction::Check => {
                            view.update(cx, |zedis, cx| zedis.check_for_updates(true, false, cx));
                        }
                        // Title-bar chip click: open the prompt straight from
                        // the store. The check that lit the chip already
                        // fetched everything the dialog shows — version,
                        // changelog, the per-arch asset — so re-fetching here
                        // only made the chip spin for a beat before anything
                        // appeared. The fallback fetch is for the (unreachable)
                        // case of a chip with no cached update behind it.
                        UpdateAction::OpenPrompt => {
                            let cached = cx.global::<ZedisGlobalStore>().read(cx).available_update();
                            view.update(cx, |zedis, cx| match cached {
                                Some(info) => {
                                    zedis.pending_update = Some(info);
                                    cx.notify();
                                }
                                None => zedis.check_for_updates(true, true, cx),
                            });
                        }
                    }
                });
                // Silent startup check: at most one per `UPDATE_CHECK_INTERVAL`,
                // skippable per-version, only if enabled. The update chip lives
                // in the always-visible title bar, so this can run on any route.
                let auto_due = {
                    let store = cx.global::<ZedisGlobalStore>().read(cx);
                    store.auto_update_check() && store.update_check_due()
                };
                if auto_due {
                    zedis_view.update(cx, |zedis, cx| zedis.check_for_updates(false, false, cx));
                }
                // Config files damaged at startup (`zedis.toml` /
                // `redis-servers.toml`) were quarantined before the window
                // existed; surface the outcome now that there is one.
                zedis_view.update(cx, |zedis, _| {
                    zedis.pending_config_recoveries = take_config_recoveries();
                    zedis.pending_crash = take_pending_crash();
                });
                // One-shot welcome card, and only for a truly fresh start
                // (no server configured — an upgrading user needs no tour).
                // Dismissed on this first evaluation either way, so it can
                // never pop up later (e.g. after deleting every server).
                if !cx.global::<ZedisGlobalStore>().read(cx).hint_dismissed(HINT_WELCOME) {
                    update_app_state_and_save_quiet(cx, "dismiss_hint_welcome", |state, _| {
                        state.dismiss_hint(HINT_WELCOME)
                    });
                    // An unreadable config counts as configured: don't greet
                    // an existing user just because the file failed to parse.
                    if get_servers().map(|servers| servers.is_empty()).unwrap_or(false) {
                        zedis_view.update(cx, |zedis, _| zedis.pending_welcome = true);
                    }
                }
                cx.new(|cx| Root::new(zedis_view, window, cx))
            },
        )?;

        Ok::<_, anyhow::Error>(())
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::moved_active_index;

    /// Reference model: perform the actual remove+insert on tab ids and
    /// locate where the active id ended up.
    fn model(len: usize, active: usize, from: usize, to: usize) -> usize {
        let mut v: Vec<usize> = (0..len).collect();
        let moved = v.remove(from);
        v.insert(to, moved);
        v.iter().position(|&id| id == active).expect("active id present")
    }

    #[test]
    fn active_index_follows_tab_moves_exhaustively() {
        // Exhaustive over every (len ≤ 6, active, from, to) combination,
        // checked against the reference model — locks the drag-reorder
        // index arithmetic in `Zedis::move_tab`.
        for len in 1..=6usize {
            for active in 0..len {
                for from in 0..len {
                    for to in 0..len {
                        assert_eq!(
                            moved_active_index(active, from, to),
                            model(len, active, from, to),
                            "len={len} active={active} from={from} to={to}"
                        );
                    }
                }
            }
        }
    }
}

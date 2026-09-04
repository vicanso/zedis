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

//! The `Zedis` root view: sidebar, workspace tabs (one connection each),
//! title bar, and the global action handlers. `main.rs` only launches it.

use crate::connection::{DangerKind, clear_expired_cache, get_server, servers_toml_redacted};
use crate::constants::{SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH};
use crate::db::{TRASH_RETENTION_MS, purge_all_trash};
use crate::dialogs::*;
use crate::helpers::{
    ConfigRecovery, CrashReport, DEFAULT_UI_FONT_SIZE, Delivery, DiagnosticsAction, DiagnosticsInput, EditorAction,
    MemuAction, NavAction, UpdateInfo, WindowAction, WorkspaceTabAction, ZoomAction, apply_default_ui_font_size,
    download_and_verify, export_diagnostics, fetch_latest_release, get_or_create_config_dir, humanize_keystroke,
    install_update, installer_requires_quit, is_app_store_build, unix_ts_millis,
};
use crate::startup::{GIT_SHA, PKG_NAME, VERSION};
use crate::states::{
    GlobalEvent, LocaleAction, NotificationCategory, Route, SelectThemeAction, ServerToolsAction, ServerView,
    SettingsAction, ThemeAction, WindowPlacement, ZedisGlobalStore, i18n_common, i18n_sidebar, i18n_update,
    save_app_state, update_app_state_and_save, update_app_state_and_save_quiet,
};
use crate::views::{
    ExportSource, ZedisCommandPalette, ZedisContent, ZedisMultiSearch, ZedisRecentKeysPalette, ZedisShortcutsOverlay,
    ZedisSidebar, ZedisTitleBar, confirm_dangerous_command, open_features_dialog, open_migration_export_window,
    open_migration_import_window, open_settings_window, open_trash_dialog,
};
use crate::window_setup::*;
use gpui::{Action, Bounds, Entity, MouseButton, Pixels, Point, SharedString, Task, Window, div, prelude::*};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
use gpui_kit::component::{
    ActiveTheme, IconName, Root, Sizable, Theme, ThemeMode, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    menu::ContextMenuExt,
    notification::Notification,
    v_flex,
};
use rust_i18n::t;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info};

/// The Settings slider's range, which ⌘+ / ⌘- step within.
const UI_ZOOM_MIN_PX: f32 = 12.0;
const UI_ZOOM_MAX_PX: f32 = 20.0;

/// Upper bound on workspace tabs — each tab holds its own `ZedisServerState`
/// (heartbeat, pooled connections, loaded keys), so the cap keeps a runaway
/// tab strip from piling up background Redis traffic.
pub(crate) const MAX_TABS: usize = 8;

/// One workspace tab: a content column bound to a connection. `server_id`
/// stays empty until a server is selected in this tab.
pub(crate) struct ContentTab {
    server_id: String,
    db: usize,
    content: Entity<ZedisContent>,
}

/// Context-menu actions on a workspace tab (dispatched by the tab strip's
/// right-click menu, handled on the `Zedis` root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
pub(crate) enum TabAction {
    Close(usize),
    CloseOthers(usize),
    CloseRight(usize),
}

/// Drag payload for reordering workspace tabs.
pub(crate) struct DraggedTab {
    from: usize,
}

/// Floating preview shown while a tab is dragged.
pub(crate) struct TabDragPreview {
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
    pub(crate) pending_update: Option<UpdateInfo>,
    /// The in-flight update check, if any — guards against overlapping checks.
    update_task: Option<Task<()>>,
    /// The in-flight installer download, if any — guards against re-entry.
    download_task: Option<Task<()>>,
    /// The installer is open and this platform needs Zedis gone to finish the
    /// install — prompt to quit. Consumed in `render` (which has the `Window`).
    pending_install_quit: bool,
    /// First launch with nothing configured — show the one-time welcome card.
    /// Consumed in `render` (which has the `Window` needed for the dialog).
    pub(crate) pending_welcome: bool,
    /// Config files that were damaged and recovered (or reset) while loading
    /// at startup — before any window existed to report it. Consumed in
    /// `render`, one notification each.
    pub(crate) pending_config_recoveries: Vec<ConfigRecovery>,
    /// The crash report the previous run left behind, if it ended in a panic.
    /// Consumed in `render` (which has the `Window` needed for the dialog).
    pub(crate) pending_crash: Option<CrashReport>,
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
    pub(crate) fn check_for_updates(&mut self, manual: bool, then_prompt: bool, cx: &mut Context<Self>) {
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
        let include_prerelease = cx.global::<ZedisGlobalStore>().read(cx).update_prerelease();
        self.update_task = Some(cx.spawn(async move |handle, cx| {
            // `fetch_latest_release` is blocking (ureq) — keep it off the UI thread.
            let result = cx
                .background_spawn(async move { fetch_latest_release(include_prerelease) })
                .await;
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
    pub(crate) fn start_download(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
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
        maximized: bool,
        cx: &mut Context<Self>,
    ) {
        self.last_bounds = new_bounds;
        let store = cx.global::<ZedisGlobalStore>().clone();
        // Anchor the placement to the current display (origin relative to it) so
        // it survives monitor rearrangement; absolute `bounds` stays as fallback.
        let placement = display.map(|(display_uuid, screen_origin)| WindowPlacement {
            display_uuid,
            bounds: new_bounds - screen_origin,
            maximized,
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
                // The maximized rectangle is the display's, not the window's.
                if !maximized {
                    state.set_bounds(new_bounds);
                }
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
            self.persist_window_state(current_bounds, display, window.is_maximized(), cx);
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
            // ⌘+ / ⌘- / ⌘0: the same UI font size the Settings slider sets,
            // stepped by one pixel inside the slider's range; the store's
            // value is applied to the theme on the next frame.
            .on_action(cx.listener(|_this, e: &ZoomAction, _window, cx| {
                let current = cx
                    .global::<ZedisGlobalStore>()
                    .read(cx)
                    .font_rem_px()
                    .unwrap_or(DEFAULT_UI_FONT_SIZE);
                let next = match e {
                    ZoomAction::In => current + 1.0,
                    ZoomAction::Out => current - 1.0,
                    ZoomAction::Reset => DEFAULT_UI_FONT_SIZE,
                }
                .clamp(UI_ZOOM_MIN_PX, UI_ZOOM_MAX_PX);
                update_app_state_and_save(cx, "zoom", move |state, _| {
                    state.set_font_rem_px(Some(next));
                });
            }))
            .on_action(cx.listener(|_this, e: &WindowAction, window, _cx| match e {
                WindowAction::Minimize => window.minimize_window(),
                WindowAction::Zoom => window.zoom_window(),
                WindowAction::ToggleFullscreen => window.toggle_fullscreen(),
            }))
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
pub(crate) fn moved_active_index(active: usize, from: usize, to: usize) -> usize {
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

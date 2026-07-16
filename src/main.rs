#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::connection::{clear_expired_cache, get_server, get_servers};
use crate::constants::{SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH};
use crate::db::{LuaScriptManager, ProtoManager, ScriptManager, TRASH_RETENTION_MS, init_database, purge_all_trash};
use crate::helpers::{
    MemuAction, NavAction, PaletteAction, RecentKeysAction, ShortcutsAction, UpdateAction, UpdateInfo,
    WorkspaceTabAction, apply_fonts, download_and_verify, fetch_latest_release, focus_installer_ui,
    get_or_create_config_dir, init_logger, installer_requires_quit, is_app_store_build, logs_dir, new_hot_keys,
    open_installer, register_extra_languages, unix_ts_millis, with_app_identity,
};
use crate::states::{
    GlobalEvent, LocaleAction, NotificationCategory, Route, SelectThemeAction, ServerToolsAction, ServerView,
    SettingsAction, ThemeAction, WindowPlacement, ZedisAppState, ZedisGlobalStore, i18n_common, i18n_sidebar,
    i18n_update, save_app_state, update_app_state_and_save,
};
use crate::views::{
    DialogCallback, ZedisCommandPalette, ZedisContent, ZedisRecentKeysPalette, ZedisShortcutsOverlay, ZedisSidebar,
    ZedisTitleBar, ZedisUpdateDialog, open_about_window, open_migration_import_window, open_settings_window,
    open_trash_dialog,
};
use gpui::{
    Action, App, Bounds, Entity, Menu, MenuItem, MouseButton, Pixels, Point, SharedString, Task, TitlebarOptions,
    WeakEntity, Window, WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px, rems, size,
};
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
// compile time; the real `locales/*.toml` are loaded (compressed) at runtime by
// `i18n_loader::runtime_backend`, which the `t!` lookups resolve through. See
// `src/i18n_loader.rs`.
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
            let saved: Vec<(String, usize)> = store
                .open_tabs()
                .iter()
                .filter(|(id, _)| get_server(id).is_ok())
                .cloned()
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
        if let Some((id, db)) = selected
            && let Some(ix) = tabs.iter().position(|tab| tab.server_id == id && tab.db == db)
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
                    if let Some(title) = e.title.as_ref() {
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
                GlobalEvent::ServerOpenInNewTab(server_id, db) => {
                    if let Some(ix) = this
                        .tabs
                        .iter()
                        .position(|tab| tab.server_id.as_str() == server_id.as_str() && tab.db == *db)
                    {
                        this.activate_tab(ix, cx);
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
        }
    }

    /// The active tab's content column (what the root lays out and whose
    /// status bar is shown).
    fn active_content(&self) -> Entity<ZedisContent> {
        self.tabs[self.active_tab].content.clone()
    }

    /// Switch the active tab: the outgoing tab stops reacting to global
    /// route/server broadcasts, the incoming one resumes, and the palettes
    /// are rebound to the incoming tab's server state. No-op when `ix` is
    /// already active (or out of range).
    fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
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
    /// the same workspace tabs. Tabs still on Home (empty id) are skipped.
    fn persist_tabs(&self, cx: &mut Context<Self>) {
        let tabs: Vec<(String, usize)> = self
            .tabs
            .iter()
            .filter(|tab| !tab.server_id.is_empty())
            .map(|tab| (tab.server_id.clone(), tab.db))
            .collect();
        update_app_state_and_save(cx, "save_open_tabs", move |state, _| state.set_open_tabs(tabs.clone()));
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
        // Reset the once-per-day throttle on every attempt so a transient
        // failure doesn't immediately retry on the next launch.
        update_app_state_and_save(cx, "mark_update_checked", |state, _| state.mark_update_checked());
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
                    .and_then(|path| open_installer(&path));
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
                    Ok(()) => {
                        info!(version = %version, "update: download finished, installer handed to the OS");
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "download_done")));
                        // macOS / Windows: the installer can't replace a running
                        // Zedis, so offer to quit (the dialog needs a `Window`,
                        // which only `render` has — hence the flag).
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
        let mut value = store.value(cx);
        value.set_bounds(new_bounds);
        // Anchor the placement to the current display (origin relative to it) so
        // it survives monitor rearrangement; absolute `bounds` stays as fallback.
        let placement = display.map(|(display_uuid, screen_origin)| WindowPlacement {
            display_uuid,
            bounds: new_bounds - screen_origin,
        });
        if let Some(p) = &placement {
            value.upsert_window_placement(p.clone());
        }
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            store.update(cx, move |state, cx| {
                state.set_bounds(new_bounds);
                if let Some(p) = placement {
                    state.upsert_window_placement(p);
                }
                cx.notify();
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
        let active_bg = cx.theme().foreground.alpha(0.1);
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
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.activate_tab(ix, cx);
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
                            .when(!is_active, |this| this.text_color(muted))
                            .child(Label::new(title).text_sm().whitespace_nowrap())
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
}

/// Open the "update available" dialog on the main window. **Download** opens the
/// release page in the browser; **Skip this version** records the version so the
/// silent startup check won't prompt for it again. Closing without choosing
/// leaves nothing recorded, so the next daily check prompts again.
/// Compact Markdown styling for release notes: the library defaults size
/// headings up to ~28px, which dwarfs the dialog body; shrink them to a
/// gentle hierarchy (mirrors the memory-analysis AI panel styling).
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
            // App state is normally persisted by an async task (`cx.spawn` →
            // background write), which `cx.quit()` does not wait for. Flush the
            // current state synchronously first so a just-made change (window
            // bounds, open tabs) can't be lost on the way out.
            let state = cx.global::<ZedisGlobalStore>().read(cx).clone();
            if let Err(e) = save_app_state(&state) {
                error!(error = %e, "update: failed to flush state before quitting");
            }
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
        update_app_state_and_save(cx, "skip_update_version", move |state, _| {
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
                update_app_state_and_save(cx, "skip_update_version", move |state, _| {
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
        // The installer is up and this platform needs Zedis closed to finish —
        // ask (the update dialog has already dismissed itself by now).
        if std::mem::take(&mut self.pending_install_quit) {
            open_install_quit_dialog(window, cx);
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
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.connect_server(id, db, cx));
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
                    None => match cx.window_appearance() {
                        WindowAppearance::Light => ThemeMode::Light,
                        _ => ThemeMode::Dark,
                    },
                };

                // A previously-applied named theme overwrote the Theme's
                // light/dark slot, so restore the registry defaults first —
                // otherwise switching mode would just re-apply that named theme.
                restore_default_themes(cx);
                // Apply theme immediately for instant visual feedback
                Theme::change(render_mode, None, cx);

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
                    ServerToolsAction::ValueSearch => ServerView::ValueSearch,
                    // A dialog, not a sub-route: keeps whatever view is
                    // active underneath.
                    ServerToolsAction::Trash => {
                        open_trash_dialog(window, cx);
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
            .on_action(cx.listener(|this, e: &WorkspaceTabAction, _window, cx| {
                let WorkspaceTabAction::Select(ix) = *e;
                if ix < this.tabs.len() {
                    this.activate_tab(ix, cx);
                    this.project_active_tab(cx);
                }
            }))
            .on_action(cx.listener(|_this, _e: &NavAction, _window, cx| {
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

/// Minimal window shown when the local database can't be opened — most often
/// because another Zedis instance is already running and holds the lock. We
/// surface this and exit instead of silently starting a half-broken instance
/// (tags / search history / proto / script / Lua features would all fail).
struct DatabaseErrorView;

impl Render for DatabaseErrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_5()
            .gap_3()
            .child(Label::new("Zedis can't open its database").font_semibold())
            .child(
                Label::new(
                    "Another instance of Zedis may already be running and holding the database \
                     lock, or the database file is inaccessible. Quit the other instance, then \
                     reopen Zedis.",
                )
                .whitespace_normal(),
            )
            .child(div().flex_1())
            .child(
                h_flex().justify_end().child(
                    Button::new("quit-db-error")
                        .label("Quit")
                        .primary()
                        .on_click(|_, _window, cx| cx.quit()),
                ),
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Held for the whole run so the non-blocking file logger keeps flushing.
    let _log_guard = init_logger()?;
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
    let app_state = ZedisAppState::try_new().unwrap_or_else(|_| ZedisAppState::new());
    if let Err(e) = get_servers() {
        error!(error = %e, "get servers fail",);
    }
    if let Err(e) = init_database() {
        error!(error = %e, "init database failed — another Zedis instance may hold the lock; showing error and exiting");
        // Don't start a half-broken second instance (the DB is required for
        // tags / history / proto / script / Lua). Show a clear window and quit.
        let saved_mode = app_state.theme();
        app.run(move |cx| {
            gpui_component::init(cx);
            // Match the user's chosen mode, or the OS appearance, so the error
            // window isn't a jarring light flash on a dark system.
            let mode = match saved_mode {
                Some(m) => m,
                None => match cx.window_appearance() {
                    WindowAppearance::Light => ThemeMode::Light,
                    _ => ThemeMode::Dark,
                },
            };
            Theme::change(mode, None, cx);
            cx.activate(true);
            let bounds = Bounds::centered(None, size(px(460.), px(220.)), cx);
            let opened = cx.open_window(
                with_app_identity(WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(380.), px(180.))),
                    ..Default::default()
                }),
                |window, cx| {
                    window.on_window_should_close(cx, |_window, cx| {
                        cx.quit();
                        true
                    });
                    let view = cx.new(|_| DatabaseErrorView);
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            if opened.is_err() {
                cx.quit();
            }
        });
        return Ok(());
    }
    // Populate the in-memory proto / script / lua caches synchronously, before
    // the window opens — they back the proto/script/lua editor tables. Loading
    // them in a post-window background task (the old placement) raced: an editor
    // restored as the startup route read an empty cache and showed no rows until
    // the view was recreated (switching away and back).
    if let Err(e) = ProtoManager::init() {
        error!(error = %e, "init protos fail");
    }
    if let Err(e) = ScriptManager::init() {
        error!(error = %e, "init script viewers fail");
    }
    if let Err(e) = LuaScriptManager::init() {
        error!(error = %e, "init lua scripts fail");
    }
    let config_dir = if let Ok(dir) = get_or_create_config_dir() {
        dir.to_string_lossy().to_string()
    } else {
        "--".to_string()
    };
    let info = os_info::get();
    let os = format!("{}-{}", info.os_type(), info.version());
    info!(
        version = VERSION,
        git_sha = GIT_SHA,
        os,
        arch = info.architecture().unwrap_or_default().to_string(),
        config_dir,
        is_app_store_build = is_app_store_build(),
        sys_locale = ?get_locale(),
        "zedis launch"
    );

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);
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
                None => match cx.window_appearance() {
                    WindowAppearance::Light => ThemeMode::Light,
                    _ => ThemeMode::Dark,
                },
            };
            Theme::change(mode, None, cx);
        }
        cx.set_global(app_store);
        // Apply the saved font preferences onto the (already-initialized)
        // Theme before the first frame, so the initial paint uses them.
        {
            let (ui_font, mono_font) = {
                let store = cx.global::<ZedisGlobalStore>().read(cx);
                (store.ui_font_family(), store.mono_font_family())
            };
            apply_fonts(cx, ui_font.as_deref(), mono_font.as_deref());
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
                        window.on_next_frame(|_window, _cx| {
                            println!("ZEDIS_SMOKE_OK");
                            std::process::exit(0);
                        });
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
                                Some(id) => Some((id.clone(), cli_db.unwrap_or_else(|| state.last_db_for(id)))),
                                None => state
                                    .selected_server()
                                    .cloned()
                                    .filter(|(id, _)| get_server(id).is_ok())
                                    .map(|(id, db)| (id, cli_db.unwrap_or(db))),
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
                    // Silent startup check: once per day at most, skippable
                    // per-version, only if enabled. The update chip lives in the
                    // always-visible title bar, so this can run on any route.
                    let auto_due = {
                        let store = cx.global::<ZedisGlobalStore>().read(cx);
                        store.auto_update_check() && store.update_check_due()
                    };
                    if auto_due {
                        zedis_view.update(cx, |zedis, cx| zedis.check_for_updates(false, false, cx));
                    }
                    cx.new(|cx| Root::new(zedis_view, window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
    Ok(())
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

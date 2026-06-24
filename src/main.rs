#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::connection::{clear_expired_cache, get_servers};
use crate::constants::SIDEBAR_WIDTH;
use crate::db::{LuaScriptManager, ProtoManager, ScriptManager, init_database};
use crate::helpers::{
    MemuAction, NavAction, PaletteAction, ShortcutsAction, UpdateAction, UpdateInfo, download_and_verify,
    fetch_latest_release, get_default_font_family, get_or_create_config_dir, is_app_store_build, is_development,
    new_hot_keys, open_installer, register_extra_languages,
};
use crate::states::{
    GlobalEvent, LocaleAction, NotificationCategory, Route, SelectThemeAction, ServerToolsAction, SettingsAction,
    ThemeAction, WindowPlacement, ZedisAppState, ZedisGlobalStore, i18n_update, save_app_state,
    update_app_state_and_save,
};
use crate::views::{
    ZedisCommandPalette, ZedisContent, ZedisShortcutsOverlay, ZedisSidebar, ZedisTitleBar, open_about_window,
    open_settings_window,
};
use gpui::{
    App, Bounds, Entity, Menu, MenuItem, Pixels, Point, Task, TitlebarOptions, WeakEntity, Window, WindowAppearance,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, Root, StyledExt, Theme, ThemeMode, ThemeRegistry, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    v_flex,
};
use std::{cell::Cell, env, rc::Rc, str::FromStr, time::Duration};
use sys_locale::get_locale;
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;
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

pub struct Zedis {
    pending_notification: Option<Notification>,
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
    // views
    sidebar: Entity<ZedisSidebar>,
    content: Entity<ZedisContent>,
    command_palette: Entity<ZedisCommandPalette>,
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
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| ZedisSidebar::new(window, cx));
        let content = cx.new(|cx| ZedisContent::new(window, cx));
        // The palette fuzzy-searches the active connection's loaded keys, so
        // hand it the content's shared ServerState entity.
        let server_state = content.read(cx).server_state();
        let command_palette = cx.new(|cx| ZedisCommandPalette::new(server_state, window, cx));
        let shortcuts_overlay = cx.new(ZedisShortcutsOverlay::new);
        let global_state = cx.global::<ZedisGlobalStore>().state();
        cx.subscribe(&global_state, |this, _server_state, event, cx| {
            if let GlobalEvent::Notification(e) = event {
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
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                clear_expired_cache();
            }
        }));
        let title_bar = Some(cx.new(|cx| ZedisTitleBar::new(window, cx)));

        Self {
            sidebar,
            save_task: None,
            content,
            command_palette,
            shortcuts_overlay,
            pending_notification: None,
            title_bar,
            theme_update_task: None,
            _clear_expired_cache: clear_expired_cache,
            last_bounds: Bounds::default(),
            pending_update: None,
            update_task: None,
            download_task: None,
        }
    }

    /// Kick off a background check for a newer release. A `manual` check always
    /// reports its outcome (up-to-date / failure toast) and ignores a skipped
    /// version; the silent startup check stays quiet unless it finds a fresh,
    /// non-skipped update.
    fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update_task.is_some() {
            return;
        }
        // Reset the once-per-day throttle on every attempt so a transient
        // failure doesn't immediately retry on the next launch.
        update_app_state_and_save(cx, "mark_update_checked", |state, _| state.mark_update_checked());
        self.update_task = Some(cx.spawn(async move |handle, cx| {
            // `fetch_latest_release` is blocking (ureq) — keep it off the UI thread.
            let result = cx.background_spawn(async move { fetch_latest_release() }).await;
            let _ = handle.update(cx, |this, cx| {
                this.update_task = None;
                match result {
                    Ok(Some(info)) => {
                        let skipped = cx.global::<ZedisGlobalStore>().read(cx).update_skipped(&info.version);
                        if manual || !skipped {
                            let version = info.version.clone();
                            // Light the persistent far-right status-bar chip (its
                            // click opens the download prompt)...
                            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                state.set_available_update(Some(info), cx);
                            });
                            // ...and fire a one-time toast so the user notices it.
                            this.pending_notification =
                                Some(Notification::info(format!("{}: v{version}", i18n_update(cx, "found"))));
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
            cx.open_url(&info.page_url);
            return;
        };
        if self.download_task.is_some() {
            return;
        }
        let page_url = info.page_url.clone();
        let starting = format!("{} {}…", i18n_update(cx, "downloading"), info.version);
        self.pending_notification = Some(Notification::info(starting));
        cx.notify();
        self.download_task = Some(cx.spawn(async move |handle, cx| {
            // Networking + checksum are blocking — keep them off the UI thread.
            let result = cx
                .background_spawn(async move { download_and_verify(&asset).and_then(|path| open_installer(&path)) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.download_task = None;
                match result {
                    Ok(()) => {
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "download_done")));
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

    /// Toggle the keyboard-shortcuts overlay. Like the command palette
    /// it is driven by a global (focus-independent) `ShortcutsAction`
    /// handler so `⌘/` works regardless of focus, and so the command
    /// palette can hand off to it via a dispatched action.
    pub fn toggle_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.shortcuts_overlay.update(cx, |overlay, cx| overlay.toggle(cx));
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
    let (light, dark) = {
        let registry = ThemeRegistry::global(cx);
        (
            registry.default_light_theme().clone(),
            registry.default_dark_theme().clone(),
        )
    };
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
}

/// Open the "update available" dialog on the main window. **Download** opens the
/// release page in the browser; **Skip this version** records the version so the
/// silent startup check won't prompt for it again. Closing without choosing
/// leaves nothing recorded, so the next daily check prompts again.
fn open_update_dialog(info: UpdateInfo, zedis: WeakEntity<Zedis>, window: &mut Window, cx: &mut App) {
    const MAX_NOTES: usize = 600;
    let title = format!("{} {}", i18n_update(cx, "available_title"), info.version);
    let mut notes = info.notes.clone();
    if notes.chars().count() > MAX_NOTES {
        notes = notes.chars().take(MAX_NOTES).collect::<String>();
        notes.push('…');
    }
    let body = if notes.trim().is_empty() {
        format!(
            "{}\n{} → {}",
            i18n_update(cx, "update_body"),
            info.current,
            info.version
        )
    } else {
        format!("{}\n\n{}", i18n_update(cx, "update_body"), notes)
    };
    let skip_version = info.version.clone();
    let download_info = info;
    // Shared flag so the Download path suppresses the skip-on-close below.
    let downloaded = Rc::new(Cell::new(false));
    let on_download = downloaded.clone();
    ZedisDialog::new(title)
        .message(body)
        .w(px(520.))
        .overlay_closable(false)
        .ok_text(i18n_update(cx, "download"))
        .cancel_text(i18n_update(cx, "skip_version"))
        .on_ok(move |_, _window, cx| {
            on_download.set(true);
            // Download + verify + open the installer (or open the release page
            // when there's no verified asset) — see `Zedis::start_download`.
            if let Some(view) = zedis.upgrade() {
                view.update(cx, |this, cx| this.start_download(download_info.clone(), cx));
            }
            true
        })
        .on_close(move |_, _window, cx| {
            // Only the explicit "Skip this version" (cancel) records a skip; the
            // Download path set the flag above, so this is a no-op there.
            if !downloaded.get() {
                let version = skip_version.clone();
                update_app_state_and_save(cx, "skip_update_version", move |state, _| {
                    state.set_skipped_version(version.clone());
                });
            }
            // The user acted on the prompt (download or skip) — clear the chip.
            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                state.set_available_update(None, cx);
            });
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
        if let Some(info) = self.pending_update.take() {
            let weak = cx.entity().downgrade();
            open_update_dialog(info, weak, window, cx);
        }
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(font_size);
        }

        let content = v_flex()
            .id(PKG_NAME)
            .font_family(get_default_font_family())
            .size_full()
            .child(self.render_titlebar(window, cx))
            .child(
                h_flex()
                    .id(PKG_NAME)
                    .bg(cx.theme().background)
                    .size_full()
                    .child(div().w(SIDEBAR_WIDTH).flex_none().h_full().child(self.sidebar.clone()))
                    .child(self.content.clone())
                    .children(dialog_layer)
                    .children(notification_layer),
            )
            // Command palette overlays everything (absolute, full-size
            // when open; zero-footprint when closed).
            .child(self.command_palette.clone())
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
            .on_action(cx.listener(move |_this, e: &ServerToolsAction, _window, cx| {
                let target = match e {
                    ServerToolsAction::Monitor => Route::Monitor,
                    ServerToolsAction::Config => Route::Config,
                    ServerToolsAction::Acl => Route::Acl,
                    ServerToolsAction::Search => Route::Search,
                    ServerToolsAction::Functions => Route::Functions,
                    ServerToolsAction::LuaScripts => Route::LuaScripts,
                    ServerToolsAction::Persistence => Route::Persistence,
                    ServerToolsAction::KeyspaceNotifications => Route::KeyspaceNotifications,
                    ServerToolsAction::Topology => Route::Topology,
                    ServerToolsAction::ServerLoad => Route::ServerLoad,
                    ServerToolsAction::ValueSearch => Route::ValueSearch,
                };
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        state.toggle_route((target, Route::Editor), cx);
                    });
                });
            }))
            // Esc mirrors the tool pages' "back to editor" button. The
            // global keybinding only reaches here when no deeper handler
            // (focused input, open dialog, command palette) claimed the
            // keystroke; no-op on routes without a back affordance.
            .on_action(cx.listener(|_this, _e: &NavAction, _window, cx| {
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        if !matches!(state.route(), Route::Home | Route::Editor | Route::Settings) {
                            state.go_to(Route::Editor, cx);
                        }
                    });
                });
            }))
    }
}

fn init_logger() -> Result<(), Box<dyn std::error::Error>> {
    let mut level = Level::INFO;
    if let Ok(log_level) = env::var("RUST_LOG")
        && let Ok(value) = Level::from_str(log_level.as_str())
    {
        level = value;
    }
    let timer = tracing_subscriber::fmt::time::OffsetTime::local_rfc_3339().unwrap_or_else(|_| {
        tracing_subscriber::fmt::time::OffsetTime::new(
            time::UtcOffset::from_hms(0, 0, 0).unwrap_or(time::UtcOffset::UTC),
            time::format_description::well_known::Rfc3339,
        )
    });

    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_timer(timer)
        .with_ansi(is_development())
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logger()?;
    // Register tree-sitter languages we want the code editor to
    // highlight beyond the JSON-only default. Today this is just Lua
    // (Functions / EVAL editors); add others by extending the helper.
    register_extra_languages();
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
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(380.), px(180.))),
                    ..Default::default()
                },
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
        });
        cx.set_menus(vec![Menu {
            name: "Zedis".into(),
            items: vec![
                MenuItem::action("About Zedis", MemuAction::About),
                MenuItem::action("Check for Updates", UpdateAction::Check),
                MenuItem::action("Close Window", MemuAction::Close),
                MenuItem::action("Quit", MemuAction::Quit),
            ],
            disabled: false,
        }]);

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    #[cfg(not(target_os = "linux"))]
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
                    }),
                    // Create the window hidden on macOS/Windows and reveal it
                    // after the first themed frame (see on_next_frame below) so
                    // there's no white flash before the theme paints. Linux is
                    // left shown immediately — Wayland can't reliably reveal a
                    // window that was never mapped.
                    show: cfg!(target_os = "linux"),
                    window_min_size: Some(size(px(600.), px(400.))),
                    ..Default::default()
                },
                |window, cx| {
                    #[cfg(target_os = "macos")]
                    window.on_window_should_close(cx, move |_window, cx| {
                        cx.hide();
                        false
                    });
                    // Reveal the (hidden) window only after the first frame has
                    // painted, so the user never sees the default white backing
                    // before the themed background. Pairs with `show: false`.
                    #[cfg(not(target_os = "linux"))]
                    window.on_next_frame(|window, _cx| window.activate_window());
                    let zedis_view = cx.new(|cx| Zedis::new(window, cx));
                    // Global (focus-independent) ⌘K handler — element
                    // `.on_action` is focus-routed and dies when the
                    // palette closes and orphans its focus handle.
                    let weak_zedis = zedis_view.downgrade();
                    cx.on_action(move |_: &PaletteAction, cx: &mut App| {
                        if let Some(view) = weak_zedis.upgrade() {
                            view.update(cx, |zedis, cx| zedis.toggle_command_palette(cx));
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
                                view.update(cx, |zedis, cx| zedis.check_for_updates(true, cx));
                            }
                            // Status-bar chip click: open the download/skip prompt for
                            // the update we already found.
                            UpdateAction::OpenPrompt => {
                                if let Some(info) = cx.global::<ZedisGlobalStore>().read(cx).available_update() {
                                    view.update(cx, |zedis, cx| {
                                        zedis.pending_update = Some(info);
                                        cx.notify();
                                    });
                                }
                            }
                        }
                    });
                    // Silent startup check: once per day at most, skippable
                    // per-version, and only if the user left it enabled.
                    let auto_due = {
                        let store = cx.global::<ZedisGlobalStore>().read(cx);
                        store.auto_update_check() && store.update_check_due()
                    };
                    if auto_due {
                        zedis_view.update(cx, |zedis, cx| zedis.check_for_updates(false, cx));
                    }
                    cx.new(|cx| Root::new(zedis_view, window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
        cx.spawn(async move |cx| {
            cx.background_spawn(async move {
                if let Err(e) = ProtoManager::init() {
                    error!(error = %e, "init protos fail",);
                }
                if let Err(e) = ScriptManager::init() {
                    error!(error = %e, "init script viewers fail",);
                }
                if let Err(e) = LuaScriptManager::init() {
                    error!(error = %e, "init lua scripts fail",);
                }
            })
            .await;
        })
        .detach();
    });
    Ok(())
}

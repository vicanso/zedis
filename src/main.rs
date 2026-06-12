#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::connection::{clear_expired_cache, get_servers};
use crate::constants::SIDEBAR_WIDTH;
use crate::db::{LuaScriptManager, ProtoManager, ScriptManager, init_database};
use crate::helpers::{
    MemuAction, NavAction, PaletteAction, ShortcutsAction, get_default_font_family, get_or_create_config_dir,
    is_app_store_build, is_development, new_hot_keys, register_extra_languages,
};
use crate::states::{
    FontSize, FontSizeAction, GlobalEvent, LocaleAction, NotificationCategory, Route, ServerToolsAction,
    SettingsAction, ThemeAction, ZedisAppState, ZedisGlobalStore, save_app_state, update_app_state_and_save,
};
use crate::views::{
    ZedisCommandPalette, ZedisContent, ZedisShortcutsOverlay, ZedisSidebar, ZedisTitleBar, open_about_window,
    open_settings_window,
};
use gpui::{
    App, Bounds, Entity, Menu, MenuItem, Pixels, Task, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode, WindowExt, h_flex, notification::Notification, v_flex};
use std::{env, str::FromStr, time::Duration};
use sys_locale::get_locale;
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

rust_i18n::i18n!("locales", fallback = "en");

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

mod assets;
mod components;
mod connection;
mod constants;
mod db;
mod error;
mod helpers;
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
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| ZedisSidebar::new(window, cx));
        let content = cx.new(|cx| ZedisContent::new(window, cx));
        let command_palette = cx.new(|cx| ZedisCommandPalette::new(window, cx));
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
            if cx.global::<ZedisGlobalStore>().read(cx).theme().is_none() {
                this.theme_update_task = Some(cx.spawn(async move |_this, cx| {
                    cx.update(|cx| {
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
        }
    }
    fn persist_window_state(&mut self, new_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.last_bounds = new_bounds;
        let store = cx.global::<ZedisGlobalStore>().clone();
        let mut value = store.value(cx);
        value.set_bounds(new_bounds);
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            store.update(cx, move |state, cx| {
                state.set_bounds(new_bounds);
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

impl Render for Zedis {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let current_bounds = window.bounds();
        if current_bounds != self.last_bounds {
            self.persist_window_state(current_bounds, cx);
        }
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_size().to_pixels() {
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

                // Apply theme immediately for instant visual feedback
                Theme::change(render_mode, None, cx);

                // Save preference to disk asynchronously
                update_app_state_and_save(cx, "save_theme", move |state, _cx| {
                    state.set_theme(mode);
                });
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
            .on_action(cx.listener(move |_this, e: &FontSizeAction, _window, cx| {
                let action = *e;

                let font_size = match action {
                    FontSizeAction::Large => Some(FontSize::Large),
                    FontSizeAction::Small => Some(FontSize::Small),
                    _ => None,
                };
                // Save locale preference and refresh UI
                update_app_state_and_save(cx, "save_font_size", move |state, _cx| {
                    state.set_font_size(font_size);
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
        error!(error = %e, "init database fail",);
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

        cx.activate(true);
        let window_bounds = if let Some(bounds) = app_state.bounds() {
            info!(bounds = ?bounds, "get window bounds from setting");
            *bounds
        } else {
            let mut window_size = size(px(1200.), px(750.));
            if let Some(display) = cx.primary_display() {
                let display_size = display.bounds().size;
                window_size.width = window_size.width.min(display_size.width * 0.85);
                window_size.height = window_size.height.min(display_size.height * 0.85);
            }
            Bounds::centered(None, window_size, cx)
        };
        let app_state = cx.new(|_| app_state);
        let app_store = ZedisGlobalStore::new(app_state);
        if let Some(theme) = app_store.read(cx).theme() {
            Theme::change(theme, None, cx);
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
        });
        cx.set_menus(vec![Menu {
            name: "Zedis".into(),
            items: vec![
                MenuItem::action("About Zedis", MemuAction::About),
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
                    show: true,
                    window_min_size: Some(size(px(600.), px(400.))),
                    ..Default::default()
                },
                |window, cx| {
                    #[cfg(target_os = "macos")]
                    window.on_window_should_close(cx, move |_window, cx| {
                        cx.hide();
                        false
                    });
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

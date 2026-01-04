#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::connection::get_servers;
use crate::constants::SIDEBAR_WIDTH;
use crate::helpers::{MemuAction, is_app_store_build, is_development, is_linux, new_hot_keys};
use crate::states::{
    FontSize, FontSizeAction, LocaleAction, NotificationCategory, Route, ServerEvent, SettingsAction, ThemeAction,
    ZedisAppState, ZedisGlobalStore, ZedisServerState, save_app_state, update_app_state_and_save,
};
use crate::views::{ZedisContent, ZedisSidebar, ZedisTitleBar, open_about_window};
use gpui::{
    App, Application, Bounds, Entity, Menu, MenuItem, Pixels, Task, TitlebarOptions, Window, WindowAppearance,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode, WindowExt, h_flex, notification::Notification, v_flex};
use std::{env, str::FromStr};
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
mod error;
mod helpers;
mod states;
mod views;

pub struct Zedis {
    pending_notification: Option<Notification>,
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
    // views
    sidebar: Entity<ZedisSidebar>,
    content: Entity<ZedisContent>,
    title_bar: Option<Entity<ZedisTitleBar>>,
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, server_state: Entity<ZedisServerState>) -> Self {
        let sidebar = cx.new(|cx| ZedisSidebar::new(server_state.clone(), window, cx));
        let content = cx.new(|cx| ZedisContent::new(server_state.clone(), window, cx));
        cx.subscribe(&server_state, |this, _server_state, event, cx| {
            match event {
                ServerEvent::Notification(e) => {
                    let message = e.message.clone();
                    let mut notification = match e.category {
                        NotificationCategory::Info => Notification::info(message),
                        NotificationCategory::Success => Notification::success(message),
                        NotificationCategory::Warning => Notification::warning(message),
                        _ => Notification::error(message),
                    };
                    if let Some(title) = e.title.as_ref() {
                        notification = notification.title(title);
                    }
                    this.pending_notification = Some(notification);
                }
                ServerEvent::ErrorOccurred(error) => {
                    this.pending_notification = Some(Notification::error(error.message.clone()));
                }
                _ => {
                    return;
                }
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_appearance(window, |_this, _window, cx| {
            if cx.global::<ZedisGlobalStore>().read(cx).theme().is_none() {
                Theme::change(cx.window_appearance(), None, cx);
                cx.refresh_windows();
            }
        })
        .detach();
        let title_bar = if is_linux() {
            None
        } else {
            Some(cx.new(|cx| ZedisTitleBar::new(window, cx)))
        };

        Self {
            sidebar,
            save_task: None,
            content,
            pending_notification: None,
            title_bar,
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

            let result = store.update(cx, move |state, cx| {
                state.set_bounds(new_bounds);
                cx.notify();
            });
            if let Err(e) = result {
                error!(error = %e, "update window bounds fail",);
                return;
            };

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

        let mut content = h_flex()
            .id(PKG_NAME)
            .bg(cx.theme().background)
            .size_full()
            .child(div().w(px(SIDEBAR_WIDTH)).h_full().child(self.sidebar.clone()))
            .child(self.content.clone())
            .children(dialog_layer)
            .children(notification_layer);

        if !is_linux() {
            content = v_flex()
                .id(PKG_NAME)
                .size_full()
                .child(self.render_titlebar(window, cx))
                .child(content)
        }
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
            .on_action(cx.listener(move |_this, e: &SettingsAction, _window, cx| {
                let action = *e;
                if action == SettingsAction::Editor {
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.go_to(Route::Settings, cx);
                        });
                    });
                }
            }))
    }
}

fn init_logger() {
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
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
}

fn main() {
    init_logger();
    let app = Application::new().with_assets(assets::Assets);
    let app_state = ZedisAppState::try_new().unwrap_or_else(|_| ZedisAppState::new());
    let mut server_state = ZedisServerState::new();
    match get_servers() {
        Ok(servers) => {
            server_state.set_servers(servers);
        }
        Err(e) => {
            error!(error = %e, "get servers fail",);
        }
    }
    info!(is_app_store_build = is_app_store_build(), "detect app build");

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
        }]);

        let server_state = cx.new(|_| server_state.clone());
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
                    let zedis_view = cx.new(|cx| Zedis::new(window, cx, server_state));
                    cx.new(|cx| Root::new(zedis_view, window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

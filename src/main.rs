use crate::connection::get_servers;
use crate::helpers::{MemuAction, new_hot_keys};
use crate::states::ServerEvent;
use crate::states::ZedisAppState;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::save_app_state;
use crate::states::{NotificationAction, NotificationCategory};
use crate::views::ZedisContent;
use crate::views::ZedisSidebar;
use crate::views::ZedisStatusBar;
use crate::views::open_about_window;
use gpui::App;
use gpui::Application;
use gpui::Bounds;
use gpui::Entity;
use gpui::Menu;
use gpui::MenuItem;
use gpui::Pixels;
use gpui::Task;
use gpui::Window;
use gpui::WindowBounds;
use gpui::WindowOptions;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui::size;
use gpui_component::ActiveTheme;
use gpui_component::Root;
use gpui_component::Theme;
use gpui_component::WindowExt;
use gpui_component::h_flex;
use gpui_component::notification::Notification;
use gpui_component::v_flex;
use std::env;
use std::str::FromStr;
use tracing::Level;
use tracing::error;
use tracing::info;
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
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
    // views
    sidebar: Entity<ZedisSidebar>,
    content: Entity<ZedisContent>,
    status_bar: Entity<ZedisStatusBar>,
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, server_state: Entity<ZedisServerState>) -> Self {
        let status_bar = cx.new(|cx| ZedisStatusBar::new(server_state.clone(), window, cx));
        let sidebar = cx.new(|cx| ZedisSidebar::new(server_state.clone(), window, cx));
        let content = cx.new(|cx| ZedisContent::new(server_state.clone(), window, cx));
        cx.subscribe(&server_state, |_this, _server_state, event, cx| {
            if let ServerEvent::ErrorOccurred(error) = event {
                cx.dispatch_action(&NotificationAction::new_error(error.message.clone()));
            }
        })
        .detach();
        cx.observe_window_appearance(window, |_this, _window, cx| {
            if cx.global::<ZedisGlobalStore>().theme(cx).is_none() {
                Theme::change(cx.window_appearance(), None, cx);
                cx.refresh_windows();
            }
        })
        .detach();

        Self {
            status_bar,
            sidebar,
            save_task: None,
            content,
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
                    info!("save window bounds success");
                }
            })
            .await;
        });
        self.save_task = Some(task);
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

        h_flex()
            .id(PKG_NAME)
            .bg(cx.theme().background)
            .size_full()
            .child(self.sidebar.clone())
            .child(
                v_flex()
                    .id("main-container")
                    .flex_1()
                    .h_full()
                    .child(div().flex_1().child(self.content.clone()))
                    .child(self.status_bar.clone()),
            )
            .children(dialog_layer)
            .children(notification_layer)
            .on_action(cx.listener(|_this, e: &NotificationAction, window, cx| {
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
                window.push_notification(notification, cx);
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
    let is_development = env::var("RUST_ENV").unwrap_or_default() == "dev";

    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_timer(timer)
        .with_ansi(is_development)
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
        if let Some(theme) = app_store.theme(cx) {
            Theme::change(theme, None, cx);
        }
        println!("primary display: {:?}", cx.primary_display());
        // TODO 校验是否在显示区域
        for item in cx.displays() {
            println!("{:?}", item.bounds());
            println!("{:?}", item.id());
            println!("{:?}", item.uuid());
            println!("{:?}", item.default_bounds());
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

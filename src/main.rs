use crate::states::Route;
use crate::states::ZedisAppState;
use crate::states::ZedisServerState;
use crate::states::save_app_state;
use crate::views::ZedisEditor;
use crate::views::ZedisKeyTree;
use crate::views::ZedisServers;
use crate::views::ZedisSidebar;
use crate::views::ZedisStatusBar;
use gpui::Application;
use gpui::Bounds;
use gpui::Entity;
use gpui::Pixels;
use gpui::Subscription;
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
use gpui_component::h_flex;
use gpui_component::resizable::h_resizable;
use gpui_component::resizable::resizable_panel;
use gpui_component::v_flex;
use std::env;
use std::str::FromStr;
use tracing::Level;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

mod assets;
mod components;
mod connection;
mod error;
mod helpers;
mod states;
mod views;

pub struct Zedis {
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    // states
    app_state: Entity<ZedisAppState>,
    server_state: Entity<ZedisServerState>,
    // views
    sidebar: Entity<ZedisSidebar>,
    key_tree: Option<Entity<ZedisKeyTree>>,
    value_editor: Option<Entity<ZedisEditor>>,
    servers: Option<Entity<ZedisServers>>,
    status_bar: Entity<ZedisStatusBar>,
}

impl Zedis {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        app_state: Entity<ZedisAppState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let server_state = cx.new(ZedisServerState::new);

        let status_bar =
            cx.new(|cx| ZedisStatusBar::new(window, cx, app_state.clone(), server_state.clone()));

        let sidebar =
            cx.new(|cx| ZedisSidebar::new(window, cx, app_state.clone(), server_state.clone()));

        server_state.update(cx, |state, cx| {
            state.fetch_servers(cx);
        });
        subscriptions.push(cx.observe(&app_state, |this, model, cx| {
            let route = model.read(cx).route();
            if route != Route::Home && this.servers.is_some() {
                debug!("remove servers view");
                let _ = this.servers.take();
            }
            if route != Route::Editor && this.value_editor.is_some() {
                debug!("remove value editor view");
                let _ = this.value_editor.take();
            }
            cx.notify();
        }));

        Self {
            app_state,
            server_state,
            status_bar,
            sidebar,
            key_tree: None,
            servers: None,
            value_editor: None,
            save_task: None,
            last_bounds: Bounds::default(),
            _subscriptions: subscriptions,
        }
    }
    fn persist_window_state(&mut self, new_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.last_bounds = new_bounds;
        let app_state = self.app_state.clone();
        let mut value = app_state.read(cx).clone();
        value.set_bounds(new_bounds);
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            let result = app_state.update(cx, move |state, cx| {
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
    fn render_content_container(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let app_state = self.app_state.clone();
        let server_state = self.server_state.clone();
        match app_state.read(cx).route() {
            Route::Home => {
                let servers = if let Some(servers) = &self.servers {
                    servers.clone()
                } else {
                    debug!("new servers view");
                    let servers =
                        cx.new(|cx| ZedisServers::new(window, cx, app_state, server_state));
                    self.servers = Some(servers.clone());
                    servers
                };
                div().m_4().child(servers).into_any_element()
            }
            _ => {
                let value_editor = if let Some(value_editor) = &self.value_editor {
                    value_editor.clone()
                } else {
                    let value_editor =
                        cx.new(|cx| ZedisEditor::new(window, cx, server_state.clone()));
                    self.value_editor = Some(value_editor.clone());
                    value_editor
                };
                let key_tree = if let Some(key_tree) = &self.key_tree {
                    key_tree.clone()
                } else {
                    debug!("new key tree view");
                    let key_tree = cx.new(|cx| ZedisKeyTree::new(window, cx, server_state));
                    self.key_tree = Some(key_tree.clone());
                    key_tree
                };
                h_resizable("editor-container")
                    .child(
                        resizable_panel()
                            .size(px(240.))
                            .size_range(px(200.)..px(400.))
                            .child(key_tree),
                    )
                    .child(resizable_panel().child(value_editor))
                    .into_any_element()
            }
        }
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
                    .child(
                        div()
                            .flex_1()
                            .child(self.render_content_container(window, cx)),
                    )
                    .child(
                        self.status_bar.clone(),
                        // h_flex()
                        //     .justify_between()
                        //     .text_sm()
                        //     .py_1p5()
                        //     .px_4()
                        //     .border_t_1()
                        //     .border_color(cx.theme().border)
                        //     .text_color(cx.theme().muted_foreground)
                        //     .child(
                        //         h_flex()
                        //             .gap_3()
                        //             .child(self.render_soft_wrap_button(window, cx))
                        //             .child(self.render_indent_guides_button(window, cx)),
                        //     )
                        //     .child(self.render_go_to_line_button(window, cx)),
                    ),
            )
            .children(dialog_layer)
            .children(notification_layer)
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
    let app = Application::new().with_assets(assets::Assets);
    let app_state = ZedisAppState::try_new().unwrap_or_else(|_| ZedisAppState::new());
    init_logger();

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);
        cx.activate(true);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
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
        println!("primary display: {:?}", cx.primary_display());
        // TODO 校验是否在显示区域
        for item in cx.displays() {
            println!("{:?}", item.bounds());
            println!("{:?}", item.id());
            println!("{:?}", item.uuid());
            println!("{:?}", item.default_bounds());
        }
        let app_state = cx.new(|_| app_state.clone());

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    show: true,
                    ..Default::default()
                },
                |window, cx| {
                    let zedis_view = cx.new(|cx| Zedis::new(window, cx, app_state.clone()));
                    cx.new(|cx| Root::new(zedis_view, window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

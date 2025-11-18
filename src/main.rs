use crate::states::Route;
use crate::states::ZedisAppState;
use crate::states::ZedisServerState;
use crate::views::ZedisEditor;
use crate::views::ZedisKeyTree;
use crate::views::ZedisServers;
use crate::views::ZedisSidebar;
use gpui::Application;
use gpui::Bounds;
use gpui::Entity;
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
use gpui_component::IconName;
use gpui_component::Root;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::resizable::h_resizable;
use gpui_component::resizable::resizable_panel;
use gpui_component::v_flex;
use gpui_component_assets::Assets;
use std::env;

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

mod components;
mod connection;
mod error;
mod helpers;
mod states;
mod views;

pub struct Zedis {
    key_tree: Entity<ZedisKeyTree>,
    value_editor: Entity<ZedisEditor>,
    sidebar: Entity<ZedisSidebar>,
    servers: Entity<ZedisServers>,
    app_state: Entity<ZedisAppState>,
    last_bounds: Bounds<Pixels>,
    save_task: Option<Task<()>>,
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app_state = cx.new(ZedisAppState::new);
        let server_state = cx.new(ZedisServerState::new);
        let key_tree =
            cx.new(|cx| ZedisKeyTree::new(window, cx, app_state.clone(), server_state.clone()));
        let value_editor =
            cx.new(|cx| ZedisEditor::new(window, cx, app_state.clone(), server_state.clone()));
        let sidebar =
            cx.new(|cx| ZedisSidebar::new(window, cx, app_state.clone(), server_state.clone()));
        let servers =
            cx.new(|cx| ZedisServers::new(window, cx, app_state.clone(), server_state.clone()));
        server_state.update(cx, |state, cx| {
            state.fetch_servers(cx);
        });

        Self {
            key_tree,
            app_state,
            value_editor,
            sidebar,
            servers,
            save_task: None,
            last_bounds: Bounds::default(),
        }
    }
    fn persist_window_state(&mut self, new_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.last_bounds = new_bounds.clone();
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            cx.background_spawn(async move {
                // TODO
                println!("save window state: {:?}", new_bounds);
            })
            .await;
        });
        self.save_task = Some(task);
    }
    fn render_soft_wrap_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(true, |this| this.icon(IconName::Check))
            .label("Soft Wrap")
            .on_click(cx.listener(|_this, _, _window, cx| {
                // this.soft_wrap = !this.soft_wrap;
                // this.editor.update(cx, |state, cx| {
                //     state.set_soft_wrap(this.soft_wrap, window, cx);
                // });
                cx.notify();
            }))
    }

    fn render_indent_guides_button(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new("indent-guides")
            .ghost()
            .xsmall()
            .when(true, |this| this.icon(IconName::Check))
            .label("Indent Guides")
            .on_click(cx.listener(|_this, _, _window, cx| {
                // this.indent_guides = !this.indent_guides;
                // this.editor.update(cx, |state, cx| {
                //     state.set_indent_guides(this.indent_guides, window, cx);
                // });
                cx.notify();
            }))
    }
    fn render_go_to_line_button(&self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // let position = self.editor.read(cx).cursor_position();
        // let cursor = self.editor.read(cx).cursor();

        Button::new("line-column").ghost().xsmall().label("abc")
        // .on_click(cx.listener(Self::go_to_line))
    }
    fn render_content_container(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.app_state.read(cx).route() {
            Route::Home => self.servers.clone().into_any_element(),
            _ => h_resizable("editor-container")
                .child(
                    resizable_panel()
                        .size(px(240.))
                        .size_range(px(200.)..px(400.))
                        .child(self.key_tree.clone()),
                )
                .child(resizable_panel().child(self.value_editor.clone()))
                .into_any_element(),
        }
        // if self.server_state.read(cx).server.is_empty() {
        //     return self.servers.clone().into_any_element();
        // }
        // h_resizable("editor-container")
        //     .child(
        //         resizable_panel()
        //             .size(px(240.))
        //             .size_range(px(200.)..px(400.))
        //             .child(self.key_tree.clone()),
        //     )
        //     .child(resizable_panel().child(self.value_editor.clone()))
        //     .into_any_element()
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
                        h_flex()
                            .justify_between()
                            .text_sm()
                            .py_1p5()
                            .px_4()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(self.render_soft_wrap_button(window, cx))
                                    .child(self.render_indent_guides_button(window, cx)),
                            )
                            .child(self.render_go_to_line_button(window, cx)),
                    ),
            )
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn main() {
    let app = Application::new().with_assets(Assets);
    let mut window_size = size(px(1200.), px(750.));

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
        if let Some(display) = cx.primary_display() {
            let display_size = display.bounds().size;
            window_size.width = window_size.width.min(display_size.width * 0.85);
            window_size.height = window_size.height.min(display_size.height * 0.85);
        }
        for item in cx.displays() {
            println!("{:?}", item.bounds());
            println!("{:?}", item.id());
            println!("{:?}", item.uuid());
            println!("{:?}", item.default_bounds());
        }
        let window_bounds = Bounds::centered(None, window_size, cx);

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    show: true,
                    ..Default::default()
                },
                |window, cx| {
                    let zedis_view = cx.new(|cx| Zedis::new(window, cx));
                    cx.new(|cx| Root::new(zedis_view, window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

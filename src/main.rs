use crate::assets::Assets;
use crate::components::ZedisEditor;
use crate::components::ZedisKeyTree;
use crate::components::ZedisSidebar;
use crate::states::ZedisServerState;
use gpui::AppContext;
use gpui::Application;
use gpui::Axis;
use gpui::Bounds;
use gpui::Context;
use gpui::Entity;
use gpui::InteractiveElement;
use gpui::IntoElement;
use gpui::ParentElement;
use gpui::Render;
use gpui::Styled;
use gpui::Subscription;
use gpui::Window;
use gpui::WindowBounds;
use gpui::WindowOptions;
use gpui::div;
use gpui::prelude::FluentBuilder;
use gpui::px;
use gpui::size;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Root;
use gpui_component::Selectable;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::list::ListItem;
use gpui_component::resizable::h_resizable;
use gpui_component::resizable::resizable_panel;
use gpui_component::select::{
    SearchableVec, Select, SelectDelegate, SelectEvent, SelectGroup, SelectItem, SelectState,
};
use gpui_component::tree::TreeItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;
use gpui_component::v_flex;
use std::env;

const PKG_NAME: &str = env!("CARGO_PKG_NAME");

mod assets;
mod components;
mod connection;
mod error;
mod helpers;
mod states;

pub struct Zedis {
    line_number: bool,
    key_tree: Entity<ZedisKeyTree>,
    value_editor: Entity<ZedisEditor>,
    server_state: Entity<ZedisServerState>,
    server_select_state: Entity<SelectState<Vec<String>>>,
    _subscriptions: Vec<Subscription>,
}

impl Zedis {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let server_state = cx.new(|cx| ZedisServerState::new(cx));
        let key_tree = cx.new(|cx| ZedisKeyTree::new(window, cx, server_state.clone()));
        let value_editor = cx.new(|cx| ZedisEditor::new(window, cx, server_state.clone()));
        let server_select_state = cx.new(|cx| {
            SelectState::new(
                vec![
                    "local".to_string(),
                    "xiaoji".to_string(),
                    "sentinel".to_string(),
                ],
                None,
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe_in(
            &server_select_state,
            window,
            |view, _, event, _, cx| match event {
                SelectEvent::Confirm(value) => {
                    if let Some(selected_value) = value {
                        view.server_state.update(cx, |state, cx| {
                            state.select_server(selected_value.clone(), cx);
                        });
                    }
                }
            },
        ));
        Self {
            line_number: false,
            key_tree,
            server_state,
            server_select_state,
            value_editor,
            _subscriptions: subscriptions,
        }
    }
    fn render_server_select(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Select::new(&self.server_select_state).w(px(150.)).small()
    }

    fn render_soft_wrap_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("soft-wrap")
            .ghost()
            .xsmall()
            .when(true, |this| this.icon(IconName::Check))
            .label("Soft Wrap")
            .on_click(cx.listener(|this, _, window, cx| {
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
            .on_click(cx.listener(|this, _, window, cx| {
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
}

impl Render for Zedis {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id(PKG_NAME)
            .bg(cx.theme().background)
            .size_full()
            .child(ZedisSidebar::new(window, cx))
            .child(
                v_flex()
                    .id("right-container")
                    .flex_1()
                    .h_full()
                    .child(
                        h_resizable("editor-container")
                            .child(
                                resizable_panel()
                                    .size(px(240.))
                                    .size_range(px(200.)..px(400.))
                                    .child(self.key_tree.clone()),
                            )
                            .child(resizable_panel().child(self.value_editor.clone())),
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
                                    .child(self.render_server_select(window, cx))
                                    .child(self.render_soft_wrap_button(window, cx))
                                    .child(self.render_indent_guides_button(window, cx)),
                            )
                            .child(self.render_go_to_line_button(window, cx)),
                    ),
            )
    }
}

fn main() {
    let app = Application::new().with_assets(Assets);
    let mut window_size = size(px(1200.), px(750.));

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);
        cx.activate(true);
        if let Some(display) = cx.primary_display() {
            let display_size = display.bounds().size;
            window_size.width = window_size.width.min(display_size.width * 0.85);
            window_size.height = window_size.height.min(display_size.height * 0.85);
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
                    cx.new(|cx| Root::new(zedis_view.into(), window, cx))
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

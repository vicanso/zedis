// Copyright 2025 Tree xie.
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

use crate::states::Route;
use crate::states::ZedisAppState;
use crate::states::ZedisServerState;
use crate::states::save_app_state;
use gpui::Action;
use gpui::Axis;
use gpui::Corner;
use gpui::Entity;
use gpui::Window;
use gpui::WindowAppearance;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::StyledExt;
use gpui_component::Theme;
use gpui_component::ThemeMode;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::menu::DropdownMenu;
use gpui_component::v_flex;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::error;
use tracing::info;

#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
pub enum ThemeAction {
    Light,
    Dark,
    System,
}

pub struct ZedisSidebar {
    server_state: Entity<ZedisServerState>,
    app_state: Entity<ZedisAppState>,
}
impl ZedisSidebar {
    pub fn new(
        _window: &mut Window,
        _cx: &mut Context<Self>,
        app_state: Entity<ZedisAppState>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self {
            server_state,
            app_state,
        }
    }
    fn render_server_list(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut server_list = vec!["".to_string()];
        let server_state = self.server_state.read(cx);
        let current_server = server_state.server();
        if let Some(servers) = server_state.servers() {
            server_list.extend(servers.iter().map(|server| server.name.clone()));
        }
        let server_elements: Vec<_> = server_list
            .iter()
            .enumerate()
            .map(|(index, server_name)| {
                let server_name = server_name.clone();
                let name = if server_name.is_empty() {
                    "home".to_string()
                } else {
                    server_name.clone()
                };
                let is_current = server_name == current_server;
                ListItem::new(("sidebar-redis-server", index))
                    .when(is_current, |this| this.bg(cx.theme().list_active))
                    .py_4()
                    .border_r_3()
                    .when(is_current, |this| {
                        this.border_color(cx.theme().list_active_border)
                    })
                    .child(
                        v_flex()
                            .items_center()
                            .child(Icon::new(IconName::LayoutDashboard))
                            .child(Label::new(name).text_ellipsis().text_xs()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_current {
                            return;
                        }
                        let route = if server_name.is_empty() {
                            Route::Home
                        } else {
                            Route::Editor
                        };
                        this.app_state.update(cx, |state, cx| {
                            state.go_to(route, cx);
                        });
                        this.server_state.update(cx, |state, cx| {
                            state.select(&server_name, cx);
                        });
                    }))
            })
            .collect();
        v_flex().flex_1().children(server_elements)
    }
    fn render_settings_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_action = match self.app_state.read(cx).theme() {
            Some(ThemeMode::Light) => ThemeAction::Light,
            Some(ThemeMode::Dark) => ThemeAction::Dark,
            _ => ThemeAction::System,
        };
        let btn = Button::new("zedis-sidebar-setting-btn")
            .ghost()
            .w_full()
            .h(px(60.))
            .child(Icon::new(IconName::Settings).size(px(18.)))
            .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, window, cx| {
                menu.submenu("Theme", window, cx, move |submenu, _window, _cx| {
                    submenu
                        .menu_element_with_check(
                            current_action == ThemeAction::Light,
                            Box::new(ThemeAction::Light),
                            |_window, _cx| Label::new("Light").text_xs(),
                        )
                        .menu_element_with_check(
                            current_action == ThemeAction::Dark,
                            Box::new(ThemeAction::Dark),
                            |_window, _cx| Label::new("Dark").text_xs(),
                        )
                        .menu_element_with_check(
                            current_action == ThemeAction::System,
                            Box::new(ThemeAction::System),
                            |_window, _cx| Label::new("System").text_xs(),
                        )
                })
            });

        div()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(btn)
            .on_action(cx.listener(|this, e: &ThemeAction, _window, cx| {
                let mode = match e {
                    ThemeAction::Light => {
                        Theme::change(ThemeMode::Light, None, cx);
                        Some(ThemeMode::Light)
                    }
                    ThemeAction::Dark => {
                        Theme::change(ThemeMode::Dark, None, cx);
                        Some(ThemeMode::Dark)
                    }
                    ThemeAction::System => {
                        let appearance = cx.window_appearance();
                        let mode = match appearance {
                            WindowAppearance::Light => ThemeMode::Light,
                            _ => ThemeMode::Dark,
                        };

                        Theme::change(mode, None, cx);
                        None
                    }
                };
                let app_state = this.app_state.clone();
                let mut value = app_state.read(cx).clone();
                value.set_theme(mode);
                let app_state = app_state.clone();
                cx.spawn(async move |_, cx| {
                    let _ = app_state.update(cx, |state, _cx| {
                        state.set_theme(mode);
                    });
                    cx.background_spawn(async move {
                        if let Err(e) = save_app_state(&value) {
                            error!(error = %e, "save theme fail",);
                        } else {
                            info!("save theme success");
                        }
                    })
                    .await;
                })
                .detach();
                cx.refresh_windows();
            }))
    }
    fn render_star(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().border_b_1().border_color(cx.theme().border).child(
            Button::new("github")
                .ghost()
                .h(px(50.))
                .w_full()
                .tooltip("Star on GitHub")
                .icon(Icon::new(IconName::GitHub))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.open_url("https://github.com/vicanso/zedis");
                })),
        )
    }
}
impl Render for ZedisSidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(80.))
            .id("sidebar-container")
            .justify_start()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_star(window, cx))
            .child(
                div().w_full().flex_1().min_h_0().child(
                    div()
                        .child(self.render_server_list(window, cx))
                        .scrollable(Axis::Vertical),
                ),
            )
            .child(self.render_settings_button(window, cx))
    }
}

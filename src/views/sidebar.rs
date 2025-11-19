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
use gpui::Entity;
use gpui::Window;
use gpui::div;
use gpui::prelude::FluentBuilder;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::v_flex;

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
}
impl Render for ZedisSidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .when(is_current, |this| this.bg(cx.theme().muted_foreground))
                    .py_2()
                    .child(
                        v_flex()
                            .items_center()
                            .child(Icon::new(IconName::LayoutDashboard))
                            .child(
                                Label::new(name)
                                    .text_ellipsis()
                                    .text_xs()
                                    .when(!is_current, |this| {
                                        this.text_color(cx.theme().muted_foreground)
                                    }),
                            ),
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
        v_flex()
            .w(px(60.))
            .id("sidebar-container")
            .justify_start()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div().border_b_1().border_color(cx.theme().border).child(
                    Button::new("github")
                        .ghost()
                        .w_full()
                        .icon(Icon::new(IconName::GitHub))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.open_url("https://github.com/vicanso/zedis");
                        })),
                ),
            )
            .child(v_flex().children(server_elements))
    }
}

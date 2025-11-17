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

use crate::components::Card;
use crate::connection::RedisServer;
use crate::states::ZedisServerState;
use gpui::App;
use gpui::AppContext;
use gpui::Context;
use gpui::Entity;
use gpui::InteractiveElement;
use gpui::IntoElement;
use gpui::ParentElement;
use gpui::Render;
use gpui::RenderOnce;
use gpui::Styled;
use gpui::Window;
use gpui::div;
use gpui::prelude::FluentBuilder;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::Side;
use gpui_component::Sizable;
use gpui_component::Size;
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::h_flex;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_component::v_flex;

pub struct ZedisServers {
    server_state: Entity<ZedisServerState>,
    name_state: Entity<InputState>,
    host_state: Entity<InputState>,
    port_state: Entity<InputState>,
    password_state: Entity<InputState>,
    is_new: bool,
}

impl ZedisServers {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let name_state = cx.new(|cx| InputState::new(window, cx));
        let host_state = cx.new(|cx| InputState::new(window, cx));
        let port_state = cx.new(|cx| InputState::new(window, cx).default_value("6379"));
        let password_state = cx.new(|cx| InputState::new(window, cx).masked(true));
        Self {
            server_state,
            name_state,
            host_state,
            port_state,
            password_state,
            is_new: false,
        }
    }
    fn add_or_update_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
        let name_state = self.name_state.clone();
        let host_state = self.host_state.clone();
        let port_state = self.port_state.clone();
        let password_state = self.password_state.clone();
        let is_new = self.is_new;

        window.open_dialog(cx, move |dialog, _, _| {
            let title = if is_new {
                "Add Server".to_string()
            } else {
                "Update Server".to_string()
            };
            let server_state = server_state.clone();
            let name_input = name_state.clone();
            let host_input = host_state.clone();
            let port_input = port_state.clone();
            let password_input = password_state.clone();
            dialog
                .title(title)
                .overlay(true)
                .child(
                    v_form()
                        .child(
                            field()
                                .label("Name")
                                .child(Input::new(&name_state).disabled(!is_new)),
                        )
                        .child(field().label("Host").child(Input::new(&host_state)))
                        .child(field().label("Port").child(Input::new(&port_state)))
                        .child(
                            field()
                                .label("Password")
                                .child(Input::new(&password_state).mask_toggle()),
                        ),
                )
                .footer(move |_, _, _, _| {
                    let name_input = name_input.clone();
                    let host_input = host_input.clone();
                    let port_input = port_input.clone();
                    let password_input = password_input.clone();
                    let server_state = server_state.clone();
                    vec![
                        Button::new("ok").primary().label("Submit").on_click(
                            move |_, window, cx| {
                                let server_state = server_state.clone();
                                let name = name_input.read(cx).value().to_string();
                                let host = host_input.read(cx).value().to_string();
                                let port =
                                    port_input.read(cx).value().parse::<u16>().unwrap_or(6379);
                                let password = password_input.read(cx).value().to_string();
                                let password = if password.is_empty() {
                                    None
                                } else {
                                    Some(password)
                                };
                                server_state.update(cx, |state, cx| {
                                    state.update_or_insrt_server(
                                        cx,
                                        RedisServer {
                                            name,
                                            host,
                                            port,
                                            password,
                                            ..Default::default()
                                        },
                                    );
                                });

                                window.close_dialog(cx);
                            },
                        ),
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    ]
                })
        });
    }
}

impl Render for ZedisServers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let children: Vec<_> = self
            .server_state
            .read(cx)
            .servers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let select_server_name = server.name.clone();
                let update_server = server.clone();
                Card::new(("server-select-card", index))
                    .icon(Icon::new(IconName::Palette))
                    .title(server.name.clone())
                    .actions(vec![
                        Button::new(("server-select-card-setting", index))
                            .ghost()
                            .icon(IconName::Settings2)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let server_name = update_server.name.clone();
                                let server_host = update_server.host.clone();
                                let server_port = update_server.port.to_string();
                                let server_password =
                                    update_server.password.clone().unwrap_or_default();
                                cx.stop_propagation();
                                this.name_state.update(cx, |state, cx| {
                                    state.set_value(server_name, window, cx);
                                });
                                this.host_state.update(cx, |state, cx| {
                                    state.set_value(server_host, window, cx);
                                });
                                this.port_state.update(cx, |state, cx| {
                                    state.set_value(server_port, window, cx);
                                });
                                this.password_state.update(cx, |state, cx| {
                                    state.set_value(server_password, window, cx);
                                });
                                this.is_new = false;
                                this.add_or_update_server(window, cx);
                            })),
                    ])
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let server_name = select_server_name.clone();
                        this.server_state.update(cx, |state, cx| {
                            state.select_server(&server_name, cx);
                        });
                    }))
            })
            .collect();

        div()
            .grid()
            .grid_cols(3)
            .gap_2()
            .w_full()
            .children(children)
            .child(
                Card::new("add-server")
                    .icon(IconName::Plus)
                    .title("add server")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.is_new = true;
                        this.add_or_update_server(window, cx);
                    })),
            )
            .into_any_element()
    }
}

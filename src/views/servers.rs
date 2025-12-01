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

use crate::assets::CustomIconName;
use crate::components::Card;
use crate::connection::RedisServer;
use crate::states::Route;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_servers;
use gpui::Entity;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Colorize;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use rust_i18n::t;
use substring::Substring;
pub struct ZedisServers {
    server_state: Entity<ZedisServerState>,
    name_state: Entity<InputState>,
    host_state: Entity<InputState>,
    port_state: Entity<InputState>,
    password_state: Entity<InputState>,
    description_state: Entity<InputState>,
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
        let description_state = cx.new(|cx| InputState::new(window, cx).auto_grow(2, 10));
        Self {
            server_state,
            name_state,
            host_state,
            port_state,
            password_state,
            description_state,
            is_new: false,
        }
    }
    fn fill_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>, server: &RedisServer) {
        self.is_new = server.name.is_empty();
        self.name_state.update(cx, |state, cx| {
            state.set_value(server.name.clone(), window, cx);
        });
        self.host_state.update(cx, |state, cx| {
            state.set_value(server.host.clone(), window, cx);
        });
        if server.port != 0 {
            self.port_state.update(cx, |state, cx| {
                state.set_value(server.port.to_string(), window, cx);
            });
        }
        self.password_state.update(cx, |state, cx| {
            state.set_value(server.password.clone().unwrap_or_default(), window, cx);
        });
        self.description_state.update(cx, |state, cx| {
            state.set_value(server.description.clone().unwrap_or_default(), window, cx);
        });
    }
    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>, server: &str) {
        let server_state = self.server_state.clone();
        let server = server.to_string();
        let locale = cx.global::<ZedisGlobalStore>().locale(cx).to_string();
        window.open_dialog(cx, move |dialog, _, _| {
            let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();
            let server_state = server_state.clone();
            let server = server.clone();
            dialog.confirm().child(message).on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| {
                    state.remove_server(&server, cx);
                });
                window.close_dialog(cx);
                true
            })
        });
    }
    fn add_or_update_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
        let name_state = self.name_state.clone();
        let host_state = self.host_state.clone();
        let port_state = self.port_state.clone();
        let password_state = self.password_state.clone();
        let description_state = self.description_state.clone();
        let is_new = self.is_new;

        window.open_dialog(cx, move |dialog, _, cx| {
            let title = if is_new {
                i18n_servers(cx, "add_server_title").to_string()
            } else {
                i18n_servers(cx, "update_server_title").to_string()
            };
            let server_state = server_state.clone();
            let name_input = name_state.clone();
            let host_input = host_state.clone();
            let port_input = port_state.clone();
            let password_input = password_state.clone();
            let description_input = description_state.clone();
            let name_label = i18n_servers(cx, "name").to_string();
            let host_label = i18n_servers(cx, "host").to_string();
            let port_label = i18n_servers(cx, "port").to_string();
            let password_label = i18n_servers(cx, "password").to_string();
            let description_label = i18n_servers(cx, "description").to_string();
            dialog
                .title(title)
                .overlay(true)
                .child(
                    v_form()
                        .child(
                            field()
                                .label(name_label)
                                .child(Input::new(&name_state).disabled(!is_new)),
                        )
                        .child(field().label(host_label).child(Input::new(&host_state)))
                        .child(field().label(port_label).child(Input::new(&port_state)))
                        .child(
                            field()
                                .label(password_label)
                                .child(Input::new(&password_state).mask_toggle()),
                        )
                        .child(
                            field()
                                .label(description_label)
                                .child(Input::new(&description_state)),
                        ),
                )
                .footer(move |_, _, _, cx| {
                    let name_input = name_input.clone();
                    let host_input = host_input.clone();
                    let port_input = port_input.clone();
                    let password_input = password_input.clone();
                    let description_input = description_input.clone();
                    let server_state = server_state.clone();
                    let submit_label = i18n_servers(cx, "submit").to_string();
                    let cancel_label = i18n_servers(cx, "cancel").to_string();
                    vec![
                        Button::new("ok").primary().label(submit_label).on_click(
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
                                let description = description_input.read(cx).value().to_string();
                                let description = if description.is_empty() {
                                    None
                                } else {
                                    Some(description)
                                };
                                server_state.update(cx, |state, cx| {
                                    state.update_or_insrt_server(
                                        RedisServer {
                                            name,
                                            host,
                                            port,
                                            password,
                                            description,
                                            ..Default::default()
                                        },
                                        is_new,
                                        cx,
                                    );
                                });

                                window.close_dialog(cx);
                            },
                        ),
                        Button::new("cancel")
                            .label(cancel_label)
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
        let width = window.viewport_size().width;
        let cols = match width {
            width if width < px(800.) => 1,
            width if width < px(1200.) => 2,
            _ => 3,
        };
        let bg = if cx.theme().is_dark() {
            cx.theme().background.lighten(1.0)
        } else {
            cx.theme().background.darken(0.02)
        };
        let update_tooltip = i18n_servers(cx, "update_tooltip").to_string();
        let remove_tooltip = i18n_servers(cx, "remove_tooltip").to_string();
        let children: Vec<_> = self
            .server_state
            .read(cx)
            .servers()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let select_server_name = server.name.clone();
                let update_server = server.clone();
                let remove_server_name = server.name.clone();
                let description = server.description.as_deref().unwrap_or_default();
                let updated_at = if let Some(updated_at) = &server.updated_at {
                    updated_at.substring(0, 9).to_string()
                } else {
                    "".to_string()
                };
                let title = format!("{} ({}:{})", server.name, server.host, server.port);
                Card::new(("servers-card", index))
                    .icon(Icon::new(CustomIconName::DatabaseZap))
                    .title(title)
                    .bg(bg)
                    .when(!description.is_empty(), |this| {
                        this.description(description)
                    })
                    .when(!updated_at.is_empty(), |this| {
                        this.footer(
                            Label::new(updated_at)
                                .text_sm()
                                .text_right()
                                .whitespace_normal()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .actions(vec![
                        Button::new(("servers-card-action-select", index))
                            .ghost()
                            .tooltip(update_tooltip.clone())
                            .icon(CustomIconName::FilePenLine)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.fill_inputs(window, cx, &update_server);
                                this.add_or_update_server(window, cx);
                            })),
                        Button::new(("servers-card-action-delete", index))
                            .ghost()
                            .tooltip(remove_tooltip.clone())
                            .icon(CustomIconName::FileXCorner)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_server(window, cx, &remove_server_name);
                            })),
                    ])
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let server_name = select_server_name.clone();
                        let query_mode = cx
                            .global::<ZedisGlobalStore>()
                            .query_mode(server_name.as_str(), cx);
                        this.server_state.update(cx, |state, cx| {
                            state.select(server_name.into(), query_mode, cx);
                        });
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, _cx| {
                                state.go_to(Route::Editor);
                            });
                            cx.notify();
                        });
                    }))
            })
            .collect();

        div()
            .grid()
            .grid_cols(cols)
            .gap_1()
            .w_full()
            .children(children)
            .child(
                Card::new("servers-card-add")
                    .icon(IconName::Plus)
                    .title(i18n_servers(cx, "add_server_title").to_string())
                    .bg(bg)
                    .description(i18n_servers(cx, "add_server_description").to_string())
                    .actions(vec![
                        Button::new("add")
                            .ghost()
                            .icon(CustomIconName::FilePlusCorner),
                    ])
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.fill_inputs(window, cx, &RedisServer::default());
                        this.add_or_update_server(window, cx);
                    })),
            )
            .into_any_element()
    }
}

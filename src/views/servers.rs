// Copyright 2026 Tree xie.
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
use crate::connection::{RedisServer, get_servers, open_single_connection};
use crate::error::Error;
use crate::states::{
    GlobalEvent, NotificationAction, Route, ZedisGlobalStore, dialog_button_props, i18n_common, i18n_servers,
};
use gpui::{SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Colorize, Icon, IconName, WindowExt,
    button::{Button, ButtonVariants},
    label::Label,
};
use redis::cmd;
use rust_i18n::t;
use substring::Substring;
use tracing::info;
use zedis_ui::ZedisCard;
use zedis_ui::ZedisDialog;
use zedis_ui::{ZedisFormField, ZedisFormFieldType, ZedisFormOptions};

// Constants for UI layout
const DEFAULT_REDIS_PORT: u16 = 6379;
const VIEWPORT_BREAKPOINT_SMALL: f32 = 800.0; // Single column
const VIEWPORT_BREAKPOINT_MEDIUM: f32 = 1200.0; // Two columns
const UPDATED_AT_SUBSTRING_LENGTH: usize = 10; // Length of date string to display
const THEME_LIGHTEN_AMOUNT_DARK: f32 = 1.0;
const THEME_DARKEN_AMOUNT_LIGHT: f32 = 0.02;

/// Server management view component
///
/// Displays a grid of server cards with:
/// - Server connection details (name, host, port)
/// - Action buttons (edit, delete)
/// - Add new server card
/// - Click to connect functionality
///
/// Uses a responsive grid layout that adjusts columns based on viewport width.
pub struct ZedisServers {
    should_popup_new_server: bool,
    _subscriptions: Vec<Subscription>,
}

impl ZedisServers {
    /// Create a new server management view
    ///
    /// Initializes all input field states with appropriate placeholders
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        info!("Creating new servers view");

        let global_state = cx.global::<ZedisGlobalStore>().state();
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&global_state, |this, state, event, cx| {
            if let GlobalEvent::RouteChanged(Route::Home) = event
                && state
                    .read(cx)
                    .get_route_query()
                    .map(|query| query.contains_key("new"))
                    .unwrap_or(false)
            {
                this.should_popup_new_server = true;
                cx.notify();
            }
        }));
        if let Some(query) = global_state.read(cx).get_route_query()
            && query.contains_key("new")
        {
            cx.defer_in(window, |this, window, cx| {
                this.add_or_update_server_dialog(
                    &RedisServer {
                        port: DEFAULT_REDIS_PORT,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            });
        }

        Self {
            should_popup_new_server: false,
            _subscriptions: subscriptions,
        }
    }
    /// Show confirmation dialog and remove server from configuration
    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>, server_id: &str) {
        let mut server = "--".to_string();
        if let Ok(servers) = get_servers()
            && let Some(found) = servers.iter().find(|item| item.id == server_id)
        {
            server = found.name.clone();
        }
        let server_id = server_id.to_string();

        // let server = server.to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();

        ZedisDialog::new_alert(i18n_servers(cx, "remove_server_title"), message)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        state.remove_server(&server_id, cx);
                    });
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    fn add_or_update_server_dialog(&mut self, redis_server: &RedisServer, window: &mut Window, cx: &mut Context<Self>) {
        let server_id = redis_server.id.clone();
        let is_new = server_id.is_empty();
        let server_type_list = i18n_servers(cx, "server_type_list");
        let validate_host = |s: &str| {
            if s.len() <= 1024 && s.is_ascii() {
                return None;
            }
            Some("host is invalid".into())
        };

        let fields = vec![
            ZedisFormField::new("name", i18n_common(cx, "name"))
                .default_value(redis_server.name.clone())
                .placeholder(i18n_common(cx, "name_placeholder"))
                .focus()
                .tab_index(0)
                .required(),
            ZedisFormField::new("host", i18n_common(cx, "host"))
                .default_value(redis_server.host.clone())
                .placeholder(i18n_common(cx, "host_placeholder"))
                .tab_index(0)
                .validate(validate_host)
                .required(),
            ZedisFormField::new("port", i18n_common(cx, "port"))
                .default_value(redis_server.port.to_string())
                .placeholder(i18n_common(cx, "port_placeholder"))
                .tab_index(0),
            ZedisFormField::new("username", i18n_common(cx, "username"))
                .default_value(redis_server.username.clone().unwrap_or_default())
                .tab_index(0)
                .placeholder(i18n_common(cx, "username_placeholder")),
            ZedisFormField::new("password", i18n_common(cx, "password"))
                .default_value(redis_server.password.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "password_placeholder"))
                .tab_index(0)
                .mask(),
            ZedisFormField::new("master_name", i18n_servers(cx, "master_name"))
                .default_value(redis_server.master_name.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "master_name_placeholder"))
                .tab_index(0),
            ZedisFormField::new("description", i18n_common(cx, "description"))
                .default_value(redis_server.description.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "description_placeholder"))
                .tab_index(0),
            // tab tls
            ZedisFormField::new("tls", i18n_common(cx, "tls"))
                .default_value(redis_server.tls.unwrap_or(false).to_string())
                .placeholder(i18n_common(cx, "tls_check_label"))
                .tab_index(1)
                .field_type(ZedisFormFieldType::Checkbox),
            ZedisFormField::new("insecure", i18n_common(cx, "insecure_tls"))
                .default_value(redis_server.insecure.unwrap_or(false).to_string())
                .placeholder(i18n_common(cx, "insecure_tls_check_label"))
                .tab_index(1)
                .field_type(ZedisFormFieldType::Checkbox),
            ZedisFormField::new("client_cert", i18n_common(cx, "client_cert"))
                .default_value(redis_server.client_cert.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "client_cert_placeholder"))
                .tab_index(1)
                .field_type(ZedisFormFieldType::AutoGrow(2, 100)),
            ZedisFormField::new("client_key", i18n_common(cx, "client_key"))
                .default_value(redis_server.client_key.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "client_key_placeholder"))
                .tab_index(1)
                .field_type(ZedisFormFieldType::AutoGrow(2, 100)),
            ZedisFormField::new("root_cert", i18n_common(cx, "root_cert"))
                .default_value(redis_server.root_cert.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "root_cert_placeholder"))
                .tab_index(1)
                .field_type(ZedisFormFieldType::AutoGrow(2, 100)),
            // tab ssh tunnel
            ZedisFormField::new("ssh_tunnel", i18n_servers(cx, "ssh_tunnel"))
                .default_value(redis_server.ssh_tunnel.unwrap_or(false).to_string())
                .placeholder(i18n_servers(cx, "ssh_tunnel_check_label"))
                .tab_index(2)
                .field_type(ZedisFormFieldType::Checkbox),
            ZedisFormField::new("ssh_addr", i18n_servers(cx, "ssh_addr"))
                .default_value(redis_server.ssh_addr.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_addr_placeholder"))
                .tab_index(2),
            ZedisFormField::new("ssh_username", i18n_servers(cx, "ssh_username"))
                .default_value(redis_server.ssh_username.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_username_placeholder"))
                .tab_index(2),
            ZedisFormField::new("ssh_password", i18n_servers(cx, "ssh_password"))
                .default_value(redis_server.ssh_password.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_password_placeholder"))
                .mask()
                .tab_index(2),
            ZedisFormField::new("ssh_key", i18n_servers(cx, "ssh_key"))
                .default_value(redis_server.ssh_key.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_key_placeholder"))
                .tab_index(2)
                .field_type(ZedisFormFieldType::AutoGrow(2, 100)),
            // tab advanced
            ZedisFormField::new("server_type", i18n_servers(cx, "server_type"))
                .default_value(redis_server.server_type.unwrap_or(0).to_string())
                .options(
                    server_type_list
                        .split(" ")
                        .map(|s| s.to_string().into())
                        .collect::<Vec<SharedString>>(),
                )
                .placeholder(i18n_servers(cx, "server_type_placeholder"))
                .tab_index(3)
                .field_type(ZedisFormFieldType::RadioGroup),
            ZedisFormField::new("readonly", i18n_servers(cx, "readonly"))
                .default_value(redis_server.readonly.unwrap_or(false).to_string())
                .placeholder(i18n_servers(cx, "readonly_check_label"))
                .tab_index(3)
                .field_type(ZedisFormFieldType::Checkbox),
        ];
        let title = if is_new {
            i18n_servers(cx, "add_server_title")
        } else {
            i18n_servers(cx, "update_server_title")
        };
        let max_h = (window.bounds().size.height - px(300.0)).min(px(600.0));

        let test_label = i18n_servers(cx, "test_connection");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        ZedisFormOptions::new(fields)
            .title(title)
            .tabs(vec![
                i18n_servers(cx, "tab_general"),
                i18n_servers(cx, "tab_tls"),
                i18n_servers(cx, "tab_ssh"),
                i18n_servers(cx, "tab_advanced"),
            ])
            .confirm_label(i18n_common(cx, "confirm"))
            .cancel_label(i18n_common(cx, "cancel"))
            .dialog_max_height(max_h)
            .foot_actions(move |_window, cx: &mut Context<zedis_ui::ZedisForm>| {
                let locale = locale.clone();
                let test_label = test_label.clone();
                vec![Button::new("test-connection").label(test_label).on_click(cx.listener(
                    move |form, _, _window, cx| {
                        if form.is_processing {
                            return;
                        }
                        let Some(values) = form.try_get_values(cx) else {
                            return;
                        };
                        let server = RedisServer::from_form_data("", &values);
                        let locale = locale.clone();
                        form.is_processing = true;
                        cx.notify();
                        cx.spawn(async move |handle, cx| {
                            let result = async {
                                let with_pass = server.password.is_some();
                                let mut conn = match open_single_connection(&server, 0, false).await {
                                    Ok(conn) => conn,
                                    Err(e) => {
                                        if with_pass && e.to_string().contains("authentication failed") {
                                            let mut new_server = server.clone();
                                            new_server.password = None;
                                            if open_single_connection(&new_server, 0, false).await.is_ok() {
                                                return Err(Error::Invalid {
                                                    message: "Client sent AUTH, but no password is set".to_string(),
                                                });
                                            }
                                        }

                                        return Err(e);
                                    }
                                };
                                let _: () = cmd("PING").query_async(&mut conn).await?;
                                Ok::<(), Error>(())
                            }
                            .await;
                            handle
                                .update(cx, |form, cx| {
                                    form.is_processing = false;
                                    let notification = match result {
                                        Ok(()) => {
                                            let msg = t!("servers.test_connection_success", locale = &locale);
                                            NotificationAction::new_success(msg.into())
                                        }
                                        Err(e) => {
                                            let msg = t!(
                                                "servers.test_connection_failed",
                                                error = e.to_string(),
                                                locale = &locale
                                            );
                                            NotificationAction::new_error(msg.into())
                                        }
                                    };
                                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                        store.update(cx, |_state, cx| {
                                            cx.emit(GlobalEvent::Notification(notification));
                                        });
                                    });
                                    cx.notify();
                                })
                                .ok();
                        })
                        .detach();
                    },
                ))]
            })
            .on_dialog_submit(move |values, _window, cx| {
                let redis_server = RedisServer::from_form_data(&server_id, &values);
                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                    store.update(cx, |state, cx| {
                        state.upsert_server(redis_server, cx);
                    })
                });
                true
            })
            .open_dialog(window, cx);
    }
}

impl Render for ZedisServers {
    /// Main render method - displays responsive grid of server cards
    ///
    /// Layout adapts based on viewport width:
    /// - < 800px: 1 column
    /// - 800-1200px: 2 columns  
    /// - > 1200px: 3 columns
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width;

        if std::mem::take(&mut self.should_popup_new_server) {
            self.add_or_update_server_dialog(
                &RedisServer {
                    port: DEFAULT_REDIS_PORT,
                    ..Default::default()
                },
                window,
                cx,
            );
        }

        // Responsive grid columns based on viewport width
        let cols = match width {
            width if width < px(VIEWPORT_BREAKPOINT_SMALL) => 1,
            width if width < px(VIEWPORT_BREAKPOINT_MEDIUM) => 2,
            _ => 3,
        };

        // Card background color (slightly lighter/darker than theme background)
        let bg = if cx.theme().is_dark() {
            cx.theme().background.lighten(THEME_LIGHTEN_AMOUNT_DARK)
        } else {
            cx.theme().background.darken(THEME_DARKEN_AMOUNT_LIGHT)
        };

        let update_tooltip = i18n_servers(cx, "update_tooltip");
        let remove_tooltip = i18n_servers(cx, "remove_tooltip");

        // Build card for each configured server
        let children: Vec<_> = get_servers()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, server)| {
                // Clone values for use in closures
                let select_server_id = server.id.clone();
                let update_server = server.clone();
                let remove_server_id = server.id.clone();

                let description = server.description.as_deref().unwrap_or_default();

                // Extract and format update timestamp (show only date part)
                let updated_at = if let Some(updated_at) = &server.updated_at {
                    updated_at.substring(0, UPDATED_AT_SUBSTRING_LENGTH).to_string()
                } else {
                    String::new()
                };

                let title = format!("{} ({}:{})", server.name, server.host, server.port);

                // Action buttons for each server card
                let actions = vec![
                    // Edit button - opens dialog to modify server configuration
                    Button::new(("servers-card-action-select", index))
                        .ghost()
                        .tooltip(update_tooltip.clone())
                        .icon(CustomIconName::FilePenLine)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation(); // Don't trigger card click
                            this.add_or_update_server_dialog(&update_server, window, cx);
                        })),
                    // Delete button - shows confirmation before removing
                    Button::new(("servers-card-action-delete", index))
                        .ghost()
                        .tooltip(remove_tooltip.clone())
                        .icon(CustomIconName::FileXCorner)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation(); // Don't trigger card click
                            this.remove_server(window, cx, &remove_server_id);
                        })),
                ];

                // Card click handler - connect to server and navigate to editor
                let handle_select_server = cx.listener(move |_this, _, _, cx| {
                    let select_server_id = select_server_id.clone();

                    // Navigate to editor view
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.go_to(Route::Editor, cx);
                            state.set_selected_server((select_server_id.clone(), 0), cx);
                        });
                    });
                });

                // Build server card with conditional footer
                ZedisCard::new(("servers-card", index))
                    .icon(Icon::new(CustomIconName::DatabaseZap))
                    .title(title)
                    .bg(bg)
                    .when(!description.is_empty(), |this| {
                        this.description(description.to_string())
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
                    .actions(actions)
                    .on_click(Box::new(handle_select_server))
            })
            .collect();
        // Render responsive grid with server cards + add new server card
        div()
            .grid()
            .grid_cols(cols)
            .gap_1()
            .w_full()
            .children(children)
            .child(
                // "Add New Server" card at the end
                ZedisCard::new("servers-card-add")
                    .icon(IconName::Plus)
                    .title(i18n_servers(cx, "add_server_title"))
                    .bg(bg)
                    .description(i18n_servers(cx, "add_server_description"))
                    .actions(vec![Button::new("add").ghost().icon(CustomIconName::FilePlusCorner)])
                    .on_click(Box::new(cx.listener(move |this, _, window, cx| {
                        // Fill with empty server data for new entry
                        this.add_or_update_server_dialog(
                            &RedisServer {
                                port: DEFAULT_REDIS_PORT,
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    }))),
            )
            .into_any_element()
    }
}

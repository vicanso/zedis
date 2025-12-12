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
use crate::states::i18n_common;
use crate::states::i18n_servers;
use gpui::App;
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
use gpui_component::input::NumberInput;
use gpui_component::label::Label;
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use substring::Substring;
use tracing::info;

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
    /// Reference to server state for Redis operations
    server_state: Entity<ZedisServerState>,

    /// Input field states for server configuration form
    name_state: Entity<InputState>,
    host_state: Entity<InputState>,
    port_state: Entity<InputState>,
    password_state: Entity<InputState>,
    description_state: Entity<InputState>,

    /// Flag indicating if we're adding a new server (vs editing existing)
    server_id: String,
}

impl ZedisServers {
    /// Create a new server management view
    ///
    /// Initializes all input field states with appropriate placeholders
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize input fields for server configuration form
        let name_state = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "name_placeholder")));
        let host_state = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "host_placeholder")));
        let port_state = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "port_placeholder")));
        let password_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "password_placeholder")));
        let description_state =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "description_placeholder")));

        info!("Creating new servers view");

        Self {
            server_state,
            name_state,
            host_state,
            port_state,
            password_state,
            description_state,
            server_id: String::new(),
        }
    }
    /// Fill input fields with server data for editing
    ///
    fn fill_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>, server: &RedisServer) {
        self.server_id = server.id.clone();

        // Populate all input fields with server data
        self.name_state.update(cx, |state, cx| {
            state.set_value(server.name.clone(), window, cx);
        });
        self.host_state.update(cx, |state, cx| {
            state.set_value(server.host.clone(), window, cx);
        });

        // Only set port if non-zero (use placeholder for 0)
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

    /// Show confirmation dialog and remove server from configuration
    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>, server_id: &str) {
        let mut server = "--".to_string();
        if let Some(servers) = self.server_state.read(cx).servers()
            && let Some(found) = servers.iter().find(|item| item.id == server_id)
        {
            server = found.name.clone();
        }
        let server_state = self.server_state.clone();
        let server_id = server_id.to_string();

        // let server = server.to_string();
        let locale = cx.global::<ZedisGlobalStore>().locale(cx).to_string();

        window.open_dialog(cx, move |dialog, _, _| {
            let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();
            let server_state = server_state.clone();
            let server_id = server_id.clone();

            dialog.confirm().child(message).on_ok(move |_, window, cx| {
                server_state.update(cx, |state, cx| {
                    state.remove_server(&server_id, cx);
                });
                window.close_dialog(cx);
                true
            })
        });
    }
    /// Open dialog to add new server or update existing server
    ///
    /// Shows a form with fields for name, host, port, password, and description.
    /// If is_new is true, name field is editable. Otherwise, it's disabled.
    fn add_or_update_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
        let name_state = self.name_state.clone();
        let host_state = self.host_state.clone();
        let port_state = self.port_state.clone();
        let password_state = self.password_state.clone();
        let description_state = self.description_state.clone();
        let server_id = self.server_id.clone();
        let is_new = server_id.is_empty();

        let server_state_clone = server_state.clone();
        let name_state_clone = name_state.clone();
        let host_state_clone = host_state.clone();
        let port_state_clone = port_state.clone();
        let password_state_clone = password_state.clone();
        let description_state_clone = description_state.clone();
        let server_id_clone = server_id.clone();

        let handle_submit = Rc::new(move |window: &mut Window, cx: &mut App| {
            let name = name_state_clone.read(cx).value();
            let host = host_state_clone.read(cx).value();
            let port = port_state_clone
                .read(cx)
                .value()
                .parse::<u16>()
                .unwrap_or(DEFAULT_REDIS_PORT);

            let password_val = password_state_clone.read(cx).value();
            let password = if password_val.is_empty() {
                None
            } else {
                Some(password_val)
            };

            let desc_val = description_state_clone.read(cx).value();
            let description = if desc_val.is_empty() { None } else { Some(desc_val) };

            server_state_clone.update(cx, |state, cx| {
                let current_server = state.server(server_id_clone.as_str()).cloned().unwrap_or_default();

                state.update_or_insrt_server(
                    RedisServer {
                        id: server_id_clone.clone(),
                        name: name.to_string(),
                        host: host.to_string(),
                        port,
                        password: password.map(|p| p.to_string()),
                        description: description.map(|d| d.to_string()),
                        ..current_server
                    },
                    cx,
                );
            });

            window.close_dialog(cx);
            true
        });

        let focus_handle_done = Cell::new(false);
        window.open_dialog(cx, move |dialog, window, cx| {
            // Set dialog title based on add/update mode
            let title = if is_new {
                i18n_servers(cx, "add_server_title")
            } else {
                i18n_servers(cx, "update_server_title")
            };

            // Prepare field labels
            let name_label = i18n_common(cx, "name");
            let host_label = i18n_common(cx, "host");
            let port_label = i18n_common(cx, "port");
            let password_label = i18n_common(cx, "password");
            let description_label = i18n_common(cx, "description");

            dialog
                .title(title)
                .overlay(true)
                .child({
                    if !focus_handle_done.get() {
                        name_state.clone().update(cx, |this, cx| {
                            this.focus(window, cx);
                        });
                        focus_handle_done.set(true);
                    }
                    v_form()
                        .child(
                            field()
                                .label(name_label)
                                // Name is read-only when editing existing server
                                .child(Input::new(&name_state)),
                        )
                        .child(field().label(host_label).child(Input::new(&host_state)))
                        .child(field().label(port_label).child(NumberInput::new(&port_state)))
                        .child(
                            field()
                                .label(password_label)
                                // Password field with show/hide toggle
                                .child(Input::new(&password_state).mask_toggle()),
                        )
                        .child(field().label(description_label).child(Input::new(&description_state)))
                })
                .on_ok({
                    let handle = handle_submit.clone();
                    move |_, window, cx| handle(window, cx)
                })
                .footer({
                    let handle = handle_submit.clone();
                    move |_, _, _, cx| {
                        let submit_label = i18n_common(cx, "submit");
                        let cancel_label = i18n_common(cx, "cancel");

                        vec![
                            // Submit button - validates and saves server configuration
                            Button::new("ok").primary().label(submit_label).on_click({
                                let handle = handle.clone();
                                move |_, window, cx| {
                                    handle.clone()(window, cx);
                                }
                            }),
                            // Cancel button - closes dialog without saving
                            Button::new("cancel").label(cancel_label).on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                        ]
                    }
                })
        });
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
        let children: Vec<_> = self
            .server_state
            .read(cx)
            .servers()
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
                            this.fill_inputs(window, cx, &update_server);
                            this.add_or_update_server(window, cx);
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
                let handle_select_server = cx.listener(move |this, _, _, cx| {
                    let select_server_id = select_server_id.clone();

                    // Connect to server
                    this.server_state.update(cx, |state, cx| {
                        state.select(select_server_id.into(), cx);
                    });

                    // Navigate to editor view
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.go_to(Route::Editor, cx);
                        });
                    });
                });

                // Build server card with conditional footer
                Card::new(("servers-card", index))
                    .icon(Icon::new(CustomIconName::DatabaseZap))
                    .title(title)
                    .bg(bg)
                    .when(!description.is_empty(), |this| this.description(description))
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
                    .on_click(handle_select_server)
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
                Card::new("servers-card-add")
                    .icon(IconName::Plus)
                    .title(i18n_servers(cx, "add_server_title"))
                    .bg(bg)
                    .description(i18n_servers(cx, "add_server_description"))
                    .actions(vec![Button::new("add").ghost().icon(CustomIconName::FilePlusCorner)])
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Fill with empty server data for new entry
                        this.fill_inputs(window, cx, &RedisServer::default());
                        this.add_or_update_server(window, cx);
                    })),
            )
            .into_any_element()
    }
}

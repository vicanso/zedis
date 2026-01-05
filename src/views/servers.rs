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
use crate::components::Card;
use crate::connection::{test_connection, RedisServer};
use crate::helpers::{validate_common_string, validate_host, validate_long_string};
use crate::states::{i18n_common, i18n_servers, Route, ZedisGlobalStore, ZedisServerState};
use gpui::{div, prelude::*, px, App, Entity, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    form::{field, v_form},
    h_flex,
    input::{Input, InputState, NumberInput},
    label::Label,
    notification::Notification,
    ActiveTheme, Colorize, Icon, IconName, Sizable, WindowExt,
};
use rust_i18n::t;
use std::{cell::{Cell, RefCell}, rc::Rc};
use substring::Substring;
use tracing::info;

// Constants for UI layout
const DEFAULT_REDIS_PORT: u16 = 6379;
const VIEWPORT_BREAKPOINT_SMALL: f32 = 800.0; // Single column
const VIEWPORT_BREAKPOINT_MEDIUM: f32 = 1200.0; // Two columns
const UPDATED_AT_SUBSTRING_LENGTH: usize = 10; // Length of date string to display
const THEME_LIGHTEN_AMOUNT_DARK: f32 = 1.0;
const THEME_DARKEN_AMOUNT_LIGHT: f32 = 0.02;

/// Test connection state
#[derive(Clone, Copy, PartialEq, Default)]
enum TestConnectionState {
    #[default]
    Idle,
    Testing,
    Success,
    Failed,
}

/// Test connection result for status icon display
#[derive(Clone, Default)]
struct TestConnectionResult {
    state: TestConnectionState,
    error_message: Option<String>,
    notification_pending: bool,
}

/// Build a RedisServer from input states for testing connection
fn build_server_from_inputs(
    name: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    master_name: Option<&str>,
) -> RedisServer {
    RedisServer {
        id: String::new(),
        name: name.to_string(),
        host: host.to_string(),
        port,
        username: username.map(|s| s.to_string()),
        password: password.map(|s| s.to_string()),
        master_name: master_name.map(|s| s.to_string()),
        description: None,
        updated_at: None,
        query_mode: None,
        soft_wrap: None,
    }
}

/// Execute test connection and update result state
fn execute_test_connection(
    server: RedisServer,
    test_result: Rc<RefCell<TestConnectionResult>>,
    cx: &mut App,
) {
    {
        let mut result = test_result.borrow_mut();
        result.state = TestConnectionState::Testing;
        result.notification_pending = false;
    }

    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { test_connection(&server).await })
            .await;

        cx.update(|cx| {
            let mut test_result_ref = test_result.borrow_mut();
            match result {
                Ok(_) => {
                    test_result_ref.state = TestConnectionState::Success;
                    test_result_ref.error_message = None;
                }
                Err(e) => {
                    test_result_ref.state = TestConnectionState::Failed;
                    test_result_ref.error_message = Some(e.to_string());
                }
            }
            test_result_ref.notification_pending = true;
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
}

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
    username_state: Entity<InputState>,
    password_state: Entity<InputState>,
    master_name_state: Entity<InputState>,
    description_state: Entity<InputState>,

    /// Flag indicating if we're adding a new server (vs editing existing)
    server_id: String,

    /// Test connection result for UI feedback
    test_result: Rc<RefCell<TestConnectionResult>>,
}

impl ZedisServers {
    /// Create a new server management view
    ///
    /// Initializes all input field states with appropriate placeholders
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize input fields for server configuration form
        let name_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_common(cx, "name_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
        });
        let host_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_common(cx, "host_placeholder"))
                .validate(|s, _cx| validate_host(s))
        });
        let port_state = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_common(cx, "port_placeholder")));
        let username_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_common(cx, "username_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
        });
        let password_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_common(cx, "password_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
                .masked(true)
        });
        let description_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_common(cx, "description_placeholder"))
                .validate(|s, _cx| validate_long_string(s))
        });
        let master_name_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_servers(cx, "master_name_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
        });
        info!("Creating new servers view");

        Self {
            server_state,
            name_state,
            host_state,
            port_state,
            username_state,
            password_state,
            master_name_state,
            description_state,
            server_id: String::new(),
            test_result: Rc::new(RefCell::new(TestConnectionResult::default())),
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
        self.username_state.update(cx, |state, cx| {
            state.set_value(server.username.clone().unwrap_or_default(), window, cx);
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
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

        window.open_dialog(cx, move |dialog, _, cx| {
            let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();
            let server_state = server_state.clone();
            let server_id = server_id.clone();

            dialog
                .confirm()
                .title(i18n_servers(cx, "remove_server_title"))
                .child(message)
                .on_ok(move |_, window, cx| {
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
        let username_state = self.username_state.clone();
        let password_state = self.password_state.clone();
        let master_name_state = self.master_name_state.clone();
        let description_state = self.description_state.clone();
        let server_id = self.server_id.clone();
        let is_new = server_id.is_empty();

        let server_state_clone = server_state.clone();
        let name_state_clone = name_state.clone();
        let host_state_clone = host_state.clone();
        let port_state_clone = port_state.clone();
        let username_state_clone = username_state.clone();
        let password_state_clone = password_state.clone();
        let master_name_state_clone = master_name_state.clone();
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
            if name.is_empty() || host.is_empty() {
                return false;
            }

            let password_val = password_state_clone.read(cx).value();
            let password = if password_val.is_empty() {
                None
            } else {
                Some(password_val)
            };
            let username_val = username_state_clone.read(cx).value();
            let username = if username_val.is_empty() {
                None
            } else {
                Some(username_val)
            };
            let master_name_val = master_name_state_clone.read(cx).value();
            let master_name = if master_name_val.is_empty() {
                None
            } else {
                Some(master_name_val)
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
                        username: username.map(|u| u.to_string()),
                        password: password.map(|p| p.to_string()),
                        master_name: master_name.map(|m| m.to_string()),
                        description: description.map(|d| d.to_string()),
                        ..current_server
                    },
                    cx,
                );
            });

            window.close_dialog(cx);
            true
        });

        // Reset test connection state when opening dialog
        self.test_result.borrow_mut().state = TestConnectionState::Idle;
        let test_result = self.test_result.clone();

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
            let username_label = i18n_common(cx, "username");
            let password_label = i18n_common(cx, "password");
            let description_label = i18n_common(cx, "description");
            let master_name_label = i18n_servers(cx, "master_name");

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
                        .child(field().label(username_label).child(Input::new(&username_state)))
                        .child(
                            field()
                                .label(password_label)
                                // Password field with show/hide toggle
                                .child(Input::new(&password_state).mask_toggle()),
                        )
                        .child(field().label(master_name_label).child(Input::new(&master_name_state)))
                        .child(field().label(description_label).child(Input::new(&description_state)))
                })
                .on_ok({
                    let handle = handle_submit.clone();
                    move |_, window, cx| handle(window, cx)
                })
                .footer({
                    let handle = handle_submit.clone();
                    let name_state = name_state.clone();
                    let host_state = host_state.clone();
                    let port_state = port_state.clone();
                    let username_state = username_state.clone();
                    let password_state = password_state.clone();
                    let master_name_state = master_name_state.clone();

                    // Use component-level test connection result
                    let test_result = test_result.clone();

                    move |_, _, window, cx| {
                        let submit_label = i18n_common(cx, "submit");
                        let cancel_label = i18n_common(cx, "cancel");
                        let test_label = i18n_servers(cx, "test_connection");
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();

                        // Check for pending notification and push it
                        {
                            let mut test_result_ref = test_result.borrow_mut();
                            if test_result_ref.notification_pending {
                                test_result_ref.notification_pending = false;
                                let notification = match test_result_ref.state {
                                    TestConnectionState::Success => {
                                        Notification::success(i18n_servers(cx, "test_connection_success"))
                                    }
                                    TestConnectionState::Failed => {
                                        let error_msg = test_result_ref.error_message.clone().unwrap_or_default();
                                        let msg = t!("servers.test_connection_failed", error = error_msg, locale = locale).to_string();
                                        Notification::error(msg)
                                    }
                                    _ => return vec![],
                                };
                                window.push_notification(notification, cx);
                            }
                        }

                        let test_name_state = name_state.clone();
                        let test_host_state = host_state.clone();
                        let test_port_state = port_state.clone();
                        let test_username_state = username_state.clone();
                        let test_password_state = password_state.clone();
                        let test_master_name_state = master_name_state.clone();

                        let current_state = test_result.borrow().state;
                        let is_testing = current_state == TestConnectionState::Testing;
                        let show_status_icon = current_state == TestConnectionState::Success
                            || current_state == TestConnectionState::Failed;
                        let is_success = current_state == TestConnectionState::Success;

                        let test_button = Button::new("test")
                            .label(test_label)
                            .loading(is_testing)
                            .on_click({
                                let test_result = test_result.clone();
                                move |_, _window, cx| {
                                    let host = test_host_state.read(cx).value();
                                    if host.is_empty() {
                                        return;
                                    }
                                    let port = test_port_state
                                        .read(cx)
                                        .value()
                                        .parse::<u16>()
                                        .unwrap_or(DEFAULT_REDIS_PORT);
                                    let password_val = test_password_state.read(cx).value();
                                    let username_val = test_username_state.read(cx).value();
                                    let master_name_val = test_master_name_state.read(cx).value();

                                    let server = build_server_from_inputs(
                                        &test_name_state.read(cx).value(),
                                        &host,
                                        port,
                                        if username_val.is_empty() { None } else { Some(&username_val) },
                                        if password_val.is_empty() { None } else { Some(&password_val) },
                                        if master_name_val.is_empty() { None } else { Some(&master_name_val) },
                                    );

                                    execute_test_connection(server, test_result.clone(), cx);
                                }
                            });

                        let left_side = h_flex()
                            .gap_2()
                            .items_center()
                            .child(test_button)
                            .when(show_status_icon, |this| {
                                let status_icon = if is_success {
                                    Icon::new(CustomIconName::CircleCheckBig)
                                        .small()
                                        .text_color(cx.theme().success)
                                } else {
                                    Icon::new(CustomIconName::X)
                                        .small()
                                        .text_color(cx.theme().danger)
                                };
                                this.child(status_icon)
                            });

                        vec![
                            left_side.into_any_element(),
                            div().flex_grow().into_any_element(),
                            Button::new("ok")
                                .primary()
                                .label(submit_label)
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, window, cx| {
                                        handle.clone()(window, cx);
                                    }
                                })
                                .into_any_element(),
                            Button::new("cancel")
                                .label(cancel_label)
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
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

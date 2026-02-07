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
use crate::connection::{QueryMode, RedisServer, get_servers};
use crate::helpers::{is_windows, validate_common_string, validate_host, validate_long_string};
use crate::states::{Route, ZedisGlobalStore, dialog_button_props, i18n_common, i18n_servers};
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Colorize, Icon, IconName, WindowExt,
    alert::Alert,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    form::{field, v_form},
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    label::Label,
    radio::RadioGroup,
    scroll::ScrollableElement,
    tab::{Tab, TabBar},
    text::TextView,
};
use rust_i18n::t;
use std::collections::HashMap;
use std::{cell::Cell, rc::Rc};
use substring::Substring;
use tracing::info;
use url::Url;

// Constants for UI layout
const DEFAULT_REDIS_PORT: u16 = 6379;
const VIEWPORT_BREAKPOINT_SMALL: f32 = 800.0; // Single column
const VIEWPORT_BREAKPOINT_MEDIUM: f32 = 1200.0; // Two columns
const UPDATED_AT_SUBSTRING_LENGTH: usize = 10; // Length of date string to display
const THEME_LIGHTEN_AMOUNT_DARK: f32 = 1.0;
const THEME_DARKEN_AMOUNT_LIGHT: f32 = 0.02;

#[derive(Debug, Clone, Default)]
struct RedisUrl {
    host: String,
    port: Option<u16>,
    username: String,
    password: Option<String>,
    tls: bool,
}

fn parse_url(host: SharedString) -> RedisUrl {
    let input_to_parse = if host.contains("://") {
        host.to_string()
    } else {
        format!("redis://{host}")
    };
    if let Ok(u) = Url::parse(input_to_parse.as_str()) {
        let host = u.host_str().unwrap_or("");
        let port = u.port();
        RedisUrl {
            host: host.to_string(),
            port,
            username: u.username().to_string(),
            password: u.password().map(|p| p.to_string()),
            tls: u.scheme() == "rediss",
        }
    } else {
        RedisUrl {
            host: host.to_string(),
            ..Default::default()
        }
    }
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
    /// Input field states for server configuration form
    name_state: Entity<InputState>,
    host_state: Entity<InputState>,
    port_state: Entity<InputState>,
    username_state: Entity<InputState>,
    password_state: Entity<InputState>,
    server_type_state: Entity<usize>,
    query_mode_state: Entity<usize>,
    client_cert_state: Entity<InputState>,
    client_key_state: Entity<InputState>,
    root_cert_state: Entity<InputState>,
    master_name_state: Entity<InputState>,
    ssh_addr_state: Entity<InputState>,
    ssh_username_state: Entity<InputState>,
    ssh_password_state: Entity<InputState>,
    ssh_key_state: Entity<InputState>,
    description_state: Entity<InputState>,
    field_errors: Entity<HashMap<String, SharedString>>,

    /// Flag indicating if we're adding a new server (vs editing existing)
    server_id: String,

    server_enable_tls: Rc<Cell<bool>>,
    server_enable_soft_wrap: Rc<Cell<bool>>,
    server_insecure_tls: Rc<Cell<bool>>,
    server_ssh_tunnel: Rc<Cell<bool>>,
    server_readonly: Rc<Cell<bool>>,

    query_modes: Vec<QueryMode>,

    _subscriptions: Vec<Subscription>,
}

impl ZedisServers {
    /// Create a new server management view
    ///
    /// Initializes all input field states with appropriate placeholders
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let (cert_min_rows, cert_max_rows) = (2, 100);

        let client_cert_state = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(cert_min_rows, cert_max_rows)
                .placeholder(i18n_common(cx, "client_cert_placeholder"))
        });
        let client_key_state = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(cert_min_rows, cert_max_rows)
                .placeholder(i18n_common(cx, "client_key_placeholder"))
        });
        let root_cert_state = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(cert_min_rows, cert_max_rows)
                .placeholder(i18n_common(cx, "root_cert_placeholder"))
        });
        let ssh_addr_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_servers(cx, "ssh_addr_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
        });
        let ssh_username_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_servers(cx, "ssh_username_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
        });
        let ssh_password_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n_servers(cx, "ssh_password_placeholder"))
                .validate(|s, _cx| validate_common_string(s))
                .masked(true)
        });
        let ssh_key_state = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(cert_min_rows, cert_max_rows)
                .placeholder(i18n_servers(cx, "ssh_key_placeholder"))
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
        let server_type_state = cx.new(|_cx| 0_usize);
        let query_mode_state = cx.new(|_cx| 0_usize);

        let port_state_clone = port_state.clone();
        let username_state_clone = username_state.clone();
        let password_state_clone = password_state.clone();
        let mut subscriptions = vec![];
        subscriptions.push(
            cx.subscribe_in(&host_state, window, move |view, state, event, window, cx| {
                if let InputEvent::Blur = event {
                    let host = state.read(cx).value();
                    let info = parse_url(host.clone());
                    if info.host != host {
                        view.server_enable_tls.set(info.tls);
                        state.update(cx, |state, cx| {
                            state.set_value(info.host, window, cx);
                        });
                        if let Some(port) = info.port {
                            port_state_clone.update(cx, |state, cx| {
                                state.set_value(port.to_string(), window, cx);
                            });
                        }
                        if !info.username.is_empty() {
                            username_state_clone.update(cx, |state, cx| {
                                state.set_value(info.username, window, cx);
                            });
                        }
                        if let Some(password) = info.password {
                            password_state_clone.update(cx, |state, cx| {
                                state.set_value(password, window, cx);
                            });
                        }
                    }
                }
            }),
        );
        subscriptions.push(cx.subscribe_in(&port_state, window, |_view, state, event, window, cx| {
            let NumberInputEvent::Step(action) = event;

            let Ok(current_val) = state.read(cx).value().parse::<u16>() else {
                return;
            };

            let new_val = match action {
                StepAction::Increment => current_val.saturating_add(1),
                StepAction::Decrement => current_val.saturating_sub(1),
            };

            if new_val != current_val {
                state.update(cx, |input, cx| {
                    input.set_value(new_val.to_string(), window, cx);
                });
            }
        }));
        let field_errors = cx.new(|_cx| HashMap::new());

        // add validation error subscription
        for item in [name_state.clone(), host_state.clone()] {
            subscriptions.push(
                cx.subscribe_in(&item.clone(), window, move |view, _state, event, _window, cx| {
                    if let InputEvent::Blur = event {
                        let id = item.entity_id().to_string();
                        if view.field_errors.read(cx).get(&id).is_some() {
                            view.field_errors.update(cx, |state, _cx| {
                                state.remove(&id);
                            });
                        }
                    }
                }),
            );
        }

        info!("Creating new servers view");

        Self {
            query_modes: vec![QueryMode::All, QueryMode::Prefix, QueryMode::Exact],
            name_state,
            host_state,
            port_state,
            username_state,
            password_state,
            server_type_state,
            client_cert_state,
            client_key_state,
            root_cert_state,
            master_name_state,
            query_mode_state,
            ssh_addr_state,
            ssh_username_state,
            ssh_password_state,
            ssh_key_state,
            description_state,
            field_errors,
            server_id: String::new(),
            server_enable_soft_wrap: Rc::new(Cell::new(false)),
            server_enable_tls: Rc::new(Cell::new(false)),
            server_insecure_tls: Rc::new(Cell::new(false)),
            server_ssh_tunnel: Rc::new(Cell::new(false)),
            server_readonly: Rc::new(Cell::new(false)),
            _subscriptions: subscriptions,
        }
    }
    /// Fill input fields with server data for editing
    ///
    fn fill_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>, server: &RedisServer) {
        self.server_id = server.id.clone();
        self.field_errors.update(cx, |state, _cx| {
            state.clear();
        });

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
        let port = if server.port != 0 {
            server.port.to_string()
        } else {
            String::new()
        };
        self.port_state.update(cx, |state, cx| {
            state.set_value(port, window, cx);
        });

        self.password_state.update(cx, |state, cx| {
            state.set_value(server.password.clone().unwrap_or_default(), window, cx);
        });
        self.master_name_state.update(cx, |state, cx| {
            state.set_value(server.master_name.clone().unwrap_or_default(), window, cx);
        });
        self.description_state.update(cx, |state, cx| {
            state.set_value(server.description.clone().unwrap_or_default(), window, cx);
        });
        self.client_cert_state.update(cx, |state, cx| {
            state.set_value(server.client_cert.clone().unwrap_or_default(), window, cx);
        });
        self.client_key_state.update(cx, |state, cx| {
            state.set_value(server.client_key.clone().unwrap_or_default(), window, cx);
        });
        self.root_cert_state.update(cx, |state, cx| {
            state.set_value(server.root_cert.clone().unwrap_or_default(), window, cx);
        });
        self.ssh_addr_state.update(cx, |state, cx| {
            state.set_value(server.ssh_addr.clone().unwrap_or_default(), window, cx);
        });
        self.ssh_username_state.update(cx, |state, cx| {
            state.set_value(server.ssh_username.clone().unwrap_or_default(), window, cx);
        });
        self.ssh_password_state.update(cx, |state, cx| {
            state.set_value(server.ssh_password.clone().unwrap_or_default(), window, cx);
        });
        self.ssh_key_state.update(cx, |state, cx| {
            state.set_value(server.ssh_key.clone().unwrap_or_default(), window, cx);
        });
        self.server_enable_tls.set(server.tls.unwrap_or_default());
        self.server_enable_soft_wrap.set(server.soft_wrap.unwrap_or(true));
        self.server_insecure_tls.set(server.insecure.unwrap_or_default());
        self.server_ssh_tunnel.set(server.ssh_tunnel.unwrap_or_default());
        self.server_readonly.set(server.readonly.unwrap_or_default());
        self.server_type_state.update(cx, |state, _cx| {
            *state = server.server_type.unwrap_or(0);
        });
        let query_mode = server.query_mode.as_deref().unwrap_or("*");
        self.query_mode_state.update(cx, |state, _cx| {
            *state = self
                .query_modes
                .iter()
                .position(|mode| mode.to_string() == query_mode)
                .unwrap_or(0);
        });
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

        window.open_dialog(cx, move |dialog, _, cx| {
            let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();
            let server_id = server_id.clone();

            dialog
                .confirm()
                .button_props(dialog_button_props(cx))
                .title(i18n_servers(cx, "remove_server_title"))
                .child(message)
                .on_ok(move |_, window, cx| {
                    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                        store.update(cx, |state, cx| {
                            state.remove_server(&server_id, cx);
                        });
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
        let name_state = self.name_state.clone();
        let host_state = self.host_state.clone();
        let port_state = self.port_state.clone();
        let username_state = self.username_state.clone();
        let password_state = self.password_state.clone();
        let master_name_state = self.master_name_state.clone();
        let description_state = self.description_state.clone();
        let client_cert_state = self.client_cert_state.clone();
        let client_key_state = self.client_key_state.clone();
        let root_cert_state = self.root_cert_state.clone();
        let server_id = self.server_id.clone();
        let is_new = server_id.is_empty();
        let ssh_addr_state = self.ssh_addr_state.clone();
        let ssh_username_state = self.ssh_username_state.clone();
        let ssh_password_state = self.ssh_password_state.clone();
        let ssh_key_state = self.ssh_key_state.clone();
        // Create shared state for TLS checkbox
        let server_enable_tls = self.server_enable_tls.clone();
        let server_insecure_tls = self.server_insecure_tls.clone();
        let server_ssh_tunnel = self.server_ssh_tunnel.clone();
        let server_type_state = self.server_type_state.clone();
        let query_mode_state = self.query_mode_state.clone();
        let name_state_clone = name_state.clone();
        let host_state_clone = host_state.clone();
        let port_state_clone = port_state.clone();
        let username_state_clone = username_state.clone();
        let password_state_clone = password_state.clone();
        let master_name_state_clone = master_name_state.clone();
        let description_state_clone = description_state.clone();
        let client_cert_state_clone = client_cert_state.clone();
        let client_key_state_clone = client_key_state.clone();
        let root_cert_state_clone = root_cert_state.clone();
        let ssh_addr_state_clone = ssh_addr_state.clone();
        let ssh_username_state_clone = ssh_username_state.clone();
        let ssh_password_state_clone = ssh_password_state.clone();
        let ssh_key_state_clone = ssh_key_state.clone();
        let server_id_clone = server_id.clone();
        let server_enable_soft_wrap = self.server_enable_soft_wrap.clone();
        let server_enable_soft_wrap_for_submit = server_enable_soft_wrap.clone();
        let query_mode_state_clone = query_mode_state.clone();
        let query_mode_state_for_submit = query_mode_state_clone.clone();
        let server_enable_tls_for_submit = self.server_enable_tls.clone();
        let server_insecure_tls_for_submit = self.server_insecure_tls.clone();
        let server_ssh_tunnel_for_submit = server_ssh_tunnel.clone();
        let server_readonly = self.server_readonly.clone();
        let server_readonly_for_submit = server_readonly.clone();
        let server_type_state_clone = server_type_state.clone();
        let field_errors = self.field_errors.clone();
        let field_errors_clone = field_errors.clone();
        let query_modes = self.query_modes.clone();
        let handle_submit = Rc::new(move |window: &mut Window, cx: &mut App| {
            field_errors.update(cx, |state, _cx| {
                state.clear();
            });
            let name = name_state_clone.read(cx).value();
            let host = host_state_clone.read(cx).value();
            let port = port_state_clone
                .read(cx)
                .value()
                .parse::<u16>()
                .unwrap_or(DEFAULT_REDIS_PORT);
            if name.is_empty() {
                let id = name_state_clone.entity_id().to_string();
                field_errors.update(cx, |state, _cx| {
                    state.insert(id, "name is required".into());
                });
            }
            if host.is_empty() {
                let id = host_state_clone.entity_id().to_string();
                field_errors.update(cx, |state, _cx| {
                    state.insert(id, "host is required".into());
                });
            }
            if !field_errors.read(cx).is_empty() {
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
            let enable_tls = server_enable_tls_for_submit.get();
            let (client_cert, client_key, root_cert) = if enable_tls {
                let client_cert_val = client_cert_state_clone.read(cx).value();
                let client_cert = if client_cert_val.is_empty() {
                    None
                } else {
                    Some(client_cert_val)
                };
                let client_key_val = client_key_state_clone.read(cx).value();
                let client_key = if client_key_val.is_empty() {
                    None
                } else {
                    Some(client_key_val)
                };
                let root_cert_val = root_cert_state_clone.read(cx).value();
                let root_cert = if root_cert_val.is_empty() {
                    None
                } else {
                    Some(root_cert_val)
                };
                (client_cert, client_key, root_cert)
            } else {
                (None, None, None)
            };

            let insecure_tls = if server_insecure_tls_for_submit.get() {
                Some(true)
            } else {
                None
            };

            let master_name_val = master_name_state_clone.read(cx).value();
            let master_name = if master_name_val.is_empty() {
                None
            } else {
                Some(master_name_val)
            };
            let desc_val = description_state_clone.read(cx).value();
            let description = if desc_val.is_empty() { None } else { Some(desc_val) };

            let ssh_tunnel = server_ssh_tunnel_for_submit.get();
            let ssh_addr_val = ssh_addr_state_clone.read(cx).value();
            let ssh_addr = if ssh_addr_val.is_empty() {
                None
            } else {
                Some(ssh_addr_val)
            };
            let ssh_username_val = ssh_username_state_clone.read(cx).value();
            let ssh_username = if ssh_username_val.is_empty() {
                None
            } else {
                Some(ssh_username_val)
            };
            let ssh_password_val = ssh_password_state_clone.read(cx).value();
            let ssh_password = if ssh_password_val.is_empty() {
                None
            } else {
                Some(ssh_password_val)
            };
            let ssh_key_val = ssh_key_state_clone.read(cx).value();
            let ssh_key = if ssh_key_val.is_empty() {
                None
            } else {
                Some(ssh_key_val)
            };

            let readonly = if server_readonly_for_submit.get() {
                Some(true)
            } else {
                None
            };
            let soft_wrap = if server_enable_soft_wrap_for_submit.get() {
                Some(true)
            } else {
                Some(false)
            };

            let server_type = *server_type_state.read(cx);
            let server_type = if server_type > 0 { Some(server_type) } else { None };
            let query_mode = *query_mode_state_for_submit.read(cx);
            let query_mode = query_modes.get(query_mode).copied().unwrap_or_default();

            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                store.update(cx, |state, cx| {
                    state.upsert_server(
                        RedisServer {
                            id: server_id_clone.clone(),
                            name: name.to_string(),
                            host: host.to_string(),
                            port,
                            username: username.map(|u| u.to_string()),
                            password: password.map(|p| p.to_string()),
                            server_type,
                            master_name: master_name.map(|m| m.to_string()),
                            description: description.map(|d| d.to_string()),
                            tls: if enable_tls { Some(enable_tls) } else { None },
                            insecure: insecure_tls,
                            client_cert: client_cert.map(|c| c.to_string()),
                            client_key: client_key.map(|k| k.to_string()),
                            root_cert: root_cert.map(|r| r.to_string()),
                            ssh_tunnel: if ssh_tunnel { Some(ssh_tunnel) } else { None },
                            ssh_addr: ssh_addr.map(|a| a.to_string()),
                            ssh_username: ssh_username.map(|u| u.to_string()),
                            ssh_password: ssh_password.map(|p| p.to_string()),
                            ssh_key: ssh_key.map(|k| k.to_string()),
                            readonly,
                            soft_wrap,
                            query_mode: Some(query_mode.to_string()),
                            ..Default::default()
                        },
                        cx,
                    );
                })
            });

            window.close_dialog(cx);
            true
        });

        let tab_selected_index = cx.new(|_cx| 0_usize);

        let focus_handle_done = Cell::new(false);
        window.open_dialog(cx, move |dialog, window, cx| {
            // let field_errors_clone = field_errors.clone();
            let tab_selected_index_clone = tab_selected_index.clone();
            // Set dialog title based on add/update mode
            let title = if is_new {
                i18n_servers(cx, "add_server_title")
            } else {
                i18n_servers(cx, "update_server_title")
            };
            let max_h = (window.bounds().size.height - px(300.0)).min(px(600.0));

            // Prepare field labels
            let name_label = i18n_common(cx, "name");
            let host_label = i18n_common(cx, "host");
            let port_label = i18n_common(cx, "port");
            let username_label = i18n_common(cx, "username");
            let password_label = i18n_common(cx, "password");
            let tls_label = i18n_common(cx, "tls");
            let tls_check_label = i18n_common(cx, "tls_check_label");
            let insecure_tls_label = i18n_common(cx, "insecure_tls");
            let insecure_tls_check_label = i18n_common(cx, "insecure_tls_check_label");
            let client_cert_label = i18n_common(cx, "client_cert");
            let client_key_label = i18n_common(cx, "client_key");
            let root_cert_label = i18n_common(cx, "root_cert");
            let description_label = i18n_common(cx, "description");
            let master_name_label = i18n_servers(cx, "master_name");
            let ssh_addr_label = i18n_servers(cx, "ssh_addr");
            let ssh_username_label = i18n_servers(cx, "ssh_username");
            let ssh_password_label = i18n_servers(cx, "ssh_password");
            let ssh_key_label = i18n_servers(cx, "ssh_key");
            let ssh_tunnel_label = i18n_servers(cx, "ssh_tunnel");
            let ssh_tunnel_check_label = i18n_servers(cx, "ssh_tunnel_check_label");
            let readonly_label = i18n_servers(cx, "readonly");
            let readonly_check_label = i18n_servers(cx, "readonly_check_label");
            let soft_wrap_label = i18n_servers(cx, "soft_wrap");
            let soft_wrap_check_label = i18n_servers(cx, "soft_wrap_check_label");
            let tab_general_label = i18n_servers(cx, "tab_general");
            let tab_tls_label = i18n_servers(cx, "tab_tls");
            let tab_ssh_label = i18n_servers(cx, "tab_ssh");
            let tab_advanced_label = i18n_servers(cx, "tab_advanced");
            let server_type_label = i18n_servers(cx, "server_type");
            let server_type_list = i18n_servers(cx, "server_type_list");
            let query_mode_label = i18n_servers(cx, "query_mode");
            let query_mode_list = i18n_servers(cx, "query_mode_list");
            let current_tab_index = *tab_selected_index.read(cx);
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
                    let mut form = v_form();
                    let server_type_state_clone = server_type_state_clone.clone();
                    let query_mode_state_clone = query_mode_state_clone.clone();
                    form = match current_tab_index {
                        1 => form
                            .child(field().label(tls_label).child({
                                let server_enable_tls = server_enable_tls.clone();
                                Checkbox::new("redis-server-tls")
                                    .label(tls_check_label)
                                    .checked(server_enable_tls.get())
                                    .on_click(move |checked, _, cx| {
                                        server_enable_tls.set(*checked);
                                        cx.stop_propagation();
                                    })
                            }))
                            .child(field().label(insecure_tls_label).child({
                                let server_insecure_tls = server_insecure_tls.clone();
                                Checkbox::new("redis-server-insecure-tls")
                                    .label(insecure_tls_check_label)
                                    .checked(server_insecure_tls.get())
                                    .on_click(move |checked, _, cx| {
                                        server_insecure_tls.set(*checked);
                                        cx.stop_propagation();
                                    })
                            }))
                            .child(field().label(client_cert_label).child(Input::new(&client_cert_state)))
                            .child(field().label(client_key_label).child(Input::new(&client_key_state)))
                            .child(field().label(root_cert_label).child(Input::new(&root_cert_state))),
                        2 => form
                            .child(field().label(ssh_tunnel_label).child({
                                let server_ssh_tunnel = server_ssh_tunnel.clone();
                                Checkbox::new("redis-server-ssh-tunnel")
                                    .label(ssh_tunnel_check_label)
                                    .checked(server_ssh_tunnel.get())
                                    .on_click(move |checked, _, cx| {
                                        server_ssh_tunnel.set(*checked);
                                        cx.stop_propagation();
                                    })
                            }))
                            .child(field().label(ssh_addr_label).child(Input::new(&ssh_addr_state)))
                            .child(field().label(ssh_username_label).child(Input::new(&ssh_username_state)))
                            .child(
                                field()
                                    .label(ssh_password_label)
                                    .child(Input::new(&ssh_password_state).mask_toggle()),
                            )
                            .child(field().label(ssh_key_label).child(Input::new(&ssh_key_state))),
                        3 => form
                            .child(
                                field().label(server_type_label).child(
                                    RadioGroup::horizontal("horizontal-group")
                                        .children(
                                            server_type_list
                                                .split(" ")
                                                .map(|s| s.to_string())
                                                .collect::<Vec<String>>(),
                                        )
                                        .selected_index(Some(*server_type_state_clone.read(cx)))
                                        .on_click(move |index, _, cx| {
                                            server_type_state_clone.update(cx, |state, _cx| {
                                                *state = *index;
                                            });
                                        }),
                                ),
                            )
                            .child(field().label(readonly_label).child({
                                let server_readonly = server_readonly.clone();
                                Checkbox::new("redis-server-readonly")
                                    .label(readonly_check_label)
                                    .checked(server_readonly.get())
                                    .on_click(move |checked, _, cx| {
                                        server_readonly.set(*checked);
                                        cx.stop_propagation();
                                    })
                            }))
                            .child(field().label(soft_wrap_label).child({
                                let server_enable_soft_wrap = server_enable_soft_wrap.clone();
                                Checkbox::new("redis-server-soft-wrap")
                                    .label(soft_wrap_check_label)
                                    .checked(server_enable_soft_wrap.get())
                                    .on_click(move |checked, _, cx| {
                                        server_enable_soft_wrap.set(*checked);
                                        cx.stop_propagation();
                                    })
                            }))
                            .child(
                                field().label(query_mode_label).child(
                                    RadioGroup::horizontal("horizontal-group")
                                        .children(
                                            query_mode_list
                                                .split(" ")
                                                .map(|s| s.to_string())
                                                .collect::<Vec<String>>(),
                                        )
                                        .selected_index(Some(*query_mode_state_clone.read(cx)))
                                        .on_click(move |index, _, cx| {
                                            query_mode_state_clone.update(cx, |state, _cx| {
                                                *state = *index;
                                            });
                                        }),
                                ),
                            ),
                        _ => {
                            form.child(
                                field()
                                    .required(true)
                                    .label(name_label)
                                    // Name is read-only when editing existing server
                                    .child(Input::new(&name_state)),
                            )
                            .child(field().required(true).label(host_label).child(Input::new(&host_state)))
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
                        }
                    };

                    div()
                        .id("servers-scrollable-container")
                        .max_h(max_h)
                        .child(
                            TabBar::new("tabs")
                                .underline()
                                .mb_3()
                                .selected_index(*tab_selected_index.read(cx))
                                .on_click(move |selected_index, _, cx| {
                                    tab_selected_index_clone.update(cx, |state, cx| {
                                        *state = *selected_index;
                                        cx.notify();
                                    });
                                })
                                .child(Tab::new().label(tab_general_label).p_1())
                                .child(Tab::new().label(tab_tls_label).p_1())
                                .child(Tab::new().label(tab_ssh_label).p_1())
                                .child(Tab::new().label(tab_advanced_label).p_1()),
                        )
                        .child(form)
                        .when(!field_errors_clone.read(cx).is_empty(), |this| {
                            let title = i18n_servers(cx, "field_errors_title");
                            let list = field_errors_clone
                                .read(cx)
                                .values()
                                .map(|value| format!("- {value}"))
                                .collect::<Vec<String>>()
                                .join("\n");
                            let markdown = t!("servers.field_errors_message", errors = list);
                            this.child(
                                Alert::error(
                                    "servers-form-errors",
                                    TextView::markdown("servers-form-errors-message", markdown, window, cx),
                                )
                                .title(title)
                                .mt_4(),
                            )
                        })
                        .overflow_y_scrollbar()
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

                        let mut buttons = vec![
                            // Cancel button - closes dialog without saving
                            Button::new("cancel").label(cancel_label).on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                            // Submit button - validates and saves server configuration
                            Button::new("ok").primary().label(submit_label).on_click({
                                let handle = handle.clone();
                                move |_, window, cx| {
                                    handle.clone()(window, cx);
                                }
                            }),
                        ];

                        if is_windows() {
                            buttons.reverse();
                        }
                        buttons
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

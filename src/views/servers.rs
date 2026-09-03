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
use crate::connection::{
    ImportError, RedisServer, SERVER_TYPE_SENTINEL, TAG_ENV_LABELS, get_server_groups, get_servers,
    open_single_connection, tag_color_index,
};
use crate::error::Error;
use crate::helpers::{
    EditorAction, card_background, decrypt_share, get_mono_font_family, is_share_token, resolve_path, resolve_tag_chip,
};
use crate::states::{
    GlobalEvent, NotificationAction, ReorderDirection, Route, ZedisGlobalStore, dialog_button_props,
    escalate_dangerous_body, i18n_common, i18n_servers, update_app_state_and_save,
};
use crate::views::{ZedisExportServersDialog, export_filename, export_to_file_global, open_connection_diagnostics};
use gpui::{
    Action, Anchor, App, ClipboardItem, Entity, ExternalPaths, FocusHandle, Focusable, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::DropdownMenu;
use gpui_component::notification::Notification;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    label::Label,
};
use gpui_component::{h_flex, v_flex};
use redis::cmd;
use rust_i18n::t;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::mem::take;
use std::rc::Rc;
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

/// Per-card secondary actions dispatched from the footer "⋯" dropdown. The
/// payload is the server id; the handlers (registered on the view's render
/// root) look the server up / route to the existing dialog + reorder methods.
/// Edit stays a direct footer button (no dispatch needed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Action)]
enum ServersCardAction {
    Export(SharedString),
    Delete(SharedString),
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
    should_popup_new_server: bool,
    /// Focus root for the page. Actions only travel the focus path, so the
    /// container has to hold focus for the `cmd-f` binding to reach the
    /// `EditorAction::Search` handler below (same pattern as the key tree).
    focus_handle: FocusHandle,
    /// Toolbar search box — live-filters the card grid by name / host /
    /// description.
    search_state: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    /// Live subscription on the import dialog's text input — replaced each
    /// time the dialog opens so a pasted file path is read back into the box.
    import_input_sub: Option<Subscription>,
}

/// Toolbar search predicate: name / host / description, case-insensitive.
/// `query` must already be trimmed and lowercased; an empty one matches
/// everything. Shared by the grid and the Enter shortcut so the two can never
/// disagree about what "matches" means.
fn matches_query(server: &RedisServer, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    server.name.to_lowercase().contains(query)
        || server.host.to_lowercase().contains(query)
        || server
            .description
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
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

        // Toolbar search box — re-render the grid on each change so the
        // filter is live.
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_servers(cx, "search_placeholder"))
        });
        subscriptions.push(
            cx.subscribe_in(&search_state, window, |this, _state, event, _window, cx| match event {
                InputEvent::Change => cx.notify(),
                // Enter opens the match when the filter has narrowed the grid to
                // exactly one server — the "type a few letters, hit enter" flow.
                InputEvent::PressEnter { .. } => this.open_only_match(cx),
                _ => {}
            }),
        );

        // Focused up front so `cmd-f` works on arrival, without a click first —
        // an action is only offered to the elements on the focus path.
        // Note: `ZedisContent` may re-focus its own shell after RouteChanged;
        // content re-targets this handle on Home (see content.rs).
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        Self {
            should_popup_new_server: false,
            focus_handle,
            search_state,
            _subscriptions: subscriptions,
            import_input_sub: None,
        }
    }

    /// Put the caret in the toolbar filter box (`EditorAction::Search` / ⌘F).
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.search_state.focus_handle(cx).focus(window, cx);
    }

    /// Enter in the filter box: open the server *if the query leaves exactly
    /// one*. Zero or several matches leave nothing unambiguous to open, so the
    /// key does nothing rather than guessing (opening the first of many would be
    /// a connection to the wrong instance — not a mistake worth risking).
    fn open_only_match(&mut self, cx: &mut Context<Self>) {
        let query = self.search_state.read(cx).value().trim().to_lowercase();
        let Ok(servers) = get_servers() else { return };
        let mut matched = servers.iter().filter(|server| matches_query(server, &query));
        // Exactly one: a first match, and no second.
        let (Some(server), None) = (matched.next(), matched.next()) else {
            return;
        };
        let id = server.id.clone();
        info!(server = %id, "opening the only server matching the filter");
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |state, cx| {
                let db = state.open_db_for(&id);
                state.connect_server(id.clone(), db, cx);
            });
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

        let message = t!("servers.remove_prompt", server = server, locale = locale).to_string();

        ZedisDialog::new_alert(
            i18n_servers(cx, "remove_server_title"),
            escalate_dangerous_body(cx, &server_id, message),
        )
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
        let host_invalid_msg = i18n_common(cx, "host_invalid");
        let validate_host = move |s: &str| {
            if s.len() <= 1024 && s.is_ascii() {
                return None;
            }
            Some(host_invalid_msg.clone())
        };
        let default_db_invalid_msg = i18n_servers(cx, "default_db_invalid");
        // Empty unpins; anything else must be a u16. The upper bound can't be
        // checked here — the server's real DB count is only known once
        // connected — so an in-range-but-nonexistent DB is left to Redis to
        // reject at SELECT.
        let validate_default_db = move |s: &str| {
            let s = s.trim();
            if s.is_empty() || s.parse::<u16>().is_ok() {
                return None;
            }
            Some(default_db_invalid_msg.clone())
        };

        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        // Separate entity so suffix builder and foot_actions can safely read it
        // during ZedisForm render without a re-entrant borrow.
        let candidates: gpui::Entity<Vec<gpui::SharedString>> = cx.new(|_| Vec::new());
        let candidates_for_suffix = candidates.clone();
        let fetch_locale = locale.clone();

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
                .tab_index(0)
                .suffix({
                    let candidates = candidates_for_suffix.clone();
                    let locale = fetch_locale.clone();
                    move |_window, cx: &mut gpui::Context<zedis_ui::ZedisForm>| {
                        let candidates = candidates.clone();
                        let locale = locale.clone();
                        Button::new("fetch-master-names")
                            .ghost()
                            .icon(Icon::new(IconName::Search))
                            .xsmall()
                            .on_click(cx.listener(move |form, _, _, cx| {
                                let host = form.get_field_value("host", cx).to_string();
                                let port: u16 = form.get_field_value("port", cx).parse().unwrap_or(26379);
                                let pw = form.get_field_value("password", cx).to_string();
                                let password = if pw.is_empty() { None } else { Some(pw) };
                                let uname = form.get_field_value("username", cx).to_string();
                                let username = if uname.is_empty() { None } else { Some(uname) };
                                let server = RedisServer {
                                    host,
                                    port,
                                    password,
                                    username,
                                    ..Default::default()
                                };
                                let locale = locale.clone();
                                let candidates = candidates.clone();
                                cx.spawn(async move |form_entity, cx| {
                                    let result: Result<Vec<String>, Error> = async {
                                        let mut conn = match open_single_connection(&server, 0, false).await {
                                            Ok(c) => c,
                                            Err(e) => {
                                                if !e.to_string().contains("AuthenticationFailed") {
                                                    return Err(e.into());
                                                }
                                                let mut tmp = server.clone();
                                                tmp.password = None;
                                                open_single_connection(&tmp, 0, false).await?
                                            }
                                        };
                                        let masters: Vec<std::collections::HashMap<String, String>> =
                                            cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
                                        Ok(masters.into_iter().filter_map(|m| m.get("name").cloned()).collect())
                                    }
                                    .await;
                                    let _ = form_entity.update(cx, |form, cx| match result {
                                        Ok(names) if names.len() == 1 => {
                                            form.schedule_field_update("master_name".into(), names[0].clone().into());
                                            candidates.update(cx, |v, _| v.clear());
                                            cx.notify();
                                        }
                                        Ok(names) if names.len() > 1 => {
                                            candidates.update(cx, |v, _| {
                                                *v = names.into_iter().map(Into::into).collect();
                                            });
                                            cx.notify();
                                        }
                                        Ok(_) => {
                                            let msg = t!("servers.fetch_master_names_empty", locale = &locale);
                                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                                store.update(cx, |_state, cx| {
                                                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                                                        msg.into(),
                                                    )));
                                                });
                                            });
                                        }
                                        Err(e) => {
                                            let msg = t!(
                                                "servers.test_connection_failed",
                                                error = e.to_string(),
                                                locale = &locale
                                            );
                                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                                store.update(cx, |_state, cx| {
                                                    cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                                                        msg.into(),
                                                    )));
                                                });
                                            });
                                        }
                                    });
                                })
                                .detach();
                            }))
                    }
                }),
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
            ZedisFormField::new("ssh_key_passphrase", i18n_servers(cx, "ssh_key_passphrase"))
                .default_value(redis_server.ssh_key_passphrase.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_key_passphrase_placeholder"))
                .mask()
                // Meaningless without a key: appears as soon as the key field
                // is filled, and is dropped from submission when it is not.
                .visible_when_filled("ssh_key")
                .tab_index(2),
            // tab advanced
            ZedisFormField::new("server_type", i18n_servers(cx, "server_type"))
                .default_value(redis_server.server_type.unwrap_or(0).to_string())
                .options(
                    server_type_list
                        .split(" ")
                        .map(|s| s.to_string().into())
                        .collect::<Vec<SharedString>>(),
                )
                .tab_index(3)
                .field_type(ZedisFormFieldType::RadioGroup),
            ZedisFormField::new("databases", i18n_servers(cx, "databases"))
                .default_value(redis_server.databases.map(|n| n.to_string()).unwrap_or_default())
                .placeholder(i18n_servers(cx, "databases_placeholder"))
                .tab_index(3),
            ZedisFormField::new("default_db", i18n_servers(cx, "default_db"))
                .default_value(redis_server.default_db.map(|n| n.to_string()).unwrap_or_default())
                .placeholder(i18n_servers(cx, "default_db_placeholder"))
                .validate(validate_default_db)
                .tab_index(3),
            ZedisFormField::new("connection_timeout", i18n_servers(cx, "connection_timeout"))
                .default_value(
                    redis_server
                        .connection_timeout
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                )
                .placeholder(i18n_servers(cx, "connection_timeout_placeholder"))
                .tab_index(3),
            ZedisFormField::new("response_timeout", i18n_servers(cx, "response_timeout"))
                .default_value(redis_server.response_timeout.map(|n| n.to_string()).unwrap_or_default())
                .placeholder(i18n_servers(cx, "response_timeout_placeholder"))
                .tab_index(3),
            ZedisFormField::new("readonly", i18n_servers(cx, "readonly"))
                .default_value(redis_server.readonly.unwrap_or(false).to_string())
                .placeholder(i18n_servers(cx, "readonly_check_label"))
                .tab_index(3)
                .field_type(ZedisFormFieldType::Checkbox),
            ZedisFormField::new("require_confirm_writes", i18n_servers(cx, "require_confirm_writes"))
                .default_value(redis_server.require_confirm_writes.unwrap_or(false).to_string())
                .placeholder(i18n_servers(cx, "require_confirm_writes_check_label"))
                .tab_index(3)
                .field_type(ZedisFormFieldType::Checkbox),
            ZedisFormField::new("group", i18n_servers(cx, "group"))
                .default_value(redis_server.group.clone().unwrap_or_default())
                .placeholder({
                    // Show the list of existing groups as a placeholder hint
                    // so users naturally reuse labels instead of creating
                    // near-duplicates ("Team A" vs "team a"). Falls back
                    // to a static prompt when no groups exist yet.
                    let existing = get_server_groups();
                    if existing.is_empty() {
                        i18n_servers(cx, "group_placeholder")
                    } else {
                        format!("{}: {}", i18n_servers(cx, "group_existing_hint"), existing.join(" / ")).into()
                    }
                })
                .tab_index(3),
            // Single "Environment" preset (None/Local/Dev/UAT/Prod/Archive)
            // drives the display tag, chip color, and high-risk (PROD)
            // escalation. The option index maps straight onto
            // TAG_COLOR_PRESETS, so the stored `tag_color` key stays the
            // source of truth and `from_form_data` derives the label from it —
            // replacing the old free-text tag + separate color picker.
            ZedisFormField::new("tag_color", i18n_servers(cx, "tag"))
                .default_value(tag_color_index(redis_server.tag_color.as_deref()).to_string())
                .options(
                    TAG_ENV_LABELS
                        .iter()
                        .map(|s| SharedString::from(*s))
                        .collect::<Vec<SharedString>>(),
                )
                .tab_index(3)
                .field_type(ZedisFormFieldType::RadioGroup),
            // —— Keys tab: key-tree / SCAN behaviour (per-server overrides) ——
            ZedisFormField::new("key_separator", i18n_servers(cx, "key_separator"))
                .default_value(redis_server.key_separator.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "key_separator_placeholder"))
                .tab_index(4),
            ZedisFormField::new("key_scan_count", i18n_servers(cx, "key_scan_count"))
                .default_value(redis_server.key_scan_count.map(|n| n.to_string()).unwrap_or_default())
                .placeholder(i18n_servers(cx, "key_scan_count_placeholder"))
                .tab_index(4),
            ZedisFormField::new("max_key_tree_depth", i18n_servers(cx, "max_key_tree_depth"))
                .default_value(
                    redis_server
                        .max_key_tree_depth
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                )
                .placeholder(i18n_servers(cx, "max_key_tree_depth_placeholder"))
                .tab_index(4),
            ZedisFormField::new("auto_expand_threshold", i18n_servers(cx, "auto_expand_threshold"))
                .default_value(
                    redis_server
                        .auto_expand_threshold
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                )
                .placeholder(i18n_servers(cx, "auto_expand_threshold_placeholder"))
                .tab_index(4),
            ZedisFormField::new("show_key_tree_ttl", i18n_servers(cx, "show_key_tree_ttl"))
                .default_value(redis_server.show_key_tree_ttl_form_index().to_string())
                .options(vec![
                    i18n_servers(cx, "show_key_tree_ttl_default"),
                    i18n_servers(cx, "show_key_tree_ttl_show"),
                    i18n_servers(cx, "show_key_tree_ttl_hide"),
                ])
                .tab_index(4)
                .field_type(ZedisFormFieldType::RadioGroup),
        ];
        let title = if is_new {
            i18n_servers(cx, "add_server_title")
        } else {
            i18n_servers(cx, "update_server_title")
        };
        let max_h = (window.bounds().size.height - px(300.0)).min(px(600.0));

        let test_label = i18n_servers(cx, "test_connection");
        let diagnose_label = i18n_servers(cx, "diagnose_connection");

        ZedisFormOptions::new(fields)
            .title(title)
            .tabs(vec![
                i18n_servers(cx, "tab_general"),
                i18n_servers(cx, "tab_tls"),
                i18n_servers(cx, "tab_ssh"),
                i18n_servers(cx, "tab_advanced"),
                i18n_servers(cx, "tab_keys"),
            ])
            .confirm_label(i18n_common(cx, "confirm"))
            .cancel_label(i18n_common(cx, "cancel"))
            .dialog_max_height(max_h)
            .foot_actions(move |_window, cx: &mut Context<zedis_ui::ZedisForm>| {
                let locale = locale.clone();
                let test_label = test_label.clone();
                let diagnose_label = diagnose_label.clone();

                // Candidate master names populated by the suffix fetch button.
                let current_candidates = candidates.read(cx).clone();
                let candidates_for_foot = candidates.clone();

                let mut items: Vec<gpui::AnyElement> = vec![];
                for name in &current_candidates {
                    let n = name.clone();
                    let c = candidates_for_foot.clone();
                    items.push(
                        Button::new(format!("mc-{n}"))
                            .xsmall()
                            .ghost()
                            .label(n.clone())
                            .on_click(cx.listener(move |form, _, _, cx| {
                                form.schedule_field_update("master_name".into(), n.clone());
                                c.update(cx, |v, _| v.clear());
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
                items.push(
                    Button::new("test-connection")
                        .label(test_label)
                        .on_click(cx.listener(move |form, _, _window, cx| {
                            if form.is_processing {
                                return;
                            }
                            let Some(values) = form.try_get_values(cx) else {
                                return;
                            };
                            let values: indexmap::IndexMap<String, String> =
                                values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                            let server = RedisServer::from_form_data("", &values);
                            let locale = locale.clone();
                            form.is_processing = true;
                            cx.notify();
                            cx.spawn(async move |handle, cx| {
                                let result = async {
                                    let mut conn = match open_single_connection(&server, 0, false).await {
                                        Ok(conn) => conn,
                                        Err(e) => {
                                            if !e.to_string().contains("AuthenticationFailed") {
                                                return Err(e.into());
                                            }
                                            // sentinel nodes typically don't require auth
                                            let mut tmp = server.clone();
                                            tmp.password = None;
                                            open_single_connection(&tmp, 0, false).await?
                                        }
                                    };
                                    if server.server_type == Some(SERVER_TYPE_SENTINEL) {
                                        // sentinel: verify by connecting to the actual master
                                        let masters: Vec<std::collections::HashMap<String, String>> =
                                            cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
                                        let master = masters.into_iter().next().ok_or_else(|| Error::Invalid {
                                            message: "no master found in sentinel".to_string(),
                                        })?;
                                        let ip = master.get("ip").ok_or_else(|| Error::Invalid {
                                            message: "master ip not found".to_string(),
                                        })?;
                                        let port: u16 = master
                                            .get("port")
                                            .ok_or_else(|| Error::Invalid {
                                                message: "master port not found".to_string(),
                                            })?
                                            .parse()
                                            .map_err(|e| Error::Invalid {
                                                message: format!("invalid master port: {e}"),
                                            })?;
                                        let mut master_server = server.clone();
                                        master_server.host = ip.clone();
                                        master_server.port = port;
                                        let mut master_conn = open_single_connection(&master_server, 0, false).await?;
                                        let _: () = cmd("PING").query_async(&mut master_conn).await?;
                                    } else {
                                        let _: () = cmd("PING").query_async(&mut conn).await?;
                                    }
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
                        }))
                        .into_any_element(),
                );
                items.push(
                    Button::new("diagnose-connection")
                        .label(diagnose_label)
                        .on_click(cx.listener(move |form, _, window, cx| {
                            let Some(values) = form.try_get_values(cx) else {
                                return;
                            };
                            let values: indexmap::IndexMap<String, String> =
                                values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                            let server = RedisServer::from_form_data("", &values);
                            open_connection_diagnostics(server, window, cx);
                        }))
                        .into_any_element(),
                );
                items
            })
            .on_dialog_submit(move |values, _window, cx| {
                let values: indexmap::IndexMap<String, String> =
                    values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
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

    /// Show the JSON export for a single server config. Defaults to
    /// "stripped" mode where credential fields (passwords, SSH key,
    /// TLS materials) are blanked — that's the safe state for
    /// pasting into chat / wiki / git. A toggle reveals the
    /// with-secrets variant for users doing personal backups.
    fn export_server_dialog(&mut self, server: &RedisServer, window: &mut Window, cx: &mut Context<Self>) {
        let include_secrets = Rc::new(Cell::new(false));
        let initial_json = server.to_export_json(false).unwrap_or_default();
        let json_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(6, 16)
                .default_value(initial_json)
        });
        let server_clone = server.clone();

        // Strings captured into the dialog body closure — i18n calls
        // can't happen inside (no cx parameter there).
        let hint = i18n_servers(cx, "export_hint");
        let include_label = i18n_servers(cx, "export_include_secrets");
        let warning_label = i18n_servers(cx, "export_secrets_warning");
        let warning_color = cx.theme().yellow;
        let copied_label = i18n_servers(cx, "export_copied");
        let copy_label = i18n_servers(cx, "export_copy_clipboard");
        let save_success = i18n_common(cx, "json_exported");
        let save_error = i18n_common(cx, "json_export_failed");
        let suggested_name = export_filename(&server.name);

        let body_json = json_state.clone();
        let body_flag = include_secrets.clone();
        let body_server = server_clone.clone();
        let submit_json = json_state.clone();

        ZedisDialog::new(i18n_servers(cx, "export_title"))
            .w(px(620.))
            .ok_text(i18n_servers(cx, "export_save_file"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_servers(cx, "export_save_file"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                let include_on = body_flag.get();
                let json_input = body_json.clone();
                let server = body_server.clone();
                let flag = body_flag.clone();

                let mut toggle_btn = Button::new("export-toggle-secrets")
                    .small()
                    .label(include_label.clone());
                toggle_btn = if include_on {
                    toggle_btn.primary()
                } else {
                    toggle_btn.outline()
                };
                let toggle_btn = toggle_btn.on_click(move |_, window, cx| {
                    let new_state = !flag.get();
                    flag.set(new_state);
                    let new_json = server.to_export_json(new_state).unwrap_or_default();
                    json_input.update(cx, |state, cx| {
                        state.set_value(SharedString::from(new_json), window, cx);
                    });
                });

                // "Copy to clipboard" alongside the secrets toggle — Save to
                // file is now the dialog's primary OK action.
                let copy_json = body_json.clone();
                let copied = copied_label.clone();
                let copy_btn = Button::new("export-copy-clipboard")
                    .small()
                    .outline()
                    .label(copy_label.clone())
                    .on_click(move |_, window, cx| {
                        let value = copy_json.read(cx).value().to_string();
                        cx.write_to_clipboard(ClipboardItem::new_string(value));
                        window.push_notification(Notification::success(copied.clone()), cx);
                    });

                gpui_component::v_flex()
                    .gap_3()
                    .w_full()
                    .child(Label::new(hint.clone()).text_xs())
                    .child(h_flex().gap_2().child(toggle_btn).child(copy_btn))
                    .when(include_on, |this| {
                        this.child(Label::new(warning_label.clone()).text_xs().text_color(warning_color))
                    })
                    .child(Textarea::new(&body_json).appearance(true))
            })
            .on_ok(move |_, _window, cx| {
                // Save the displayed JSON to a file (default ~/Downloads,
                // timestamped). Copy to clipboard is the secondary body action.
                let value = submit_json.read(cx).value().to_string();
                export_to_file_global(
                    cx,
                    value.into_bytes(),
                    &suggested_name,
                    save_success.clone(),
                    save_error.clone(),
                );
                true
            })
            .open(window, cx);
    }

    /// Show the multi-server export picker: tick which connections to export
    /// and whether credentials are included, then copy a JSON **array** to the
    /// clipboard (round-trippable through the import dialog). The per-card
    /// export action covers the single-server case.
    fn export_servers_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| ZedisExportServersDialog::new(window, cx));
        let view_ok = view.clone();
        let view_child = view.clone();
        let select_none_label = i18n_servers(cx, "export_select_none");
        let save_success = i18n_common(cx, "json_exported");
        let save_error = i18n_common(cx, "json_export_failed");

        ZedisDialog::new(i18n_servers(cx, "export_servers_title"))
            .w(px(560.))
            .ok_text(i18n_servers(cx, "export_save_file"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_servers(cx, "export_save_file"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, window, cx| {
                // Plain JSON — or an encrypted share token when a passphrase
                // was set in the body. `None` ⇒ nothing ticked; keep the
                // dialog open until at least one is.
                let Some(payload) = view_ok.read(cx).export_payload(cx) else {
                    window.push_notification(Notification::warning(select_none_label.clone()), cx);
                    return false;
                };
                // Save to a file (default ~/Downloads, timestamped). Copy to
                // clipboard is the body action.
                let name = export_filename("servers");
                export_to_file_global(
                    cx,
                    payload.into_bytes(),
                    &name,
                    save_success.clone(),
                    save_error.clone(),
                );
                true
            })
            .open(window, cx);
    }

    /// Show the import-from-JSON dialog. Paste any JSON produced by
    /// `to_export_json` (or hand-edited equivalent); on submit a new
    /// server entry is created with a freshly-allocated UUID — never
    /// overwrites an existing config.
    fn import_server_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let json_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(6, 16)
                .placeholder(i18n_servers(cx, "import_placeholder"))
        });
        // Live: the moment the input becomes a path to an existing file, read
        // its contents back into the box so the user sees (and can review) the
        // real config before importing. Writing the multi-line content back
        // never re-triggers the path check, so there's no loop.
        self.import_input_sub = Some(cx.subscribe_in(&json_state, window, |_this, state, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = state.read(cx).value().to_string();
            match resolve_import_input(&value) {
                // A file was read — its contents differ from the pasted path.
                Ok(content) if content != value => {
                    state.update(cx, |s, cx| s.set_value(SharedString::from(content), window, cx));
                }
                Ok(_) => {}
                Err(e) => {
                    window.push_notification(
                        Notification::error(SharedString::from(format!(
                            "{}: {}",
                            i18n_servers(cx, "import_error_prefix"),
                            import_file_error_message(cx, &e)
                        ))),
                        cx,
                    );
                }
            }
        }));
        let pass_state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(i18n_servers(cx, "import_passphrase_placeholder"))
        });
        // The body is its own view so it re-renders on input changes: the
        // passphrase row reveals itself only when the pasted content is an
        // encrypted share token (plain JSON imports look exactly as before).
        let body_view = cx.new(|cx| ImportServersBody::new(json_state.clone(), pass_state.clone(), cx));
        let bad_json_label = i18n_servers(cx, "import_error_prefix");
        let submit_json = json_state.clone();
        let submit_pass = pass_state;

        ZedisDialog::new(i18n_servers(cx, "import_title"))
            .w(px(620.))
            .ok_text(i18n_servers(cx, "import_submit"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_servers(cx, "import_submit"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || body_view.clone())
            .on_ok(move |_, window, cx| {
                let raw = submit_json.read(cx).value().to_string();
                // If the pasted text is a path to an existing file, read it
                // (size-capped); otherwise use it verbatim as JSON / URI.
                let value = match resolve_import_input(&raw) {
                    Ok(content) => content,
                    Err(e) => {
                        window.push_notification(
                            Notification::error(SharedString::from(format!(
                                "{bad_json_label}: {}",
                                import_file_error_message(cx, &e)
                            ))),
                            cx,
                        );
                        return false;
                    }
                };
                // Encrypted share token → decrypt with the body's passphrase
                // before handing off to the unchanged JSON import path.
                let value = if is_share_token(&value) {
                    match decrypt_share(&value, submit_pass.read(cx).value().as_ref()) {
                        Ok(json) => json,
                        Err(_) => {
                            window.push_notification(Notification::error(i18n_servers(cx, "share_decrypt_failed")), cx);
                            return false;
                        }
                    }
                } else {
                    value
                };
                match RedisServer::from_import_multi(&value) {
                    Ok(servers) => {
                        let count = servers.len();
                        // One atomic batch — looping upsert_server races and
                        // would drop all but one entry (each is a detached
                        // read-modify-save of the whole list).
                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                            store.update(cx, |state, cx| state.upsert_servers(servers, cx));
                        });
                        // A Redis Insight export can carry several databases —
                        // confirm the count so the user knows they all landed.
                        if count > 1 {
                            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                            window.push_notification(
                                Notification::info(SharedString::from(
                                    t!("servers.import_multi_done", count = count, locale = locale).to_string(),
                                )),
                                cx,
                            );
                        }
                        true
                    }
                    Err(e) => {
                        // Surface the parse error as a localized notification so
                        // the user can fix the input; keep the dialog open by
                        // returning false.
                        let detail = import_error_message(cx, &e);
                        window.push_notification(
                            Notification::error(SharedString::from(format!("{bad_json_label}: {detail}"))),
                            cx,
                        );
                        false
                    }
                }
            })
            .open(window, cx);
    }

    /// Welcoming empty state for the Home view when no servers are configured.
    ///
    /// The first thing a brand-new user saw was otherwise a near-blank page
    /// (only the floating Add/Import action cards). A centered hero — muted
    /// icon, one-line orientation, a primary "add connection" CTA and an
    /// import shortcut — gives an obvious next step and surfaces the Redis
    /// Insight migration path right where it matters. `min_h` keeps it
    /// vertically centered even though the parent scroll viewport sizes to
    /// its content.
    fn render_empty(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let add_label = i18n_servers(cx, "empty_add");
        let import_label = i18n_servers(cx, "empty_import");
        // Stay a touch shorter than the viewport so centering doesn't add a
        // scrollbar (the parent scroll box wraps us in its own padding).
        let min_h = (window.viewport_size().height - px(96.)).max(px(380.));

        v_flex()
            .w_full()
            .min_h(min_h)
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                Icon::new(CustomIconName::DatabaseZap)
                    .with_size(px(56.))
                    .text_color(muted.alpha(0.5)),
            )
            .child(
                Label::new(i18n_servers(cx, "empty_title"))
                    .text_lg()
                    .font_medium()
                    .text_color(cx.theme().foreground),
            )
            .child(Label::new(i18n_servers(cx, "empty_hint")).text_sm().text_color(muted))
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("servers-empty-add")
                            .primary()
                            .icon(IconName::Plus)
                            .label(add_label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_or_update_server_dialog(
                                    &RedisServer {
                                        port: DEFAULT_REDIS_PORT,
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("servers-empty-import")
                            .outline()
                            .icon(IconName::Asterisk)
                            .label(import_label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.import_server_dialog(window, cx);
                            })),
                    ),
            )
    }
}

/// Body of the paste-to-import dialog, hoisted into its own view so it can
/// re-render on input changes: the passphrase row reveals itself only when
/// the pasted content is an encrypted share token; plain-JSON imports render
/// exactly as before.
struct ImportServersBody {
    json_state: Entity<TextareaState>,
    pass_state: Entity<InputState>,
    /// One-shot flag: focus the paste box on the first render only, so the
    /// user can paste right away without clicking into the input.
    should_focus: bool,
    _subscriptions: Vec<Subscription>,
}

impl ImportServersBody {
    fn new(json_state: Entity<TextareaState>, pass_state: Entity<InputState>, cx: &mut Context<Self>) -> Self {
        // Re-render whenever the pasted content changes so the passphrase row
        // appears the moment a share token lands in the box.
        let sub = cx.subscribe(&json_state, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            json_state,
            pass_state,
            should_focus: true,
            _subscriptions: vec![sub],
        }
    }
}

impl Render for ImportServersBody {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if take(&mut self.should_focus) {
            self.json_state.update(cx, |state, cx| state.focus(window, cx));
        }
        let needs_pass = is_share_token(self.json_state.read(cx).value().as_ref());
        let hint_color = cx.theme().yellow;
        let drop_state = self.json_state.clone();
        gpui_component::v_flex()
            .gap_2()
            .w_full()
            // Drop a file onto the dialog to load it — dropping carries
            // the full path (unlike a copy-paste, which is often just
            // the filename). Reuses the extension-allowlist / size-cap read.
            .on_drop::<ExternalPaths>(move |dropped, window, cx| {
                let Some(path) = dropped.paths().first() else {
                    return;
                };
                let path_str = path.to_string_lossy().to_string();
                match resolve_import_input(&path_str) {
                    // A .json file was read — its contents differ from the path.
                    Ok(content) if content != path_str => {
                        drop_state.update(cx, |s, cx| s.set_value(SharedString::from(content), window, cx));
                    }
                    // Dropped a file with an unsupported extension.
                    Ok(_) => {
                        window
                            .push_notification(Notification::warning(i18n_servers(cx, "import_drop_unsupported")), cx);
                    }
                    Err(e) => {
                        window.push_notification(
                            Notification::error(SharedString::from(format!(
                                "{}: {}",
                                i18n_servers(cx, "import_error_prefix"),
                                import_file_error_message(cx, &e)
                            ))),
                            cx,
                        );
                    }
                }
            })
            .child(Label::new(i18n_servers(cx, "import_hint")).text_xs())
            .child(Textarea::new(&self.json_state).appearance(true))
            .when(needs_pass, |this| {
                this.child(
                    Label::new(i18n_servers(cx, "import_passphrase_hint"))
                        .text_xs()
                        .text_color(hint_color),
                )
                .child(Input::new(&self.pass_state).appearance(true))
            })
    }
}

/// Largest file the paste-a-path import shortcut will read. Connection exports
/// are KB-sized, so this is a generous ceiling that simply guards against
/// pasting a path to some huge unrelated file.
const MAX_IMPORT_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// A failure resolving a pasted file path (distinct from a parse error).
#[derive(Debug)]
enum FileImportError {
    /// The file exists but exceeds [`MAX_IMPORT_FILE_BYTES`].
    TooLarge,
    /// The file exists but couldn't be read (detail = io error).
    ReadFailed(String),
}

/// If `raw` (trimmed) is a single-line path to an existing `.json` file within
/// the size cap, read and return its contents; otherwise return `raw` unchanged
/// so it is parsed as literal JSON / URI. Multi-line, empty, or non-`.json`
/// input never touches the filesystem, so normal pasted JSON costs nothing.
/// File extensions the import dialog will read from a dropped file or a
/// pasted path — one per supported client export.
const IMPORT_FILE_EXTENSIONS: [&str; 4] = ["json", "ano", "yaml", "yml"];

fn resolve_import_input(raw: &str) -> Result<String, FileImportError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return Ok(raw.to_string());
    }
    // Only a path with a known export extension triggers a read: `.json`
    // (Zedis / Redis Insight), `.ano` (Another Redis Desktop Manager) and
    // `.yaml` / `.yml` (Tiny RDM's connections.yaml). Anything else —
    // URIs, hand-typed JSON, an unrelated existing file — stays literal.
    let is_import_path = std::path::Path::new(trimmed).extension().is_some_and(|ext| {
        IMPORT_FILE_EXTENSIONS
            .iter()
            .any(|allowed| ext.eq_ignore_ascii_case(allowed))
    });
    if !is_import_path {
        return Ok(raw.to_string());
    }
    // Reuse the shared resolver: expands `~`/`~/` via get_home_dir and
    // absolutizes, the same way SSH key / proto paths are handled.
    let path = resolve_path(trimmed);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {
            if meta.len() > MAX_IMPORT_FILE_BYTES {
                return Err(FileImportError::TooLarge);
            }
            std::fs::read_to_string(&path).map_err(|e| FileImportError::ReadFailed(e.to_string()))
        }
        // Not an existing file (missing, or a directory) — treat as literal text.
        _ => Ok(raw.to_string()),
    }
}

/// Localize a [`FileImportError`] for the paste-to-import dialog.
fn import_file_error_message(cx: &gpui::App, err: &FileImportError) -> SharedString {
    match err {
        FileImportError::TooLarge => format!(
            "{} (≤ {} MiB)",
            i18n_servers(cx, "import_file_too_large"),
            MAX_IMPORT_FILE_BYTES / 1024 / 1024
        )
        .into(),
        FileImportError::ReadFailed(e) => format!("{}: {e}", i18n_servers(cx, "import_file_read_failed")).into(),
    }
}

/// Localize an [`ImportError`] for the paste-to-import dialog. Detail-carrying
/// variants append the parser's own message, which can't be meaningfully
/// translated.
fn import_error_message(cx: &gpui::App, err: &ImportError) -> SharedString {
    match err {
        ImportError::InvalidJson(d) => format!("{}: {d}", i18n_servers(cx, "import_err_invalid_json")).into(),
        ImportError::InvalidUri(d) => format!("{}: {d}", i18n_servers(cx, "import_err_invalid_uri")).into(),
        ImportError::UnsupportedScheme(s) => {
            format!("{} {s}", i18n_servers(cx, "import_err_unsupported_scheme")).into()
        }
        ImportError::MissingName => i18n_servers(cx, "import_err_missing_name"),
        ImportError::MissingHost => i18n_servers(cx, "import_err_missing_host"),
        ImportError::InvalidPort => i18n_servers(cx, "import_err_invalid_port"),
        ImportError::EmptyRedisInsight => i18n_servers(cx, "import_err_empty_insight"),
    }
}

/// Format a stored `updated_at` (`Local::now().to_string()`, so it begins with
/// `YYYY-MM-DD`) as a localized relative time for the card footer — "Today" /
/// "3d ago" / "2mo ago" / "1y ago". Falls back to the raw string if the date
/// can't be parsed. Date-only granularity (no time-of-day) keeps it robust to
/// the `T`-vs-space separator and avoids timezone parsing.
fn format_updated_relative(updated_at: &str, locale: &str) -> String {
    use chrono::{Local, NaiveDate};
    let Some(date_str) = updated_at.get(..10) else {
        return updated_at.to_string();
    };
    let Ok(then) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
        return updated_at.to_string();
    };
    let days = (Local::now().date_naive() - then).num_days().max(0);
    if days == 0 {
        t!("servers.relative_today", locale = locale).to_string()
    } else if days < 30 {
        t!("servers.relative_days", count = days as usize, locale = locale).to_string()
    } else if days < 365 {
        t!("servers.relative_months", count = (days / 30) as usize, locale = locale).to_string()
    } else {
        t!("servers.relative_years", count = (days / 365) as usize, locale = locale).to_string()
    }
}

impl Focusable for ZedisServers {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
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

        // First-run / empty Home: before any connection is configured, show a
        // centered welcome hero (primary "add" CTA + import shortcut) instead
        // of the near-blank page the floating Add/Import cards left behind.
        let all_servers = get_servers().unwrap_or_default();
        if all_servers.is_empty() {
            return self.render_empty(window, cx).into_any_element();
        }

        // Responsive grid columns based on viewport width
        let cols = match width {
            width if width < px(VIEWPORT_BREAKPOINT_SMALL) => 1,
            width if width < px(VIEWPORT_BREAKPOINT_MEDIUM) => 2,
            _ => 3,
        };

        // Shared card surface (same helper the config-editor cards use).
        let bg = card_background(cx);

        let dark = cx.theme().is_dark();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let subtitle_font = get_mono_font_family();
        let update_tooltip = i18n_servers(cx, "update_tooltip");
        let move_up_tooltip = i18n_servers(cx, "move_up_tooltip");
        let move_down_tooltip = i18n_servers(cx, "move_down_tooltip");
        let ungrouped_label = i18n_servers(cx, "ungrouped_label");

        // Partition servers into ordered (group_label, servers) buckets.
        // `get_servers()` already returns them in canonical sort order
        // (group A→Z, then sort_order ASC, ungrouped last). We just
        // need to find the boundaries.
        // Toolbar search filter — match name / host / description (case
        // insensitive). Empty groups simply don't get built, so they vanish
        // while filtering.
        let query = self.search_state.read(cx).value().trim().to_lowercase();
        let mut groups: Vec<(Option<String>, Vec<RedisServer>)> = Vec::new();
        for server in &all_servers {
            if !matches_query(server, &query) {
                continue;
            }
            let g = server
                .group
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            match groups.last_mut() {
                Some((existing, list)) if *existing == g => list.push(server.clone()),
                _ => groups.push((g, vec![server.clone()])),
            }
        }

        // Build one section per group: header + grid of cards. Each
        // card gets ↑/↓ reorder buttons gated by group-edge position.
        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        let mut card_index_counter: usize = 0;
        for (group_label, group_servers) in &groups {
            let group_name = match group_label {
                Some(g) => SharedString::from(g.clone()),
                None => ungrouped_label.clone(),
            };
            let group_count = SharedString::from(group_servers.len().to_string());
            let group_key = group_label.as_deref().unwrap_or("__none__").to_string();
            let is_collapsed = cx
                .global::<ZedisGlobalStore>()
                .read(cx)
                .is_server_group_collapsed(&group_key);
            let toggle_key = group_key.clone();
            let section_id = SharedString::from(format!("servers-group-{group_key}"));
            // Skip building the (listener-heavy) cards entirely when
            // the group is collapsed — but still advance the index
            // counter so element ids stay stable across toggles.
            let cards: Vec<gpui::AnyElement> = if is_collapsed {
                Vec::new()
            } else {
                group_servers
                    .iter()
                    .enumerate()
                    .map(|(in_group_index, server)| {
                        let index = card_index_counter + in_group_index;
                        let is_first = in_group_index == 0;
                        let is_last = in_group_index + 1 == group_servers.len();
                        let single_in_group = group_servers.len() == 1;

                        let select_server_id = server.id.clone();
                        let update_server = server.clone();
                        // Server id carried by the footer "⋯" dropdown actions.
                        let more_server_id: SharedString = server.id.clone().into();
                        let move_up_id = server.id.clone();
                        let move_down_id = server.id.clone();

                        let description = server.description.as_deref().unwrap_or_default();
                        let updated_at = if let Some(updated_at) = &server.updated_at {
                            updated_at.substring(0, UPDATED_AT_SUBSTRING_LENGTH).to_string()
                        } else {
                            String::new()
                        };
                        // Footer shows a localized relative time ("28d ago"); the
                        // absolute date stays in the hover tooltip.
                        let updated_relative = server
                            .updated_at
                            .as_deref()
                            .map(|s| format_updated_relative(s, &locale))
                            .unwrap_or_default();
                        let title = server.name.clone();
                        let tag_label = server.tag_label().unwrap_or_default().to_string();
                        let tag_chip = resolve_tag_chip(server.tag_color.as_deref(), dark);
                        let subtitle = format!("{}:{}", server.host, server.port);
                        let updated_label = if updated_at.is_empty() {
                            String::new()
                        } else {
                            t!("servers.updated_at_label", date = updated_at, locale = locale).to_string()
                        };

                        // Reorder ↑/↓ stay top-right, hover-only (design); skip
                        // entirely when the group has a single member.
                        let mut hover_actions: Vec<Button> = Vec::new();
                        if !single_in_group {
                            hover_actions.push(
                                Button::new(("servers-card-action-up", index))
                                    .ghost()
                                    .tooltip(move_up_tooltip.clone())
                                    .icon(IconName::ChevronUp)
                                    .disabled(is_first)
                                    .on_click(cx.listener(move |_this, _, _window, cx| {
                                        cx.stop_propagation();
                                        let id = move_up_id.clone();
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| {
                                                state.reorder_server(&id, ReorderDirection::Up, cx);
                                            });
                                        });
                                    })),
                            );
                            hover_actions.push(
                                Button::new(("servers-card-action-down", index))
                                    .ghost()
                                    .tooltip(move_down_tooltip.clone())
                                    .icon(IconName::ChevronDown)
                                    .disabled(is_last)
                                    .on_click(cx.listener(move |_this, _, _window, cx| {
                                        cx.stop_propagation();
                                        let id = move_down_id.clone();
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| {
                                                state.reorder_server(&id, ReorderDirection::Down, cx);
                                            });
                                        });
                                    })),
                            );
                        }

                        // Footer (design): relative update time on the left;
                        // edit + a "⋯" dropdown (export / delete) on the right.
                        // Built here so the ZedisCard call just drops it into the
                        // footer slot.
                        let footer = {
                            let muted = cx.theme().muted_foreground;
                            let tip = updated_label.clone();
                            let has_time = !updated_at.is_empty();
                            let edit_server = update_server.clone();
                            let more_id = more_server_id.clone();
                            h_flex()
                                .id(("servers-card-footer", index))
                                .w_full()
                                .items_center()
                                .justify_between()
                                .child(
                                    h_flex()
                                        .id(("servers-card-updated", index))
                                        .items_center()
                                        .gap_1()
                                        .when(has_time, |this| {
                                            this.child(Icon::new(CustomIconName::Clock3).xsmall().text_color(muted))
                                                .child(Label::new(updated_relative.clone()).text_xs().text_color(muted))
                                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                        }),
                                )
                                .child(
                                    // Footer actions own their clicks so they
                                    // don't bubble to the card's connect-on-click.
                                    h_flex()
                                        .id(("servers-card-footer-actions", index))
                                        .items_center()
                                        .gap_1()
                                        .on_click(|_, _, cx| cx.stop_propagation())
                                        .child(
                                            Button::new(("servers-card-edit", index))
                                                .ghost()
                                                .xsmall()
                                                .tooltip(update_tooltip.clone())
                                                .icon(CustomIconName::FilePenLine)
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.add_or_update_server_dialog(&edit_server, window, cx);
                                                })),
                                        )
                                        .child(
                                            Button::new(("servers-card-more", index))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Ellipsis)
                                                .dropdown_menu_with_anchor(
                                                    Anchor::TopRight,
                                                    move |menu, _window, _cx| {
                                                        let exp_id = more_id.clone();
                                                        let del_id = more_id.clone();
                                                        menu.menu_element_with_icon(
                                                            Icon::new(IconName::ExternalLink),
                                                            Box::new(ServersCardAction::Export(exp_id)),
                                                            |_, cx| Label::new(i18n_servers(cx, "export_tooltip")),
                                                        )
                                                        .menu_element_with_icon(
                                                            Icon::new(CustomIconName::FileXCorner),
                                                            Box::new(ServersCardAction::Delete(del_id)),
                                                            |_, cx| Label::new(i18n_servers(cx, "remove_tooltip")),
                                                        )
                                                    },
                                                ),
                                        ),
                                )
                        };

                        let handle_select_server = cx.listener(move |_this, _, _, cx| {
                            let select_server_id = select_server_id.clone();
                            cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                store.update(cx, |state, cx| {
                                    let db = state.open_db_for(&select_server_id);
                                    state.connect_server(select_server_id.clone(), db, cx);
                                });
                            });
                        });

                        ZedisCard::new(("servers-card", index))
                            .icon(Icon::new(CustomIconName::Database))
                            .title(title)
                            .subtitle(subtitle)
                            .subtitle_font(subtitle_font.clone())
                            .tag(tag_label, tag_chip)
                            .bg(bg)
                            .when(!description.is_empty(), |this| {
                                this.description(description.to_string())
                            })
                            .when(!hover_actions.is_empty(), |this| this.hover_only_actions(hover_actions))
                            .footer(footer)
                            .on_click(Box::new(handle_select_server))
                            .into_any_element()
                    })
                    .collect()
            };
            card_index_counter += group_servers.len();
            let chevron = if is_collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            };
            sections.push(
                gpui_component::v_flex()
                    .id(section_id)
                    .gap_2()
                    .w_full()
                    .child(
                        // pl_2 matches the `.m_2()` left margin every
                        // ZedisCard applies, so the group label lines
                        // up with the cards' left edge. Whole header
                        // row toggles collapse on click.
                        h_flex()
                            .id(SharedString::from(format!("group-header-{group_key}")))
                            .items_center()
                            .gap_2()
                            .pt_2()
                            .px_2()
                            .cursor_pointer()
                            .child(Icon::new(chevron).text_color(cx.theme().muted_foreground))
                            .child(Label::new(group_name).text_sm().text_color(cx.theme().muted_foreground))
                            // Count as a theme-colored pill badge —
                            // same chip language as the sidebar tag
                            // chips, adapts to light/dark via theme
                            // tokens.
                            .child(
                                div().px_1p5().rounded_full().bg(cx.theme().muted).child(
                                    Label::new(group_count)
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                            )
                            // Divider line trailing the group label out to the
                            // right edge (design).
                            .child(div().flex_1().h_px().bg(cx.theme().border))
                            .on_click(cx.listener(move |_this, _, _window, cx| {
                                let key = toggle_key.clone();
                                update_app_state_and_save(cx, "toggle_server_group_collapsed", move |state, _| {
                                    state.toggle_server_group_collapsed(&key);
                                });
                            })),
                    )
                    .when(!is_collapsed, |this| {
                        this.child(div().grid().grid_cols(cols).gap_1().w_full().children(cards))
                    })
                    .into_any_element(),
            );
        }

        // Top toolbar (design): "Connections" + total-count badge on the left,
        // a live search box in the middle, and the New / Import / Export
        // actions on the right — the old bottom action cards moved up here.
        let muted_fg = cx.theme().muted_foreground;
        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .pt_1()
            .px_2()
            .pb_3()
            // Full-width divider under the toolbar (design).
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .child(
                        Label::new(i18n_servers(cx, "connections"))
                            .font_semibold()
                            .text_color(cx.theme().foreground),
                    )
                    .child(
                        div().px_1p5().rounded_full().bg(cx.theme().muted).child(
                            Label::new(SharedString::from(all_servers.len().to_string()))
                                .text_xs()
                                .text_color(muted_fg),
                        ),
                    ),
            )
            .child(
                Input::new(&self.search_state)
                    .w(px(200.))
                    .h(px(32.))
                    .prefix(Icon::new(IconName::Search).text_color(muted_fg)),
            )
            // Spacer pushes the action buttons to the right edge while the
            // search box keeps a fixed 200px width.
            .child(div().flex_1())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .child(
                        Button::new("servers-toolbar-add")
                            .primary()
                            .icon(IconName::Plus)
                            .label(i18n_servers(cx, "toolbar_new"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_or_update_server_dialog(
                                    &RedisServer {
                                        port: DEFAULT_REDIS_PORT,
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("servers-toolbar-import")
                            .outline()
                            .icon(IconName::Asterisk)
                            .label(i18n_servers(cx, "toolbar_import"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.import_server_dialog(window, cx);
                            })),
                    )
                    .child(
                        Button::new("servers-toolbar-export")
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label(i18n_servers(cx, "toolbar_export"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.export_servers_dialog(window, cx);
                            })),
                    ),
            );

        gpui_component::v_flex()
            .gap_4()
            .w_full()
            .track_focus(&self.focus_handle)
            // `cmd-f` (the same `EditorAction::Search` the key tree binds) puts the
            // caret in the toolbar's filter box. Anything else is propagated so the
            // other editor bindings still reach their own handlers.
            .on_action(cx.listener(|this, e: &EditorAction, window, cx| match e {
                EditorAction::Search => {
                    this.search_state.focus_handle(cx).focus(window, cx);
                }
                _ => cx.propagate(),
            }))
            // Footer "⋯" dropdown actions are dispatched here.
            .on_action(cx.listener(|this, e: &ServersCardAction, window, cx| match e {
                ServersCardAction::Export(id) => {
                    if let Ok(servers) = get_servers()
                        && let Some(server) = servers.iter().find(|s| s.id == id.as_ref())
                    {
                        let server = server.clone();
                        this.export_server_dialog(&server, window, cx);
                    }
                }
                ServersCardAction::Delete(id) => this.remove_server(window, cx, id.as_ref()),
            }))
            .child(toolbar)
            .children(sections)
            .into_any_element()
    }
}

#[cfg(test)]
mod path_import_tests {
    use super::resolve_import_input;

    #[test]
    fn literal_text_is_passed_through_untouched() {
        // Multi-line JSON is never treated as a path (no filesystem touch).
        let json = "{\n  \"name\": \"x\"\n}";
        assert_eq!(resolve_import_input(json).expect("multiline"), json);
        // A single-line URI that isn't an existing file is returned verbatim.
        let uri = "redis://h:6379";
        assert_eq!(resolve_import_input(uri).expect("uri"), uri);
        // A single-line path that doesn't exist falls back to literal text.
        let missing = "/no/such/zedis-import-9d3f.json";
        assert_eq!(resolve_import_input(missing).expect("missing"), missing);
        // An existing non-.json file is NOT read — only .json paths are slurped.
        let non_json = "/etc/hosts";
        assert_eq!(resolve_import_input(non_json).expect("non-json"), non_json);
    }
}

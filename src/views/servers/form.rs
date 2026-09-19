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

//! The add / edit server dialog: every field of a [`RedisServer`] entry, its
//! validation, the Test button and the save.
//!
//! Split out of `servers.rs`; the methods are `ZedisServers`'s as before.

use super::*;

impl ZedisServers {
    pub(super) fn add_or_update_server_dialog(
        &mut self,
        redis_server: &RedisServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                                    let result: Result<Vec<String>, Error> =
                                        sentinel_master_names(&server).await.map_err(Error::from);
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
            // Sentinel-only, so they follow the master name: a sentinel with
            // credentials of its own is the exception, not the rule.
            ZedisFormField::new("sentinel_username", i18n_servers(cx, "sentinel_username"))
                .default_value(redis_server.sentinel_username.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "sentinel_username_placeholder"))
                .visible_when_filled("master_name")
                .tab_index(0),
            ZedisFormField::new("sentinel_password", i18n_servers(cx, "sentinel_password"))
                .default_value(redis_server.sentinel_password.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "sentinel_password_placeholder"))
                .visible_when_filled("master_name")
                .tab_index(0)
                .mask(),
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
            ZedisFormField::new("tls_server_name", i18n_common(cx, "tls_server_name"))
                .default_value(redis_server.tls_server_name.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "tls_server_name_placeholder"))
                .tab_index(1),
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
            ZedisFormField::new("client_key_passphrase", i18n_common(cx, "client_key_passphrase"))
                .default_value(redis_server.client_key_passphrase.clone().unwrap_or_default())
                .placeholder(i18n_common(cx, "client_key_passphrase_placeholder"))
                .visible_when_filled("client_key")
                .tab_index(1)
                .mask(),
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
            ZedisFormField::new("ssh_jump", i18n_servers(cx, "ssh_jump"))
                .default_value(redis_server.ssh_jump.clone().unwrap_or_default())
                .placeholder(i18n_servers(cx, "ssh_jump_placeholder"))
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
            ZedisFormField::new("cluster_read_replicas", i18n_servers(cx, "cluster_read_replicas"))
                .default_value(redis_server.cluster_read_replicas.unwrap_or(false).to_string())
                .placeholder(i18n_servers(cx, "cluster_read_replicas_check_label"))
                .tab_index(3)
                .field_type(ZedisFormFieldType::Checkbox),
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
        // Only a bridge has accounts, so only the browser's form asks who an
        // entry is for. Unticked — the default for a new entry, because an
        // entry carries credentials — it is private to whoever is signed in;
        // the bridge, not this form, decides whose name that is (ADR 9).
        #[cfg(target_family = "wasm")]
        let fields = {
            let mut fields = fields;
            let shared = !is_new && redis_server.owner.is_none();
            fields.push(
                ZedisFormField::new("shared", i18n_servers(cx, "shared"))
                    .default_value(shared.to_string())
                    .placeholder(i18n_servers(cx, "shared_check_label"))
                    .tab_index(0)
                    .field_type(ZedisFormFieldType::Checkbox),
            );
            fields
        };
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
                #[cfg(not(target_family = "wasm"))]
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
                                let result: Result<(), Error> = test_connection(&server).await.map_err(Error::from);
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
                #[cfg(not(target_family = "wasm"))]
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
}

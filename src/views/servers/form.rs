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
use crate::connection::{SERVER_TYPE_AUTO, SERVER_TYPE_SENTINEL};
use zedis_db::get_alert_rules_manager;

/// The server types a Sentinel's settings apply to: named outright, or Auto,
/// which discovers one. Standalone and Cluster entries have no master name.
const SENTINEL_CAPABLE_TYPES: [usize; 2] = [SERVER_TYPE_AUTO, SERVER_TYPE_SENTINEL];

impl ZedisServers {
    pub(super) fn add_or_update_server_dialog(
        &mut self,
        redis_server: &RedisServer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every way here is hidden from a read-only bridge account, whose
        // save the bridge would refuse; a route or a shortcut is not.
        if account_is_read_only() {
            return;
        }
        let server_id = redis_server.id.clone();
        let is_new = server_id.is_empty();

        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        // Separate entity so suffix builder and foot_actions can safely read it
        // during ZedisForm render without a re-entrant borrow.
        let candidates: gpui::Entity<Vec<gpui::SharedString>> = cx.new(|_| Vec::new());
        let candidates_for_suffix = candidates.clone();
        let fetch_locale = locale.clone();

        let fields: Vec<ZedisFormField> = [
            address_fields(redis_server, cx),
            sentinel_fields(redis_server, candidates_for_suffix, fetch_locale, cx),
            note_fields(redis_server, cx),
            tls_fields(redis_server, cx),
            ssh_fields(redis_server, cx),
            connection_fields(redis_server, cx),
            safety_fields(redis_server, cx),
            key_tree_fields(redis_server, cx),
            sharing_fields(redis_server, is_new, cx),
        ]
        .into_iter()
        .flatten()
        .collect();
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
                i18n_servers(cx, "tab_safety"),
                i18n_servers(cx, "tab_keys"),
            ])
            // The button says what it does: add the entry, or save the edit.
            .confirm_label(i18n_common(cx, if is_new { "add" } else { "save" }))
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
                items.push(test_connection_button(test_label, locale, cx));
                #[cfg(not(target_family = "wasm"))]
                items.push(diagnose_button(diagnose_label, cx));
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

/// The General tab's first run: the name, the address and the credentials.
fn address_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    let host_invalid_msg = i18n_common(cx, "host_invalid");
    let validate_host = move |s: &str| {
        if s.len() <= 1024 && s.is_ascii() {
            return None;
        }
        Some(host_invalid_msg.clone())
    };
    vec![
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
            // Host and port are one address: one row, the port a
            // quarter of it.
            .span(9)
            .validate(validate_host)
            .required(),
        ZedisFormField::new("port", i18n_common(cx, "port"))
            .default_value(redis_server.port.to_string())
            .placeholder(i18n_common(cx, "port_placeholder"))
            .span(3)
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
    ]
}

/// The Advanced tab's first run: the server type, and the Sentinel
/// settings that follow it — the master name, which a button can fetch
/// from the address, and the sentinel's own credentials.
fn sentinel_fields(
    redis_server: &RedisServer,
    candidates_for_suffix: gpui::Entity<Vec<SharedString>>,
    fetch_locale: String,
    cx: &App,
) -> Vec<ZedisFormField> {
    let server_type_list = i18n_servers(cx, "server_type_list");
    vec![
        // tab advanced. The server type leads it, and the Sentinel
        // settings follow the type: they are offered for Auto (which
        // finds a Sentinel by itself) and Sentinel, and dropped for
        // Standalone and Cluster, where a master name means nothing.
        // They used to sit on the General tab of every entry.
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
        ZedisFormField::new("master_name", i18n_servers(cx, "master_name"))
            .default_value(redis_server.master_name.clone().unwrap_or_default())
            .placeholder(i18n_servers(cx, "master_name_placeholder"))
            .visible_on("server_type", &SENTINEL_CAPABLE_TYPES)
            .tab_index(3)
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
                            fetch_master_names(form, candidates.clone(), locale.clone(), cx);
                        }))
                }
            }),
        // Sentinel-only, so they follow the master name: a sentinel with
        // credentials of its own is the exception, not the rule.
        ZedisFormField::new("sentinel_username", i18n_servers(cx, "sentinel_username"))
            .default_value(redis_server.sentinel_username.clone().unwrap_or_default())
            .placeholder(i18n_servers(cx, "sentinel_username_placeholder"))
            .visible_on("server_type", &SENTINEL_CAPABLE_TYPES)
            .visible_when_filled("master_name")
            .tab_index(3),
        ZedisFormField::new("sentinel_password", i18n_servers(cx, "sentinel_password"))
            .default_value(redis_server.sentinel_password.clone().unwrap_or_default())
            .placeholder(i18n_servers(cx, "sentinel_password_placeholder"))
            .visible_on("server_type", &SENTINEL_CAPABLE_TYPES)
            .visible_when_filled("master_name")
            .tab_index(3)
            .mask(),
    ]
}

/// The General tab's second run: the description and the group.
fn note_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    vec![
        ZedisFormField::new("description", i18n_common(cx, "description"))
            .default_value(redis_server.description.clone().unwrap_or_default())
            .placeholder(i18n_common(cx, "description_placeholder"))
            .tab_index(0),
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
            .tab_index(0),
    ]
}

/// The TLS tab.
fn tls_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    vec![
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
    ]
}

/// The SSH tunnel tab.
fn ssh_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    vec![
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
    ]
}

/// The Advanced tab's second run: the databases, the timeouts, reading from replicas.
fn connection_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
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
    vec![
        // tab advanced (the server type and what follows it are defined
        // above, next to each other)
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
    ]
}

/// The Safety tab: the environment, the writes it allows, confirmations.
fn safety_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    vec![
        // —— Safety tab: the tag first, because the writes choice below
        // follows it ——
        // Single "Environment" preset (None/Dev/UAT/Prod) drives the
        // display tag, chip color, the high-risk (PROD) escalation and the
        // default write lock. The option index maps straight onto
        // TAG_COLOR_PRESETS, so the stored `tag_color` key stays the
        // source of truth and `from_form_data` derives the label from it.
        ZedisFormField::new("tag_color", i18n_servers(cx, "tag"))
            .default_value(tag_color_index(redis_server.tag_color.as_deref()).to_string())
            .options(
                TAG_ENV_LABELS
                    .iter()
                    .map(|s| SharedString::from(*s))
                    .collect::<Vec<SharedString>>(),
            )
            .tab_index(4)
            .field_type(ZedisFormFieldType::RadioGroup),
        // One axis for how much the app lets you write — follow the tag,
        // allowed, locked, read-only — over the two stored fields
        // (`write_lock`, `readonly`), which used to be two controls of
        // overlapping meaning that could both be set (ADR 14). A radio
        // rather than boxes because the default *follows the tag*, which a
        // box could not say: it would freeze the answer at the moment the
        // form opened, before the tag was picked.
        ZedisFormField::new("writes", i18n_servers(cx, "writes"))
            .default_value(writes_index(redis_server.readonly, redis_server.write_lock).to_string())
            // In `writes_index` order.
            .options(vec![
                i18n_servers(cx, "writes_follow_tag"),
                i18n_servers(cx, "writes_allowed"),
                i18n_servers(cx, "writes_locked"),
                i18n_servers(cx, "writes_readonly"),
            ])
            // Four phrases: one per line, or they wrap into two rows
            // that read as two separate choices.
            .stacked()
            .tab_index(4)
            .field_type(ZedisFormFieldType::RadioGroup),
        ZedisFormField::new("require_confirm_writes", i18n_servers(cx, "require_confirm_writes"))
            .default_value(redis_server.require_confirm_writes.unwrap_or(false).to_string())
            .placeholder(i18n_servers(cx, "require_confirm_writes_check_label"))
            .tab_index(4)
            .field_type(ZedisFormFieldType::Checkbox),
        ZedisFormField::new("alerts_enabled", i18n_servers(cx, "alerts_enabled"))
            .default_value(
                if redis_server.id.is_empty() {
                    redis_server.alerts_enabled.unwrap_or(false)
                } else {
                    get_alert_rules_manager().is_enabled(&redis_server.id, redis_server.alerts_enabled)
                }
                .to_string(),
            )
            .placeholder(i18n_servers(cx, "alerts_enabled_check_label"))
            .tab_index(4)
            .field_type(ZedisFormFieldType::Checkbox),
    ]
}

/// The Keys tab: how this server's tree scans and draws its keys.
fn key_tree_fields(redis_server: &RedisServer, cx: &App) -> Vec<ZedisFormField> {
    vec![
        // —— Keys tab: key-tree / SCAN behaviour (per-server overrides) ——
        ZedisFormField::new("key_separator", i18n_servers(cx, "key_separator"))
            .default_value(redis_server.key_separator.clone().unwrap_or_default())
            .placeholder(i18n_servers(cx, "key_separator_placeholder"))
            .tab_index(5),
        ZedisFormField::new("key_scan_count", i18n_servers(cx, "key_scan_count"))
            .default_value(redis_server.key_scan_count.map(|n| n.to_string()).unwrap_or_default())
            .placeholder(i18n_servers(cx, "key_scan_count_placeholder"))
            .tab_index(5),
        ZedisFormField::new("max_key_tree_depth", i18n_servers(cx, "max_key_tree_depth"))
            .default_value(
                redis_server
                    .max_key_tree_depth
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            )
            .placeholder(i18n_servers(cx, "max_key_tree_depth_placeholder"))
            .tab_index(5),
        ZedisFormField::new("auto_expand_threshold", i18n_servers(cx, "auto_expand_threshold"))
            .default_value(
                redis_server
                    .auto_expand_threshold
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            )
            .placeholder(i18n_servers(cx, "auto_expand_threshold_placeholder"))
            .tab_index(5),
        ZedisFormField::new("show_key_tree_ttl", i18n_servers(cx, "show_key_tree_ttl"))
            .default_value(redis_server.show_key_tree_ttl_form_index().to_string())
            .options(vec![
                i18n_servers(cx, "show_key_tree_ttl_default"),
                i18n_servers(cx, "show_key_tree_ttl_show"),
                i18n_servers(cx, "show_key_tree_ttl_hide"),
            ])
            .tab_index(5)
            .field_type(ZedisFormFieldType::RadioGroup),
    ]
}

/// Who an entry is for: a question for a bridge, which has accounts.
#[cfg(not(target_family = "wasm"))]
fn sharing_fields(_redis_server: &RedisServer, _is_new: bool, _cx: &App) -> Vec<ZedisFormField> {
    Vec::new()
}

/// Only a bridge has accounts, so only the browser's form asks who an entry
/// is for. Unticked — the default for a new entry, because an entry carries
/// credentials — it is private to whoever is signed in; the bridge, not this
/// form, decides whose name that is (ADR 9).
#[cfg(target_family = "wasm")]
fn sharing_fields(redis_server: &RedisServer, is_new: bool, cx: &App) -> Vec<ZedisFormField> {
    let shared = !is_new && redis_server.owner.is_none();
    vec![
        ZedisFormField::new("shared", i18n_servers(cx, "shared"))
            .default_value(shared.to_string())
            .placeholder(i18n_servers(cx, "shared_check_label"))
            .tab_index(0)
            .field_type(ZedisFormFieldType::Checkbox),
    ]
}

/// The names of the masters the Sentinel at the form's address watches:
/// one goes straight into the field, several are offered as buttons under
/// the form, none or an error is a notice.
fn fetch_master_names(
    form: &mut zedis_ui::ZedisForm,
    candidates: gpui::Entity<Vec<SharedString>>,
    locale: String,
    cx: &mut gpui::Context<zedis_ui::ZedisForm>,
) {
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
    cx.spawn(async move |form_entity, cx| {
        let result: Result<Vec<String>, Error> = sentinel_master_names(&server).await.map_err(Error::from);
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
                        cx.emit(GlobalEvent::Notification(NotificationAction::new_error(msg.into())));
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
                        cx.emit(GlobalEvent::Notification(NotificationAction::new_error(msg.into())));
                    });
                });
            }
        });
    })
    .detach();
}

/// Test: dial the server as the form describes it, and say how it went.
#[cfg(not(target_family = "wasm"))]
fn test_connection_button(
    test_label: SharedString,
    locale: String,
    cx: &mut Context<zedis_ui::ZedisForm>,
) -> gpui::AnyElement {
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
        .into_any_element()
}

/// Diagnose: the connection's stages, one by one, for the form's values.
#[cfg(not(target_family = "wasm"))]
fn diagnose_button(diagnose_label: SharedString, cx: &mut Context<zedis_ui::ZedisForm>) -> gpui::AnyElement {
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
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::get_server;
    use crate::test_support::{hold_the_server_list, init_ui};
    use gpui::{ElementId, TestAppContext, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use std::sync::Arc;

    /// An entry with every field the form has filled, on a type that shows
    /// the Sentinel settings, with both passphrases' keys there to show them.
    fn every_field() -> RedisServer {
        RedisServer {
            id: "form-round-trip".into(),
            name: "orders".into(),
            host: "redis.internal".into(),
            port: 6390,
            username: Some("app".into()),
            password: Some("app-password".into()),
            server_type: Some(SERVER_TYPE_SENTINEL),
            master_name: Some("mymaster".into()),
            sentinel_username: Some("watcher".into()),
            sentinel_password: Some("watcher-password".into()),
            description: Some("the orders cache".into()),
            group: Some("prod".into()),
            tls: Some(true),
            insecure: Some(true),
            tls_server_name: Some("redis.example".into()),
            client_cert: Some("CERT".into()),
            client_key: Some("KEY".into()),
            client_key_passphrase: Some("key-passphrase".into()),
            root_cert: Some("ROOT".into()),
            ssh_tunnel: Some(true),
            ssh_addr: Some("bastion:22".into()),
            ssh_jump: Some("jump:22".into()),
            ssh_username: Some("ops".into()),
            ssh_password: Some("ssh-password".into()),
            ssh_key: Some("SSHKEY".into()),
            ssh_key_passphrase: Some("ssh-passphrase".into()),
            databases: Some(32),
            default_db: Some(3),
            connection_timeout: Some(7),
            response_timeout: Some(9),
            cluster_read_replicas: Some(true),
            tag: Some("UAT".into()),
            tag_color: Some("purple".into()),
            write_lock: Some(true),
            require_confirm_writes: Some(true),
            alerts_enabled: Some(true),
            key_separator: Some("/".into()),
            key_scan_count: Some(2000),
            max_key_tree_depth: Some(4),
            auto_expand_threshold: Some(500),
            show_key_tree_ttl: Some(false),
            ..Default::default()
        }
    }

    /// What the form shows of an entry is what it hands back: every field
    /// is on the form, filled from the entry, and read back into the same
    /// one; and the desktop's Test and Diagnose are under it. Saved through the store, so the entry is then in the process's
    /// server list, under an id no other test uses.
    #[gpui::test]
    fn an_entry_comes_back_from_its_form_as_it_went_in(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let mut servers = None;
        let handle = cx.open_window(size(px(1000.), px(900.)), |window, cx| {
            let view = cx.new(|cx| ZedisServers::new(window, cx));
            servers = Some(view.clone());
            Root::new(view, window, cx)
        });
        let servers = servers.expect("the window built the list");
        let entry = every_field();
        cx.update_window(handle.into(), |_, window, cx| {
            servers.update(cx, |servers, cx| {
                servers.add_or_update_server_dialog(&entry, window, cx)
            });
        })
        .expect("the window is open");
        cx.run_until_parked();
        // The save writes the file on a thread of its own (`unblock`), which
        // a test's scheduler does not drive: allow that, and wait for the
        // list to say it is saved, so nothing the write wakes outlives the
        // test.
        let saved = std::rc::Rc::new(std::cell::Cell::new(false));
        cx.update(|cx| {
            let global = cx.global::<ZedisGlobalStore>().state();
            let saved = saved.clone();
            cx.subscribe(&global, move |_, event, _| {
                if matches!(event, GlobalEvent::ServerListUpdated) {
                    saved.set(true);
                }
            })
            .detach();
        });
        cx.executor().allow_parking();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            // The desktop's own two: the form's values tried, and diagnosed.
            assert!(window.try_find("test-connection").is_some());
            assert!(window.try_find("diagnose-connection").is_some());
            window.click(
                ElementId::NamedChild(Arc::new("dialog-form".into()), "confirm".into()),
                cx,
            );
        })
        .expect("the window is open");
        // The write is on a thread of its own, which a loaded machine may
        // be slow to run.
        for _ in 0..3_000 {
            cx.run_until_parked();
            if saved.get() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saved.get(), "the list was saved");
        let saved = get_server(&entry.id).expect("the form saved the entry");
        // The two the store stamps, not the form: when, and the place a new
        // entry takes in its list.
        assert_eq!(
            RedisServer {
                updated_at: None,
                sort_order: None,
                ..saved
            },
            entry
        );
    }
}

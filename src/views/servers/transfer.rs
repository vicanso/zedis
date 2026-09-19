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

//! Moving entries in and out: the export dialogs (one entry, all of them)
//! and the import dialog.
//!
//! Split out of `servers.rs`; the methods are `ZedisServers`'s as before.

use super::*;

impl ZedisServers {
    /// Show the JSON export for a single server config. Defaults to
    /// "stripped" mode where credential fields (passwords, SSH key,
    /// TLS materials) are blanked — that's the safe state for
    /// pasting into chat / wiki / git. A toggle reveals the
    /// with-secrets variant for users doing personal backups.
    pub(super) fn export_server_dialog(&mut self, server: &RedisServer, window: &mut Window, cx: &mut Context<Self>) {
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

                gpui_kit::component::v_flex()
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
    pub(super) fn export_servers_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn import_server_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}

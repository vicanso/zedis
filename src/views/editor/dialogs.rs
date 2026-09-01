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

//! Key-level dialogs of the editor toolbar: rename (with overwrite
//! confirm), copy-to-server, and the cross-server value diff.
//! Split out of `editor.rs`.

use super::*;

impl ZedisEditor {
    /// Open the rename dialog, prefilled with the current key name. OK
    /// fires a `RENAMENX`; a destination collision comes back via
    /// `ServerEvent::RenameTargetExists` and routes to the overwrite confirm.
    pub(super) fn open_rename_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        self.rename_input_state.update(cx, |state, cx| {
            state.set_value(key.clone(), window, cx);
            state.focus(window, cx);
        });
        let input_child = self.rename_input_state.clone();
        let input_ok = self.rename_input_state.clone();
        let server_state = self.server_state.clone();
        let old = key.clone();
        ZedisDialog::new(i18n_editor(cx, "rename_title"))
            .w(px(420.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || Input::new(&input_child))
            .on_ok(move |_, _window, cx| {
                let new = input_ok.read(cx).value().trim().to_string();
                if new.is_empty() || new.as_str() == old.as_ref() {
                    return true;
                }
                let old = old.clone();
                let new: SharedString = new.into();
                server_state.update(cx, move |state, cx| {
                    state.rename_key(old, new, false, cx);
                });
                true
            })
            .open(window, cx);
    }

    /// Confirm dialog shown when a rename would overwrite an existing key;
    /// proceeding issues a clobbering `RENAME`.
    pub(super) fn open_overwrite_confirm(
        &mut self,
        old: SharedString,
        new: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server_state = self.server_state.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("editor.rename_overwrite_prompt", key = new.as_ref(), locale = locale).to_string();
        ZedisDialog::new_alert(i18n_editor(cx, "rename_overwrite_title"), message)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let (old, new) = (old.clone(), new.clone());
                server_state.update(cx, move |state, cx| {
                    state.rename_key(old, new, true, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Confirm dialog for a refused compare-and-set save (`SET … IFEQ`
    /// answered nil): another writer changed the value after it was loaded,
    /// and the reload has already put the winner's value on screen.
    /// Proceeding force-writes the carried draft — the exact bytes the
    /// refused save tried — via the non-CAS path.
    pub(super) fn open_save_conflict_dialog(
        &mut self,
        key: SharedString,
        draft: Bytes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server_state = self.server_state.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("editor.save_conflict_body", key = key.as_ref(), locale = locale).to_string();
        ZedisDialog::new_alert(i18n_editor(cx, "save_conflict_title"), message)
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_editor(cx, "save_conflict_overwrite"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, window, cx| {
                let (key, draft) = (key.clone(), draft.clone());
                server_state.update(cx, move |state, cx| {
                    state.update_value_bytes(key, draft.to_vec(), true, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Open the cross-server "copy to…" dialog for the selected key. On OK
    /// the chosen target server / db (and overwrite flag) drive `run_copy`.
    pub(super) fn open_copy_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let source_id = state.server_id().to_string();
        let source_db = state.db();
        let Some(key) = state.key() else {
            return;
        };
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.clone().into(), source_db, true, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        let source_id_ok = source_id.clone();
        let key_ok = key.clone();
        ZedisDialog::new(i18n_copy(cx, "title"))
            .w(px(460.))
            .ok_text(i18n_copy(cx, "copy"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_copy(cx, "copy"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                let conflict = view_ok.read(cx).conflict();
                if let Some(editor) = editor.upgrade() {
                    let req = CopyRequest {
                        source_id: source_id_ok.clone(),
                        source_db,
                        target_id,
                        target_db,
                        key: key_ok.clone(),
                        conflict,
                    };
                    editor.update(cx, move |this, cx| this.run_copy(req, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// Open the cross-server diff picker (the copy dialog reused as a pure
    /// server / db picker). On OK, diff this server's value of the key against
    /// the same key on the chosen server.
    pub(super) fn open_diff_with_server_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let source_id = state.server_id().to_string();
        let source_db = state.db();
        let Some(key) = state.key() else {
            return;
        };
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.into(), source_db, false, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        let key_ok = key.clone();
        ZedisDialog::new(i18n_editor(cx, "diff_with_server"))
            .w(px(460.))
            .ok_text(i18n_editor(cx, "diff_with_server"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_editor(cx, "diff_with_server"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                if let Some(editor) = editor.upgrade() {
                    let key = key_ok.clone();
                    editor.update(cx, move |this, cx| {
                        this.run_cross_server_diff(target_id, target_db, key, cx)
                    });
                }
                true
            })
            .open(window, cx);
    }

    /// Fetch the same key's value from `target_id`/`target_db` and open the
    /// diff view: the other server's value (left) vs this server's (right).
    /// String keys only — non-string keys have no bytes editor to diff.
    pub(super) fn run_cross_server_diff(
        &mut self,
        target_id: SharedString,
        target_db: usize,
        key: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(bytes_editor) = self.bytes_editor.clone() else {
            self.server_state.update(cx, |s, cx| {
                s.emit_warning_notification(i18n_editor(cx, "diff_string_only"), cx);
            });
            return;
        };
        // Snapshot this server's current bytes for the right pane.
        let current_bytes: bytes::Bytes = bytes_editor.update(cx, |state, cx| match state.value_bytes_for_save(cx) {
            Some(Ok(b)) => bytes::Bytes::from(b),
            Some(Err(_)) | None => bytes::Bytes::from(state.value(cx).to_string()),
        });
        let is_json = self
            .server_state
            .read(cx)
            .value()
            .map(|v| v.is_redis_json())
            .unwrap_or(false);
        let target_name: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        let label: SharedString = format!("{target_name} / db{target_db}").into();
        cx.spawn(async move |this, cx| {
            let fetched = async {
                let client = get_connection_manager().get_client(&target_id, target_db).await?;
                client.get_key_bytes(&key).await
            }
            .await;
            let _ = this.update(cx, move |this, cx| match fetched {
                Ok(other_bytes) => {
                    let session = DiffSession {
                        history_idx: 0,
                        reference_bytes: bytes::Bytes::from(other_bytes),
                        reference_at: 0,
                        current_bytes,
                        is_json,
                        reference_label: Some(label),
                    };
                    let editor_weak = cx.entity().downgrade();
                    let on_close: DiffCloseCallback = std::sync::Arc::new(move |_w, cx| {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.update(cx, |this, cx| this.close_diff_session(cx));
                        }
                    });
                    let view = cx.new(|cx| ZedisValueDiff::new(session.clone(), on_close, cx));
                    this.diff_session = Some(session);
                    this.diff_view = Some(view);
                    cx.notify();
                }
                Err(e) => this.server_state.update(cx, |s, cx| {
                    s.emit_error_notification(format!("{}: {e}", i18n_editor(cx, "diff_with_server")).into(), cx);
                }),
            });
        })
        .detach();
    }

    /// Run the cross-server copy (`DUMP` + `RESTORE`) in the background and
    /// report the outcome via a notification.
    pub(super) fn run_copy(&mut self, req: CopyRequest, cx: &mut Context<Self>) {
        let CopyRequest {
            source_id,
            source_db,
            target_id,
            target_db,
            key,
            conflict,
        } = req;
        let target_name: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        cx.spawn(async move |this, cx| {
            let result = copy_key(
                source_id,
                source_db,
                target_id.to_string(),
                target_db,
                key.to_string(),
                conflict,
            )
            .await;
            let _ = this.update(cx, move |this, cx| {
                this.server_state.update(cx, |state, cx| match result {
                    Ok(Some(RestoreStatus::Written)) => state.emit_success_notification(
                        format!("{target_name} / db{target_db}").into(),
                        i18n_copy(cx, "done"),
                        cx,
                    ),
                    Ok(Some(RestoreStatus::Skipped)) => state.emit_warning_notification(i18n_copy(cx, "skipped"), cx),
                    Ok(None) => state.emit_warning_notification(i18n_copy(cx, "key_gone"), cx),
                    Ok(Some(RestoreStatus::Failed(msg))) => {
                        state.emit_error_notification(copy_failure_message(cx, &msg), cx)
                    }
                    Err(e) => state.emit_error_notification(copy_failure_message(cx, &e.to_string()), cx),
                });
            });
        })
        .detach();
    }
}

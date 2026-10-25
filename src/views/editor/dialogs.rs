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

/// The rename dialog's field, by which a UI test finds it.
const RENAME_INPUT_ID: &str = "rename-key-input";

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
        });
        let input_child = self.rename_input_state.clone();
        let input_ok = self.rename_input_state.clone();
        let input_focus = self.rename_input_state.clone();
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
            .child(move || Input::new(&input_child).id(RENAME_INPUT_ID))
            .on_ok(move |_, _window, cx| {
                let typed = input_ok.read(cx).value();
                // Unchanged is decided before trimming: a key whose name
                // ends in a space would otherwise be renamed to its trimmed
                // self by a plain OK.
                if typed.as_ref() == old.as_ref() {
                    return true;
                }
                let new = typed.trim().to_string();
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
        // After the dialog is open, not before: opening one takes the focus
        // for itself, so a field focused first was left without it — the
        // name could not be typed over until it was clicked — and was what
        // the dialog gave the focus back to when it closed.
        input_focus.update(cx, |state, cx| state.focus(window, cx));
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
        // The key under the new name is gone once this goes ahead, so it is
        // a danger confirm — Return answers Cancel — and the button says
        // "Overwrite", where the shared default said "Delete" (#186).
        ZedisDialog::new_alert(i18n_editor(cx, "rename_overwrite_title"), message)
            .danger()
            .ok_text(i18n_editor(cx, "save_conflict_overwrite"))
            .cancel_text(i18n_common(cx, "cancel"))
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
            .danger()
            .ok_text(i18n_editor(cx, "save_conflict_overwrite"))
            .cancel_text(i18n_common(cx, "cancel"))
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
        let view = cx.new(|cx| {
            ZedisCopyKeyDialog::new(
                source_id.clone().into(),
                source_db,
                CopyDialogPurpose::CopyKey,
                window,
                cx,
            )
        });
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
            .on_ok(move |_, window, cx| {
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
                    editor.update(cx, move |this, cx| this.run_copy(req, window, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// Open the absolute-expiry picker (`EXPIREAT`). The dialog composes a
    /// calendar date and a typed time into one instant and previews it; the
    /// OK path re-checks that instant rather than trusting the preview,
    /// because a dialog left open across midnight would otherwise submit a
    /// deadline that has since passed — and a past `EXPIREAT` deletes the key.
    pub(super) fn open_expire_at_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let Some(key) = state.key() else {
            return;
        };
        let current = state.value().and_then(|value| value.expire_at).filter(|at| *at > 0);
        let view = cx.new(|cx| ZedisExpireAtDialog::new(current, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        ZedisDialog::new(i18n_expire_at(cx, "title"))
            .w(px(420.))
            .ok_text(i18n_expire_at(cx, "apply"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_expire_at(cx, "apply"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(at) = view_ok.read(cx).expire_at(cx).filter(|at| *at > unix_ts()) else {
                    // The preview already says why; keep the dialog open
                    // rather than closing on a value that cannot be sent.
                    return false;
                };
                if let Some(editor) = editor.upgrade() {
                    let key = key.clone();
                    editor.update(cx, move |this, cx| {
                        this.server_state.update(cx, move |state, cx| {
                            state.update_key_expire_at(key, at, cx);
                        });
                    });
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
        let view = cx
            .new(|cx| ZedisCopyKeyDialog::new(source_id.into(), source_db, CopyDialogPurpose::PickServer, window, cx));
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
            let fetched = async { key_bytes(&ServerDb::new(&*target_id, target_db), &key).await }.await;
            let _ = this.update(cx, move |this, cx| match fetched {
                // Another key is open now: this diff is of a key that is no
                // longer on screen, against a snapshot of it that is not the
                // editor's any more. Drop it rather than open it over the new
                // key.
                Ok(_) if this.server_state.read(cx).key().as_ref() != Some(&key) => {}
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

    /// Copy the key to another server / db and say how it went.
    ///
    /// Between two servers it is checked first (`copy_preflight`, ADR 16):
    /// a key the target cannot take as a `DUMP` — another product, an older
    /// version — is re-created by type, and what that would leave behind (a
    /// stream's consumer groups, a hash's field TTLs) is put to the user
    /// before anything is written. Within one server there is nothing to ask:
    /// it reads its own payloads.
    pub(super) fn run_copy(&mut self, req: CopyRequest, window: &mut Window, cx: &mut Context<Self>) {
        if req.source_id == req.target_id.as_ref() {
            self.copy_without_check(req, cx);
            return;
        }
        let source = ServerDb::new(req.source_id.clone(), req.source_db);
        let target = ServerDb::new(req.target_id.to_string(), req.target_db);
        let key = req.key.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let checked = copy_preflight(&source, &target, &key).await;
            let _ = this.update_in(cx, move |this, window, cx| match checked {
                Ok(Some(check)) => this.copy_checked(req, check, window, cx),
                Ok(None) => this.report_copy(&req, Ok(None), None, cx),
                Err(e) => this.report_copy(&req, Err(e.to_string()), None, cx),
            });
        })
        .detach();
    }

    /// What the check found decides: a key that cannot travel at all is
    /// refused here, one that loses something is asked about, and one that
    /// loses nothing is copied from the check's own `DUMP`.
    fn copy_checked(&mut self, req: CopyRequest, check: CopyPreflight, window: &mut Window, cx: &mut Context<Self>) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        if let Some(CopyLoss::Uncopyable { key_type }) = check
            .losses
            .iter()
            .find(|loss| matches!(loss, CopyLoss::Uncopyable { .. }))
        {
            let message: SharedString = rust_i18n::t!(
                "copy.cannot_recreate",
                target = &check.target,
                source = &check.source,
                key_type = key_type,
                locale = &locale
            )
            .to_string()
            .into();
            self.server_state
                .update(cx, |state, cx| state.emit_error_notification(message, cx));
            return;
        }
        if check.losses.is_empty() {
            let source = ServerDb::new(req.source_id.clone(), req.source_db);
            let target = ServerDb::new(req.target_id.to_string(), req.target_db);
            cx.spawn(async move |this, cx| {
                let status = check.copy(&source, &target, req.conflict).await;
                let _ = this.update(cx, |this, cx| {
                    this.report_copy(&req, status.map(Some).map_err(|e| e.to_string()), Some(&check), cx);
                });
            })
            .detach();
            return;
        }
        let mut body = rust_i18n::t!(
            "copy.recreate_body",
            target = &check.target,
            source = &check.source,
            locale = &locale
        )
        .to_string();
        for loss in &check.losses {
            body.push_str("\n• ");
            body.push_str(&loss_line(loss, &locale));
        }
        let editor = cx.entity().downgrade();
        ZedisDialog::new_alert(i18n_copy(cx, "recreate_title"), body)
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_copy(cx, "copy"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _window, cx| {
                if let Some(editor) = editor.upgrade() {
                    let (req, check) = (req.clone(), check.clone());
                    editor.update(cx, move |this, cx| this.copy_confirmed(req, check, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// The copy someone agreed to after reading what it leaves behind. It
    /// dumps the key again: it may have changed while the question was up.
    fn copy_confirmed(&mut self, req: CopyRequest, check: CopyPreflight, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = copy_key(
                req.source_id.clone(),
                req.source_db,
                req.target_id.to_string(),
                req.target_db,
                req.key.to_string(),
                req.conflict,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = this.update(cx, |this, cx| this.report_copy(&req, result, Some(&check), cx));
        })
        .detach();
    }

    /// A copy within one server: `DUMP` and `RESTORE`, nothing to check.
    fn copy_without_check(&mut self, req: CopyRequest, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = copy_key(
                req.source_id.clone(),
                req.source_db,
                req.target_id.to_string(),
                req.target_db,
                req.key.to_string(),
                req.conflict,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = this.update(cx, |this, cx| this.report_copy(&req, result, None, cx));
        })
        .detach();
    }

    /// Say how a copy went. A key re-created by type says so, and names
    /// what it left behind — it used to read "Copied" like any other.
    fn report_copy(
        &mut self,
        req: &CopyRequest,
        result: Result<Option<RestoreStatus>, String>,
        check: Option<&CopyPreflight>,
        cx: &mut Context<Self>,
    ) {
        let target_name: SharedString = get_server(&req.target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| req.target_id.clone());
        let title: SharedString = format!("{target_name} / db{}", req.target_db).into();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let recreated: SharedString = match check.map(|check| check.losses.as_slice()).unwrap_or_default() {
            [] => i18n_copy(cx, "done_recreated"),
            losses => {
                let lost: Vec<String> = losses.iter().map(|loss| loss_line(loss, &locale)).collect();
                rust_i18n::t!("copy.done_recreated_lost", lost = lost.join("; "), locale = &locale)
                    .to_string()
                    .into()
            }
        };
        self.server_state.update(cx, |state, cx| match result {
            Ok(Some(RestoreStatus::Written)) => state.emit_success_notification(title, i18n_copy(cx, "done"), cx),
            Ok(Some(RestoreStatus::Recreated)) => state.emit_success_notification(title, recreated, cx),
            Ok(Some(RestoreStatus::Skipped)) => state.emit_warning_notification(i18n_copy(cx, "skipped"), cx),
            Ok(None) => state.emit_warning_notification(i18n_copy(cx, "key_gone"), cx),
            Ok(Some(RestoreStatus::Failed(msg))) => state.emit_error_notification(copy_failure_message(cx, &msg), cx),
            Err(e) => state.emit_error_notification(copy_failure_message(cx, &e), cx),
        });
    }
}

/// One thing a key re-created by type leaves behind, as a line of the
/// question and of the result.
fn loss_line(loss: &CopyLoss, locale: &str) -> String {
    match loss {
        CopyLoss::ConsumerGroups(count) => rust_i18n::t!("copy.loss_groups", count = count, locale = locale),
        CopyLoss::FieldTtls {
            count,
            all_checked: true,
        } => rust_i18n::t!("copy.loss_field_ttls", count = count, locale = locale),
        CopyLoss::FieldTtls {
            count,
            all_checked: false,
        } => rust_i18n::t!("copy.loss_field_ttls_more", count = count, locale = locale),
        CopyLoss::Uncopyable { key_type } => {
            rust_i18n::t!("copy.loss_whole_key", key_type = key_type, locale = locale)
        }
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{RENAME_INPUT_ID, ZedisEditor};
    use crate::states::ZedisServerState;
    use crate::test_support::init_ui;
    use gpui::{AppContext, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    /// The rename dialog opens ready to type over the name: the field holds
    /// the key's name and the focus. It used to be focused *before* the
    /// dialog opened, which takes the focus for itself — the name could not
    /// be typed over until the field was clicked.
    #[gpui::test]
    fn the_rename_dialog_opens_with_its_field_focused(cx: &mut TestAppContext) {
        init_ui(cx);
        // A selected key and no server: the dialog needs only the name, and
        // the load that selecting starts finds no such entry and gives up.
        let server_state = cx.new(|_| ZedisServerState::default());
        server_state.update(cx, |state, cx| state.select_key("user:1".into(), cx));

        let mut editor = None;
        let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx));
            editor = Some(view.clone());
            Root::new(view, window, cx)
        });
        let editor = editor.expect("the window built the editor");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(RENAME_INPUT_ID).is_none(), "no dialog yet");
            editor.update(cx, |editor, cx| editor.open_rename_dialog(window, cx));
        })
        .expect("the window is open");
        // Opening a dialog and focusing a field are both deferred: they
        // happen once the update that asked for them has returned.
        cx.run_until_parked();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let field = window.find(RENAME_INPUT_ID);
            assert_eq!(field.value(), Some("user:1"));
            assert_eq!(field.focused(), Some(true));

            // And typing goes into it, with no click first.
            window.input("0", cx);
            assert_eq!(window.find(RENAME_INPUT_ID).value(), Some("user:10"));
        })
        .expect("the window is open");
    }
}

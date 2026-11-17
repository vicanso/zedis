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

//! Index creation, alteration, and drop handlers for [`ZedisSearchManager`].

use super::*;

impl ZedisSearchManager {
    pub(super) fn open_create_dialog(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // A write: asked of the connection as it is now, so a read-only
        // session or a locked production entry cannot reach it by a key
        // press or a stale button either.
        if !self.can_write_index(cx) {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "create_name_placeholder")));
        let prefixes =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "create_prefixes_placeholder")));
        let first_field_name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        self.create_form = Some(CreateIndexForm {
            name,
            on_json: false,
            prefixes,
            fields: vec![CreateFieldRow {
                id: 0,
                name: first_field_name,
                field_type: SharedString::from("TEXT"),
                sortable: false,
                no_stem: false,
                no_index: false,
            }],
            next_field_id: 1,
        });
        cx.notify();
    }

    pub(super) fn close_create_dialog(&mut self, cx: &mut gpui::Context<Self>) {
        self.create_form = None;
        cx.notify();
    }

    /// Open a confirm dialog for `FT.DROPINDEX`. Keeps documents
    /// (data) intact — the destructive `DD` variant is documented in
    /// the dialog body so users who really want it know to go to the
    /// terminal. This is a deliberate guardrail; one click in the
    /// schema header shouldn't be able to nuke an entire dataset.
    pub(super) fn confirm_drop_index(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // A write: asked of the connection as it is now, so a read-only
        // session or a locked production entry cannot reach it by a key
        // press or a stale button either.
        if !self.can_write_index(cx) {
            return;
        }
        let Some(index) = self.selected_index.clone() else {
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = i18n_search(cx, "drop_title");
        // Capture as owned values for the closure — the dialog's
        // on_ok runs in &mut App without our Context.
        let index_for_task = index.clone();
        let server_id_for_task = server_id.clone();
        let body_message: SharedString = format!(
            "{}\n\n{}: {}\n\n{}",
            i18n_search(cx, "drop_message"),
            i18n_search(cx, "drop_index_label"),
            index,
            i18n_search(cx, "drop_dd_hint"),
        )
        .into();
        // A production entry says so in the body, as dropping a function or
        // an ACL user does.
        let body_message = escalate_dangerous_body(cx, &server_id, body_message);
        ZedisDialog::new_alert(title, body_message)
            .danger()
            .ok_text(i18n_search(cx, "drop_confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, _window, cx| {
                let Some(this) = entity.upgrade() else { return true };
                // `on_ok` is `Fn` (callable multiple times), so we
                // clone captured strings each time rather than moving.
                let index_inner = index_for_task.clone();
                let server_id_inner = server_id_for_task.clone();
                this.update(cx, |this, cx| {
                    this.dropping_index = true;
                    this.error = None;
                    let log_name = index_inner.clone();
                    let cmd_index = index_inner.clone();
                    this._drop_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let at = ServerDb::new(&*server_id_inner, db);
                            ft_dropindex(&at, cmd_index.as_ref(), false).await
                        });
                        let result: Result<()> = task.await.map_err(Into::into);
                        let _ = handle.update(cx, |this, cx| {
                            this.dropping_index = false;
                            match result {
                                Ok(()) => {
                                    info!(index = %log_name, "FT.DROPINDEX succeeded");
                                    this.selected_index = None;
                                    this.index_info = None;
                                    this.last_result = None;
                                    this.refresh_indexes(cx);
                                }
                                Err(e) => {
                                    this.error = Some(e.to_string().into());
                                }
                            }
                            cx.notify();
                        });
                    }));
                });
                true
            })
            .open(window, cx);
    }

    pub(super) fn open_add_field_form(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // A write: asked of the connection as it is now, so a read-only
        // session or a locked production entry cannot reach it by a key
        // press or a stale button either.
        if !self.can_write_index(cx) {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        self.add_field_form = Some(AddFieldForm {
            name,
            field_type: SharedString::from("TEXT"),
            sortable: false,
            no_stem: false,
            no_index: false,
        });
        cx.notify();
    }

    pub(super) fn close_add_field_form(&mut self, cx: &mut gpui::Context<Self>) {
        self.add_field_form = None;
        cx.notify();
    }

    pub(super) fn set_add_field_type(&mut self, ty: &str, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.add_field_form.as_mut() else {
            return;
        };
        form.field_type = SharedString::from(ty.to_string());
        if ty != "TEXT" {
            form.no_stem = false;
        }
        cx.notify();
    }

    pub(super) fn toggle_add_field_flag(&mut self, flag: CreateFieldFlag, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.add_field_form.as_mut() else {
            return;
        };
        match flag {
            CreateFieldFlag::Sortable => form.sortable = !form.sortable,
            CreateFieldFlag::NoStem => form.no_stem = !form.no_stem,
            CreateFieldFlag::NoIndex => form.no_index = !form.no_index,
        }
        cx.notify();
    }

    pub(super) fn submit_add_field(&mut self, cx: &mut gpui::Context<Self>) {
        // A write: asked of the connection as it is now, so a read-only
        // session or a locked production entry cannot reach it by a key
        // press or a stale button either.
        if !self.can_write_index(cx) {
            return;
        }
        let Some(form) = self.add_field_form.as_ref() else {
            return;
        };
        let Some(index) = self.selected_index.clone() else {
            return;
        };
        let name = form.name.read(cx).value().to_string().trim().to_string();
        if name.is_empty() {
            self.error = Some(i18n_search(cx, "create_name_required"));
            cx.notify();
            return;
        }
        let spec = CreateFieldSpec {
            name: SharedString::from(name).to_string(),
            field_type: form.field_type.clone().to_string(),
            sortable: form.sortable,
            no_stem: form.no_stem,
            no_index: form.no_index,
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.altering_index = true;
        self.error = None;
        let index_for_task = index.clone();
        self._alter_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let at = ServerDb::new(&*server_id, db);
                ft_alter_add(&at, index_for_task.as_ref(), &spec).await
            });
            let result: Result<()> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.altering_index = false;
                match result {
                    Ok(()) => {
                        info!(index = %index, "FT.ALTER ADD succeeded");
                        this.add_field_form = None;
                        // Re-pull FT.INFO so the schema panel
                        // reflects the new attribute immediately.
                        this.select_index(index.clone(), cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Append a fresh row to the form. Stable id comes from the form's
    /// `next_field_id` counter.
    pub(super) fn add_create_field(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        let id = form.next_field_id;
        form.next_field_id += 1;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        form.fields.push(CreateFieldRow {
            id,
            name,
            field_type: SharedString::from("TEXT"),
            sortable: false,
            no_stem: false,
            no_index: false,
        });
        cx.notify();
    }

    pub(super) fn remove_create_field(&mut self, id: u64, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        // Don't let the form go to zero rows — `FT.CREATE` requires at
        // least one SCHEMA entry, and the empty-state would be
        // confusing to recover from.
        if form.fields.len() <= 1 {
            return;
        }
        form.fields.retain(|f| f.id != id);
        cx.notify();
    }

    pub(super) fn set_create_field_type(&mut self, id: u64, ty: &str, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        for f in form.fields.iter_mut() {
            if f.id == id {
                f.field_type = SharedString::from(ty.to_string());
                // NOSTEM only applies to TEXT; clear it on type switch
                // so submitting doesn't send a wire option Redis will
                // reject.
                if ty != "TEXT" {
                    f.no_stem = false;
                }
                break;
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_create_field_flag(&mut self, id: u64, flag: CreateFieldFlag, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        for f in form.fields.iter_mut() {
            if f.id == id {
                match flag {
                    CreateFieldFlag::Sortable => f.sortable = !f.sortable,
                    CreateFieldFlag::NoStem => f.no_stem = !f.no_stem,
                    CreateFieldFlag::NoIndex => f.no_index = !f.no_index,
                }
                break;
            }
        }
        cx.notify();
    }

    pub(super) fn set_create_key_type(&mut self, on_json: bool, cx: &mut gpui::Context<Self>) {
        if let Some(form) = self.create_form.as_mut() {
            form.on_json = on_json;
            cx.notify();
        }
    }

    /// Validate the form, build the `CreateIndexOptions`, and dispatch
    /// the FT.CREATE task. On validation failure the error is surfaced
    /// inline and the form stays open; on success the form closes once
    /// the request completes and the new index is auto-selected.
    pub(super) fn submit_create_index(&mut self, cx: &mut gpui::Context<Self>) {
        // A write: asked of the connection as it is now, so a read-only
        // session or a locked production entry cannot reach it by a key
        // press or a stale button either.
        if !self.can_write_index(cx) {
            return;
        }
        let Some(form) = self.create_form.as_ref() else { return };
        let name = form.name.read(cx).value().to_string().trim().to_string();
        if name.is_empty() {
            self.error = Some(i18n_search(cx, "create_name_required"));
            cx.notify();
            return;
        }
        let mut fields = Vec::with_capacity(form.fields.len());
        for f in &form.fields {
            let fname = f.name.read(cx).value().to_string().trim().to_string();
            if fname.is_empty() {
                continue;
            }
            fields.push(CreateFieldSpec {
                name: SharedString::from(fname).to_string(),
                field_type: f.field_type.clone().to_string(),
                sortable: f.sortable,
                no_stem: f.no_stem,
                no_index: f.no_index,
            });
        }
        if fields.is_empty() {
            self.error = Some(i18n_search(cx, "create_fields_required"));
            cx.notify();
            return;
        }

        let opts = CreateIndexOptions {
            index: SharedString::from(name.clone()).to_string(),
            on_json: form.on_json,
            prefixes: split_csv(&form.prefixes.read(cx).value()),
            fields,
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.creating_index = true;
        self.error = None;
        let created_name = SharedString::from(name);
        self._create_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let at = ServerDb::new(&*server_id, db);
                ft_create(&at, &opts).await
            });
            let result: Result<()> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.creating_index = false;
                match result {
                    Ok(()) => {
                        info!(index = created_name.as_ref(), "FT.CREATE succeeded");
                        // Close the form, then refresh the index list
                        // and auto-select the freshly created index so
                        // the user can verify the schema landed.
                        this.create_form = None;
                        this.refresh_indexes(cx);
                        this.select_index(created_name, cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }
}

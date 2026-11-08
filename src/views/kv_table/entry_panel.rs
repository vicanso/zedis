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

//! Entry panel for viewing, previewing (Decoded / JSON Tree / Hex), and editing
//! a single collection row in [`ZedisKvTable`].

use super::*;

/// The entry panel's decoded preview: what a MessagePack / gzip / JSON
/// element decodes to, above the form that edits its bytes.
const PREVIEW_HEIGHT: f32 = 220.0;
/// Bytes per row in the preview's hex view.
const PREVIEW_HEX_BYTES_PER_ROW: usize = 16;

/// Which rendering of the open row's element the preview shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PreviewMode {
    /// The decoded text — or, for an element whose bytes are JSON, that
    /// text as stored (the string editor's *Text*).
    #[default]
    Decoded,
    /// The document as a JSON tree, the string editor's *Tree*.
    Tree,
    /// The stored bytes as a hex dump.
    Hex,
}

impl PreviewMode {
    fn id(self) -> &'static str {
        match self {
            Self::Decoded => "kv-preview-decoded",
            Self::Tree => "kv-preview-tree",
            Self::Hex => "kv-preview-hex",
        }
    }
}

/// The renderings the preview offers: the decoded text, a tree when the
/// element is JSON-shaped, and always the bytes as hex.
pub(super) fn preview_modes(json_shaped: bool) -> Vec<PreviewMode> {
    if json_shaped {
        vec![PreviewMode::Decoded, PreviewMode::Tree, PreviewMode::Hex]
    } else {
        vec![PreviewMode::Decoded, PreviewMode::Hex]
    }
}

/// The rendering a row's element opens in: a tree where it is JSON-shaped
/// and Settings opens JSON as one (`json_view`), as the string editor does;
/// else its text.
pub(super) fn opening_preview(json_shaped: bool, view: JsonView) -> PreviewMode {
    if json_shaped && view == JsonView::Tree {
        PreviewMode::Tree
    } else {
        PreviewMode::Decoded
    }
}

/// Whether the tree may edit the element. Only one whose stored bytes *are*
/// JSON, on a connection and a fetcher that let the row be edited: a tree
/// built from a decoding (MessagePack, a gzip of JSON) is a read-only view
/// of it — ADR 8 — and a row being added has no stored bytes yet.
pub(super) fn tree_editable(format: DataFormat, adding: bool, readonly: bool, readonly_on_edit: bool) -> bool {
    format == DataFormat::Json && !adding && !readonly_on_edit && Capability::MutateContainer.allowed(readonly)
}

/// The JSON document behind the element's preview: the stored text when the
/// bytes are JSON, otherwise what they decode to. `None` when neither
/// parses — a decoding clipped by `max_truncate_length` included, so a tree
/// is never built from a truncated payload.
pub(super) fn element_json_document(element: &KvElement, max_truncate_length: usize) -> Option<Value> {
    let text = match element.format() {
        DataFormat::Json => element.edit_text(),
        _ if element.is_decoded() => detect_and_decode(element.raw(), max_truncate_length).1,
        _ => return None,
    };
    serde_json::from_str(text.as_ref()).ok()
}

/// Whether a row still holds `original` — the value-column texts an editor
/// captured when it opened — given how to read the row's cell for a column
/// index. Every value column must match; a missing cell never does.
pub(super) fn row_holds(
    columns: &[KvTableColumn],
    original: &IndexMap<SharedString, SharedString>,
    cell: impl Fn(usize) -> Option<SharedString>,
) -> bool {
    columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.column_type == KvTableColumnType::Value)
        .all(|(index, column)| match (cell(index), original.get(&column.name)) {
            (Some(current), Some(opened)) => &current == opened,
            _ => false,
        })
}

impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    pub(super) fn is_adding_row(&self) -> bool {
        self.edit_row == Some(usize::MAX)
    }

    pub(super) fn close_row_editor(&mut self) {
        self.edit_row = None;
        self.editor_form = None;
    }

    /// Close the row editor when the row it is addressed by no longer holds
    /// the element it opened on. Update and Remove act on `edit_row`, so an
    /// editor left open over a renumbered table writes to whatever element
    /// moved into that slot — a hash would even take it for a rename and
    /// delete that field. A row that still holds the same values keeps its
    /// editor, so a reload that changed nothing does not throw away typing.
    pub(super) fn close_editor_if_row_moved(&mut self) {
        let Some(row_ix) = self.edit_row.filter(|row| *row != usize::MAX) else {
            return;
        };
        let fetcher = &self.fetcher;
        let unchanged = row_ix < fetcher.rows_count()
            && row_holds(&self.columns, &self.original_values, |column_ix| {
                fetcher.get_edit(row_ix, column_ix + 1)
            });
        if !unchanged {
            self.close_row_editor();
        }
    }

    pub(super) fn handle_select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        // Open the detail panel on select. An action mode (UPDATE/REMOVE/ADD)
        // opens the editor; otherwise `Capability::ViewEntry` still allows a
        // view-only preview (entry contents — e.g. a stream entry's id +
        // message — stay inspectable on read-only connections; the form is
        // disabled and the update/remove actions are hidden below).
        if !Capability::ViewEntry.allowed(self.readonly)
            && !self
                .mode
                .intersects(KvTableMode::UPDATE | KvTableMode::REMOVE | KvTableMode::ADD)
        {
            return;
        }

        // if is add mode, clear the form
        if self.is_adding_row() {
            self.editor_form = None;
        } else {
            self.values_should_fill = true;
        }
        self.edit_row = Some(row_ix);
        self.original_values.clear();
        for (index, column) in self.columns.iter().enumerate() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            let value = self.fetcher.get_edit(row_ix, index + 1).unwrap_or_default();
            self.original_values.insert(column.name.clone(), value);
        }
        // The preview follows the main value column: the flexible one (a
        // hash's value, a member), or the first value column without one.
        let main_column = self
            .columns
            .iter()
            .position(|column| column.column_type == KvTableColumnType::Value && column.flex)
            .or_else(|| {
                self.columns
                    .iter()
                    .position(|column| column.column_type == KvTableColumnType::Value)
            });
        self.edit_element_column = main_column.map(|index| self.columns[index].name.clone());
        self.edit_element = main_column.and_then(|index| self.fetcher.element(row_ix, index + 1));
        let store = cx.global::<ZedisGlobalStore>().read(cx);
        let (max_truncate_length, json_view) = (store.max_truncate_length(), store.json_view());
        self.edit_element_json = self
            .edit_element
            .as_ref()
            .is_some_and(|element| element_json_document(element, max_truncate_length).is_some());
        self.preview = opening_preview(self.edit_element_json, json_view);
        self.json_tree_stale = true;
        self.editor_form = None;
        self.values_modified = false;
    }

    /// The element whose decoding is worth a preview: anything but plain
    /// text, which the form already shows as is.
    fn previewed_element(&self) -> Option<&KvElement> {
        self.edit_element
            .as_ref()
            .filter(|_| self.edit_row.is_some_and(|row| row != usize::MAX))
            .filter(|element| element.format() != DataFormat::Text)
    }

    /// Switch the preview's rendering. Entering the tree marks it stale, so
    /// it is rebuilt from the form's current text rather than from whatever
    /// it showed last.
    fn set_preview(&mut self, mode: PreviewMode, cx: &mut Context<Self>) {
        if self.preview == mode {
            return;
        }
        self.preview = mode;
        if mode == PreviewMode::Tree {
            self.json_tree_stale = true;
        }
        cx.notify();
    }

    /// Hand the tree its document. A JSON element's tree follows the form —
    /// unsaved edits show, and an operation starts from what the user typed
    /// — while a decoded element's tree is the decoding, read-only.
    fn sync_json_tree(&mut self, cx: &mut Context<Self>) {
        self.json_tree_stale = false;
        let Some(element) = self.edit_element.clone() else {
            return;
        };
        let max_truncate_length = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();
        // A number a double cannot hold would be rewritten by any edit to
        // the document, which the tree re-serializes whole: read-only then.
        let (doc, exact) = match (element.format(), &self.editor_form, &self.edit_element_column) {
            (DataFormat::Json, Some(form), Some(column)) => {
                let text = form.read(cx).get_field_value(column, cx);
                (
                    serde_json::from_str::<Value>(text.as_ref()).ok(),
                    numbers_survive_round_trip(text.as_ref()),
                )
            }
            _ => (
                element_json_document(&element, max_truncate_length),
                numbers_survive_round_trip(element.edit_text().as_ref()),
            ),
        };
        let editable = exact
            && tree_editable(
                element.format(),
                self.is_adding_row(),
                self.readonly,
                self.fetcher.readonly_on_edit(),
            );
        self.json_tree.update(cx, |tree, cx| {
            tree.set_document(doc, JsonTreeTarget::Local, editable, cx)
        });
    }

    /// A tree operation rewrote the document: the form's value field takes
    /// the new text, so Save writes it like any typed edit.
    pub(super) fn apply_tree_edit(&mut self, text: SharedString, cx: &mut Context<Self>) {
        let (Some(form), Some(column)) = (&self.editor_form, &self.edit_element_column) else {
            return;
        };
        let column = column.clone();
        form.update(cx, |form, cx| {
            form.schedule_field_update(column, text);
            cx.notify();
        });
        cx.notify();
    }

    /// The decoded rendering, the JSON tree, or the hex of the open row's
    /// element, above the form that edits its bytes.
    fn render_element_preview(&self, element: &KvElement, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let decodable = element.is_decoded() || element.format() == DataFormat::Json;
        let mode = if decodable { self.preview } else { PreviewMode::Hex };
        let modes = preview_modes(self.edit_element_json);
        // A JSON element's decoded rendering is its text as stored, so the
        // switch reads Text / Tree / Hex exactly like the string editor's;
        // any other decoding keeps the Decoded label.
        let stored_json = element.format() == DataFormat::Json;
        let label_for = |mode: PreviewMode| -> SharedString {
            match mode {
                PreviewMode::Decoded if stored_json => i18n_editor(cx, "json_view_text"),
                PreviewMode::Decoded => i18n_kv_table(cx, "preview_decoded"),
                PreviewMode::Tree => i18n_editor(cx, "json_view_tree"),
                PreviewMode::Hex => i18n_kv_table(cx, "preview_hex"),
            }
        };
        let switch = modes.iter().fold(
            ButtonGroup::new("kv-preview-mode").compact().xsmall().outline(),
            |group, candidate| {
                group.child(
                    Button::new(candidate.id())
                        .label(label_for(*candidate))
                        .selected(*candidate == mode),
                )
            },
        );
        let body: AnyElement = if mode == PreviewMode::Tree {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .border_1()
                .border_color(theme.border)
                .rounded(px(4.))
                .child(self.json_tree.clone())
                .into_any_element()
        } else {
            let text: SharedString = if mode == PreviewMode::Hex {
                bytes_to_hex_text(element.raw(), PREVIEW_HEX_BYTES_PER_ROW).into()
            } else {
                let max_len = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();
                detect_and_decode(element.raw(), max_len).1
            };
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .border_1()
                .border_color(theme.border)
                .rounded(px(4.))
                .px_2()
                .py_1()
                .overflow_y_scrollbar()
                .child(Label::new(text).text_xs().font_family(get_mono_font_family()))
                .into_any_element()
        };
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let format_label: SharedString = t!(
            "kv_table.preview_format",
            format = element.format().as_str(),
            locale = &locale
        )
        .to_string()
        .into();
        v_flex()
            .w_full()
            .h(px(PREVIEW_HEIGHT))
            .mb_2()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(format_label).text_xs().text_color(muted))
                    .when(element.is_binary(), |this| {
                        this.child(
                            Label::new(i18n_kv_table(cx, "binary_hex_hint"))
                                .text_xs()
                                .text_color(theme.yellow),
                        )
                    })
                    .child(div().flex_1())
                    .when(decodable, |this| {
                        this.child(
                            switch.on_click(cx.listener(move |this, clicks: &Vec<usize>, _window, cx| {
                                if let Some(mode) = clicks.first().and_then(|ix| modes.get(*ix)) {
                                    this.set_preview(*mode, cx);
                                }
                            })),
                        )
                    }),
            )
            .child(body)
    }

    pub(super) fn enhance_handle_add_or_update_value(
        &mut self,
        data: IndexMap<SharedString, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_ix) = self.edit_row else {
            return;
        };

        // Check if the operation is allowed based on mode
        if row_ix == usize::MAX {
            // Adding new row
            if !self.mode.contains(KvTableMode::ADD) {
                return;
            }
        } else {
            // Updating existing row
            if !self.mode.contains(KvTableMode::UPDATE) {
                return;
            }
        }

        if row_ix == usize::MAX {
            let mut static_field_names = Vec::with_capacity(self.columns.len() + 1);
            if self.fetcher.key_type() == KeyType::List {
                static_field_names.push("position".into());
            }
            for column in &self.columns {
                if column.column_type == KvTableColumnType::Value {
                    static_field_names.push(column.name.clone());
                }
            }
            let batches = build_kv_add_batches(
                self.fetcher.key_type(),
                self.fetcher.include_field_names(),
                &static_field_names,
                &data,
            );
            for batch in batches {
                self.fetcher.handle_add_value(batch, window, cx);
            }
        } else {
            let mut values = Vec::with_capacity(data.len());
            let include_field_names = self.fetcher.include_field_names();
            for (name, value) in data {
                if include_field_names {
                    values.push(name);
                }
                values.push(value);
            }
            self.fetcher.handle_update_value(row_ix, values, window, cx);
        }
        self.editor_form = None;
        self.edit_row = None;
    }

    /// The entry panel: the decoded preview, when the element has one, above
    /// the edit form, which fills what is left.
    pub(super) fn enhance_render_edit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = self.edit_form(window, cx);
        // The form exists now, so a stale tree can read its text.
        if self.preview == PreviewMode::Tree && self.json_tree_stale && self.previewed_element().is_some() {
            self.sync_json_tree(cx);
        }
        let preview = self
            .previewed_element()
            .cloned()
            .map(|element| self.render_element_preview(&element, cx).into_any_element());
        v_flex()
            .size_full()
            .children(preview)
            .child(v_flex().flex_1().min_h_0().w_full().child(form))
    }

    fn edit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ZedisForm> {
        if let Some(form) = &self.editor_form {
            if std::mem::take(&mut self.values_should_fill) {
                let original_values = &self.original_values;
                form.update(cx, move |form, cx| {
                    form.reset_form(original_values, window, cx);
                });
            }
            return form.clone();
        }
        let mut fields = Vec::with_capacity(4);
        let is_adding = self.is_adding_row();
        // A fixed-height editor (a column that is not the flexible one)
        // scales with the font; the flexible column takes whatever height
        // the form has left, so nothing is computed for it.
        let font_scale = window.rem_size().as_f32() / 14.0;
        if is_adding && self.fetcher.key_type() == KeyType::List {
            fields.push(
                ZedisFormField::new("position", i18n_list_editor(cx, "position"))
                    .field_type(ZedisFormFieldType::RadioGroup)
                    .options(vec!["RPUSH".into(), "LPUSH".into()]),
            );
        }

        let mut first = true;
        // A read-only *connection* (no `MutateContainer` capability) makes the
        // edit form view-only too (disabled, empty fields skipped, no
        // add-fields), same as a fetcher that is inherently read-only-on-edit
        // (e.g. streams).
        let readonly_on_edit =
            !is_adding && (!Capability::MutateContainer.allowed(self.readonly) || self.fetcher.readonly_on_edit());
        let pair_companion = is_adding
            .then(|| {
                if !matches!(self.fetcher.key_type(), KeyType::Hash | KeyType::Zset) {
                    return None;
                }
                self.columns
                    .iter()
                    .filter(|c| c.column_type == KvTableColumnType::Value)
                    .nth(1)
                    .map(|c| c.name.clone())
            })
            .flatten();
        for column in self.columns.iter() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            let value = self.original_values.get(&column.name);
            if readonly_on_edit && value.map(|item| item.is_empty()).unwrap_or(true) {
                continue;
            }
            let mut field = ZedisFormField::new(column.name.clone(), column.name.clone())
                .focus()
                .font_family(get_mono_font_family());
            if self.fetcher.fields_required() && !column.optional {
                field = field.required();
            }
            if first {
                field = field.focus().when_some(pair_companion.clone(), |this, companion| {
                    this.paste_pair_with(companion)
                });
                first = false;
            }
            if !is_adding && column.flex {
                field = field.fill();
            }
            if column.verbatim {
                field = field.verbatim();
            }
            if let Some(placeholder) = column.placeholder.clone() {
                field = field.placeholder(placeholder);
            }
            if let Some(field_type) = column.field_type.clone() {
                let resolved_type = if is_adding && field_type == ZedisFormFieldType::Editor {
                    ZedisFormFieldType::AutoGrow(2, 6)
                } else {
                    if field_type == ZedisFormFieldType::Editor && !column.flex {
                        field = field.h(px(150. * font_scale));
                    }
                    field_type
                };
                field = field.field_type(resolved_type);
            }

            if !is_adding && let Some(value) = value {
                field = field.default_value(value.clone());
            }
            fields.push(field);
        }
        let submit_entity = cx.entity().clone();
        let cancel_entity = submit_entity.clone();
        let remove_entity = submit_entity.clone();
        let on_cancel = move |_window: &mut Window, cx: &mut Context<ZedisForm>| {
            cancel_entity.update(cx, |this, _| {
                this.edit_row = None;
            });
            true
        };
        let on_submit =
            move |values: IndexMap<SharedString, SharedString>, window: &mut Window, cx: &mut Context<ZedisForm>| {
                submit_entity.update(cx, |this, cx| {
                    this.enhance_handle_add_or_update_value(values, window, cx);
                });
                true
            };
        let can_remove = self.mode.contains(KvTableMode::REMOVE);
        let can_update = self.mode.contains(KvTableMode::UPDATE);
        let key_type = self.fetcher.key_type();
        // When editing an existing row with a flexible column, the form fills
        // the panel and that column takes the rest; when adding row(s) or
        // viewing a stream entry, keep the scrolling grid so multiple added
        // rows stay compact and scrollable.
        let fills_panel = !is_adding
            && self
                .columns
                .iter()
                .any(|column| column.column_type == KvTableColumnType::Value && column.flex);
        let form_opts = ZedisFormOptions::new(fields)
            .when(fills_panel, |this| this.fill_height())
            .on_cancel(on_cancel)
            .cancel_label(i18n_common(cx, "cancel"))
            .when(is_adding || can_update, |this| {
                this.on_submit(on_submit)
                    .when_some(hot_key_label("save"), |this, shortcut| this.confirm_tooltip(shortcut))
            })
            .when_else(
                is_adding,
                |this| this.confirm_label(i18n_common(cx, "save")),
                |this| this.confirm_label(i18n_common(cx, "update")),
            )
            .when(!is_adding && can_remove, |this| {
                let remove_label = i18n_common(cx, "remove");
                this.foot_actions(move |_window, _cx| {
                    vec![
                        Button::new("remove-edit-btn")
                            .icon(CustomIconName::FileXCorner)
                            .label(remove_label.clone())
                            .on_click({
                                let remove_entity = remove_entity.clone();
                                move |_, window, cx| {
                                    remove_entity.update(cx, |this, cx| {
                                        this.handle_remove_row(window, cx);
                                    });
                                }
                            }),
                    ]
                })
            })
            .when(!readonly_on_edit && self.fetcher.support_add_fields(), |this| {
                this.support_add_fields()
            })
            .when(is_adding && !readonly_on_edit && key_type == KeyType::List, |this| {
                this.support_add_single_fields(i18n_common(cx, "value_placeholder"), 0)
            })
            .when(is_adding && !readonly_on_edit && key_type == KeyType::Set, |this| {
                this.support_add_single_fields(i18n_common(cx, "member_placeholder"), 0)
            })
            .when(is_adding && !readonly_on_edit && key_type == KeyType::Zset, |this| {
                this.support_add_pair_fields(
                    i18n_common(cx, "member_placeholder"),
                    i18n_common(cx, "score_placeholder"),
                    0,
                )
            })
            .when(is_adding && !readonly_on_edit && key_type == KeyType::Hash, |this| {
                this.support_add_pair_fields(
                    i18n_common(cx, "field_placeholder"),
                    i18n_common(cx, "value_placeholder"),
                    0,
                )
            });

        let form = cx.new(|cx| {
            let mut f = ZedisForm::new("kv-table-edit-form", form_opts, window, cx);
            if readonly_on_edit {
                f.set_disabled(true, cx);
            }
            f
        });
        self.editor_form = Some(form.clone());
        form
    }
}

pub(super) fn build_kv_add_batches(
    key_type: KeyType,
    include_field_names: bool,
    static_field_names: &[SharedString],
    data: &IndexMap<SharedString, SharedString>,
) -> Vec<Vec<SharedString>> {
    if include_field_names {
        let mut values = Vec::with_capacity(data.len() * 2);
        for (name, value) in data {
            values.push(decode_extra_pair_key(name.as_ref()));
            values.push(value.clone());
        }
        return vec![values];
    }

    let primary: Vec<SharedString> = static_field_names
        .iter()
        .filter_map(|name| data.get(name).cloned())
        .collect();
    let mut batches = vec![primary];
    let static_refs: Vec<&str> = static_field_names.iter().map(|s| s.as_ref()).collect();

    match key_type {
        KeyType::List => {
            let pos = data.get("position").cloned().unwrap_or_else(|| "0".into());
            for extra in extra_single_rows(data) {
                if !extra.is_empty() {
                    batches.push(vec![pos.clone(), extra]);
                }
            }
        }
        KeyType::Set => {
            for extra in extra_single_rows(data) {
                if !extra.is_empty() {
                    batches.push(vec![extra]);
                }
            }
        }
        KeyType::Zset => {
            for (member, score) in extra_pair_rows(data, &static_refs) {
                if !member.is_empty() {
                    let score_val = if score.trim().is_empty() {
                        "0".into()
                    } else {
                        score.trim().to_string().into()
                    };
                    batches.push(vec![member, score_val]);
                }
            }
        }
        KeyType::Hash => {
            let ttl = static_field_names
                .get(2)
                .and_then(|ttl_name| data.get(ttl_name))
                .cloned()
                .unwrap_or_default();
            for (field, value) in extra_pair_rows(data, &static_refs) {
                if !field.is_empty() {
                    batches.push(vec![field, value, ttl.clone()]);
                }
            }
        }
        _ => {}
    }

    batches
}

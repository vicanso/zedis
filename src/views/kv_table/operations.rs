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

//! Row and table operations for [`ZedisKvTable`]: CSV export, bulk paste,
//! add / remove / filter, type-native key ops, and the row right-click menu.

use super::*;

/// Parse pasted text into rows of values for bulk insertion.
///
/// - Lines are split on `\n` (also handles `\r\n` because `lines()`
///   trims `\r`). Trimmed-empty lines are skipped.
/// - For `expected_columns <= 1` the entire trimmed line is one
///   value (commas/tabs are preserved verbatim).
/// - For `expected_columns >= 2` the line is split into at most that
///   many parts. Separator is auto-detected per-line: tab takes
///   precedence over comma so users can paste Redis values that
///   themselves contain commas as long as their rows use tabs.
///   Use `splitn(N, ...)` so the trailing column absorbs any extra
///   separator runs — handy when a hash value happens to contain a
///   comma. Missing trailing columns are padded with empty strings
///   so the row always has exactly `expected_columns` slots.
/// - Per-cell trimming strips leading/trailing whitespace.
pub(crate) fn parse_bulk_rows(text: &str, expected_columns: usize) -> Vec<Vec<SharedString>> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if expected_columns <= 1 {
            rows.push(vec![SharedString::from(line.to_string())]);
            continue;
        }
        let sep: char = if line.contains('\t') { '\t' } else { ',' };
        let mut row: Vec<SharedString> = line
            .splitn(expected_columns, sep)
            .map(|part| SharedString::from(part.trim().to_string()))
            .collect();
        while row.len() < expected_columns {
            row.push(SharedString::default());
        }
        rows.push(row);
    }
    rows
}

impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    /// Export the currently-loaded rows to a CSV file via the shared
    /// `export_to_file` flow. Headers come from the table columns and cells
    /// from the fetcher, so one implementation covers Hash/List/Set/Zset/Stream.
    /// Honest about completeness: when the table isn't fully loaded the success
    /// notification notes "loaded / total", so a partial export is never
    /// mistaken for the whole collection.
    pub(super) fn export_csv(&mut self, cx: &mut Context<Self>) {
        if self.items_count == 0 || !Capability::ExportCsv.allowed(self.readonly) {
            return;
        }
        let headers: Vec<&str> = self.columns.iter().map(|c| c.name.as_ref()).collect();
        let rows: Vec<Vec<String>> = (0..self.items_count)
            .map(|r| {
                // The delegate prepends an Index ("#") column at position 0,
                // so the fetcher's value columns (matching `self.columns`)
                // start at delegate index 1 — offset `get` accordingly.
                (0..self.columns.len())
                    .map(|c| self.fetcher.get(r, c + 1).map(|s| s.to_string()).unwrap_or_default())
                    .collect()
            })
            .collect();
        let csv = build_csv(&headers, &rows);

        // Filename from the key, sanitized so a name with `:` / `/` can't
        // break the suggested save path.
        let key = self.server_state.read(cx).key().unwrap_or_default();
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let suggested = format!("{}.csv", if safe.is_empty() { "export" } else { safe.as_str() });

        let success: SharedString = if self.done {
            i18n_common(cx, "csv_exported")
        } else {
            format!(
                "{} ({} / {})",
                i18n_common(cx, "csv_exported"),
                self.items_count,
                self.total_count
            )
            .into()
        };
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            &suggested,
            success,
            error,
        );
    }

    /// Open a dialog to paste many rows at once. The pasted text is
    /// parsed into rows (one per line, tab-separated preferred,
    /// comma fallback) and each row is dispatched through the same
    /// `handle_add_value` path as the single-row form, so backend
    /// semantics (HSET, LPUSH/RPUSH, SADD, ZADD) stay identical.
    ///
    /// Per key type the dialog adapts:
    /// - Hash: 2 cols → "field<sep>value"
    /// - ZSet: 2 cols → "member<sep>score"  (score must parse as f64
    ///   downstream — invalid scores fall back to 0.0 like the
    ///   single-row form does)
    /// - List: 1 col → "value" per line, always pushed as RPUSH for
    ///   bulk paste so the visual top-to-bottom order in the textarea
    ///   matches the resulting list order
    /// - Set:  1 col → "member" per line
    ///
    /// Stream/other key types fall through (no-op).
    pub(super) fn handle_bulk_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.contains(KvTableMode::ADD) {
            return;
        }
        let key_type = self.fetcher.key_type();
        let column_count: usize = match key_type {
            KeyType::Hash | KeyType::Zset => 2,
            KeyType::List | KeyType::Set => 1,
            _ => return,
        };

        let placeholder = match key_type {
            KeyType::Hash => i18n_kv_table(cx, "bulk_add_placeholder_hash"),
            KeyType::Zset => i18n_kv_table(cx, "bulk_add_placeholder_zset"),
            KeyType::List => i18n_kv_table(cx, "bulk_add_placeholder_list"),
            KeyType::Set => i18n_kv_table(cx, "bulk_add_placeholder_set"),
            _ => SharedString::default(),
        };
        let hint = i18n_kv_table(cx, "bulk_add_hint");
        let success_template = i18n_kv_table(cx, "bulk_add_success");
        let empty_label = i18n_kv_table(cx, "bulk_add_empty");

        let textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 20)
                .placeholder(placeholder.clone())
        });
        let body_state = textarea.clone();
        let submit_state = textarea.clone();
        let fetcher = self.fetcher.clone();

        ZedisDialog::new(i18n_kv_table(cx, "bulk_add_title"))
            .w(px(680.))
            .ok_text(i18n_common(cx, "save"))
            .cancel_text(i18n_common(cx, "cancel"))
            .child(move || {
                v_flex()
                    .gap_2()
                    .w_full()
                    .child(Label::new(hint.clone()).text_xs())
                    .child(Textarea::new(&body_state).appearance(true))
            })
            .on_ok(move |_, window, cx| {
                let text = submit_state.read(cx).value().to_string();
                let rows = parse_bulk_rows(&text, column_count);
                if rows.is_empty() {
                    window.push_notification(Notification::warning(empty_label.clone()), cx);
                    return false;
                }
                let imported = rows.len();
                for mut row in rows {
                    if key_type == KeyType::List {
                        // Prepend the position selector; matches the
                        // [position, value] shape the list fetcher
                        // expects.
                        row.insert(0, SharedString::from("RPUSH"));
                    }
                    fetcher.handle_add_value(row, window, cx);
                }
                let msg = success_template.replace("%{count}", &imported.to_string());
                window.push_notification(Notification::success(SharedString::from(msg)), cx);
                true
            })
            .open(window, cx);
    }

    pub(super) fn handle_add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only allow adding if ADD mode is enabled
        if !self.mode.contains(KvTableMode::ADD) {
            return;
        }

        self.edit_row = Some(usize::MAX);
        self.list_push_mode_state.update(cx, |state, _| {
            *state = 0;
        });
        let mut focused = false;
        self.value_states.iter().for_each(|(index, state)| {
            let auto_created = self
                .columns
                .get(*index)
                .map(|column| column.auto_created)
                .unwrap_or(false);

            state.update(cx, |input, cx| {
                if !auto_created && !focused {
                    input.focus(window, cx);
                    focused = true;
                }
                input.set_value(SharedString::default(), window, cx);
            });
        });
        self.editor_form = None;
        self.original_values.clear();
        self.values_modified = false;
    }

    /// Triggers a filter operation using the current keyword from the input field.
    pub(super) fn handle_filter(&mut self, cx: &mut Context<Self>) {
        // Only allow filtering if FILTER mode is enabled
        if !self.mode.contains(KvTableMode::FILTER) {
            return;
        }

        let keyword = self.keyword_state.read(cx).value();
        self.applied_keyword = keyword.clone();
        self.loading = true;
        // A different keyword means different rows behind the same indices,
        // so neither the open editor nor the ticks can carry over. (Adding a
        // row is not tied to an index and stays open.)
        if !self.is_adding_row() {
            self.close_row_editor();
        }
        self.table_state.update(cx, |state, cx| {
            state.delegate_mut().clear_selection();
            state.delegate().fetcher().filter(keyword, cx);
        });
    }

    /// The type-native operations this key type offers, or an empty list
    /// when it has none. Gated on the same capability as any other write.
    pub(super) fn key_ops(&self) -> &'static [KeyOpAction] {
        if !Capability::MutateContainer.allowed(self.readonly) {
            return &[];
        }
        match self.fetcher.key_type() {
            KeyType::List => &[
                KeyOpAction::ListTrim,
                KeyOpAction::ListPopHead,
                KeyOpAction::ListPopTail,
            ],
            KeyType::Zset => &[
                KeyOpAction::ZsetIncrBy,
                KeyOpAction::ZsetPopMin,
                KeyOpAction::ZsetPopMax,
            ],
            KeyType::Hash => &[KeyOpAction::HashIncrBy],
            _ => &[],
        }
    }

    /// Open the shared form for `action`, then run the operation it builds.
    ///
    /// A destructive one gets a second dialog rather than a checkbox in the
    /// first: `LTRIM` and the pops throw data away, and the form that
    /// collects the numbers is the wrong place to also be the warning.
    pub(super) fn open_key_op_dialog(&mut self, action: KeyOpAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        open_key_op_dialog(self.server_state.clone(), key, action, window, cx);
    }

    /// Delete every ticked row in one command, behind one confirmation.
    ///
    /// The count is what the dialog names — listing fifty members would not
    /// help anyone decide, and the ticks are still on screen behind it.
    pub(super) fn handle_remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.contains(KvTableMode::REMOVE) {
            return;
        }
        let rows = self.table_state.read(cx).delegate().selected_rows();
        if rows.is_empty() {
            return;
        }
        let fetcher = self.fetcher.clone();
        let entity = cx.entity().clone();
        let table_state = self.table_state.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("common.remove_selected_prompt", count = rows.len(), locale = locale);
        let title = i18n_common(cx, "remove_title");

        ZedisDialog::new_alert(title, message.to_string())
            .danger()
            .ok_text(i18n_common(cx, "delete"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, window, cx| {
                fetcher.remove_many(&rows, cx);
                table_state.update(cx, |state, _| state.delegate_mut().clear_selection());
                entity.update(cx, |this, _cx| {
                    this.edit_row = None;
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    pub(super) fn handle_remove_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_ix) = self.edit_row else {
            return;
        };
        self.confirm_remove_row(row_ix, window, cx);
    }

    /// What a row's right-click menu offers beyond copying: what a click on
    /// the row would open, and whether the panel then edits it or only shows
    /// it — the same tests `handle_select_row` and the form make.
    pub(super) fn row_menu(&self, cx: &App) -> KvRowMenu {
        let mutates = self
            .mode
            .intersects(KvTableMode::UPDATE | KvTableMode::REMOVE | KvTableMode::ADD);
        let editable = self.mode.contains(KvTableMode::UPDATE)
            && Capability::MutateContainer.allowed(self.readonly)
            && !self.fetcher.readonly_on_edit();
        KvRowMenu {
            openable: Capability::ViewEntry.allowed(self.readonly) || mutates,
            editable,
            removable: self.mode.contains(KvTableMode::REMOVE),
            ttl: editable && self.fetcher.supports_row_ttl(cx),
        }
    }

    /// Carry out a row-menu choice.
    pub(super) fn run_row_action(&mut self, action: KvRowAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            // As a click: the table selects the row and says so, and the
            // subscription to that opens the panel.
            KvRowAction::Open(row_ix) => self
                .table_state
                .update(cx, |state, cx| state.set_selected_row(row_ix, cx)),
            KvRowAction::Remove(row_ix) => self.confirm_remove_row(row_ix, window, cx),
            KvRowAction::SetTtl(row_ix) => {
                let name = self
                    .fetcher
                    .get(row_ix, self.fetcher.primary_index())
                    .unwrap_or_default();
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let prompt = t!("kv_table.field_ttl_prompt", field = name, locale = locale).to_string();
                let fetcher = self.fetcher.clone();
                open_ttl_dialog(
                    prompt.into(),
                    false,
                    move |secs, _, cx| fetcher.set_row_ttl(row_ix, Some(secs), cx),
                    window,
                    cx,
                );
            }
            // At once, as the editor's Remove TTL is: nothing is destroyed,
            // and the row's TTL cell says what happened.
            KvRowAction::RemoveTtl(row_ix) => self.fetcher.set_row_ttl(row_ix, None, cx),
        }
    }

    /// Ask before deleting the row at `row_ix` — the one open in the entry
    /// panel, or one picked from its right-click menu.
    fn confirm_remove_row(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Only allow removing if REMOVE mode is enabled
        if !self.mode.contains(KvTableMode::REMOVE) {
            return;
        }
        let fetcher = self.fetcher.clone();
        let value = fetcher.get(row_ix, fetcher.primary_index()).unwrap_or_default();
        let entity = cx.entity().clone();

        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!(
            "common.remove_item_prompt",
            row = row_ix + 1,
            value = value,
            locale = locale
        );
        let title = i18n_common(cx, "remove_title");

        ZedisDialog::new_alert(title, message.to_string())
            .danger()
            .ok_text(i18n_common(cx, "delete"))
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, window, cx| {
                fetcher.remove(row_ix, cx);
                entity.update(cx, |this, _cx| {
                    this.edit_row = None;
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
}

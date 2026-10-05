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

//! Redis HASH editor UI component.
//!
//! This module provides a table-based editor for viewing and managing Redis HASH values.
//! It supports operations like:
//! - Viewing HASH field-value pairs in a two-column table
//! - Adding new fields with values via a dialog form
//! - Updating values of existing fields (inline editing)
//! - Removing field-value pairs
//! - Filtering fields with pattern matching
//! - Incremental loading of large HASHes with pagination

use crate::{
    components::KvTableColumn,
    components::ZedisKvFetcher,
    helpers::{format_duration, ttl_secs},
    states::{KeyType, KvElement, RedisValue, ZedisServerState, i18n_kv_table},
    views::{ZedisKvTable, kv_table::define_kv_editor},
};
use gpui::{App, Entity, SharedString, Window, prelude::*};
use std::time::Duration;
use zedis_ui::ZedisFormFieldType;

/// Column index for the TTL column (1-based, after field and value columns).
const TTL_COL_IX: usize = 3;
/// Fixed widths of the Field and TTL columns, so the value — what the table
/// is opened to read — takes everything else. Field used to be 40% of the
/// width and TTL 120px, which left the value a third of a default window.
const FIELD_WIDTH: f32 = 200.;
const TTL_WIDTH: f32 = 90.;

/// Data adapter for Redis HASH values to work with the KV table component.
///
/// This struct implements the `ZedisKvFetcher` trait to provide data access
/// and operations for the two-column table view (field and value columns).
/// On Redis 7.4+ an extra TTL column is shown and editable.
struct ZedisHashValues {
    /// Current Redis HASH value data
    value: RedisValue,
    /// Reference to server state for executing Redis operations
    server_state: Entity<ZedisServerState>,
}

/// What a field's TTL cell asks for: `Ok(None)` when empty, `Ok(Some(secs))`
/// for a TTL of at least a second (`ttl_secs`: `90`, `1h30m`), `Err` for
/// anything else — `1,5h`, `30mm`, or a `500ms` that would truncate to 0.
fn field_ttl_input(text: &str) -> Result<Option<i64>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    ttl_secs(text)
        .and_then(|secs| i64::try_from(secs).ok())
        .map(Some)
        .ok_or(())
}

impl ZedisKvFetcher for ZedisHashValues {
    fn key_type(&self) -> KeyType {
        KeyType::Hash
    }

    /// Creates a new data adapter instance.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }

    /// Retrieves a cell value for the table at the given row and column.
    ///
    /// Column layout:
    /// - Column 1: Field name
    /// - Column 2: Field value
    /// - Column 3: TTL in seconds (Redis 7.4+, empty string = no expiry)
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let hash = self.value.hash_value()?;
        let (field, value) = hash.values.get(row_ix)?;

        if col_ix == TTL_COL_IX {
            return Some(match hash.field_ttls.get(field.text()).copied() {
                Some(t) => format_duration(Duration::from_secs(t as u64)).into(),
                None => SharedString::default(),
            });
        }

        if col_ix == 2 {
            Some(value.text().clone())
        } else {
            Some(field.text().clone())
        }
    }

    /// The edit form starts from the bytes as stored (hex for a binary
    /// element), and from raw seconds for the TTL column where `get`
    /// shows a human-readable duration.
    fn get_edit(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        if col_ix == TTL_COL_IX {
            let hash = self.value.hash_value()?;
            let (field, _) = hash.values.get(row_ix)?;
            return Some(match hash.field_ttls.get(field.text()).copied() {
                Some(t) => format!("{}", t).into(),
                None => SharedString::default(),
            });
        }
        self.element(row_ix, col_ix).map(|element| element.edit_text())
    }

    fn element(&self, row_ix: usize, col_ix: usize) -> Option<KvElement> {
        let (field, value) = self.value.hash_value()?.values.get(row_ix)?;
        match col_ix {
            1 => Some(field.clone()),
            2 => Some(value.clone()),
            _ => None,
        }
    }

    /// Returns the total number of fields in the HASH (from Redis HLEN).
    fn count(&self) -> usize {
        self.value.hash_value().map_or(0, |v| v.size)
    }

    /// Returns the number of currently loaded rows (not total HASH size).
    ///
    /// This may be less than `count()` if pagination is in progress.
    fn rows_count(&self) -> usize {
        self.value.hash_value().map_or(0, |v| v.values.len())
    }

    /// Checks if all HASH fields have been loaded via HSCAN.
    ///
    /// Returns `true` when the cursor has completed iteration (cursor == 0).
    fn is_done(&self) -> bool {
        self.value.hash_value().is_some_and(|v| v.done)
    }

    /// Triggers loading of the next batch of HASH field-value pairs.
    ///
    /// Uses cursor-based pagination via HSCAN to load more values.
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_hash_value(cx);
        });
    }

    /// Removes a field-value pair from the HASH at the given index.
    ///
    /// Executes Redis HDEL command to delete the field.
    fn supports_batch_remove(&self) -> bool {
        true
    }

    fn remove_many(&self, rows: &[usize], cx: &mut App) {
        let Some(hash) = self.value.hash_value() else {
            return;
        };
        let fields: Vec<KvElement> = rows
            .iter()
            .filter_map(|row| hash.values.get(*row).map(|(field, _)| field.clone()))
            .collect();
        self.server_state.update(cx, |this, cx| {
            this.remove_hash_values(fields, cx);
        });
    }

    fn remove(&self, index: usize, cx: &mut App) {
        // Get the HASH field at the specified index
        let Some(hash) = self.value.hash_value() else {
            return;
        };
        let Some((field, _value)) = hash.values.get(index).cloned() else {
            return;
        };

        // Execute removal operation
        self.server_state.update(cx, |this, cx| {
            this.remove_hash_value(field, cx);
        });
    }

    /// Applies a filter to HASH fields by pattern matching.
    ///
    /// Resets the scan and loads fields matching the keyword pattern.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_hash_value(keyword, cx);
        });
    }

    /// Handles inline editing of a HASH field's value (and optionally TTL).
    ///
    /// Called when the user saves edits in the table row form.
    /// values[0] = new field name, values[1] = new value, values[2] = new TTL (optional).
    fn handle_update_value(&self, row_ix: usize, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let Some(field) = values.first() else {
            return;
        };
        let Some(value) = values.get(1) else {
            return;
        };
        let Some((old_field, old_value)) = self.value.hash_value().and_then(|v| v.values.get(row_ix).cloned()) else {
            return;
        };

        // The TTL cell: None leaves the field's TTL as it is, Some(-1)
        // removes it, Some(secs) sets it. Text that is not a TTL is refused
        // with the reason — it used to become "no expiry" without a word.
        let old_secs = self
            .value
            .hash_value()
            .and_then(|h| h.field_ttls.get(old_field.text()).copied());
        let ttl: Option<i64> = match values.get(2).map(|text| field_ttl_input(text)) {
            None => None,
            Some(Err(())) => {
                self.server_state.update(cx, |this, cx| {
                    this.emit_error_notification(i18n_kv_table(cx, "field_ttl_invalid"), cx);
                });
                return;
            }
            // Emptied: remove a TTL that was there, otherwise nothing to do.
            Some(Ok(None)) => old_secs.map(|_| -1),
            // Only sent when it changed.
            Some(Ok(Some(secs))) => (Some(secs) != old_secs).then_some(secs),
        };

        // The form edits a binary element as hex; the bytes are what go
        // back to the server.
        let (Ok(field), Ok(value)) = (old_field.bytes_from_edit(field), old_value.bytes_from_edit(value)) else {
            self.server_state.update(cx, |this, cx| {
                this.emit_error_notification(i18n_kv_table(cx, "hex_invalid"), cx);
            });
            return;
        };
        self.server_state.update(cx, |this, cx| {
            this.update_hash_value(old_field, field, value, ttl, cx);
        });
    }

    /// Adds a new field-value pair to the HASH.
    ///
    /// `values[0]` = field, `values[1]` = value, `values[2]` = optional TTL
    /// seconds (present only on Redis 7.4+, empty = no expiry).
    fn handle_add_value(&self, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let Some(field) = values.first().cloned() else {
            return;
        };
        let Some(value) = values.get(1).cloned() else {
            return;
        };
        // Optional per-field TTL; empty / absent means "no expiry", and text
        // that is not a TTL is refused rather than dropped.
        let ttl: Option<i64> = match values.get(2).map(|text| field_ttl_input(text)) {
            None | Some(Ok(None)) => None,
            Some(Ok(Some(secs))) => Some(secs),
            Some(Err(())) => {
                self.server_state.update(cx, |this, cx| {
                    this.emit_error_notification(i18n_kv_table(cx, "field_ttl_invalid"), cx);
                });
                return;
            }
        };

        let server_state = self.server_state.clone();
        server_state.update(cx, |this, cx| {
            this.add_hash_value(field, value, ttl, cx);
        });
    }
}
define_kv_editor!(ZedisHashEditor, ZedisHashValues);

impl ZedisHashEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let supports_field_ttl = server_state.read(cx).supports_hash_field_ttl();

        let mut columns = vec![
            KvTableColumn::new(i18n_kv_table(cx, "field").as_ref(), Some(FIELD_WIDTH)).verbatim(),
            KvTableColumn::new_flex(i18n_kv_table(cx, "value").as_ref())
                .field_type(ZedisFormFieldType::Editor)
                .verbatim(),
        ];
        if supports_field_ttl {
            // TTL column: shows seconds remaining (empty = no expiry). Optional
            // so the add/edit form never forces a TTL.
            columns.push(KvTableColumn::new(i18n_kv_table(cx, "ttl_seconds").as_ref(), Some(TTL_WIDTH)).optional());
        }

        let table_state = cx.new(|cx| ZedisKvTable::<ZedisHashValues>::new(columns, server_state, window, cx));

        Self { table_state }
    }
}

#[cfg(test)]
mod tests {
    use super::field_ttl_input;

    #[test]
    fn a_field_ttl_is_empty_a_real_ttl_or_refused() {
        assert_eq!(field_ttl_input("  "), Ok(None));
        assert_eq!(field_ttl_input("90"), Ok(Some(90)));
        assert_eq!(field_ttl_input("1h30m"), Ok(Some(5400)));
        for text in ["1,5h", "30mm", "500ms", "0"] {
            assert_eq!(field_ttl_input(text), Err(()), "{text}");
        }
    }
}

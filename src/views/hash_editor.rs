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
    components::ZedisKvFetcher,
    states::{KeyType, RedisValue, ZedisServerState},
    components::KvTableColumn,
    views::{ZedisKvTable, kv_table::define_kv_editor},
};
use gpui::{App, Entity, SharedString, Window, prelude::*};
use zedis_ui::ZedisFormFieldType;

/// Data adapter for Redis HASH values to work with the KV table component.
///
/// This struct implements the `ZedisKvFetcher` trait to provide data access
/// and operations for the two-column table view (field and value columns).
struct ZedisHashValues {
    /// Current Redis HASH value data
    value: RedisValue,
    /// Reference to server state for executing Redis operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisHashValues {
    fn key_type(&self) -> KeyType { KeyType::Hash }

    /// Creates a new data adapter instance.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }

    /// Retrieves a cell value for the table at the given row and column.
    ///
    /// Column layout:
    /// - Column 1: Field name
    /// - Column 2: Field value
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let hash = self.value.hash_value()?;
        let (field, value) = hash.values.get(row_ix)?;

        // Column 2 is the value, others show the field name
        if col_ix == 2 {
            Some(value.clone())
        } else {
            Some(field.clone())
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

    /// Handles inline editing of a HASH field's value.
    ///
    /// Called when the user edits the value column directly in the table.
    /// Updates the value for the existing field using Redis HSET.
    fn handle_update_value(&self, row_ix: usize, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        // Extract field name and new value from values
        let Some(field) = values.first() else {
            return;
        };
        let Some(value) = values.get(1) else {
            return;
        };
        let Some(old_field) = self
            .value
            .hash_value()
            .and_then(|v| v.values.get(row_ix).map(|(field, _)| field.clone()))
        else {
            return;
        };

        // Execute update operation
        self.server_state.update(cx, |this, cx| {
            this.update_hash_value(old_field, field.clone(), value.clone(), cx);
        });
    }

    /// Adds a new field-value pair to the HASH.
    fn handle_add_value(&self, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        // Validate that both field and value were provided
        if values.len() != 2 {
            return;
        }

        let server_state = self.server_state.clone();
        // Execute the add operation on server state
        server_state.update(cx, |this, cx| {
            this.add_hash_value(values[0].clone(), values[1].clone(), cx);
        });
    }
}
define_kv_editor!(ZedisHashEditor, ZedisHashValues);

impl ZedisHashEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_width = window.viewport_size().width.to_f64();
        let field_width = if window_width > 1800. {
            0.2
        } else if window_width > 1400. {
            0.3
        } else {
            0.4
        };

        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisHashValues>::new(
                vec![
                    KvTableColumn::new("Field", Some(field_width)),
                    KvTableColumn::new_flex("Value").field_type(ZedisFormFieldType::Editor),
                ],
                server_state,
                window,
                cx,
            )
        });

        Self { table_state }
    }
}

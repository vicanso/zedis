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

use crate::{
    components::ZedisKvFetcher,
    helpers::fast_contains_ignore_case,
    states::{KeyType, RedisValue, ZedisServerState},
    views::{KvTableColumn, KvTableMode, ZedisKvTable},
};
use gpui::{App, Entity, SharedString, Window, div, prelude::*};

fn new_fields(value: &RedisValue) -> Vec<SharedString> {
    let mut fields = if let Some(stream_value) = value.stream_value() {
        stream_value.fields()
    } else {
        vec![]
    };
    fields.insert(0, "Entry Id".to_string().into());
    fields
}

/// Manages Redis Stream values and their display state.
///
/// Handles both filtered and unfiltered views of stream data, maintaining
/// a mapping between visible items and their original indices when filtering.
struct ZedisStreamValues {
    /// Currently visible entry indices (filtered subset or all entries)
    visible_entry_indexes: Vec<usize>,
    /// Maps visible entry indices to original stream entry indices (Some when filtered, None otherwise)
    visible_item_indexes: Option<Vec<usize>>,
    /// The underlying Redis value data
    value: RedisValue,
    /// Field names for the stream
    fields: Vec<SharedString>,
    /// Reference to server state for performing operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisStreamValues {
    /// Recalculates visible entries based on the current keyword filter.
    ///
    /// When a keyword is present:
    /// - Filters entries by checking if any field value contains the keyword (case-insensitive)
    /// - Maintains index mapping to original positions
    ///
    /// When no keyword:
    /// - Shows all entries directly
    /// - Clears index mapping
    fn recalc_visible_items(&mut self) {
        let Some(stream_value) = self.value.stream_value() else {
            return;
        };

        let keyword = stream_value.keyword.clone().unwrap_or_default().to_lowercase();

        // No filter: show all entries
        if keyword.is_empty() {
            self.visible_entry_indexes = (0..stream_value.values.len()).collect();
            self.visible_item_indexes = None;
            return;
        }

        // Filter entries by keyword (search in entry_id and all field values)
        let capacity = stream_value.values.len().max(100) / 10;
        let mut visible_item_indexes = Vec::with_capacity(capacity);
        let mut visible_entry_indexes = Vec::with_capacity(capacity);

        for (index, (entry_id, values)) in stream_value.values.iter().enumerate() {
            // Check entry_id
            if fast_contains_ignore_case(entry_id.as_str(), &keyword) {
                visible_item_indexes.push(index);
                visible_entry_indexes.push(index);
                continue;
            }

            // Check all field values
            let mut found = false;
            for (_, value) in values.iter() {
                if fast_contains_ignore_case(value.as_str(), &keyword) {
                    found = true;
                    break;
                }
            }

            if found {
                visible_item_indexes.push(index);
                visible_entry_indexes.push(index);
            }
        }

        self.visible_entry_indexes = visible_entry_indexes;
        self.visible_item_indexes = Some(visible_item_indexes);
    }
}

impl ZedisKvFetcher for ZedisStreamValues {
    fn key_type(&self) -> KeyType {
        KeyType::Stream
    }
    fn primary_index(&self) -> usize {
        1
    }
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        let fields = new_fields(&value);
        let mut this = Self {
            server_state,
            value,
            fields,
            visible_entry_indexes: Vec::default(),
            visible_item_indexes: None,
        };

        this.recalc_visible_items();
        this
    }

    /// Retrieves the value at the specified row and column index.
    ///
    /// Returns from the filtered visible entries when a keyword filter is active,
    /// otherwise returns directly from the original stream values.
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let stream_value = self.value.stream_value()?;
        if col_ix == 0 {
            return None;
        }

        // Map visible row index to real entry index
        let real_row_ix = *self.visible_entry_indexes.get(row_ix)?;

        if col_ix == 1 {
            return stream_value.get_entry_id(real_row_ix);
        }
        let field = self.fields.get(col_ix - 1)?;
        stream_value.get_field_value(real_row_ix, field)
    }

    /// Returns the total count of entries in the Redis stream (from XLEN).
    fn count(&self) -> usize {
        self.value.stream_value().map_or(0, |v| v.size)
    }

    /// Returns the number of currently visible rows.
    ///
    /// When filtered, returns the count of matching entries.
    /// Otherwise, returns the count of loaded entries.
    fn rows_count(&self) -> usize {
        self.visible_entry_indexes.len()
    }

    fn is_done(&self) -> bool {
        self.value.stream_value().is_some_and(|v| v.done)
    }

    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_stream_value(cx);
        });
    }

    /// Removes the entry at the specified visible index.
    ///
    /// When a filter is active, maps the visible index to the real index
    /// in the underlying stream before performing the deletion (XDEL command).
    fn remove(&self, index: usize, cx: &mut App) {
        let Some(stream) = self.value.stream_value() else {
            return;
        };

        // Map visible index to real index
        let real_index = *self.visible_entry_indexes.get(index).unwrap_or(&index);

        let Some(entry_id) = stream.get_entry_id(real_index) else {
            return;
        };
        self.server_state.update(cx, |this, cx| {
            this.remove_stream_value(entry_id, cx);
        });
    }

    /// Applies a keyword filter to the stream entries.
    ///
    /// Searches only within already loaded entries for matching entry IDs or field values.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            state.filter_stream_value(keyword, cx);
        });
    }

    fn handle_update_value(&self, _row_ix: usize, _values: Vec<SharedString>, _window: &mut Window, _cx: &mut App) {}

    fn handle_add_value(&self, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let field_values: Vec<(SharedString, SharedString)> = self
            .fields
            .iter()
            .zip(values.iter())
            .map(|(field, value)| (field.clone(), value.clone()))
            .collect();

        let entry_id = field_values
            .first()
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty());

        let field_values: Vec<(SharedString, SharedString)> = field_values
            .into_iter()
            .skip(1)
            .filter(|(_, value)| !value.is_empty())
            .collect();

        self.server_state.update(cx, |this, cx| {
            this.add_stream_value(entry_id, field_values, cx);
        });
    }
}

pub struct ZedisStreamEditor {
    table_state: Entity<ZedisKvTable<ZedisStreamValues>>,
}

impl ZedisStreamEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = if let Some(values) = server_state.read(cx).value() {
            new_fields(values)
        } else {
            vec![]
        };

        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisStreamValues>::new(
                fields
                    .iter()
                    .map(|field| KvTableColumn::new(field.as_str(), None))
                    .collect(),
                server_state,
                window,
                cx,
            )
            .mode(KvTableMode::ADD | KvTableMode::REMOVE | KvTableMode::FILTER)
        });
        Self { table_state }
    }
}

impl Render for ZedisStreamEditor {
    /// Renders the stream editor as a full-size container with the table.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

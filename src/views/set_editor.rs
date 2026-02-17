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
    states::{RedisValue, ZedisServerState},
    views::{KvTableColumn, ZedisKvTable},
};
use gpui::{App, Entity, SharedString, Window, div, prelude::*};
use tracing::info;

/// Data adapter for Redis SET values to work with the KV table component.
///
/// This struct implements the `ZedisKvFetcher` trait to provide data access
/// and operations for the table view.
struct ZedisSetValues {
    /// Current Redis SET value data
    value: RedisValue,
    /// Reference to server state for executing Redis operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisSetValues {
    /// Adds a new member to the SET.
    ///
    /// # Arguments
    /// * `values` - A vector of one SharedString value: [value]
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    fn handle_add_value(&self, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let server_state = self.server_state.clone();
        // Validate that a value was provided
        if values.is_empty() {
            return;
        }

        // Execute the add operation on server state
        server_state.update(cx, |this, cx| {
            this.add_set_value(values[0].clone(), cx);
        });
    }

    /// Returns the total cardinality of the SET (from Redis SCARD).
    fn count(&self) -> usize {
        self.value.set_value().map_or(0, |v| v.size)
    }

    /// Creates a new data adapter instance.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }

    /// Retrieves a cell value for the table at the given row and column.
    ///
    /// For SETs, there's only one column (the member value itself).
    fn get(&self, row_ix: usize, _col_ix: usize) -> Option<SharedString> {
        self.value.set_value()?.values.get(row_ix).cloned()
    }

    /// Returns the number of currently loaded rows (not total SET size).
    ///
    /// This may be less than `count()` if pagination is in progress.
    fn rows_count(&self) -> usize {
        self.value.set_value().map_or(0, |v| v.values.len())
    }

    /// Checks if all SET members have been loaded via SSCAN.
    ///
    /// Returns `true` when the cursor has completed iteration (cursor == 0).
    fn is_done(&self) -> bool {
        self.value.set_value().is_some_and(|v| v.done)
    }

    /// Triggers loading of the next batch of SET members.
    ///
    /// Uses cursor-based pagination via SSCAN to load more values.
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_set_value(cx);
        });
    }

    /// Applies a filter to SET members by pattern matching.
    ///
    /// Resets the scan and loads members matching the keyword pattern.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_set_value(keyword, cx);
        });
    }

    fn handle_update_value(&self, index: usize, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let Some(new_value) = values.first() else {
            return;
        };
        let Some(old_value) = self.value.set_value().and_then(|v| v.values.get(index).cloned()) else {
            return;
        };

        self.server_state.update(cx, |this, cx| {
            this.update_set_value(old_value, new_value.clone(), cx);
        });
    }

    /// Removes a member from the SET at the given index.
    ///
    /// Executes Redis SREM command to delete the member.
    fn remove(&self, index: usize, cx: &mut App) {
        // Get the SET value at the specified index
        let Some(set) = self.value.set_value() else {
            return;
        };
        let Some(value) = set.values.get(index) else {
            return;
        };

        // Execute removal operation
        self.server_state.update(cx, |this, cx| {
            this.remove_set_value(value.clone(), cx);
        });
    }
}

/// Main SET editor view component.
///
/// Provides a table-based UI for viewing and managing Redis SET values.
/// Wraps the generic `ZedisKvTable` component with SET-specific configuration.
pub struct ZedisSetEditor {
    /// The table component that renders the SET members
    table_state: Entity<ZedisKvTable<ZedisSetValues>>,
}

impl ZedisSetEditor {
    /// Creates a new SET editor instance.
    ///
    /// # Arguments
    /// * `server_state` - Reference to the server state for Redis operations
    /// * `window` - GPUI window handle
    /// * `cx` - GPUI context for component initialization
    ///
    /// # Returns
    /// A new `ZedisSetEditor` instance with a single-column table
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize the KV table with a single "Value" column
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisSetValues>::new(
                vec![KvTableColumn::new("Value", None)],
                server_state,
                window,
                cx,
            )
        });

        info!("Creating new SET editor view");
        Self { table_state }
    }
}

impl Render for ZedisSetEditor {
    /// Renders the SET editor as a full-size container with the table.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

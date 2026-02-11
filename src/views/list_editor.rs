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
    components::{FormDialog, FormField, ZedisKvFetcher, open_add_form_dialog},
    helpers::fast_contains_ignore_case,
    states::{RedisValue, ZedisServerState, i18n_common, i18n_list_editor},
    views::{KvTableColumn, ZedisKvTable},
};
use gpui::{App, Entity, SharedString, Window, div, prelude::*};
use gpui_component::WindowExt;
use std::rc::Rc;
use tracing::info;

/// Manages Redis List values and their display state.
///
/// Handles both filtered and unfiltered views of list data, maintaining
/// a mapping between visible items and their original indices when filtering.
struct ZedisListValues {
    /// Currently visible items (filtered subset or all items)
    visible_items: Vec<SharedString>,
    /// Maps visible item indices to original list indices (Some when filtered, None otherwise)
    visible_item_indexes: Option<Vec<usize>>,
    /// The underlying Redis value data
    value: RedisValue,
    /// Reference to server state for performing operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisListValues {
    /// Recalculates visible items based on the current keyword filter.
    ///
    /// When a keyword is present:
    /// - Filters items using case-insensitive substring matching
    /// - Maintains index mapping to original positions
    ///
    /// When no keyword:
    /// - Shows all items directly
    /// - Clears index mapping
    fn recalc_visible_items(&mut self) {
        let Some(value) = self.value.list_value() else {
            return;
        };

        let keyword = value.keyword.clone().unwrap_or_default().to_lowercase();

        // No filter: show all items
        if keyword.is_empty() {
            self.visible_items = value.values.clone();
            self.visible_item_indexes = None;
            return;
        }

        // Filter items by keyword
        // Pre-allocate 10% capacity as an estimate for filtered results
        let capacity = value.values.len().max(100) / 10;
        let mut visible_item_indexes = Vec::with_capacity(capacity);
        let mut visible_items = Vec::with_capacity(capacity);

        for (index, item) in value.values.iter().enumerate() {
            if fast_contains_ignore_case(item.as_str(), &keyword) {
                visible_item_indexes.push(index);
                visible_items.push(item.clone());
            }
        }

        self.visible_items = visible_items;
        self.visible_item_indexes = Some(visible_item_indexes);
    }
}

impl ZedisKvFetcher for ZedisListValues {
    /// Retrieves the value at the specified row index.
    ///
    /// Returns from the filtered visible items when a keyword filter is active,
    /// otherwise returns directly from the original list values.
    fn get(&self, row_ix: usize, _col_ix: usize) -> Option<SharedString> {
        let value = self.value.list_value()?;
        if value.keyword.is_some() {
            self.visible_items.get(row_ix).cloned()
        } else {
            value.values.get(row_ix).cloned()
        }
    }

    /// Returns the total count of items in the Redis list (from LLEN).
    fn count(&self) -> usize {
        self.value.list_value().map_or(0, |v| v.size)
    }

    /// Returns the number of currently visible rows.
    ///
    /// When filtered, returns the count of matching items.
    /// Otherwise, returns the count of loaded items.
    fn rows_count(&self) -> usize {
        if self.value.list_value().is_none() {
            return 0;
        }
        self.visible_items.len()
    }

    /// Checks whether all list items have been loaded from Redis.
    fn is_done(&self) -> bool {
        self.value.list_value().is_some_and(|v| v.values.len() == v.size)
    }

    /// Triggers loading more list items from Redis (pagination).
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            state.load_more_list_value(cx);
        });
    }

    /// Removes the item at the specified visible index.
    ///
    /// When a filter is active, maps the visible index to the real index
    /// in the underlying list before performing the deletion (LREM command).
    fn remove(&self, index: usize, cx: &mut App) {
        // Map visible index to real index when filtering is active
        let real_index = self
            .visible_item_indexes
            .as_ref()
            .and_then(|indexes| indexes.get(index).copied())
            .unwrap_or(index);

        self.server_state.update(cx, |state, cx| {
            state.remove_list_value(real_index, cx);
        });
    }

    /// Applies a keyword filter to the list values.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            state.filter_list_value(keyword, cx);
        });
    }
    /// Opens a dialog to add a new value to the Redis list.
    ///
    /// The dialog allows users to choose between:
    /// - RPUSH: Append to the end of the list
    /// - LPUSH: Prepend to the beginning of the list
    fn handle_add_value(&self, window: &mut Window, cx: &mut App) {
        let server_state = self.server_state.clone();

        let handle_submit = Rc::new(move |values: Vec<SharedString>, window: &mut Window, cx: &mut App| {
            // Expect exactly 2 values: [position_choice, value]
            if values.len() != 2 {
                return false;
            }

            // values[0] = RPUSH/LPUSH choice, values[1] = actual value
            server_state.update(cx, |state, cx| {
                state.push_list_value(values[1].clone(), values[0].clone(), cx);
            });

            window.close_dialog(cx);
            true
        });

        let fields = vec![
            // Position choice: RPUSH (right/end) or LPUSH (left/start)
            FormField::new(i18n_list_editor(cx, "position")).with_options(vec!["RPUSH".into(), "LPUSH".into()]),
            // Value input field
            FormField::new(i18n_common(cx, "value"))
                .with_placeholder(i18n_common(cx, "value_placeholder"))
                .with_focus(),
        ];

        open_add_form_dialog(
            FormDialog {
                title: i18n_list_editor(cx, "add_value_title"),
                fields,
                handle_submit,
            },
            window,
            cx,
        );
    }

    /// Updates the value at the specified visible index using LSET command.
    ///
    /// When a filter is active, maps the visible index to the real index
    /// in the underlying list. Requires the original value for optimistic updates.
    fn handle_update_value(&self, index: usize, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        let Some(new_value) = values.first() else {
            return;
        };

        // Map visible index to real index when filtering is active
        let real_index = self
            .visible_item_indexes
            .as_ref()
            .and_then(|indexes| indexes.get(index).copied())
            .unwrap_or(index);

        let Some(list_value) = self.value.list_value() else {
            return;
        };

        let Some(original_value) = list_value.values.get(real_index) else {
            return;
        };

        self.server_state.update(cx, |state, cx| {
            state.update_list_value(real_index, original_value.clone(), new_value.clone(), cx);
        });
    }

    /// Creates a new instance and initializes the visible items list.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        let mut this = Self {
            server_state,
            value,
            visible_items: Vec::default(),
            visible_item_indexes: None,
        };

        this.recalc_visible_items();
        this
    }
}

/// Editor view for Redis List data type.
///
/// Provides a table-based interface for viewing and manipulating Redis lists,
/// supporting operations like LRANGE, LSET, LREM, LPUSH, and RPUSH.
///
/// Features:
/// - Paginated loading of large lists
/// - Keyword-based filtering
/// - In-place value editing
/// - Add values to either end of the list
/// - Delete individual items
pub struct ZedisListEditor {
    /// Table component managing the list data display and interactions
    table_state: Entity<ZedisKvTable<ZedisListValues>>,
}

impl ZedisListEditor {
    /// Creates a new list editor view for the given server state.
    ///
    /// Initializes a single-column table to display list values.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisListValues>::new(vec![KvTableColumn::new("Value", None)], server_state, window, cx)
        });

        info!("Creating new list editor view");

        Self { table_state }
    }
}

impl Render for ZedisListEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

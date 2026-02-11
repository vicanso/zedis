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

//! Redis ZSET (Sorted Set) editor UI component.
//!
//! This module provides a table-based editor for viewing and managing Redis ZSET values.
//! It supports operations like:
//! - Viewing ZSET members with their scores in a two-column table
//! - Adding new members with scores via a dialog form
//! - Updating scores of existing members (inline editing)
//! - Removing members
//! - Filtering members with pattern matching
//! - Incremental loading of large ZSETs with pagination

use crate::{
    components::{FormDialog, FormField, ZedisKvFetcher, open_add_form_dialog},
    states::{RedisValue, ZedisServerState, i18n_common, i18n_zset_editor},
    views::{KvTableColumn, ZedisKvTable},
};
use gpui::{App, Entity, SharedString, Window, div, prelude::*};
use gpui_component::WindowExt;
use std::rc::Rc;

/// Data adapter for Redis ZSET values to work with the KV table component.
///
/// This struct implements the `ZedisKvFetcher` trait to provide data access
/// and operations for the two-column table view (member and score columns).
struct ZedisZsetValues {
    /// Current Redis ZSET value data
    value: RedisValue,
    /// Reference to server state for executing Redis operations
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisZsetValues {
    /// Retrieves a cell value for the table at the given row and column.
    ///
    /// Column layout:
    /// - Column 1: Member name
    /// - Column 2: Score (as formatted string)
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let zset = self.value.zset_value()?;
        let (member, score) = zset.values.get(row_ix)?;

        // Column 2 is the score, others show the member name
        if col_ix == 2 {
            Some(score.to_string().into())
        } else {
            Some(member.clone())
        }
    }

    /// Returns the total cardinality of the ZSET (from Redis ZCARD).
    fn count(&self) -> usize {
        self.value.zset_value().map_or(0, |v| v.size)
    }

    /// Returns the number of currently loaded rows (not total ZSET size).
    ///
    /// This may be less than `count()` if pagination is in progress.
    fn rows_count(&self) -> usize {
        self.value.zset_value().map_or(0, |v| v.values.len())
    }

    /// Checks if all ZSET members have been loaded.
    ///
    /// Returns `true` when either:
    /// - All members are loaded (loaded count equals total size)
    /// - For filtered results: the cursor has completed iteration
    fn is_done(&self) -> bool {
        self.value
            .zset_value()
            .is_some_and(|v| v.values.len() == v.size || v.done)
    }

    /// Triggers loading of the next batch of ZSET members.
    ///
    /// Uses range-based or scan-based pagination depending on filter state.
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_zset_value(cx);
        });
    }

    /// Removes a member from the ZSET at the given index.
    ///
    /// Executes Redis ZREM command to delete the member.
    fn remove(&self, index: usize, cx: &mut App) {
        // Get the ZSET member at the specified index
        let Some(zset) = self.value.zset_value() else {
            return;
        };
        let Some((member, _score)) = zset.values.get(index) else {
            return;
        };

        // Execute removal operation
        self.server_state.update(cx, |this, cx| {
            this.remove_zset_value(member.clone(), cx);
        });
    }

    /// Applies a filter to ZSET members by pattern matching.
    ///
    /// Resets the scan and loads members matching the keyword pattern.
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_zset_value(keyword, cx);
        });
    }

    /// Opens a dialog to add a new member to the ZSET.
    ///
    /// Creates a form with member and score input fields and handles submission
    /// by calling the server state's `add_zset_value` method.
    fn handle_add_value(&self, window: &mut Window, cx: &mut App) {
        let server_state = self.server_state.clone();

        // Create submission handler that validates and calls Redis ZADD
        let handle_submit = Rc::new(move |values: Vec<SharedString>, window: &mut Window, cx: &mut App| {
            // Validate that both member and score were provided
            if values.len() != 2 {
                return false;
            }

            // Parse score from string (default to 0.0 if invalid)
            let score = values[1].parse::<f64>().unwrap_or(0.0);

            // Execute the add operation on server state
            server_state.update(cx, |this, cx| {
                this.add_zset_value(values[0].clone(), score, cx);
            });

            // Close the dialog on successful submission
            window.close_dialog(cx);
            true
        });

        // Build form with member and score input fields
        let fields = vec![
            FormField::new(i18n_common(cx, "value"))
                .with_placeholder(i18n_common(cx, "value_placeholder"))
                .with_focus(),
            FormField::new(i18n_common(cx, "score"))
                .with_placeholder(i18n_common(cx, "score_placeholder"))
                .with_focus(),
        ];

        // Open the form dialog
        open_add_form_dialog(
            FormDialog {
                title: i18n_zset_editor(cx, "add_value_title"),
                fields,
                handle_submit,
            },
            window,
            cx,
        );
    }

    /// Handles inline editing of a ZSET member's score.
    ///
    /// Called when the user edits the score column directly in the table.
    /// Updates the score for the existing member using Redis ZADD.
    fn handle_update_value(&self, row_ix: usize, values: Vec<SharedString>, _window: &mut Window, cx: &mut App) {
        // Extract member name and new score from values
        let Some(member) = values.first() else {
            return;
        };
        let Some(score_str) = values.get(1) else {
            return;
        };
        let Some(original_member) = self
            .value
            .zset_value()
            .and_then(|v| v.values.get(row_ix).map(|(m, _)| m.clone()))
        else {
            return;
        };

        // Parse score and execute update operation
        let score = score_str.parse::<f64>().unwrap_or(0.0);
        self.server_state.update(cx, |state, cx| {
            state.update_zset_value(original_member, member.clone(), score, cx);
        });
    }

    /// Creates a new data adapter instance.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }
}

/// Main ZSET editor view component.
///
/// Provides a table-based UI for viewing and managing Redis ZSET values.
/// Wraps the generic `ZedisKvTable` component with ZSET-specific configuration
/// including two columns (member name and score).
pub struct ZedisZsetEditor {
    /// The table component that renders the ZSET members and scores
    table_state: Entity<ZedisKvTable<ZedisZsetValues>>,
}

impl ZedisZsetEditor {
    /// Creates a new ZSET editor instance.
    ///
    /// # Arguments
    /// * `server_state` - Reference to the server state for Redis operations
    /// * `window` - GPUI window handle
    /// * `cx` - GPUI context for component initialization
    ///
    /// # Returns
    /// A new `ZedisZsetEditor` instance with a two-column table (Value and Score)
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Initialize the KV table with two columns: member and score
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisZsetValues>::new(
                vec![
                    KvTableColumn::new("Value", None),       // Member name column (flexible width)
                    KvTableColumn::new("Score", Some(150.)), // Score column (fixed 150px width)
                ],
                server_state,
                window,
                cx,
            )
        });

        Self { table_state }
    }
}

impl Render for ZedisZsetEditor {
    /// Renders the ZSET editor as a full-size container with the table.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

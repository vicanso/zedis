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

use crate::assets::CustomIconName;
use crate::states::{RedisValue, ZedisGlobalStore, ZedisServerState, i18n_common};
use crate::views::{KvTableColumn, KvTableColumnType};
use gpui::{App, Edges, Entity, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    table::{Column, TableDelegate, TableState},
};
use rust_i18n::t;
use std::{cell::Cell, collections::HashMap, rc::Rc, sync::Arc};

pub const INDEX_COLUMN_NAME: &str = "#";

/// Trait defining the data fetching and manipulation interface for Key-Value data.
/// Implementers allow the `ZedisKvDelegate` to display and edit various Redis data types (Hash, Set, List, ZSet).
pub trait ZedisKvFetcher: 'static {
    /// Retrieves a value for a specific cell in the table.
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString>;

    /// Returns the total count of items available.
    fn count(&self) -> usize;

    /// Returns the number of rows currently loaded.
    fn rows_count(&self) -> usize;

    /// Returns true if all data has been loaded.
    fn is_eof(&self) -> bool {
        !self.is_done()
    }

    /// Returns whether the data supports in-place updates.
    fn can_update(&self) -> bool {
        false
    }

    /// Returns the column index used as the primary identifier (e.g., for deletion).
    fn primary_index(&self) -> usize {
        0
    }

    /// Returns the column indices that are readonly.
    fn readonly_columns(&self) -> Vec<usize> {
        vec![]
    }

    /// Returns true if the fetcher is finished loading data.
    fn is_done(&self) -> bool;

    /// Triggers loading more data (pagination).
    fn load_more(&self, _window: &mut Window, _cx: &mut App);

    /// Removes an item at the specified index.
    fn remove(&self, index: usize, _cx: &mut App);

    /// Filters data based on a keyword.
    fn filter(&self, keyword: SharedString, _cx: &mut App);

    /// Opens a dialog to add a new value.
    fn handle_add_value(&self, _window: &mut Window, _cx: &mut App);

    /// Updates values for a specific row.
    fn handle_update_value(&self, _row_ix: usize, _values: Vec<SharedString>, _window: &mut Window, _cx: &mut App) {}

    /// Factory method to create a new instance.
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self;
}

/// A Table Delegate that manages the display and editing of Key-Value pairs.
/// It bridges the UI (Table) and the Data Source (ZedisKvFetcher).
pub struct ZedisKvDelegate<T: ZedisKvFetcher> {
    /// Configuration for table columns.
    table_columns: Vec<KvTableColumn>,
    /// State tracking if an async operation (like delete/load) is in progress.
    processing: Rc<Cell<bool>>,
    /// The data source provider.
    fetcher: Arc<T>,
    /// Column definitions for the UI component.
    columns: Vec<Column>,
    /// Tracks which row is currently being edited (if any).
    editing_row: Cell<Option<usize>>,
    /// Input states for editable cells, keyed by column index.
    value_states: HashMap<usize, Entity<InputState>>,
    /// Flag to ensure focus is applied only once when entering edit mode.
    edit_focus_done: bool,
}

impl<T: ZedisKvFetcher> ZedisKvDelegate<T> {
    /// Creates a new delegate instance with columns configuration and data fetcher.
    ///
    /// # Arguments
    /// * `columns` - Column definitions (name, width, alignment, type)
    /// * `fetcher` - Data source implementing ZedisKvFetcher trait
    /// * `window` - GPUI window context
    /// * `cx` - GPUI application context
    pub fn new(columns: Vec<KvTableColumn>, fetcher: T, window: &mut Window, cx: &mut App) -> Self {
        let mut value_states = HashMap::new();

        // Convert KvTableColumns to UI Columns and initialize input states
        let ui_columns = columns
            .iter()
            .enumerate()
            .map(|(index, item)| {
                // Create input state for editable value columns
                if item.column_type == KvTableColumnType::Value {
                    value_states.insert(index, cx.new(|cx| InputState::new(window, cx).clean_on_escape()));
                }

                // Build column with standard padding
                Column::new(item.name.clone(), item.name.clone())
                    .when_some(item.width, |col, width| col.width(width))
                    .map(|mut col| {
                        if let Some(align) = item.align {
                            col.align = align;
                        }
                        col.paddings = Some(Edges {
                            top: px(2.),
                            bottom: px(2.),
                            left: px(10.),
                            right: px(10.),
                        });
                        col
                    })
            })
            .collect();

        Self {
            table_columns: columns,
            columns: ui_columns,
            value_states,
            fetcher: Arc::new(fetcher),
            processing: Rc::new(Cell::new(false)),
            editing_row: Cell::new(None),
            edit_focus_done: false,
        }
    }

    /// Returns a cloned Arc reference to the current fetcher.
    pub fn fetcher(&self) -> Arc<T> {
        self.fetcher.clone()
    }

    /// Replaces the current fetcher with a new one (e.g., when switching keys).
    /// Resets processing state to ensure clean transition.
    pub fn set_fetcher(&mut self, fetcher: T) {
        self.fetcher = Arc::new(fetcher);
        self.processing = Rc::new(Cell::new(false));
    }

    /// Exits edit mode and resets related state flags.
    fn reset_edit(&mut self) {
        self.edit_focus_done = false;
        self.editing_row.set(None);
    }

    /// Enters edit mode for the specified row, populating input fields with current values.
    ///
    /// # Arguments
    /// * `row_ix` - The row index to edit
    pub fn handle_edit_row(&mut self, row_ix: usize, window: &mut Window, cx: &mut App) {
        self.edit_focus_done = false;
        self.editing_row.set(Some(row_ix));

        // Populate input fields with current values from fetcher
        let fetcher = self.fetcher();
        for (col_ix, state) in &self.value_states {
            if let Some(value) = fetcher.get(row_ix, *col_ix) {
                state.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
    }

    /// Commits the edited values and exits edit mode.
    ///
    /// # Arguments
    /// * `row_ix` - The row index being updated
    pub fn handle_update_row(&mut self, row_ix: usize, window: &mut Window, cx: &mut App) {
        self.reset_edit();

        // Collect values from input fields in sorted column order
        let values: Vec<SharedString> = {
            let mut col_indices: Vec<_> = self.value_states.keys().copied().collect();
            col_indices.sort_unstable();

            col_indices
                .iter()
                .filter_map(|&col_ix| self.value_states.get(&col_ix).map(|state| state.read(cx).value()))
                .collect()
        };

        self.fetcher().handle_update_value(row_ix, values, window, cx);
    }

    /// Renders action buttons (edit/save/cancel/delete) for a table row.
    ///
    /// # Button behavior:
    /// - Edit mode: Shows save (check) and cancel (X) buttons
    /// - View mode: Shows edit (pen) and delete (X) buttons
    ///
    /// # Arguments
    /// * `base` - Base container to add buttons to
    /// * `row_ix` - Row index for button handlers
    /// * `is_editing` - Whether the row is currently in edit mode
    fn render_action_buttons(
        &mut self,
        base: gpui::Div,
        row_ix: usize,
        is_editing: bool,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::Div {
        let processing = self.processing.clone();
        let mut base = base;

        // Edit/Save button (only shown if fetcher supports updates)
        if self.fetcher.can_update() {
            let icon = if is_editing {
                Icon::new(IconName::Check)
            } else {
                Icon::new(CustomIconName::FilePenLine)
            };

            let update_btn = Button::new(("zedis-editor-table-action-update-btn", row_ix))
                .small()
                .ghost()
                .mr_2()
                .icon(icon)
                .tooltip(i18n_common(cx, "update_tooltip"))
                .disabled(processing.get())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if is_editing {
                        this.delegate_mut().handle_update_row(row_ix, window, cx);
                    } else {
                        this.delegate_mut().handle_edit_row(row_ix, window, cx);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }));

            base = base.child(update_btn);
        }

        // Cancel/Delete button
        if is_editing {
            // Cancel button (exits edit mode without saving)
            let cancel_btn = Button::new(("zedis-editor-table-action-cancel-btn", row_ix))
                .small()
                .ghost()
                .mr_2()
                .icon(Icon::new(CustomIconName::X))
                .tooltip(i18n_common(cx, "cancel_tooltip"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.delegate_mut().editing_row.set(None);
                    cx.stop_propagation();
                    cx.notify();
                }));
            base = base.child(cancel_btn);
        } else {
            // Delete button (shows confirmation dialog)
            let fetcher = self.fetcher.clone();
            let remove_btn = Button::new(("zedis-editor-table-action-remove-btn", row_ix))
                .small()
                .ghost()
                .icon(Icon::new(CustomIconName::FileXCorner))
                .tooltip(i18n_common(cx, "remove_tooltip"))
                .disabled(processing.get())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let processing = this.delegate_mut().processing.clone();
                    let value = fetcher.get(row_ix, fetcher.primary_index()).unwrap_or_default();
                    let fetcher = fetcher.clone();

                    cx.stop_propagation();

                    window.open_dialog(cx, move |dialog, _, cx| {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let message = t!(
                            "common.remove_item_prompt",
                            row = row_ix + 1,
                            value = value,
                            locale = locale
                        );

                        let processing = processing.clone();
                        let fetcher = fetcher.clone();

                        dialog.confirm().child(message.to_string()).on_ok(move |_, window, cx| {
                            processing.replace(true);
                            fetcher.remove(row_ix, cx);
                            window.close_dialog(cx);
                            true
                        })
                    });
                }));
            base = base.child(remove_btn);
        }

        base
    }
}

impl<T: ZedisKvFetcher + 'static> TableDelegate for ZedisKvDelegate<T> {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.fetcher.rows_count()
    }

    fn column(&self, index: usize, _: &App) -> &Column {
        &self.columns[index]
    }

    /// Renders a table header cell with styled column name.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        div()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    /// Renders a table data cell, handling different column types:
    /// - Index: Shows row number
    /// - Action: Shows edit/save/cancel/delete buttons
    /// - Value: Shows editable input or static label
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let base = h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings));

        let is_editing = self.editing_row.get() == Some(row_ix) && !self.fetcher.readonly_columns().contains(&col_ix);

        // Handle special column types
        if let Some(table_column) = self.table_columns.get(col_ix) {
            match table_column.column_type {
                // Index column: Display row number (1-based)
                KvTableColumnType::Index => {
                    return base.child(Label::new((row_ix + 1).to_string()).text_align(column.align).w_full());
                }
                // Action column: Display edit/delete/cancel buttons
                KvTableColumnType::Action => {
                    return self.render_action_buttons(base, row_ix, is_editing, window, cx);
                }
                _ => {}
            }
        }

        // Render editable input or static label for value columns
        if is_editing && let Some(value_state) = self.value_states.get(&col_ix) {
            // Auto-focus the first input when entering edit mode
            if !self.edit_focus_done {
                value_state.update(cx, |input, cx| input.focus(window, cx));
                self.edit_focus_done = true;
            }
            return base.child(Input::new(value_state).small().cleanable(true));
        }

        // Default: Render value as label
        let value = self.fetcher.get(row_ix, col_ix).unwrap_or_else(|| "--".into());
        base.child(Label::new(value).text_align(column.align))
    }
    /// Returns whether all data has been loaded (end of file).
    fn is_eof(&self, _: &App) -> bool {
        self.fetcher.is_eof()
    }

    /// Defines how many rows from the bottom should trigger load_more.
    /// When user scrolls within 50 rows of the bottom, more data is loaded.
    fn load_more_threshold(&self) -> usize {
        50
    }

    /// Loads more data when user scrolls near the bottom of the table.
    /// Prevents concurrent load operations using the processing flag.
    fn load_more(&mut self, window: &mut Window, cx: &mut Context<TableState<ZedisKvDelegate<T>>>) {
        // Don't load if already done or currently processing
        if self.fetcher.is_done() || self.processing.replace(true) {
            return;
        }

        self.fetcher.load_more(window, cx);
    }
}

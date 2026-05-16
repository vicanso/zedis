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

use super::{KvTableColumn, KvTableColumnType};
use crate::states::{KeyType, RedisValue, ZedisServerState, i18n_common};
use gpui::{App, ClipboardItem, Edges, Entity, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, IconName, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use std::{cell::Cell, rc::Rc, sync::Arc};

pub const INDEX_COLUMN_NAME: &str = "#";

/// Trait defining the data fetching and manipulation interface for Key-Value data.
/// Implementers allow the `ZedisKvDelegate` to display and edit various Redis data types (Hash, Set, List, ZSet).
pub trait ZedisKvFetcher: 'static {
    fn key_type(&self) -> KeyType {
        KeyType::Unknown
    }
    /// Retrieves a value for a specific cell in the table (display form).
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString>;

    /// Retrieves a value for a specific cell in the edit form.
    ///
    /// Defaults to `get`. Override when the display format differs from the
    /// editable format (e.g. a humanized duration vs. raw seconds).
    fn get_edit(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        self.get(row_ix, col_ix)
    }

    /// Returns the total count of items available.
    fn count(&self) -> usize;

    /// Returns the number of rows currently loaded.
    fn rows_count(&self) -> usize;

    /// Returns true if all data has been loaded (end of the scan).
    /// Equivalent to [`Self::is_done`]; `has_more` is its negation, so
    /// this must NOT be inverted — doing so disables scroll pagination
    /// (the table only calls `load_more` while `has_more` is true).
    fn is_eof(&self) -> bool {
        self.is_done()
    }

    /// Returns the column index used as the primary identifier (e.g., for deletion).
    fn primary_index(&self) -> usize {
        0
    }

    /// Returns true if the fetcher is finished loading data.
    fn is_done(&self) -> bool;

    /// Triggers loading more data (pagination).
    fn load_more(&self, _window: &mut Window, _cx: &mut App);

    /// Removes an item at the specified index.
    fn remove(&self, index: usize, _cx: &mut App);

    /// Whether form fields are required when adding/editing.
    fn fields_required(&self) -> bool {
        true
    }

    /// Whether submitted form values should include field names alongside values.
    fn include_field_names(&self) -> bool {
        false
    }

    /// Whether the edit form should support dynamic add-fields.
    fn support_add_fields(&self) -> bool {
        false
    }

    /// Filters data based on a keyword.
    ///
    /// Filtering strategy varies by data type:
    /// - **Client-side** (List, Stream): searches already-loaded data in memory,
    ///   maintains visible item index mapping for correct row operations.
    /// - **Server-side** (Set, Hash, Zset): sends keyword to server,
    ///   resets scan cursor and loads matching results via SCAN commands.
    fn filter(&self, keyword: SharedString, _cx: &mut App);

    /// Adds values for a new row.
    fn handle_add_value(&self, _values: Vec<SharedString>, _window: &mut Window, _cx: &mut App);

    /// Updates values for a specific row.
    fn handle_update_value(&self, _row_ix: usize, _values: Vec<SharedString>, _window: &mut Window, _cx: &mut App) {}

    /// Returns updated column definitions if they may change dynamically (e.g., Stream fields).
    /// Default returns `None` (columns are static).
    fn columns(&self) -> Option<Vec<KvTableColumn>> {
        None
    }

    /// Whether the edit form should be readonly when viewing an existing row.
    /// When `true`, selecting a row shows values as read-only; only adding allows editing.
    fn readonly_on_edit(&self) -> bool {
        false
    }

    /// Whether this data type supports reverse sort order toggling.
    /// Only Stream returns `true` (XRANGE vs XREVRANGE).
    fn support_reverse(&self) -> bool {
        false
    }

    /// Returns the current reverse flag for this data source.
    fn current_reverse(&self) -> bool {
        false
    }

    /// Triggers a full reload with the given sort order.
    /// Called when the user clicks the sort-order toggle button.
    fn toggle_reverse(&self, _reverse: bool, _cx: &mut App) {}

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
}

impl<T: ZedisKvFetcher> ZedisKvDelegate<T> {
    /// Creates a new delegate instance with columns configuration and data fetcher.
    ///
    /// # Arguments
    /// * `columns` - Column definitions (name, width, alignment, type)
    /// * `fetcher` - Data source implementing ZedisKvFetcher trait
    /// * `window` - GPUI window context
    /// * `cx` - GPUI application context
    pub fn new(columns: Vec<KvTableColumn>, fetcher: Arc<T>, _window: &mut Window, _cx: &mut App) -> Self {
        // Convert KvTableColumns to UI Columns and initialize input states
        let primary_ix = fetcher.primary_index();
        let support_reverse = fetcher.support_reverse();
        let is_reverse = fetcher.current_reverse();
        let ui_columns = columns
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                Column::new(item.name.clone(), item.name.clone())
                    .when_some(item.width, |col, width| col.width(width))
                    .when(ix == primary_ix && support_reverse, |col| {
                        col.sort(if is_reverse {
                            ColumnSort::Descending
                        } else {
                            ColumnSort::Ascending
                        })
                    })
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
            fetcher,
            processing: Rc::new(Cell::new(false)),
        }
    }

    /// Returns a cloned Arc reference to the current fetcher.
    pub fn fetcher(&self) -> Arc<T> {
        self.fetcher.clone()
    }

    /// Replaces the current fetcher with a new one (e.g., when switching keys).
    /// Resets processing state to ensure clean transition.
    pub fn set_fetcher(&mut self, fetcher: Arc<T>) {
        self.fetcher = fetcher;
        self.processing = Rc::new(Cell::new(false));
    }

    /// Replaces the column definitions (e.g., when Stream fields change).
    pub fn set_columns(&mut self, columns: Vec<KvTableColumn>) {
        let primary_ix = self.fetcher.primary_index();
        let support_reverse = self.fetcher.support_reverse();
        let is_reverse = self.fetcher.current_reverse();
        let ui_columns = columns
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                Column::new(item.name.clone(), item.name.clone())
                    .when_some(item.width, |col, width| col.width(width))
                    .when(ix == primary_ix && support_reverse, |col| {
                        col.sort(if is_reverse {
                            ColumnSort::Descending
                        } else {
                            ColumnSort::Ascending
                        })
                    })
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
        self.table_columns = columns;
        self.columns = ui_columns;
    }
}

impl<T: ZedisKvFetcher + 'static> TableDelegate for ZedisKvDelegate<T> {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.fetcher.rows_count()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        self.columns[index].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let primary_color = cx.theme().primary;

        div()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_color(primary_color)
                    .text_sm(),
            )
    }

    /// Handles sort toggling for the primary column (e.g., Stream Entry ID).
    /// Descending/Default → reverse=true (XREVRANGE), Ascending → reverse=false (XRANGE).
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if col_ix == self.fetcher.primary_index() && self.fetcher.support_reverse() {
            let reverse = matches!(sort, ColumnSort::Descending | ColumnSort::Default);
            self.fetcher.toggle_reverse(reverse, cx);
        }
    }

    /// Renders a table data cell, handling different column types:
    /// - Index: Shows row number
    /// - Action: Shows edit/save/cancel/delete buttons
    /// - Value: Shows editable input or static label
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let base = h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings));

        // Handle special column types
        if self
            .table_columns
            .get(col_ix)
            .map(|item| item.column_type == KvTableColumnType::Index)
            .unwrap_or_default()
        {
            // Index column: Display row number (1-based)
            return base.child(Label::new((row_ix + 1).to_string()).text_align(column.align).w_full());
        }

        // Default: Render value as label with copy button on hover
        let value = self.fetcher.get(row_ix, col_ix).unwrap_or_else(|| "--".into());
        let group_name: SharedString = format!("td-{}-{}", row_ix, col_ix).into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        base.group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(value.clone())
                    .text_align(column.align)
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id(("copy-wrapper", row_ix * 100 + col_ix))
                    .invisible()
                    .group_hover(group_name, |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                    .child(
                        Button::new(("copy-cell", row_ix * 100 + col_ix))
                            .ghost()
                            .icon(IconName::Copy)
                            .on_click(move |_, window, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                                window.push_notification(Notification::info(copied_message.clone()), cx);
                            }),
                    ),
            )
    }
    /// Returns whether all data has been loaded (end of file).
    fn has_more(&self, _: &App) -> bool {
        !self.fetcher.is_eof()
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

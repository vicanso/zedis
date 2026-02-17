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

use crate::helpers::get_font_family;
use crate::{
    assets::CustomIconName,
    components::{INDEX_COLUMN_NAME, ZedisKvDelegate, ZedisKvFetcher},
    helpers::{EditorAction, humanize_keystroke},
    states::{
        KeyType, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_kv_table,
        i18n_list_editor,
    },
};
use gpui::{Entity, SharedString, Subscription, TextAlign, Window, div, prelude::*, px};
use gpui_component::highlighter::Language;
use gpui_component::input::Position;
use gpui_component::radio::RadioGroup;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, PixelsExt, WindowExt,
    button::{Button, ButtonVariants},
    form::field,
    h_flex,
    input::{Escape, Input, InputEvent, InputState},
    label::Label,
    table::{Table, TableEvent, TableState},
    v_flex,
};
use rust_i18n::t;
use std::sync::Arc;
use tracing::info;

bitflags::bitflags! {
    /// Defines the operations supported by the table.
    ///
    /// Use bitwise operations to combine multiple modes:
    /// - `KvTableMode::ADD | KvTableMode::UPDATE` - Allow add and update
    /// - `KvTableMode::ALL` - Allow all operations
    /// - `KvTableMode::empty()` - Read-only mode (no operations)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KvTableMode: u8 {
        /// Support adding new values
        const ADD    = 0b0001;
        /// Support updating existing values
        const UPDATE = 0b0010;
        /// Support removing values
        const REMOVE = 0b0100;
        /// Support filtering/searching values
        const FILTER = 0b1000;
        /// All operations enabled
        const ALL    = Self::ADD.bits() | Self::UPDATE.bits() | Self::REMOVE.bits() | Self::FILTER.bits();
    }
}

/// Width of the keyword search input field in pixels
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

/// Defines the type of table column for different purposes.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub enum KvTableColumnType {
    /// Standard value column displaying data
    #[default]
    Value,
    /// Row index/number column
    Index,
}

/// Configuration for a table column including name, width, and alignment.
#[derive(Clone, Default, Debug)]
pub struct KvTableColumn {
    /// Whether the column is readonly
    pub readonly: bool,
    /// Whether the column is flexible
    pub flex: bool,
    /// Type of the column
    pub column_type: KvTableColumnType,
    /// Display name of the column
    pub name: SharedString,
    /// Optional fixed width in pixels
    pub width: Option<f32>,
    /// Text alignment (left, center, right)
    pub align: Option<TextAlign>,
}

impl KvTableColumn {
    /// Creates a new value column with the given name and optional width.
    pub fn new(name: &str, width: Option<f32>) -> Self {
        Self {
            name: name.to_string().into(),
            width,
            ..Default::default()
        }
    }
    pub fn new_flex(name: &str) -> Self {
        Self {
            name: name.to_string().into(),
            flex: true,
            ..Default::default()
        }
    }
}

/// A generic table view for displaying Redis key-value data.
///
/// This component handles:
/// - Displaying paginated Redis data in a table format
/// - Keyword search/filtering
/// - Real-time updates via server events
/// - Loading states and pagination indicators
pub struct ZedisKvTable<T: ZedisKvFetcher> {
    /// Table state managing the delegate and data
    table_state: Entity<TableState<ZedisKvDelegate<T>>>,
    /// Input field state for keyword search/filter
    keyword_state: Entity<InputState>,
    /// Number of currently loaded items
    items_count: usize,
    /// Total number of items available
    total_count: usize,
    /// Whether all data has been loaded
    done: bool,
    /// Whether a filter operation is in progress
    loading: bool,
    /// Flag indicating the selected key has changed (triggers input reset)
    key_changed: Option<bool>,
    /// Whether the table is readonly
    readonly: bool,
    /// Supported operations mode (add, update, remove, filter)
    mode: KvTableMode,
    /// The row index that is being edited
    edit_row: Option<usize>,
    /// The original values of the row that is being edited
    original_values: Vec<SharedString>,
    /// Whether the values have been modified
    values_modified: bool,
    /// Whether the values should be filled
    values_should_fill: Option<bool>,
    columns: Vec<KvTableColumn>,
    /// Input states for editable cells, keyed by column index.
    value_states: Vec<(usize, Entity<InputState>)>,
    /// The push mode for the list
    list_push_mode_state: Entity<usize>,
    /// Fetcher instance
    fetcher: Arc<T>,
    /// Event subscriptions for server state and input changes
    _subscriptions: Vec<Subscription>,
}
impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    /// Creates a new fetcher instance with the current server value.
    fn new_values(server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) -> T {
        let value = server_state.read(cx).value().cloned().unwrap_or_default();
        T::new(server_state, value)
    }

    /// Prepares table columns by adding index and action columns, then calculating widths.
    ///
    /// # Logic:
    /// 1. Adds an index column at the start (80px, right-aligned)
    /// 2. Adds an action column at the end (100px, center-aligned)
    /// 3. Calculates remaining space for columns without fixed widths
    /// 4. Distributes remaining width evenly among flexible columns
    fn new_columns(mut columns: Vec<KvTableColumn>, window: &Window, cx: &mut Context<Self>) -> Vec<KvTableColumn> {
        // Calculate available width (window - sidebar - key tree - padding)
        let window_width = window.viewport_size().width;

        // Insert index column at the beginning
        columns.insert(
            0,
            KvTableColumn {
                column_type: KvTableColumnType::Index,
                name: INDEX_COLUMN_NAME.to_string().into(),
                width: Some(80.),
                align: Some(TextAlign::Right),
                ..Default::default()
            },
        );

        // Calculate remaining width and count columns without fixed width
        let content_width = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .unwrap_or(window_width);
        let mut remaining_width = content_width.as_f32() - 10.;
        let mut flexible_columns = 0;

        for column in columns.iter_mut() {
            if let Some(mut width) = column.width {
                if width < 1.0 {
                    width *= remaining_width;
                    column.width = Some(width);
                }
                remaining_width -= width;
            } else {
                flexible_columns += 1;
            }
        }

        // Distribute remaining width among flexible columns
        let flexible_width = if flexible_columns > 0 {
            Some((remaining_width / flexible_columns as f32) - 5.)
        } else {
            None
        };

        for column in &mut columns {
            if column.width.is_none() {
                column.width = flexible_width;
            }
        }

        columns
    }
    /// Creates a new table view with the given columns and server state.
    ///
    /// Sets up:
    /// - Event subscriptions for server state changes
    /// - Keyword search input field
    /// - Table state with data delegate
    /// - Default mode is `KvTableMode::ALL` (all operations enabled)
    ///
    /// # Arguments
    /// * `columns` - Column definitions for the table
    /// * `server_state` - Reference to the server state
    /// * `window` - Current window
    /// * `cx` - GPUI context
    ///
    /// # Example
    /// ```
    /// // Create with default mode (ALL)
    /// let table = ZedisKvTable::new(columns, server_state, window, cx);
    ///
    /// // Create with custom mode
    /// let table = ZedisKvTable::new(columns, server_state, window, cx)
    ///     .mode(KvTableMode::ADD | KvTableMode::REMOVE);
    /// ```
    pub fn new(
        columns: Vec<KvTableColumn>,
        server_state: Entity<ZedisServerState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        // Subscribe to server events to update table data
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            match event {
                // Update fetcher when data changes
                ServerEvent::ValuePaginationFinished
                | ServerEvent::ValueLoaded
                | ServerEvent::ValueAdded
                | ServerEvent::ValueUpdated => {
                    let fetcher = Arc::new(Self::new_values(server_state.clone(), cx));
                    this.fetcher = fetcher.clone();
                    this.loading = false;
                    this.done = fetcher.is_done();
                    this.items_count = fetcher.rows_count();
                    this.total_count = fetcher.count();
                    this.table_state.update(cx, |state, _| {
                        state.delegate_mut().set_fetcher(fetcher);
                    });
                }
                // Clear search when key selection changes
                ServerEvent::KeySelected => {
                    this.edit_row = None;
                    this.key_changed = Some(true);
                }
                _ => {}
            }
        }));

        // Initialize keyword search input field
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });

        // Subscribe to input events to trigger search on Enter
        subscriptions.push(cx.subscribe(&keyword_state, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.handle_filter(cx);
            }
        }));

        let readonly = server_state.read(cx).readonly();
        
        // If readonly, disable all operations; otherwise default to ALL
        let mode = if readonly {
            KvTableMode::empty()
        } else {
            KvTableMode::ALL
        };

        // Initialize table data and state
        let fetcher = Arc::new(Self::new_values(server_state, cx));
        let done = fetcher.is_done();
        let items_count = fetcher.rows_count();
        let total_count = fetcher.count();
        let delegate = ZedisKvDelegate::new(
            Self::new_columns(columns.clone(), window, cx),
            fetcher.clone(),
            window,
            cx,
        );

        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));
        
        // Subscribe to row selection events (mode check will be done in handler)
        subscriptions.push(cx.subscribe(&table_state, |this, _, event, cx| {
            if let TableEvent::SelectRow(row_ix) = event {
                this.handle_select_row(*row_ix, cx);
            }
        }));

        let value_states = columns
            .iter()
            .enumerate()
            .flat_map(|(index, column)| {
                if column.column_type != KvTableColumnType::Value {
                    return None;
                }
                let state = cx.new(|cx| {
                    if column.readonly {
                        InputState::new(window, cx)
                    } else {
                        InputState::new(window, cx)
                            .code_editor(Language::from_str("json").name())
                            .line_number(true)
                            .indent_guides(true)
                            .searchable(true)
                            .soft_wrap(true)
                    }
                });
                subscriptions.push(cx.subscribe(&state, move |this, _, event, cx| {
                    if let InputEvent::Change = event {
                        this.check_values_modified(cx);
                    }
                }));
                Some((index, state))
            })
            .collect::<Vec<_>>();
        info!("Creating new key value table view with mode: {:?}", mode);

        Self {
            table_state,
            keyword_state,
            items_count,
            total_count,
            done,
            loading: false,
            key_changed: None,
            edit_row: None,
            values_should_fill: None,
            original_values: vec![],
            values_modified: false,
            value_states,
            readonly,
            mode,
            fetcher,
            columns,
            list_push_mode_state: cx.new(|_cx| 0),
            _subscriptions: subscriptions,
        }
    }

    /// Sets the operation mode for the table.
    ///
    /// This method allows you to customize which operations are available:
    /// - `KvTableMode::ALL` - All operations (add, update, remove, filter)
    /// - `KvTableMode::ADD | KvTableMode::REMOVE` - Only add and remove
    /// - `KvTableMode::FILTER` - Only filtering, no modifications
    /// - `KvTableMode::empty()` - Read-only mode
    ///
    /// # Note
    /// If the server state is readonly, the mode will be forced to `empty()` regardless
    /// of the provided mode.
    ///
    /// # Example
    /// ```
    /// let table = ZedisKvTable::new(columns, server_state, window, cx)
    ///     .mode(KvTableMode::ADD | KvTableMode::REMOVE | KvTableMode::FILTER);
    /// ```
    pub fn mode(mut self, mode: KvTableMode) -> Self {
        // If readonly, mode is always empty
        if self.readonly {
            self.mode = KvTableMode::empty();
        } else {
            self.mode = mode;
        }
        self
    }

    fn is_adding_row(&self) -> bool {
        self.edit_row == Some(usize::MAX)
    }

    fn handle_select_row(&mut self, row_ix: usize, _cx: &mut Context<Self>) {
        // Only allow row selection if UPDATE, REMOVE, or ADD mode is enabled
        if !self.mode.intersects(KvTableMode::UPDATE | KvTableMode::REMOVE | KvTableMode::ADD) {
            return;
        }
        
        self.edit_row = Some(row_ix);
        let values = self
            .value_states
            .iter()
            .map(|(index, _)| self.fetcher.get(row_ix, *index + 1).unwrap_or_default())
            .collect::<Vec<_>>();
        self.values_modified = false;
        self.original_values = values;
        self.values_should_fill = Some(true);
    }
    fn handle_add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only allow adding if ADD mode is enabled
        if !self.mode.contains(KvTableMode::ADD) {
            return;
        }
        
        self.edit_row = Some(usize::MAX);
        self.list_push_mode_state.update(cx, |state, _| {
            *state = 0;
        });
        let mut foucused = false;
        self.value_states.iter().for_each(|(_, state)| {
            state.update(cx, |input, cx| {
                if !foucused {
                    input.focus(window, cx);
                    foucused = true;
                }
                input.set_value(SharedString::default(), window, cx);
            });
        });
        self.original_values.clear();
        self.values_modified = false;
    }
    fn check_values_modified(&mut self, cx: &mut Context<Self>) {
        let mut values_modified = false;
        for (index, (_, state)) in self.value_states.iter().enumerate() {
            let value = state.read(cx).value();
            if self
                .original_values
                .get(index)
                .map(|original_value| original_value.clone() != value)
                .unwrap_or(true)
            {
                values_modified = true;
            }
        }
        self.values_modified = values_modified;
    }

    /// Triggers a filter operation using the current keyword from the input field.
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        // Only allow filtering if FILTER mode is enabled
        if !self.mode.contains(KvTableMode::FILTER) {
            return;
        }
        
        let keyword = self.keyword_state.read(cx).value();
        self.loading = true;
        self.table_state.update(cx, |state, cx| {
            state.delegate().fetcher().filter(keyword, cx);
        });
    }
    fn handle_add_or_update_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.values_modified {
            return;
        }
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
        
        let mut values = Vec::with_capacity(self.value_states.len());
        for (_, state) in self.value_states.iter() {
            let value = state.read(cx).value();
            values.push(value);
        }
        if row_ix == usize::MAX {
            if self.fetcher.key_type() == KeyType::List {
                let index = *self.list_push_mode_state.read(cx);
                values.insert(0, index.to_string().into());
            }
            self.fetcher.handle_add_value(values, window, cx);
        } else {
            self.fetcher.handle_update_value(row_ix, values, window, cx);
        }
        self.edit_row = None;
    }
    fn handle_remove_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only allow removing if REMOVE mode is enabled
        if !self.mode.contains(KvTableMode::REMOVE) {
            return;
        }
        
        let Some(row_ix) = self.edit_row else {
            return;
        };
        let fetcher = self.fetcher.clone();
        let value = fetcher.get(row_ix, fetcher.primary_index()).unwrap_or_default();
        let entity = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _, cx| {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
            let message = t!(
                "common.remove_item_prompt",
                row = row_ix + 1,
                value = value,
                locale = locale
            );

            let fetcher = fetcher.clone();
            let entity = entity.clone();

            dialog
                .confirm()
                .button_props(dialog_button_props(cx))
                .child(message.to_string())
                .on_ok(move |_, window, cx| {
                    fetcher.remove(row_ix, cx);
                    entity.update(cx, |this, _cx| {
                        this.edit_row = None;
                    });
                    window.close_dialog(cx);
                    true
                })
        });
    }
    /// Renders the edit form for the current row.
    fn render_edit_form(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex().size_full().gap_3();
        let count = self.value_states.len();
        if self.is_adding_row() && self.fetcher.key_type() == KeyType::List {
            let positions = vec!["RPUSH".to_string(), "LPUSH".to_string()];
            let index = *self.list_push_mode_state.read(cx);
            form = form.child(
                v_flex().w_full().child(
                    field().label(i18n_list_editor(cx, "position")).child(
                        RadioGroup::horizontal("kv-table-list-position-radio-group")
                            .flex_none()
                            .children(positions)
                            .selected_index(Some(index))
                            .on_click(cx.listener(move |this, index, _, cx| {
                                this.list_push_mode_state.update(cx, |state, _| {
                                    *state = *index;
                                });
                            })),
                    ),
                ),
            );
        }
        let mut flexible_columns = 0;
        for (index, (column_index, value_state)) in self.value_states.iter().enumerate() {
            let Some(column) = self.columns.get(*column_index) else {
                continue;
            };
            let last = index == count - 1;
            let mut flex = column.flex;
            if last && flexible_columns == 0 {
                flex = true;
            }
            let input = Input::new(value_state)
                .disabled(column.readonly)
                .h_full()
                .p_0()
                .font_family(get_font_family())
                .focus_bordered(false);

            let inner_content = if flex {
                flexible_columns += 1;
                v_flex()
                    .size_full()
                    .gap_1()
                    .child(Label::new(column.name.clone()))
                    .child(div().flex_1().size_full().child(input))
                    .into_any_element()
            } else {
                field().label(column.name.clone()).child(input).into_any_element()
            };

            let wrapped_field = v_flex()
                .w_full()
                .child(inner_content)
                .when(flex, |this| this.flex_1().h_full());

            form = form.child(wrapped_field);
        }
        let cancel_label = i18n_common(cx, "cancel");
        let save_label = i18n_common(cx, "save");
        let remove_label = i18n_common(cx, "remove");
        
        let can_add = self.mode.contains(KvTableMode::ADD);
        let can_remove = self.mode.contains(KvTableMode::REMOVE);
        let can_update = self.mode.contains(KvTableMode::UPDATE);
        let is_adding = self.is_adding_row();
        
        form.child(
            div().flex_none().child(
                field().child(
                    h_flex()
                        .id("kv-table-edit-form-btn-group")
                        .w_full()
                        .gap_2()
                        .when(!is_adding && can_remove, |this| {
                            this.child(
                                Button::new("remove-edit-btn")
                                    .icon(CustomIconName::FileXCorner)
                                    .label(remove_label)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.handle_remove_row(window, cx);
                                    })),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new("cancel-edit-btn")
                                .icon(IconName::CircleX)
                                .label(cancel_label)
                                .on_click(cx.listener(|this, _, _, _cx| {
                                    this.edit_row = None;
                                })),
                        )
                        .when((is_adding && can_add) || (!is_adding && can_update), |this| {
                            this.child(
                                Button::new("save-edit-btn")
                                    .disabled(!self.values_modified)
                                    .tooltip(humanize_keystroke("cmd-s"))
                                    .primary()
                                    .icon(CustomIconName::Save)
                                    .label(save_label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.handle_add_or_update_value(window, cx);
                                    })),
                            )
                        }),
                ),
            ),
        )
        .into_any_element()
    }
}
impl<T: ZedisKvFetcher> Render for ZedisKvTable<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_color = cx.theme().muted_foreground;

        // Clear search input when key changes
        if let Some(true) = self.key_changed.take() {
            self.keyword_state.update(cx, |input, cx| {
                input.set_value(SharedString::default(), window, cx);
            });
        }
        if let Some(true) = self.values_should_fill.take() {
            let mut first = true;
            for (index, value) in self.original_values.iter().enumerate() {
                let Some((_, state)) = self.value_states.get(index) else {
                    continue;
                };
                state.update(cx, |input, cx| {
                    input.set_value(value.clone(), window, cx);
                    if first {
                        input.focus(window, cx);
                        let position = Position::new(0, value.len() as u32);
                        input.set_cursor_position(position, window, cx);
                        first = false;
                    }
                });
            }
        }

        // Determine if operations are allowed based on mode
        let can_add = self.mode.contains(KvTableMode::ADD);
        let can_filter = self.mode.contains(KvTableMode::FILTER);

        // Search button with loading state
        let search_btn = Button::new("kv-table-search-btn")
            .ghost()
            .icon(IconName::Search)
            .tooltip(i18n_kv_table(cx, "search_tooltip"))
            .loading(self.loading)
            .disabled(self.loading || !can_filter)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));

        // Completion indicator icon
        let status_icon = if self.done {
            Icon::new(CustomIconName::CircleCheckBig) // All data loaded
        } else {
            Icon::new(CustomIconName::CircleDotDashed) // More data available
        };

        h_flex()
            .h_full()
            .w_full()
            // Left side: table + footer
            .child(
                v_flex()
                    .h_full()
                    .when(self.edit_row.is_some(), |this| this.w_1_2())
                    .when(self.edit_row.is_none(), |this| this.w_full())
                    // Main table area
                    .child(
                        div().flex_1().w_full().child(
                            Table::new(&self.table_state)
                                .stripe(true) // Alternating row colors for better readability
                                .bordered(true) // Table borders
                                .scrollbar_visible(true, true), // Show both scrollbars
                        ),
                    )
                    // Footer toolbar with search and status
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .p_3()
                            // Left side: Add button and search input
                            .child(
                                h_flex()
                                    .gap_2()
                                    .when(can_add, |this| {
                                        this.child(
                                            Button::new("add-value-btn")
                                                .icon(CustomIconName::FilePlusCorner)
                                                .tooltip(i18n_kv_table(cx, "add_value_tooltip"))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.handle_add_row(window, cx);
                                                })),
                                        )
                                    })
                                    .when(can_filter, |this| {
                                        this.child(
                                            Input::new(&self.keyword_state)
                                                .w(px(KEYWORD_INPUT_WIDTH))
                                                .suffix(search_btn)
                                                .cleanable(true),
                                        )
                                    })
                                    .flex_1(),
                            )
                            // Right side: Status icon and count
                            .child(status_icon.text_color(text_color).mr_2())
                            .child(
                                Label::new(format!("{} / {}", self.items_count, self.total_count))
                                    .text_sm()
                                    .text_color(text_color),
                            ),
                    ),
            )
            // Right side: edit panel (full height)
            .when(self.edit_row.is_some(), |this| {
                this.child(
                    div()
                        .id("kv-table-on-edit-overlay")
                        .w_1_2()
                        .h_full()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .p_2()
                        .flex()
                        .flex_col()
                        .child(self.render_edit_form(cx))
                        .on_click(cx.listener(|_this, _, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            })
            .on_action(cx.listener(move |this, event: &EditorAction, window, cx| match event {
                EditorAction::Save => {
                    this.handle_add_or_update_value(window, cx);
                }
                _ => {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(move |this, event: &Escape, _window, cx| match event {
                Escape => {
                    this.edit_row = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .into_any_element()
    }
}

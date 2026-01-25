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
    assets::CustomIconName,
    components::{INDEX_COLUMN_NAME, ZedisKvDelegate, ZedisKvFetcher},
    states::{ServerEvent, ZedisGlobalStore, ZedisServerState, i18n_common, i18n_kv_table},
};
use gpui::{Entity, SharedString, Subscription, TextAlign, Window, div, prelude::*, px};
use gpui_component::highlighter::Language;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, PixelsExt,
    button::{Button, ButtonVariants},
    form::field,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{Table, TableState},
    v_flex,
};
use std::sync::Arc;
use tracing::info;

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
    /// Action buttons column (edit, delete, etc.)
    Action,
}

/// Configuration for a table column including name, width, and alignment.
#[derive(Clone, Default, Debug)]
pub struct KvTableColumn {
    /// Whether the column is readonly
    pub readonly: bool,
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
    pub fn with_readonly(mut self, readonly: bool) -> Self {
        self.readonly = readonly;
        self
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
    key_changed: bool,
    /// Whether the table is readonly
    readonly: bool,
    /// The row index that is being edited
    edit_row: Option<usize>,
    /// Columns configuration
    edit_columns: Vec<KvTableColumn>,
    /// Input states for editable cells, keyed by column index.
    value_states: Vec<Entity<InputState>>,
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

        // Append action column at the end
        columns.push(KvTableColumn {
            column_type: KvTableColumnType::Action,
            name: i18n_common(cx, "action"),
            width: Some(100.0),
            align: Some(TextAlign::Center),
            ..Default::default()
        });

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
                ServerEvent::ValuePaginationFinished(_)
                | ServerEvent::ValueLoaded(_)
                | ServerEvent::ValueAdded(_)
                | ServerEvent::ValueUpdated(_) => {
                    let fetcher = Self::new_values(server_state.clone(), cx);
                    this.loading = false;
                    this.done = fetcher.is_done();
                    this.items_count = fetcher.rows_count();
                    this.total_count = fetcher.count();
                    this.table_state.update(cx, |state, _| {
                        state.delegate_mut().set_fetcher(fetcher);
                    });
                }
                // Clear search when key selection changes
                ServerEvent::KeySelected(_) => {
                    this.key_changed = true;
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
        // Initialize table data and state
        let fetcher = Self::new_values(server_state, cx);
        let done = fetcher.is_done();
        let items_count = fetcher.rows_count();
        let total_count = fetcher.count();
        let edit_columns = columns
            .iter()
            .filter(|column| column.column_type == KvTableColumnType::Value)
            .cloned()
            .collect::<Vec<_>>();
        let fetcher = Arc::new(fetcher);
        let mut delegate = ZedisKvDelegate::new(
            Self::new_columns(columns.clone(), window, cx),
            fetcher.clone(),
            window,
            cx,
        );
        if readonly {
            delegate.enable_readonly();
        }

        if fetcher.is_form_editor() {
            let view = cx.entity();
            delegate.set_on_edit(Some(Box::new(move |row_ix, values, window, cx| {
                view.update(cx, |state, cx| {
                    state.set_edit_values(row_ix, values, window, cx);
                });
            })));
        }

        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));
        let value_states = edit_columns
            .iter()
            .map(|column| {
                cx.new(|cx| {
                    if column.readonly {
                        InputState::new(window, cx)
                    } else {
                        InputState::new(window, cx)
                            .code_editor(Language::from_str("json").name())
                            .line_number(true)
                            .indent_guides(true)
                            .soft_wrap(true)
                    }
                })
            })
            .collect::<Vec<_>>();
        info!("Creating new key value table view");

        Self {
            table_state,
            keyword_state,
            items_count,
            total_count,
            done,
            loading: false,
            key_changed: false,
            edit_row: None,
            value_states,
            readonly,
            fetcher,
            edit_columns,
            _subscriptions: subscriptions,
        }
    }

    fn set_edit_values(
        &mut self,
        row_ix: usize,
        values: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edit_row == Some(row_ix) {
            self.edit_row = None;
            return;
        }
        self.edit_row = Some(row_ix);
        self.value_states.iter().enumerate().for_each(|(index, state)| {
            state.update(cx, |input, cx| {
                let Some(value) = values.get(index) else {
                    return;
                };
                input.set_value(value.clone(), window, cx);
            });
        });
    }

    /// Triggers a filter operation using the current keyword from the input field.
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        let keyword = self.keyword_state.read(cx).value();
        self.loading = true;
        self.table_state.update(cx, |state, cx| {
            state.delegate().fetcher().filter(keyword, cx);
        });
    }
    fn handle_update_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_ix) = self.edit_row else {
            return;
        };
        let mut values = Vec::with_capacity(self.value_states.len());
        for state in self.value_states.iter() {
            let value = state.read(cx).value();
            values.push(value);
        }
        self.fetcher.handle_update_value(row_ix, values, window, cx);
        self.edit_row = None;
    }
    /// Renders the edit form for the current row.
    fn render_edit_form(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = v_flex().size_full().gap_3();
        for (index, column) in self.edit_columns.iter().enumerate() {
            let Some(value_state) = self.value_states.get(index) else {
                continue;
            };
            let input = Input::new(value_state).disabled(column.readonly).h_full();

            let inner_content = if index != 0 {
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
                .when(index != 0, |this| this.flex_1().h_full());

            form = form.child(wrapped_field);
        }
        form.child(
            div().flex_none().child(
                field().child(
                    h_flex()
                        .id("kv-table-edit-form-btn-group")
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("cancel-edit-btn")
                                .h(px(30.))
                                .icon(IconName::CircleX)
                                .label("Cancel")
                                .flex_1()
                                .on_click(cx.listener(|this, _, _, _cx| {
                                    this.edit_row = None;
                                })),
                        )
                        .child(
                            Button::new("save-edit-btn")
                                .h(px(30.))
                                .icon(IconName::Check)
                                .label("Save")
                                .flex_1()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.handle_update_row(window, cx);
                                })),
                        ),
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
        if self.key_changed {
            self.keyword_state.update(cx, |input, cx| {
                input.set_value(SharedString::default(), window, cx);
            });
            self.key_changed = false;
        }

        // Handler for adding new values
        let handle_add_value = cx.listener(|this, _, window, cx| {
            this.table_state.update(cx, |state, cx| {
                state.delegate().fetcher().handle_add_value(window, cx);
            });
        });

        // Search button with loading state
        let search_btn = Button::new("kv-table-search-btn")
            .ghost()
            .icon(IconName::Search)
            .tooltip(i18n_kv_table(cx, "search_tooltip"))
            .loading(self.loading)
            .disabled(self.loading)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));

        // Completion indicator icon
        let status_icon = if self.done {
            Icon::new(CustomIconName::CircleCheckBig) // All data loaded
        } else {
            Icon::new(CustomIconName::CircleDotDashed) // More data available
        };

        v_flex()
            .h_full()
            .w_full()
            .relative()
            // Main table area
            .child(
                div().size_full().flex_1().child(
                    Table::new(&self.table_state)
                        .stripe(true) // Alternating row colors for better readability
                        .bordered(true) // Table borders
                        .scrollbar_visible(true, true), // Show both scrollbars
                ),
            )
            // Footer toolbar with search and status
            .child(
                h_flex()
                    .w_full()
                    .p_2()
                    // Left side: Add button and search input
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("add-value-btn")
                                    .icon(CustomIconName::FilePlusCorner)
                                    .disabled(self.readonly)
                                    .tooltip(if self.readonly {
                                        i18n_common(cx, "disable_in_readonly")
                                    } else {
                                        i18n_kv_table(cx, "add_value_tooltip")
                                    })
                                    .on_click(handle_add_value),
                            )
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .suffix(search_btn)
                                    .cleanable(true),
                            )
                            .flex_1(),
                    )
                    // Right side: Status icon and count
                    .child(status_icon.text_color(text_color).mr_2())
                    .child(
                        Label::new(format!("{} / {}", self.items_count, self.total_count))
                            .text_sm()
                            .text_color(text_color),
                    ),
            )
            .when(self.edit_row.is_some(), |this| {
                this.child(
                    div()
                        .id("kv-table-on-edit-overlay")
                        .absolute()
                        .top_1_2()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .border_t_1()
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
            .into_any_element()
    }
}

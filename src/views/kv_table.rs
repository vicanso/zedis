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

use crate::constants::{EDITOR_KEY_BAR_HEIGHT, STATUS_BAR_HEIGHT};
use crate::helpers::get_font_family;
use crate::{
    assets::CustomIconName,
    components::{INDEX_COLUMN_NAME, KvTableColumn, KvTableColumnType, KvTableMode, ZedisKvDelegate, ZedisKvFetcher},
    helpers::{EditorAction, humanize_keystroke},
    states::{
        KeyType, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_kv_table,
        i18n_list_editor,
    },
};
use gpui::{Entity, SharedString, Subscription, TextAlign, Window, div, prelude::*, px};
use gpui_component::TITLE_BAR_HEIGHT;
use gpui_component::highlighter::Language;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Escape, Input, InputEvent, InputState},
    label::Label,
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use indexmap::IndexMap;
use rust_i18n::t;
use std::sync::Arc;
use tracing::info;
use zedis_ui::{ZedisDialog, ZedisForm, ZedisFormField, ZedisFormFieldType, ZedisFormOptions};

const FOOTER_HEIGHT: f32 = 50.0;

/// Width of the keyword search input field in pixels
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

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
    original_values: IndexMap<SharedString, SharedString>,
    /// Whether the values have been modified
    values_modified: bool,
    /// Whether the values should be filled
    values_should_fill: bool,
    columns: Vec<KvTableColumn>,
    /// Input states for editable cells, keyed by column index.
    value_states: Vec<(usize, Entity<InputState>)>,
    /// The push mode for the list
    list_push_mode_state: Entity<usize>,
    /// The form for the editor
    editor_form: Option<Entity<ZedisForm>>,
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
        subscriptions.push(cx.subscribe(&table_state, |this, _, event, cx| match event {
            TableEvent::SelectRow(row_ix) => {
                this.handle_select_row(*row_ix, cx);
            }
            TableEvent::ClearSelection => {
                this.edit_row = None;
            }
            _ => {}
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
            values_should_fill: false,
            original_values: IndexMap::new(),
            values_modified: false,
            value_states,
            readonly,
            mode,
            fetcher,
            columns,
            editor_form: None,
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
        if !self
            .mode
            .intersects(KvTableMode::UPDATE | KvTableMode::REMOVE | KvTableMode::ADD)
        {
            return;
        }

        // if is add mode, clear the form
        if self.is_adding_row() {
            self.editor_form = None;
        } else {
            self.values_should_fill = true;
        }
        self.edit_row = Some(row_ix);
        self.original_values.clear();
        for (index, column) in self.columns.iter().enumerate() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            let value = self.fetcher.get(row_ix, index + 1).unwrap_or_default();
            self.original_values.insert(column.name.clone(), value);
        }
        self.editor_form = None;
        self.values_modified = false;
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
        let mut focused = false;
        self.value_states.iter().for_each(|(index, state)| {
            let auto_created = self
                .columns
                .get(*index)
                .map(|column| column.auto_created)
                .unwrap_or(false);

            state.update(cx, |input, cx| {
                if !auto_created && !focused {
                    input.focus(window, cx);
                    focused = true;
                }
                input.set_value(SharedString::default(), window, cx);
            });
        });
        self.editor_form = None;
        self.original_values.clear();
        self.values_modified = false;
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

        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!(
            "common.remove_item_prompt",
            row = row_ix + 1,
            value = value,
            locale = locale
        );
        let title = i18n_common(cx, "remove_title");

        ZedisDialog::new_alert(title, message.to_string())
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                fetcher.remove(row_ix, cx);
                entity.update(cx, |this, _cx| {
                    this.edit_row = None;
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
    fn enhance_handle_add_or_update_value(
        &mut self,
        data: IndexMap<SharedString, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        let mut values = Vec::with_capacity(data.len());
        let include_field_names = self.fetcher.include_field_names();
        for (name, value) in data {
            if include_field_names {
                values.push(name);
            }
            values.push(value);
        }
        if row_ix == usize::MAX {
            self.fetcher.handle_add_value(values, window, cx);
        } else {
            self.fetcher.handle_update_value(row_ix, values, window, cx);
        }
        self.editor_form = None;
        self.edit_row = None;
    }
    fn enhance_render_edit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(form) = &self.editor_form {
            if std::mem::take(&mut self.values_should_fill) {
                let original_values = &self.original_values;
                form.update(cx, move |form, cx| {
                    form.reset_form(original_values, window, cx);
                });
            }
            return form.clone();
        }
        let mut fields = Vec::with_capacity(4);
        let is_adding = self.is_adding_row();
        let mut reset_form_height = window.viewport_size().height.as_f32()
            - TITLE_BAR_HEIGHT.as_f32()
            - STATUS_BAR_HEIGHT
            - EDITOR_KEY_BAR_HEIGHT
            - FOOTER_HEIGHT;
        let normal_field_height = 60.;
        if is_adding && self.fetcher.key_type() == KeyType::List {
            fields.push(
                ZedisFormField::new("position", i18n_list_editor(cx, "position"))
                    .field_type(ZedisFormFieldType::RadioGroup)
                    .options(vec!["RPUSH".into(), "LPUSH".into()]),
            );
            reset_form_height -= normal_field_height;
        }

        let mut flex_field_count = 0;

        for column in self.columns.iter() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            if column.flex {
                flex_field_count += 1;
                continue;
            }
            reset_form_height -= normal_field_height;
        }
        let flex_field_height = (reset_form_height / flex_field_count as f32).max(150.);

        let mut first = true;
        for column in self.columns.iter() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            let mut field = ZedisFormField::new(column.name.clone(), column.name.clone())
                .focus()
                .font_family(get_font_family());
            if self.fetcher.fields_required() {
                field = field.required();
            }
            if first {
                field = field.focus();
                first = false;
            }
            // TODO adjust to flex_1
            if column.flex {
                field = field.h(px(flex_field_height - 30.));
            }
            if let Some(field_type) = column.field_type.clone() {
                if field_type == ZedisFormFieldType::Editor && !column.flex {
                    field = field.h(px(150.));
                }
                field = field.field_type(field_type);
            }

            if !is_adding && let Some(value) = self.original_values.get(&column.name) {
                field = field.default_value(value.clone());
            }
            fields.push(field);
        }
        let submit_entity = cx.entity().clone();
        let cancel_entity = submit_entity.clone();
        let remove_entity = submit_entity.clone();
        let on_cancel = move |_window: &mut Window, cx: &mut Context<ZedisForm>| {
            cancel_entity.update(cx, |this, _| {
                this.edit_row = None;
            });
            true
        };
        let on_submit =
            move |values: IndexMap<SharedString, SharedString>, window: &mut Window, cx: &mut Context<ZedisForm>| {
                submit_entity.update(cx, |this, cx| {
                    this.enhance_handle_add_or_update_value(values, window, cx);
                });
                true
            };
        let can_remove = self.mode.contains(KvTableMode::REMOVE);
        let can_update = self.mode.contains(KvTableMode::UPDATE);
        let form_opts = ZedisFormOptions::new(fields)
            .on_cancel(on_cancel)
            .cancel_label(i18n_common(cx, "cancel"))
            .when(is_adding || can_update, |this| {
                this.on_submit(on_submit).confirm_tooltip(humanize_keystroke("cmd-s"))
            })
            .when_else(
                is_adding,
                |this| this.confirm_label(i18n_common(cx, "save")),
                |this| this.confirm_label(i18n_common(cx, "update")),
            )
            .when(!is_adding && can_remove, |this| {
                let remove_label = i18n_common(cx, "remove");
                this.foot_actions(move |_window, _cx| {
                    vec![
                        Button::new("remove-edit-btn")
                            .icon(CustomIconName::FileXCorner)
                            .label(remove_label.clone())
                            .on_click({
                                let remove_entity = remove_entity.clone();
                                move |_, window, cx| {
                                    remove_entity.update(cx, |this, cx| {
                                        this.handle_remove_row(window, cx);
                                    });
                                }
                            }),
                    ]
                })
            })
            .when(self.fetcher.support_add_fields(), |this| this.support_add_fields());

        let form = cx.new(|cx| ZedisForm::new("kv-table-edit-form", form_opts, window, cx));
        self.editor_form = Some(form.clone());
        form.clone()
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
                            DataTable::new(&self.table_state)
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
                        .child(self.enhance_render_edit_form(window, cx))
                        .on_click(cx.listener(|_this, _, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            })
            .on_action(cx.listener(move |this, event: &EditorAction, window, cx| match event {
                EditorAction::Save => {
                    let Some(values) = this
                        .editor_form
                        .as_ref()
                        .and_then(|item| item.update(cx, |form, cx| form.try_get_values(cx)))
                    else {
                        return;
                    };
                    this.enhance_handle_add_or_update_value(values, window, cx);
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

/// Generates a KvTable-based editor struct and its `Render` implementation.
///
/// Each generated editor wraps a `ZedisKvTable<$values>` and renders it
/// as a full-size container. The `new()` constructor is left to be
/// implemented manually per editor.
macro_rules! define_kv_editor {
    ($editor:ident, $values:ident) => {
        pub struct $editor {
            table_state: gpui::Entity<$crate::views::ZedisKvTable<$values>>,
        }

        impl gpui::Render for $editor {
            fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                gpui::div()
                    .size_full()
                    .min_h_0()
                    .child(self.table_state.clone())
                    .into_any_element()
            }
        }
    };
}

pub(crate) use define_kv_editor;

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

use crate::connection::Capability;
use crate::helpers::saves_files;
use crate::helpers::{bytes_to_hex_text, clearing_an_applied_filter, get_mono_font_family};
use crate::{
    assets::CustomIconName,
    components::{
        KvRowAction, KvRowMenu, KvTableColumn, KvTableColumnType, KvTableMode, ZedisKvDelegate, ZedisKvFetcher,
        with_leading_columns,
    },
    helpers::{EditorAction, KeyOpAction, build_csv, hot_key_label},
    states::{
        DataFormat, JsonView, KeyType, KvElement, ServerEvent, ZedisGlobalStore, ZedisServerState, detect_and_decode,
        i18n_common, i18n_editor, i18n_key_ops, i18n_kv_table, i18n_list_editor, i18n_zset_editor,
        update_app_state_and_save_quiet_debounced,
    },
    views::{
        JsonTreeEvent, JsonTreeTarget, ZedisJsonTree, export_to_file, key_op_title_key, open_key_op_dialog,
        open_score_filter_dialog, open_ttl_dialog, placeholders::Placeholders,
    },
};
use gpui::{
    AnyElement, App, Bounds, Entity, FocusHandle, Pixels, SharedString, Subscription, WeakEntity, Window, canvas, div,
    prelude::*, px,
};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    input::{Escape, Input, InputEvent, InputState},
    label::Label,
    resizable::{ResizableState, h_resizable, resizable_panel},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use indexmap::IndexMap;
use rust_i18n::t;
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::info;
use zedis_core::json::numbers_survive_round_trip;
use zedis_ui::{
    ZedisDialog, ZedisForm, ZedisFormField, ZedisFormFieldType, ZedisFormOptions, decode_extra_pair_key,
    extra_pair_rows, extra_single_rows,
};

mod entry_panel;
mod layout;
mod operations;
mod toolbar;

use entry_panel::PreviewMode;
#[cfg(test)]
use entry_panel::{
    build_kv_add_batches, element_json_document, opening_preview, preview_modes, row_holds, tree_editable,
};
#[cfg(test)]
use layout::{DATA_COLUMN_MIN_WIDTH, ENTRY_PANEL_MIN_WIDTH, PanelSplit, fit_columns};
use layout::{TABLE_MIN_WIDTH, entry_panel_split, width_probe};

pub const FOOTER_HEIGHT: f32 = 50.0;

type ZedisKvTableActionButtonFactory = Box<dyn Fn(&mut Window, &mut App) -> Vec<Button>>;

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
    /// Re-words the placeholders above after a language change (#198).
    placeholders: Placeholders,
    /// The keyword the rows on screen were fetched with: empty until a
    /// filter runs, and again once the key changes.
    applied_keyword: SharedString,
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
    /// Flag indicating columns have changed and delegate needs rebuild
    columns_dirty: bool,
    /// Whether the table is readonly
    readonly: bool,
    /// Supported operations mode (add, update, remove, filter)
    mode: KvTableMode,
    /// The mode this table was configured with, before read-only is applied —
    /// kept so a live read-only toggle restores the intended affordances rather
    /// than always falling back to `ALL`.
    base_mode: KvTableMode,
    /// The row index that is being edited
    edit_row: Option<usize>,
    /// Where focus goes when the entry panel closes. The panel's field held
    /// it, and a focus on nothing has no path for a key binding to travel:
    /// ⌘J after Cancel did nothing until something was clicked.
    focus_handle: FocusHandle,
    /// Whether the entry panel was open at the last render.
    panel_was_open: bool,
    /// The original values of the row that is being edited
    original_values: IndexMap<SharedString, SharedString>,
    /// The element the open row's main value column holds, for the decoded
    /// preview above the form.
    edit_element: Option<KvElement>,
    /// The form field that column edits — where a tree operation's result
    /// is written.
    edit_element_column: Option<SharedString>,
    /// Whether `edit_element` parses as JSON (stored or decoded), so the
    /// preview can offer a tree. Decided once when the row opens.
    edit_element_json: bool,
    /// Which rendering the preview shows.
    preview: PreviewMode,
    /// The tree rendering of `edit_element`; `Local` target, so operations
    /// rewrite the form's text and Save writes them.
    json_tree: Entity<ZedisJsonTree>,
    /// The tree's document lags the form's text; rebuilt when the tree is
    /// next shown (see `sync_json_tree`).
    json_tree_stale: bool,
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
    /// User-resized width of the entry panel (right pane); mirrors the
    /// persisted `kv_edit_panel_width` so renders don't re-read the store.
    panel_width: Option<Pixels>,
    /// The width this view was last laid out at — what the table and the
    /// entry panel have to share. Written after layout (`measure` in
    /// `render`), read by the next render; `None` until the first paint.
    pane_width: Rc<Cell<Option<Pixels>>>,
    /// The width the table itself was last laid out at: the pane's, less the
    /// entry panel while that is open. Written and read like `pane_width`.
    table_width: Rc<Cell<Option<Pixels>>>,
    /// The `table_width` the columns were last fitted to.
    fitted_width: Option<Pixels>,
    /// The user dragged a column border: the widths stay as they left them
    /// until the table has other columns.
    columns_sized_by_hand: bool,
    /// Fetcher instance
    fetcher: Arc<T>,
    /// Server state, kept so the CSV export action can call `export_to_file`.
    server_state: Entity<ZedisServerState>,
    /// Factory that produces extra action buttons for the footer toolbar each render.
    /// Set by the owner of this table (e.g. ZedisStreamEditor) so button logic
    /// can reference the owner's entity context.
    action_button_factory: Option<ZedisKvTableActionButtonFactory>,
    /// Event subscriptions for server state and input changes
    _subscriptions: Vec<Subscription>,
}

impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    /// Project the configured mode through the capability matrix
    /// (`connection::Capability`): container mutations (add / update /
    /// remove) drop out on a read-only connection, while FILTER — a pure
    /// read (`Capability::SearchFilter`) — survives it.
    fn effective_mode(base: KvTableMode, readonly: bool) -> KvTableMode {
        let mut mode = base;
        if !Capability::MutateContainer.allowed(readonly) {
            mode.remove(KvTableMode::ADD | KvTableMode::UPDATE | KvTableMode::REMOVE);
        }
        if !Capability::SearchFilter.allowed(readonly) {
            mode.remove(KvTableMode::FILTER);
        }
        mode
    }

    /// Creates a new fetcher instance with the current server value.
    fn new_values(server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) -> T {
        let value = server_state.read(cx).value().cloned().unwrap_or_default();
        T::new(server_state, value)
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
    ///
    /// Illustrative, not runnable — it names a caller's own bindings. Now
    /// that this crate has a library target its doc examples are compiled,
    /// so the fence says so.
    /// ```ignore
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

                    // Check if columns changed (e.g., Stream with new fields)
                    if let Some(new_columns) = fetcher.columns(cx) {
                        let columns_changed = new_columns.len() != this.columns.len()
                            || new_columns
                                .iter()
                                .zip(this.columns.iter())
                                .any(|(a, b)| a.name != b.name);
                        if columns_changed {
                            this.columns = new_columns.clone();
                            this.columns_dirty = true;
                            this.edit_row = None;
                            this.editor_form = None;
                        }
                    }

                    // A selection is a set of row *indices*, so anything that
                    // can renumber rows invalidates it. Paging only appends,
                    // which is why it is the one event that keeps it.
                    let renumbered = !matches!(event, ServerEvent::ValuePaginationFinished);
                    this.table_state.update(cx, |state, _| {
                        let delegate = state.delegate_mut();
                        delegate.set_fetcher(fetcher);
                        if renumbered {
                            delegate.clear_selection();
                        }
                    });
                    // The open editor is addressed by row index too: a reload,
                    // a pop or a filter can put another element behind it, and
                    // Update would then write to that one.
                    if renumbered {
                        this.close_editor_if_row_moved();
                    }
                }
                // Read-only was toggled from the status bar — recompute the
                // effective mode from the configured base mode so the edit /
                // add / remove affordances update live, instead of only after
                // the editor is recreated on a type switch.
                ServerEvent::ServerInfoUpdated => {
                    let readonly = server_state.read(cx).readonly();
                    if readonly != this.readonly {
                        this.readonly = readonly;
                        this.mode = Self::effective_mode(this.base_mode, readonly);
                        // Locking mid-edit: close any in-progress row editor.
                        this.edit_row = None;
                        this.editor_form = None;
                        cx.notify();
                    }
                }
                // Clear search when key selection changes
                ServerEvent::KeySelected(_) => {
                    this.edit_row = None;
                    this.key_changed = Some(true);
                    // Another key's rows are fetched unfiltered.
                    this.applied_keyword = SharedString::default();
                    this.table_state
                        .update(cx, |state, _| state.delegate_mut().clear_selection());
                }
                _ => {}
            }
        }));

        // Initialize keyword search input field
        let keyword_state = cx.new(|cx| InputState::new(window, cx).clean_on_escape());
        let mut placeholders = Placeholders::new(cx);
        placeholders.add(&keyword_state, |cx| i18n_common(cx, "keyword_placeholder"), window, cx);

        // Enter filters. Emptying the box after a filter ran — its ×,
        // Escape, or deleting to empty — shows every row again, which only
        // Enter in the empty box used to (#194).
        subscriptions.push(cx.subscribe(&keyword_state, |this, input, event, cx| match event {
            InputEvent::PressEnter { .. } => this.handle_filter(cx),
            InputEvent::Change
                if clearing_an_applied_filter(input.read(cx).value().as_ref(), this.applied_keyword.as_ref()) =>
            {
                this.handle_filter(cx)
            }
            _ => {}
        }));

        let readonly = server_state.read(cx).readonly();

        // Default to ALL, projected through the capability matrix.
        let mode = Self::effective_mode(KvTableMode::ALL, readonly);

        // Initialize table data and state
        let fetcher = Arc::new(Self::new_values(server_state.clone(), cx));
        let done = fetcher.is_done();
        let items_count = fetcher.rows_count();
        let total_count = fetcher.count();
        let selectable = fetcher.supports_batch_remove() && mode.contains(KvTableMode::REMOVE);
        let delegate = ZedisKvDelegate::new(
            Self::new_columns(columns.clone(), selectable, Self::estimated_width(window, cx)),
            fetcher.clone(),
            window,
            cx,
        );

        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));
        // The row menu's Open and Delete come back here (#217).
        let table = cx.entity().downgrade();
        table_state.update(cx, |state, _| {
            state.delegate_mut().on_row_action(Rc::new(move |action, window, cx| {
                let _ = table.update(cx, |table, cx| table.run_row_action(action, window, cx));
            }));
        });

        // Subscribe to row selection events (mode check will be done in handler)
        subscriptions.push(cx.subscribe(&table_state, |this, _, event, cx| match event {
            TableEvent::SelectRow(row_ix) => {
                this.handle_select_row(*row_ix, cx);
            }
            TableEvent::ClearSelection => {
                this.edit_row = None;
            }
            // A border dragged by hand: these widths are the user's now.
            TableEvent::ColumnWidthsChanged(_) => {
                this.columns_sized_by_hand = true;
            }
            _ => {}
        }));

        // The tree behind the entry panel's JSON preview. No JSONPath bar
        // lives there, so its row menu offers no Query.
        let json_tree = cx.new(|cx| ZedisJsonTree::new(server_state.clone(), cx).without_query());
        subscriptions.push(cx.subscribe(&json_tree, |this, _, event, cx| match event {
            JsonTreeEvent::DocEdited(text) => this.apply_tree_edit(text.clone(), cx),
            JsonTreeEvent::QueryPath(_) => {}
        }));

        // Nothing renders these — `reset_edit_state` clears them and focuses
        // the first, and the visible cell editor is the `ZedisForm` built in
        // `render_edit_form`. They stay plain `InputState`s for that reason;
        // the code-editor options they used to carry never reached a frame.
        let value_states = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.column_type == KvTableColumnType::Value)
            .map(|(index, _)| (index, cx.new(|cx| InputState::new(window, cx))))
            .collect::<Vec<_>>();
        info!("Creating new key value table view with mode: {:?}", mode);

        Self {
            table_state,
            keyword_state,
            placeholders,
            applied_keyword: SharedString::default(),
            items_count,
            total_count,
            done,
            loading: false,
            key_changed: None,
            columns_dirty: false,
            edit_row: None,
            focus_handle: cx.focus_handle(),
            panel_was_open: false,
            values_should_fill: false,
            original_values: IndexMap::new(),
            edit_element: None,
            edit_element_column: None,
            edit_element_json: false,
            preview: PreviewMode::Decoded,
            json_tree,
            json_tree_stale: true,
            values_modified: false,
            value_states,
            readonly,
            mode,
            base_mode: KvTableMode::ALL,
            panel_width: cx.global::<ZedisGlobalStore>().read(cx).kv_edit_panel_width(),
            pane_width: Rc::new(Cell::new(None)),
            table_width: Rc::new(Cell::new(None)),
            fitted_width: None,
            columns_sized_by_hand: false,
            fetcher,
            server_state,
            columns,
            editor_form: None,
            action_button_factory: None,
            list_push_mode_state: cx.new(|_cx| 0),
            _subscriptions: subscriptions,
        }
    }

    /// Sets a factory closure that produces extra action buttons for the footer each render.
    /// Call this from the component that owns this table (e.g. ZedisStreamEditor)
    /// to inject custom buttons whose click handlers reference the owner's entity context.
    pub fn set_action_button_factory(&mut self, factory: ZedisKvTableActionButtonFactory) {
        self.action_button_factory = Some(factory);
    }

    /// Sets the keyword search input to `keyword` and runs the filter immediately.
    pub fn apply_filter(&mut self, keyword: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.keyword_state.update(cx, |input, cx| {
            input.set_value(keyword, window, cx);
        });
        self.handle_filter(cx);
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
    ///
    /// Illustrative, not runnable — see [`Self::new`].
    /// ```ignore
    /// let table = ZedisKvTable::new(columns, server_state, window, cx)
    ///     .mode(KvTableMode::ADD | KvTableMode::REMOVE | KvTableMode::FILTER);
    /// ```
    pub fn mode(mut self, mode: KvTableMode) -> Self {
        // Remember the intended mode so a later read-only toggle can restore it.
        self.base_mode = mode;
        self.mode = Self::effective_mode(mode, self.readonly);
        self
    }

    /// Whether the multi-select column belongs in this table: the type must
    /// have a batch delete, and the connection must allow deleting at all.
    fn selectable(&self) -> bool {
        self.fetcher.supports_batch_remove() && self.mode.contains(KvTableMode::REMOVE)
    }
}

impl<T: ZedisKvFetcher> Render for ZedisKvTable<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.placeholders.follow(window, cx);

        // The panel has just closed — by Cancel, Escape, a save, the row
        // going away — and it held the focus: hand it to the table. Only
        // then: a panel that closes because another key was opened must not
        // pull the caret out of the key filter the user is typing in.
        let panel_open = self.edit_row.is_some();
        if self.panel_was_open && !panel_open && self.focus_handle.contains_focused(window, cx) {
            self.focus_handle.focus(window, cx);
        }
        self.panel_was_open = panel_open;

        // The cells draw their hover button on the row's colour, and only
        // the table's state knows which row it paints as selected.
        let open_row = self.table_state.read(cx).selected_row();
        let row_menu = self.row_menu(cx);
        self.table_state.update(cx, |state, _| {
            let delegate = state.delegate_mut();
            delegate.set_open_row(open_row);
            delegate.set_row_menu(row_menu);
        });

        // The columns are fitted to the table's own width, as it was last
        // laid out: opening the entry panel, dragging its handle or resizing
        // the window gives the table another one, and the columns follow on
        // the next frame. Widths the user dragged are left alone.
        let table_width = self.table_width.get();
        let fit_width = table_width.unwrap_or_else(|| Self::estimated_width(window, cx));
        // Rebuild delegate columns when they changed (e.g., Stream with new fields)
        let columns_changed = std::mem::take(&mut self.columns_dirty);
        if columns_changed {
            self.columns_sized_by_hand = false;
            // Rebuild value_states for the new columns
            self.value_states = self
                .columns
                .iter()
                .enumerate()
                .filter(|(_, column)| column.column_type == KvTableColumnType::Value)
                .map(|(index, _)| (index, cx.new(|cx| InputState::new(window, cx))))
                .collect();
        }
        let width_changed = table_width.is_some() && table_width != self.fitted_width;
        if columns_changed || (width_changed && !self.columns_sized_by_hand) {
            let new_delegate_columns = Self::new_columns(self.columns.clone(), self.selectable(), fit_width);
            self.table_state.update(cx, |state, cx| {
                state.delegate_mut().set_columns(new_delegate_columns);
                state.refresh(cx);
            });
            self.fitted_width = table_width;
        }

        // Clear search input when key changes
        if let Some(true) = self.key_changed.take() {
            self.keyword_state.update(cx, |input, cx| {
                input.set_value(SharedString::default(), window, cx);
            });
        }

        // Left side: table + footer
        let left = v_flex()
            .h_full()
            .w_full()
            // Main table area
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .child(
                        DataTable::new(&self.table_state)
                            // One step under the default: 30px rows. This is a
                            // data grid, and at 32 a 1280×800 window showed 19
                            // rows of a hash that has 100,000 — but the densest
                            // size (26px rows, 12px text) read as cramped.
                            .small()
                            .stripe(true) // Alternating row colors for better readability
                            .bordered(false) // Table borders
                            .scrollbar_visible(true, true), // Show both scrollbars
                    )
                    // The table's width, for the columns to be fitted to.
                    .child(width_probe(self.table_width.clone(), cx.entity().downgrade(), true)),
            )
            // Footer toolbar with search and status
            .child(self.render_footer(window, cx));

        // With the entry panel open, split into user-resizable panes (width
        // persisted globally as `kv_edit_panel_width` — a display preference,
        // same policy as the key tree width). Without it, the table takes the
        // full width as before.
        let body = if self.edit_row.is_some() {
            let panel = div()
                .id("kv-table-on-edit-overlay")
                .size_full()
                .border_l_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .p_2()
                .flex()
                .flex_col()
                .child(self.enhance_render_edit_form(window, cx))
                .on_click(cx.listener(|_this, _, _, cx| {
                    cx.stop_propagation();
                }));
            // The pane as it was last laid out. Before the first paint —
            // the panel cannot be open that early — the app's own estimate.
            let pane_width = self.pane_width.get().unwrap_or_else(|| {
                let estimate = cx.global::<ZedisGlobalStore>().read(cx).content_width();
                estimate.unwrap_or(window.viewport_size().width * 0.5)
            });
            match entry_panel_split(pane_width.as_f32(), self.panel_width.map(|width| width.as_f32())) {
                Some(split) => h_resizable("kv-table-split")
                    .child(
                        resizable_panel()
                            .size_range(px(TABLE_MIN_WIDTH)..Pixels::MAX)
                            .child(left),
                    )
                    .child(
                        resizable_panel()
                            .size(px(split.panel))
                            .size_range(px(split.panel_min)..px(split.panel_max))
                            .child(panel),
                    )
                    .on_resize(cx.listener(|this, event: &Entity<ResizableState>, _window, cx| {
                        let Some(width) = event.read(cx).sizes().get(1).copied() else {
                            return;
                        };
                        this.panel_width = Some(width);
                        // Drags fire a stream of resize events — update state per
                        // event, write the config once the drag settles. Quiet:
                        // the width repaints locally via `this.panel_width`.
                        update_app_state_and_save_quiet_debounced(cx, "save_kv_edit_panel_width", move |state, _| {
                            state.set_kv_edit_panel_width(width);
                        });
                    }))
                    .into_any_element(),
                // No room for both: the entry being edited has the pane.
                None => panel.into_any_element(),
            }
        } else {
            left.into_any_element()
        };

        // The view's width, measured where it is known: after layout. The
        // split's limits come from it, so while the panel is open a new
        // width lays the split out again on the next frame — a window made
        // narrower with the panel open is what #182 was reported from.
        let measure = width_probe(self.pane_width.clone(), cx.entity().downgrade(), panel_open);

        h_flex()
            .track_focus(&self.focus_handle)
            .relative()
            .h_full()
            .w_full()
            .child(body)
            .child(measure)
            .on_action(cx.listener(move |this, event: &KeyOpAction, window, cx| {
                this.open_key_op_dialog(*event, window, cx);
            }))
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

#[cfg(test)]
mod tests {
    use super::{
        DATA_COLUMN_MIN_WIDTH, ENTRY_PANEL_MIN_WIDTH, JsonView, KeyType, PanelSplit, PreviewMode, TABLE_MIN_WIDTH,
        build_kv_add_batches, element_json_document, entry_panel_split, fit_columns, opening_preview, preview_modes,
        row_holds, tree_editable,
    };

    /// The table and the entry panel share the pane the table is in, not the
    /// window: the table keeps its naming column and its value wherever it
    /// is drawn at all, and the two never add up to more than the pane.
    #[test]
    fn the_entry_panel_leaves_the_table_its_columns() {
        let split = |pane, saved| entry_panel_split(pane, saved).expect("room for both");
        // A wide pane: the panel opens at half, the handle stops where the
        // table would lose its columns.
        assert_eq!(
            split(1200., None),
            PanelSplit {
                panel: 600.,
                panel_min: ENTRY_PANEL_MIN_WIDTH,
                panel_max: 880.,
            }
        );
        // The pane of the default window (649): half of the *window* was 600
        // here and left the table 49.
        assert_eq!(split(649., None).panel, 324.5);
        // A width dragged on a wide window does not take the table's share
        // on a narrow one — and is not forgotten either: it is only clamped.
        assert_eq!(split(649., Some(600.)).panel, 329.);
        assert_eq!(split(1200., Some(600.)).panel, 600.);
        assert_eq!(split(1200., Some(200.)).panel, ENTRY_PANEL_MIN_WIDTH);

        // The narrowest pane with room for both: each at its minimum.
        let least = ENTRY_PANEL_MIN_WIDTH + TABLE_MIN_WIDTH;
        assert_eq!(split(least, Some(600.)).panel, ENTRY_PANEL_MIN_WIDTH);
        // Narrower than that, the panel is shown alone — a table of 258
        // beside it (the pane of the smallest window) cut the value off.
        assert_eq!(entry_panel_split(least - 1., None), None);
        assert_eq!(entry_panel_split(578., None), None);

        for pane in [640., 649., 900., 1200., 2400.] {
            for saved in [None, Some(100.), Some(600.), Some(5000.)] {
                let split = split(pane, saved);
                assert!(
                    split.panel_min <= split.panel && split.panel <= split.panel_max,
                    "{pane} {saved:?}"
                );
                assert!(
                    split.panel_max + TABLE_MIN_WIDTH <= pane + 0.01,
                    "{pane} {saved:?}: {split:?}"
                );
            }
        }
    }

    /// The columns follow the table's own width, and in a narrow one the
    /// value stays in view: 320 beside the entry panel used to be the tick
    /// box, the row number, a 200px Field and 16px of the value (#182).
    #[test]
    fn a_narrow_table_keeps_its_value_in_view() {
        let widths = |columns: Vec<KvTableColumn>, table: f32| -> Vec<f32> {
            fit_columns(with_leading_columns(columns, true), table)
                .iter()
                .map(|column| column.width.expect("every column is given a width"))
                .collect()
        };
        let hash = || {
            vec![
                KvTableColumn::new("field", Some(200.)),
                KvTableColumn::new_flex("value"),
            ]
        };

        // Room for everything: Field keeps its width, the value has the rest.
        assert_eq!(widths(hash(), 700.), vec![32., 72., 200., 381.]);
        // Beside the entry panel, at the table's narrowest: both in view,
        // and nothing to scroll for.
        let beside_panel = widths(hash(), TABLE_MIN_WIDTH);
        assert_eq!(beside_panel, vec![32., 72., DATA_COLUMN_MIN_WIDTH, 105.]);
        assert!(beside_panel.iter().sum::<f32>() <= TABLE_MIN_WIDTH);
        // In between, the value is the wider of the two: 200px of Field in
        // a table of 414 would leave it 91.
        let between = widths(hash(), 414.);
        assert!(
            (between[2] - 120.).abs() < 0.01 && (between[3] - 175.).abs() < 0.01,
            "{between:?}"
        );

        // A column after the value — the field TTL — does not take its place:
        // it is the one that scrolls out of view.
        let with_ttl = || {
            let mut columns = hash();
            columns.push(KvTableColumn::new("ttl", Some(90.)));
            columns
        };
        assert_eq!(
            widths(with_ttl(), TABLE_MIN_WIDTH),
            vec![32., 72., DATA_COLUMN_MIN_WIDTH, DATA_COLUMN_MIN_WIDTH, 90.]
        );
        // And is in view where there is room for it: here Field at 200 used
        // to push it out.
        let fitted = widths(with_ttl(), 440.);
        assert!(fitted.iter().sum::<f32>() <= 440., "{fitted:?}");
        assert!(fitted[3] >= DATA_COLUMN_MIN_WIDTH, "{fitted:?}");

        // A sorted set has its value first, and the score after it gives way
        // the same.
        let zset = vec![
            KvTableColumn::new_flex("value"),
            KvTableColumn::new("score", Some(196.)),
        ];
        assert_eq!(
            widths(zset, TABLE_MIN_WIDTH),
            vec![32., 72., 105., DATA_COLUMN_MIN_WIDTH]
        );

        // A stream of many fields scrolls sideways rather than draw each a
        // few characters wide, and its id is not cut to make room for them.
        let mut stream = vec![KvTableColumn::new("id", Some(196.))];
        stream.extend((0..10).map(|ix| KvTableColumn::new(&format!("f{ix}"), None)));
        let fitted = widths(stream, 700.);
        assert_eq!(fitted[2], 196.);
        assert!(fitted[3..].iter().all(|width| *width == DATA_COLUMN_MIN_WIDTH));

        // Nothing to share: the widths are the ones that were asked for.
        let fixed = vec![KvTableColumn::new("a", Some(120.)), KvTableColumn::new("b", Some(80.))];
        assert_eq!(widths(fixed, 100.), vec![32., 72., 120., 80.]);
    }
    use crate::components::{KvTableColumn, with_leading_columns};
    use crate::states::{DataFormat, KvElement};
    use gpui::SharedString;
    use indexmap::IndexMap;

    fn opened(pairs: &[(&str, &str)]) -> IndexMap<SharedString, SharedString> {
        pairs
            .iter()
            .map(|(k, v)| (SharedString::from(k.to_string()), SharedString::from(v.to_string())))
            .collect()
    }

    #[test]
    fn an_editor_stays_only_on_the_row_it_opened_on() {
        let columns = vec![KvTableColumn::new("field", None), KvTableColumn::new_flex("value")];
        let original = opened(&[("field", "name"), ("value", "zedis")]);

        // The same field and value in the slot: nothing moved.
        let same = |ix: usize| Some(SharedString::from(["name", "zedis"][ix]));
        assert!(row_holds(&columns, &original, same));

        // A filter put another field behind the index: Update would rename
        // it away, so the editor must close.
        let other = |ix: usize| Some(SharedString::from(["age", "42"][ix]));
        assert!(!row_holds(&columns, &original, other));

        // The same field with a value changed elsewhere is not the element
        // the user started editing either.
        let changed = |ix: usize| Some(SharedString::from(["name", "other"][ix]));
        assert!(!row_holds(&columns, &original, changed));

        // A row that is gone never matches, even against empty originals.
        let empty = opened(&[("field", ""), ("value", "")]);
        assert!(!row_holds(&columns, &empty, |_| None));
    }

    #[test]
    fn json_shaped_elements_offer_a_tree_and_the_rest_do_not() {
        assert_eq!(
            preview_modes(true),
            vec![PreviewMode::Decoded, PreviewMode::Tree, PreviewMode::Hex]
        );
        assert_eq!(preview_modes(false), vec![PreviewMode::Decoded, PreviewMode::Hex]);
    }

    /// A row's element opens as the string editor's value does: a tree where
    /// it is JSON-shaped and Settings says tree, its text otherwise.
    #[test]
    fn a_json_element_opens_in_the_view_settings_names() {
        assert_eq!(opening_preview(true, JsonView::Tree), PreviewMode::Tree);
        assert_eq!(opening_preview(true, JsonView::Text), PreviewMode::Decoded);
        // No tree to open into.
        assert_eq!(opening_preview(false, JsonView::Tree), PreviewMode::Decoded);
    }

    #[test]
    fn only_a_json_element_gets_an_editable_tree() {
        // Stored JSON on a writable connection edits through the tree.
        assert!(tree_editable(DataFormat::Json, false, false, false));
        // ADR 8: a decoding is a read-only view, however JSON-shaped.
        assert!(!tree_editable(DataFormat::MessagePack, false, false, false));
        assert!(!tree_editable(DataFormat::Gzip, false, false, false));
        // A row being added, a read-only connection, and a fetcher that is
        // read-only on edit (streams) each keep the tree read-only.
        assert!(!tree_editable(DataFormat::Json, true, false, false));
        assert!(!tree_editable(DataFormat::Json, false, true, false));
        assert!(!tree_editable(DataFormat::Json, false, false, true));
    }

    #[test]
    fn the_tree_document_is_the_stored_json_or_the_decoding() {
        let stored = KvElement::from_text(r#"{"a":1}"#);
        assert_eq!(element_json_document(&stored, 1024), Some(serde_json::json!({"a": 1})));

        let packed = rmp_serde::to_vec(&serde_json::json!({"name": "zedis", "n": 7})).expect("msgpack");
        let decoded = KvElement::from_raw(packed);
        assert_eq!(decoded.format(), DataFormat::MessagePack);
        assert_eq!(
            element_json_document(&decoded, 1024),
            Some(serde_json::json!({"name": "zedis", "n": 7}))
        );

        // Plain text has no document, and neither does JSON whose decoding
        // was clipped: a tree must never be built from a truncated payload.
        assert_eq!(element_json_document(&KvElement::from_text("plain"), 1024), None);
    }

    #[test]
    fn build_kv_add_batches_dispatches_multiple_list_and_set_rows() {
        let mut list_data: IndexMap<SharedString, SharedString> = IndexMap::new();
        list_data.insert("position".into(), "1".into());
        list_data.insert("Value".into(), "first".into());
        list_data.insert("__extra_single_row_0".into(), "dup".into());
        list_data.insert("__extra_single_row_1".into(), "".into());
        list_data.insert("__extra_single_row_2".into(), "dup".into());

        let list_batches = build_kv_add_batches(KeyType::List, false, &["position".into(), "Value".into()], &list_data);
        assert_eq!(
            list_batches,
            vec![
                vec![SharedString::from("1"), SharedString::from("first")],
                vec![SharedString::from("1"), SharedString::from("dup")],
                vec![SharedString::from("1"), SharedString::from("dup")],
            ]
        );

        let mut set_data: IndexMap<SharedString, SharedString> = IndexMap::new();
        set_data.insert("Value".into(), "m1".into());
        set_data.insert("__extra_single_row_0".into(), "m2".into());

        let set_batches = build_kv_add_batches(KeyType::Set, false, &["Value".into()], &set_data);
        assert_eq!(
            set_batches,
            vec![vec![SharedString::from("m1")], vec![SharedString::from("m2")]]
        );
    }

    #[test]
    fn build_kv_add_batches_dispatches_multiple_zset_and_hash_rows() {
        let mut zset_data: IndexMap<SharedString, SharedString> = IndexMap::new();
        zset_data.insert("Value".into(), "m1".into());
        zset_data.insert("Score".into(), "1".into());
        zset_data.insert("m2".into(), "2.5".into());
        zset_data.insert("m3".into(), "".into());

        let zset_batches = build_kv_add_batches(KeyType::Zset, false, &["Value".into(), "Score".into()], &zset_data);
        assert_eq!(
            zset_batches,
            vec![
                vec![SharedString::from("m1"), SharedString::from("1")],
                vec![SharedString::from("m2"), SharedString::from("2.5")],
                vec![SharedString::from("m3"), SharedString::from("0")],
            ]
        );

        let mut hash_data: IndexMap<SharedString, SharedString> = IndexMap::new();
        hash_data.insert("Field".into(), "f1".into());
        hash_data.insert("Value".into(), "v1".into());
        hash_data.insert("TTL".into(), "60".into());
        hash_data.insert("f2".into(), "v2".into());

        let hash_batches = build_kv_add_batches(
            KeyType::Hash,
            false,
            &["Field".into(), "Value".into(), "TTL".into()],
            &hash_data,
        );
        assert_eq!(
            hash_batches,
            vec![
                vec![
                    SharedString::from("f1"),
                    SharedString::from("v1"),
                    SharedString::from("60"),
                ],
                vec![
                    SharedString::from("f2"),
                    SharedString::from("v2"),
                    SharedString::from("60"),
                ],
            ]
        );
    }
}

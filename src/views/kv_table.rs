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
use crate::helpers::get_mono_font_family;
use crate::{
    assets::CustomIconName,
    components::{INDEX_COLUMN_NAME, KvTableColumn, KvTableColumnType, KvTableMode, ZedisKvDelegate, ZedisKvFetcher},
    helpers::{EditorAction, build_csv, humanize_keystroke},
    states::{
        KeyType, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_kv_table,
        i18n_list_editor,
    },
    views::export_to_file,
};
use gpui::{App, Entity, SharedString, Subscription, TextAlign, Window, div, prelude::*, px};
use gpui_component::TITLE_BAR_HEIGHT;
use gpui_component::highlighter::Language;
use gpui_component::notification::Notification;
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

pub const FOOTER_HEIGHT: f32 = 50.0;
/// Width of the keyword search input field in pixels
const KEYWORD_INPUT_WIDTH: f32 = 200.0;

/// Parse pasted text into rows of values for bulk insertion.
///
/// - Lines are split on `\n` (also handles `\r\n` because `lines()`
///   trims `\r`). Trimmed-empty lines are skipped.
/// - For `expected_columns <= 1` the entire trimmed line is one
///   value (commas/tabs are preserved verbatim).
/// - For `expected_columns >= 2` the line is split into at most that
///   many parts. Separator is auto-detected per-line: tab takes
///   precedence over comma so users can paste Redis values that
///   themselves contain commas as long as their rows use tabs.
///   Use `splitn(N, ...)` so the trailing column absorbs any extra
///   separator runs — handy when a hash value happens to contain a
///   comma. Missing trailing columns are padded with empty strings
///   so the row always has exactly `expected_columns` slots.
/// - Per-cell trimming strips leading/trailing whitespace.
pub(crate) fn parse_bulk_rows(text: &str, expected_columns: usize) -> Vec<Vec<SharedString>> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if expected_columns <= 1 {
            rows.push(vec![SharedString::from(line.to_string())]);
            continue;
        }
        let sep: char = if line.contains('\t') { '\t' } else { ',' };
        let mut row: Vec<SharedString> = line
            .splitn(expected_columns, sep)
            .map(|part| SharedString::from(part.trim().to_string()))
            .collect();
        while row.len() < expected_columns {
            row.push(SharedString::default());
        }
        rows.push(row);
    }
    rows
}

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

                    // Check if columns changed (e.g., Stream with new fields)
                    if let Some(new_columns) = fetcher.columns() {
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

                    this.table_state.update(cx, |state, _| {
                        state.delegate_mut().set_fetcher(fetcher);
                    });
                }
                // Read-only was toggled from the status bar — recompute the
                // effective mode from the configured base mode so the edit /
                // add / remove affordances update live, instead of only after
                // the editor is recreated on a type switch.
                ServerEvent::ServerInfoUpdated => {
                    let readonly = server_state.read(cx).readonly();
                    if readonly != this.readonly {
                        this.readonly = readonly;
                        this.mode = if readonly { KvTableMode::empty() } else { this.base_mode };
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
        let fetcher = Arc::new(Self::new_values(server_state.clone(), cx));
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
            columns_dirty: false,
            edit_row: None,
            values_should_fill: false,
            original_values: IndexMap::new(),
            values_modified: false,
            value_states,
            readonly,
            mode,
            base_mode: KvTableMode::ALL,
            fetcher,
            server_state,
            columns,
            editor_form: None,
            action_button_factory: None,
            list_push_mode_state: cx.new(|_cx| 0),
            _subscriptions: subscriptions,
        }
    }

    /// Export the currently-loaded rows to a CSV file via the shared
    /// `export_to_file` flow. Headers come from the table columns and cells
    /// from the fetcher, so one implementation covers Hash/List/Set/Zset/Stream.
    /// Honest about completeness: when the table isn't fully loaded the success
    /// notification notes "loaded / total", so a partial export is never
    /// mistaken for the whole collection.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        if self.items_count == 0 {
            return;
        }
        let headers: Vec<&str> = self.columns.iter().map(|c| c.name.as_ref()).collect();
        let rows: Vec<Vec<String>> = (0..self.items_count)
            .map(|r| {
                // The delegate prepends an Index ("#") column at position 0,
                // so the fetcher's value columns (matching `self.columns`)
                // start at delegate index 1 — offset `get` accordingly.
                (0..self.columns.len())
                    .map(|c| self.fetcher.get(r, c + 1).map(|s| s.to_string()).unwrap_or_default())
                    .collect()
            })
            .collect();
        let csv = build_csv(&headers, &rows);

        // Filename from the key, sanitized so a name with `:` / `/` can't
        // break the suggested save path.
        let key = self.server_state.read(cx).key().unwrap_or_default();
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let suggested = format!("{}.csv", if safe.is_empty() { "export" } else { safe.as_str() });

        let success: SharedString = if self.done {
            i18n_common(cx, "csv_exported")
        } else {
            format!(
                "{} ({} / {})",
                i18n_common(cx, "csv_exported"),
                self.items_count,
                self.total_count
            )
            .into()
        };
        let error = i18n_common(cx, "csv_export_failed");
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            &suggested,
            success,
            error,
        );
    }

    /// Sets a factory closure that produces extra action buttons for the footer each render.
    /// Call this from the component that owns this table (e.g. ZedisStreamEditor)
    /// to inject custom buttons whose click handlers reference the owner's entity context.
    pub fn set_action_button_factory(&mut self, factory: ZedisKvTableActionButtonFactory) {
        self.action_button_factory = Some(factory);
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
        // Remember the intended mode so a later read-only toggle can restore it.
        self.base_mode = mode;
        // If readonly, the effective mode is always empty.
        self.mode = if self.readonly { KvTableMode::empty() } else { mode };
        self
    }

    fn is_adding_row(&self) -> bool {
        self.edit_row == Some(usize::MAX)
    }

    fn handle_select_row(&mut self, row_ix: usize, _cx: &mut Context<Self>) {
        // Open the detail panel on select. Normally an action mode
        // (UPDATE/REMOVE/ADD) is required; in a read-only connection the mode is
        // empty, but we still open a *view-only* preview so an entry's contents
        // (e.g. a stream entry's id + message) can be inspected — the form is
        // disabled and the update/remove actions are hidden below.
        if !self.readonly
            && !self
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
            let value = self.fetcher.get_edit(row_ix, index + 1).unwrap_or_default();
            self.original_values.insert(column.name.clone(), value);
        }
        self.editor_form = None;
        self.values_modified = false;
    }
    /// Open a dialog to paste many rows at once. The pasted text is
    /// parsed into rows (one per line, tab-separated preferred,
    /// comma fallback) and each row is dispatched through the same
    /// `handle_add_value` path as the single-row form, so backend
    /// semantics (HSET, LPUSH/RPUSH, SADD, ZADD) stay identical.
    ///
    /// Per key type the dialog adapts:
    /// - Hash: 2 cols → "field<sep>value"
    /// - ZSet: 2 cols → "member<sep>score"  (score must parse as f64
    ///   downstream — invalid scores fall back to 0.0 like the
    ///   single-row form does)
    /// - List: 1 col → "value" per line, always pushed as RPUSH for
    ///   bulk paste so the visual top-to-bottom order in the textarea
    ///   matches the resulting list order
    /// - Set:  1 col → "member" per line
    ///
    /// Stream/other key types fall through (no-op).
    fn handle_bulk_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.contains(KvTableMode::ADD) {
            return;
        }
        let key_type = self.fetcher.key_type();
        let column_count: usize = match key_type {
            KeyType::Hash | KeyType::Zset => 2,
            KeyType::List | KeyType::Set => 1,
            _ => return,
        };

        let placeholder = match key_type {
            KeyType::Hash => i18n_kv_table(cx, "bulk_add_placeholder_hash"),
            KeyType::Zset => i18n_kv_table(cx, "bulk_add_placeholder_zset"),
            KeyType::List => i18n_kv_table(cx, "bulk_add_placeholder_list"),
            KeyType::Set => i18n_kv_table(cx, "bulk_add_placeholder_set"),
            _ => SharedString::default(),
        };
        let hint = i18n_kv_table(cx, "bulk_add_hint");
        let success_template = i18n_kv_table(cx, "bulk_add_success");
        let empty_label = i18n_kv_table(cx, "bulk_add_empty");

        let textarea = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(8, 20)
                .placeholder(placeholder.clone())
        });
        let body_state = textarea.clone();
        let submit_state = textarea.clone();
        let fetcher = self.fetcher.clone();

        ZedisDialog::new(i18n_kv_table(cx, "bulk_add_title"))
            .w(px(680.))
            .ok_text(i18n_common(cx, "save"))
            .cancel_text(i18n_common(cx, "cancel"))
            .child(move || {
                v_flex()
                    .gap_2()
                    .w_full()
                    .child(Label::new(hint.clone()).text_xs())
                    .child(Input::new(&body_state).appearance(true))
            })
            .on_ok(move |_, window, cx| {
                let text = submit_state.read(cx).value().to_string();
                let rows = parse_bulk_rows(&text, column_count);
                if rows.is_empty() {
                    window.push_notification(Notification::warning(empty_label.clone()), cx);
                    return false;
                }
                let imported = rows.len();
                for mut row in rows {
                    if key_type == KeyType::List {
                        // Prepend the position selector; matches the
                        // [position, value] shape the list fetcher
                        // expects.
                        row.insert(0, SharedString::from("RPUSH"));
                    }
                    fetcher.handle_add_value(row, window, cx);
                }
                let msg = success_template.replace("%{count}", &imported.to_string());
                window.push_notification(Notification::success(SharedString::from(msg)), cx);
                true
            })
            .open(window, cx);
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
            - STATUS_BAR_HEIGHT.as_f32()
            - EDITOR_KEY_BAR_HEIGHT.as_f32()
            - FOOTER_HEIGHT;
        // The field / editor height estimates below were tuned against a ~14px
        // base font. Scale them by the live rem size so a larger font enlarges
        // the reserved per-field space too, instead of overflowing the form and
        // forcing a scrollbar. The chrome heights above are fixed-height bars,
        // so they are deliberately not scaled.
        let font_scale = window.rem_size().as_f32() / 14.0;
        let normal_field_height = 60. * font_scale;
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
        let flex_field_height = (reset_form_height / flex_field_count as f32).max(150. * font_scale);

        let mut first = true;
        // A read-only *connection* makes the edit form view-only too (disabled,
        // empty fields skipped, no add-fields), same as a fetcher that is
        // inherently read-only-on-edit (e.g. streams).
        let readonly_on_edit = !is_adding && (self.readonly || self.fetcher.readonly_on_edit());
        for column in self.columns.iter() {
            if column.column_type != KvTableColumnType::Value {
                continue;
            }
            let value = self.original_values.get(&column.name);
            if readonly_on_edit && value.map(|item| item.is_empty()).unwrap_or(true) {
                continue;
            }
            let mut field = ZedisFormField::new(column.name.clone(), column.name.clone())
                .focus()
                .font_family(get_mono_font_family());
            if self.fetcher.fields_required() && !column.optional {
                field = field.required();
            }
            if first {
                field = field.focus();
                first = false;
            }
            // Flexible fields get an explicit height derived from the form
            // height (rather than `flex_1`) so the editor area has a definite
            // size inside the form's scroll container.
            if column.flex {
                field = field.h(px(flex_field_height - 30. * font_scale));
            }
            if let Some(field_type) = column.field_type.clone() {
                if field_type == ZedisFormFieldType::Editor && !column.flex {
                    field = field.h(px(150. * font_scale));
                }
                field = field.field_type(field_type);
            }

            if !is_adding && let Some(value) = value {
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
            .when(!readonly_on_edit && self.fetcher.support_add_fields(), |this| {
                this.support_add_fields()
            });

        let form = cx.new(|cx| {
            let mut f = ZedisForm::new("kv-table-edit-form", form_opts, window, cx);
            if readonly_on_edit {
                f.set_disabled(true, cx);
            }
            f
        });
        self.editor_form = Some(form.clone());
        form.clone()
    }
}
impl<T: ZedisKvFetcher> Render for ZedisKvTable<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_color = cx.theme().muted_foreground;

        // Rebuild delegate columns when they changed (e.g., Stream with new fields)
        if std::mem::take(&mut self.columns_dirty) {
            let new_delegate_columns = Self::new_columns(self.columns.clone(), window, cx);
            // Rebuild value_states for the new columns
            self.value_states = self
                .columns
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
                .collect();
            self.table_state.update(cx, |state, cx| {
                state.delegate_mut().set_columns(new_delegate_columns);
                state.refresh(cx);
            });
        }

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
                                .bordered(false) // Table borders
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
                                    // Bulk paste only makes sense for the
                                    // four flat-table types — Stream rows
                                    // require structured fields (ID + N
                                    // entries) that don't map cleanly to
                                    // TSV/CSV, so we hide the button
                                    // there.
                                    .when(
                                        can_add
                                            && matches!(
                                                self.fetcher.key_type(),
                                                KeyType::Hash | KeyType::List | KeyType::Set | KeyType::Zset
                                            ),
                                        |this| {
                                            this.child(
                                                Button::new("bulk-add-value-btn")
                                                    .icon(IconName::Asterisk)
                                                    .tooltip(i18n_kv_table(cx, "bulk_add_tooltip"))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.handle_bulk_add(window, cx);
                                                    })),
                                            )
                                        },
                                    )
                                    .when(can_filter, |this| {
                                        this.child(
                                            Input::new(&self.keyword_state)
                                                .w(px(KEYWORD_INPUT_WIDTH))
                                                .suffix(search_btn)
                                                .cleanable(true),
                                        )
                                    })
                                    .when_some(self.action_button_factory.as_ref(), |this, factory| {
                                        this.children(factory(window, cx))
                                    })
                                    // Export loaded rows to CSV — one button on the
                                    // shared table covers every collection type.
                                    .when(self.items_count > 0, |this| {
                                        this.child(
                                            Button::new("kv-table-export-btn")
                                                .ghost()
                                                .icon(CustomIconName::Download)
                                                .tooltip(i18n_common(cx, "export_csv"))
                                                .on_click(cx.listener(|this, _, _window, cx| {
                                                    this.export_csv(cx);
                                                })),
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

#[cfg(test)]
mod tests {
    use super::parse_bulk_rows;

    #[test]
    fn parse_single_column_preserves_commas() {
        // For 1-col types (Set/List) we never split — the line is
        // the value, commas inside are part of the data.
        let rows = parse_bulk_rows("a,b,c\nplain\n", 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_ref(), "a,b,c");
        assert_eq!(rows[1][0].as_ref(), "plain");
    }

    #[test]
    fn parse_two_columns_prefers_tab_over_comma() {
        // Tab present → tab wins; commas inside the value column stay.
        let rows = parse_bulk_rows("field1\tvalue,with,commas\nfield2\tplain\n", 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_ref(), "field1");
        assert_eq!(rows[0][1].as_ref(), "value,with,commas");
        assert_eq!(rows[1][0].as_ref(), "field2");
        assert_eq!(rows[1][1].as_ref(), "plain");
    }

    #[test]
    fn parse_two_columns_falls_back_to_comma() {
        let rows = parse_bulk_rows("a,1\nb,2\n", 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_ref(), "a");
        assert_eq!(rows[0][1].as_ref(), "1");
    }

    #[test]
    fn parse_pads_missing_columns_with_empty() {
        // A single-cell line under a 2-column expectation pads the
        // second slot — the downstream `handle_add_value` for the
        // editor decides whether that empty cell is acceptable.
        let rows = parse_bulk_rows("lonely\n", 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].as_ref(), "lonely");
        assert_eq!(rows[0][1].as_ref(), "");
    }

    #[test]
    fn parse_skips_blank_lines_and_handles_crlf() {
        let rows = parse_bulk_rows("a\tb\r\n\r\nc\td\n", 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_ref(), "a");
        assert_eq!(rows[0][1].as_ref(), "b");
        assert_eq!(rows[1][0].as_ref(), "c");
        assert_eq!(rows[1][1].as_ref(), "d");
    }

    #[test]
    fn parse_trims_each_cell() {
        let rows = parse_bulk_rows("  member  ,  9.5  \n", 2);
        assert_eq!(rows[0][0].as_ref(), "member");
        assert_eq!(rows[0][1].as_ref(), "9.5");
    }

    #[test]
    fn parse_extra_separators_absorbed_into_last_column() {
        // splitn(2, ',') leaves "v,with,more,commas" in slot 1.
        let rows = parse_bulk_rows("k,v,with,more,commas\n", 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].as_ref(), "k");
        assert_eq!(rows[0][1].as_ref(), "v,with,more,commas");
    }
}

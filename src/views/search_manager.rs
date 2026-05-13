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

//! RediSearch browser view.
//!
//! Surfaces the `FT.*` command family in a single panel:
//! * Index picker built from `FT._LIST`
//! * Schema inspector built from `FT.INFO`
//! * Raw query input with schema-derived hint chips
//! * `FT.SEARCH` / `FT.AGGREGATE` toggle, plus per-mode option chips
//!   (LIMIT, RETURN, HIGHLIGHT, GROUPBY, REDUCE)
//! * Tabular result panel
//!
//! State is kept entirely inside the view — there is no separate state
//! entity. This mirrors the ACL Manager, where the panel is short-lived
//! (tied to the route) and re-fetches when the user re-enters it.

use crate::{
    assets::CustomIconName,
    connection::{
        AggregateOptions, AggregateResult, CreateFieldSpec, CreateIndexOptions, FieldSchema, IndexInfo, ReducerFn,
        ReducerSpec, SearchOptions, SearchResult, ft_aggregate, ft_alter_add, ft_create, ft_dropindex, ft_info,
        ft_list, ft_search, get_connection_manager,
    },
    error::Error,
    states::{Route, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_search},
};
use gpui::{Action, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::info;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Whether the run button issues an `FT.SEARCH` or an `FT.AGGREGATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Search,
    Aggregate,
}

/// What kind of result was last produced — drives which result widget renders.
#[derive(Debug, Clone)]
enum LastResult {
    Search(SearchResult),
    Aggregate(AggregateResult),
}

const DEFAULT_LIMIT_COUNT: u32 = 10;
const SCHEMA_PANEL_HEIGHT: f32 = 160.0;

/// The four field types we expose in the create-index form. Vector and
/// GeoShape are intentionally excluded — they need more configuration
/// (dimensions, distance metric, GEOSHAPE coordinate system) than a
/// quick-create form should ask for. Users wanting those can issue
/// `FT.CREATE` from the terminal.
const CREATE_FIELD_TYPES: &[&str] = &["TEXT", "NUMERIC", "TAG", "GEO"];

/// One row in the create-index dialog's schema list. `id` is stable
/// across re-renders (rows can be removed mid-list, so we can't key off
/// vector position).
struct CreateFieldRow {
    id: u64,
    name: Entity<InputState>,
    field_type: SharedString,
    sortable: bool,
    no_stem: bool,
    no_index: bool,
}

/// Mutable state for the in-flight create-index dialog. `None` while
/// the form panel is closed.
struct CreateIndexForm {
    name: Entity<InputState>,
    on_json: bool,
    prefixes: Entity<InputState>,
    fields: Vec<CreateFieldRow>,
    next_field_id: u64,
}

/// Which boolean flag on a field-row to flip. Localised to this view —
/// see `toggle_create_field_flag`.
#[derive(Clone, Copy)]
enum CreateFieldFlag {
    Sortable,
    NoStem,
    NoIndex,
}

/// Single-field form for `FT.ALTER … SCHEMA ADD`. Lives alongside the
/// full create form but is intentionally simpler — adding an attribute
/// is a much smaller operation than reconfiguring the whole index.
struct AddFieldForm {
    name: Entity<InputState>,
    field_type: SharedString,
    sortable: bool,
    no_stem: bool,
    no_index: bool,
}

pub struct ZedisSearchManager {
    server_state: Entity<ZedisServerState>,
    indexes: Vec<SharedString>,
    selected_index: Option<SharedString>,
    /// Schema + stats for the currently selected index. `None` while loading
    /// or when no index is selected.
    index_info: Option<IndexInfo>,
    /// `true` if `FT._LIST` came back with the "unknown command" sentinel —
    /// the server doesn't have the RediSearch module loaded.
    module_unsupported: bool,
    mode: SearchMode,

    // Input entities (each owns its own state so they stay focusable
    // independently).
    query_input: Entity<InputState>,
    limit_offset_input: Entity<InputState>,
    limit_count_input: Entity<InputState>,
    return_input: Entity<InputState>,
    highlight_open_input: Entity<InputState>,
    highlight_close_input: Entity<InputState>,
    highlight_fields_input: Entity<InputState>,
    groupby_input: Entity<InputState>,
    reducer_args_input: Entity<InputState>,
    reducer_alias_input: Entity<InputState>,

    reducer_fn: ReducerFn,
    last_result: Option<LastResult>,
    error: Option<SharedString>,
    loading_indexes: bool,
    loading_info: bool,
    running_query: bool,
    /// Live form state for the create-index dialog. Lives on the view
    /// (not in an Rc inside the closure) so dialog buttons can mutate
    /// it through `cx.entity().update(...)` and trigger redraws via
    /// the same notify path the rest of the panel uses.
    create_form: Option<CreateIndexForm>,
    creating_index: bool,
    /// Live form state for the FT.ALTER add-field flow. At most one of
    /// `create_form` / `add_field_form` is `Some` at a time.
    add_field_form: Option<AddFieldForm>,
    altering_index: bool,
    dropping_index: bool,
    _fetch_task: Option<Task<()>>,
    _info_task: Option<Task<()>>,
    _query_task: Option<Task<()>>,
    _create_task: Option<Task<()>>,
    _alter_task: Option<Task<()>>,
    _drop_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisSearchManager {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, _state, event, cx| match event {
            ServerEvent::ServerSelected(_) | ServerEvent::ServerInfoUpdated => {
                this.indexes.clear();
                this.selected_index = None;
                this.index_info = None;
                this.last_result = None;
                this.error = None;
                this.module_unsupported = false;
                this.refresh_indexes(cx);
            }
            _ => {}
        }));

        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "query_placeholder")));
        // Pressing Enter in the query bar triggers a run — keeps the UX
        // similar to `redis-cli` and the existing JSONPath bar.
        subscriptions.push(
            cx.subscribe_in(&query_input, window, |this, _state, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.run(window, cx);
                }
            }),
        );
        let limit_offset_input = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let limit_count_input = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_LIMIT_COUNT.to_string()));
        let return_input = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "return_placeholder")));
        let highlight_open_input = cx.new(|cx| InputState::new(window, cx).default_value("<mark>"));
        let highlight_close_input = cx.new(|cx| InputState::new(window, cx).default_value("</mark>"));
        let highlight_fields_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "highlight_fields_placeholder")));
        let groupby_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "groupby_placeholder")));
        let reducer_args_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "reducer_args_placeholder")));
        let reducer_alias_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "reducer_alias_placeholder")));

        let mut this = Self {
            server_state,
            indexes: Vec::new(),
            selected_index: None,
            index_info: None,
            module_unsupported: false,
            mode: SearchMode::Search,
            query_input,
            limit_offset_input,
            limit_count_input,
            return_input,
            highlight_open_input,
            highlight_close_input,
            highlight_fields_input,
            groupby_input,
            reducer_args_input,
            reducer_alias_input,
            reducer_fn: ReducerFn::Count,
            last_result: None,
            error: None,
            loading_indexes: false,
            loading_info: false,
            running_query: false,
            create_form: None,
            creating_index: false,
            add_field_form: None,
            altering_index: false,
            dropping_index: false,
            _fetch_task: None,
            _info_task: None,
            _query_task: None,
            _create_task: None,
            _alter_task: None,
            _drop_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh_indexes(cx);
        this
    }

    /// Re-fetch the list of indexes via `FT._LIST`. Triggered on view
    /// creation, when the server changes, and from the refresh button.
    fn refresh_indexes(&mut self, cx: &mut gpui::Context<Self>) {
        if self.loading_indexes {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        self.loading_indexes = true;
        self._fetch_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                ft_list(&mut conn).await
            });
            let result = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading_indexes = false;
                match result {
                    Ok(listing) => {
                        this.module_unsupported = listing.unsupported;
                        this.indexes = listing.names;
                        // Auto-select the first index if nothing's selected
                        // yet — saves the user a click for the common
                        // "open the panel, see what's in the first index"
                        // flow.
                        if this.selected_index.is_none()
                            && let Some(first) = this.indexes.first().cloned()
                        {
                            this.select_index(first, cx);
                        }
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn select_index(&mut self, name: SharedString, cx: &mut gpui::Context<Self>) {
        self.selected_index = Some(name.clone());
        self.index_info = None;
        self.last_result = None;
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.loading_info = true;
        self._info_task = Some(cx.spawn(async move |handle, cx| {
            let server_id_for_task = server_id.clone();
            let name_for_task = name.clone();
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id_for_task, db).await?;
                ft_info(&mut conn, name_for_task.as_ref()).await
            });
            let result: Result<IndexInfo> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.loading_info = false;
                match result {
                    Ok(info) => {
                        this.index_info = Some(info);
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Initialise the create-index form state and switch the panel into
    /// "create mode". The form is reset every time — there's no draft
    /// persistence since FT.CREATE is typically a one-shot configuration
    /// step. Rendered as an inline takeover of the panel body (not a
    /// modal dialog) so all event handlers can use the standard
    /// `cx.listener` reactivity path.
    fn open_create_dialog(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "create_name_placeholder")));
        let prefixes =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "create_prefixes_placeholder")));
        let first_field_name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        self.create_form = Some(CreateIndexForm {
            name,
            on_json: false,
            prefixes,
            fields: vec![CreateFieldRow {
                id: 0,
                name: first_field_name,
                field_type: SharedString::from("TEXT"),
                sortable: false,
                no_stem: false,
                no_index: false,
            }],
            next_field_id: 1,
        });
        cx.notify();
    }

    fn close_create_dialog(&mut self, cx: &mut gpui::Context<Self>) {
        self.create_form = None;
        cx.notify();
    }

    /// Open a confirm dialog for `FT.DROPINDEX`. Keeps documents
    /// (data) intact — the destructive `DD` variant is documented in
    /// the dialog body so users who really want it know to go to the
    /// terminal. This is a deliberate guardrail; one click in the
    /// schema header shouldn't be able to nuke an entire dataset.
    fn confirm_drop_index(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(index) = self.selected_index.clone() else {
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = i18n_search(cx, "drop_title");
        // Capture as owned values for the closure — the dialog's
        // on_ok runs in &mut App without our Context.
        let index_for_task = index.clone();
        let server_id_for_task = server_id.clone();
        let body_message: SharedString = format!(
            "{}\n\n{}: {}\n\n{}",
            i18n_search(cx, "drop_message"),
            i18n_search(cx, "drop_index_label"),
            index,
            i18n_search(cx, "drop_dd_hint"),
        )
        .into();
        ZedisDialog::new_alert(title, body_message)
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_search(cx, "drop_confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .on_ok(move |_, _window, cx| {
                let Some(this) = entity.upgrade() else { return true };
                // `on_ok` is `Fn` (callable multiple times), so we
                // clone captured strings each time rather than moving.
                let index_inner = index_for_task.clone();
                let server_id_inner = server_id_for_task.clone();
                this.update(cx, |this, cx| {
                    this.dropping_index = true;
                    this.error = None;
                    let log_name = index_inner.clone();
                    let cmd_index = index_inner.clone();
                    this._drop_task = Some(cx.spawn(async move |handle, cx| {
                        let task = cx.background_spawn(async move {
                            let mut conn = get_connection_manager().get_connection(&server_id_inner, db).await?;
                            ft_dropindex(&mut conn, cmd_index.as_ref(), false).await
                        });
                        let result: Result<()> = task.await;
                        let _ = handle.update(cx, |this, cx| {
                            this.dropping_index = false;
                            match result {
                                Ok(()) => {
                                    info!(index = %log_name, "FT.DROPINDEX succeeded");
                                    this.selected_index = None;
                                    this.index_info = None;
                                    this.last_result = None;
                                    this.refresh_indexes(cx);
                                }
                                Err(e) => {
                                    this.error = Some(e.to_string().into());
                                }
                            }
                            cx.notify();
                        });
                    }));
                });
                true
            })
            .open(window, cx);
    }

    fn open_add_field_form(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        self.add_field_form = Some(AddFieldForm {
            name,
            field_type: SharedString::from("TEXT"),
            sortable: false,
            no_stem: false,
            no_index: false,
        });
        cx.notify();
    }

    fn close_add_field_form(&mut self, cx: &mut gpui::Context<Self>) {
        self.add_field_form = None;
        cx.notify();
    }

    fn set_add_field_type(&mut self, ty: &str, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.add_field_form.as_mut() else {
            return;
        };
        form.field_type = SharedString::from(ty.to_string());
        if ty != "TEXT" {
            form.no_stem = false;
        }
        cx.notify();
    }

    fn toggle_add_field_flag(&mut self, flag: CreateFieldFlag, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.add_field_form.as_mut() else {
            return;
        };
        match flag {
            CreateFieldFlag::Sortable => form.sortable = !form.sortable,
            CreateFieldFlag::NoStem => form.no_stem = !form.no_stem,
            CreateFieldFlag::NoIndex => form.no_index = !form.no_index,
        }
        cx.notify();
    }

    fn submit_add_field(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.add_field_form.as_ref() else {
            return;
        };
        let Some(index) = self.selected_index.clone() else {
            return;
        };
        let name = form.name.read(cx).value().to_string().trim().to_string();
        if name.is_empty() {
            self.error = Some(i18n_search(cx, "create_name_required"));
            cx.notify();
            return;
        }
        let spec = CreateFieldSpec {
            name: SharedString::from(name),
            field_type: form.field_type.clone(),
            sortable: form.sortable,
            no_stem: form.no_stem,
            no_index: form.no_index,
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.altering_index = true;
        self.error = None;
        let index_for_task = index.clone();
        self._alter_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                ft_alter_add(&mut conn, index_for_task.as_ref(), &spec).await
            });
            let result: Result<()> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.altering_index = false;
                match result {
                    Ok(()) => {
                        info!(index = %index, "FT.ALTER ADD succeeded");
                        this.add_field_form = None;
                        // Re-pull FT.INFO so the schema panel
                        // reflects the new attribute immediately.
                        this.select_index(index.clone(), cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Append a fresh row to the form. Stable id comes from the form's
    /// `next_field_id` counter.
    fn add_create_field(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        let id = form.next_field_id;
        form.next_field_id += 1;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("field"));
        form.fields.push(CreateFieldRow {
            id,
            name,
            field_type: SharedString::from("TEXT"),
            sortable: false,
            no_stem: false,
            no_index: false,
        });
        cx.notify();
    }

    fn remove_create_field(&mut self, id: u64, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        // Don't let the form go to zero rows — `FT.CREATE` requires at
        // least one SCHEMA entry, and the empty-state would be
        // confusing to recover from.
        if form.fields.len() <= 1 {
            return;
        }
        form.fields.retain(|f| f.id != id);
        cx.notify();
    }

    fn set_create_field_type(&mut self, id: u64, ty: &str, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        for f in form.fields.iter_mut() {
            if f.id == id {
                f.field_type = SharedString::from(ty.to_string());
                // NOSTEM only applies to TEXT; clear it on type switch
                // so submitting doesn't send a wire option Redis will
                // reject.
                if ty != "TEXT" {
                    f.no_stem = false;
                }
                break;
            }
        }
        cx.notify();
    }

    fn toggle_create_field_flag(&mut self, id: u64, flag: CreateFieldFlag, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_mut() else { return };
        for f in form.fields.iter_mut() {
            if f.id == id {
                match flag {
                    CreateFieldFlag::Sortable => f.sortable = !f.sortable,
                    CreateFieldFlag::NoStem => f.no_stem = !f.no_stem,
                    CreateFieldFlag::NoIndex => f.no_index = !f.no_index,
                }
                break;
            }
        }
        cx.notify();
    }

    fn set_create_key_type(&mut self, on_json: bool, cx: &mut gpui::Context<Self>) {
        if let Some(form) = self.create_form.as_mut() {
            form.on_json = on_json;
            cx.notify();
        }
    }

    /// Validate the form, build the `CreateIndexOptions`, and dispatch
    /// the FT.CREATE task. On validation failure the error is surfaced
    /// inline and the form stays open; on success the form closes once
    /// the request completes and the new index is auto-selected.
    fn submit_create_index(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(form) = self.create_form.as_ref() else { return };
        let name = form.name.read(cx).value().to_string().trim().to_string();
        if name.is_empty() {
            self.error = Some(i18n_search(cx, "create_name_required"));
            cx.notify();
            return;
        }
        let mut fields = Vec::with_capacity(form.fields.len());
        for f in &form.fields {
            let fname = f.name.read(cx).value().to_string().trim().to_string();
            if fname.is_empty() {
                continue;
            }
            fields.push(CreateFieldSpec {
                name: SharedString::from(fname),
                field_type: f.field_type.clone(),
                sortable: f.sortable,
                no_stem: f.no_stem,
                no_index: f.no_index,
            });
        }
        if fields.is_empty() {
            self.error = Some(i18n_search(cx, "create_fields_required"));
            cx.notify();
            return;
        }

        let opts = CreateIndexOptions {
            index: SharedString::from(name.clone()),
            on_json: form.on_json,
            prefixes: split_csv(&form.prefixes.read(cx).value()),
            fields,
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }
        self.creating_index = true;
        self.error = None;
        let created_name = SharedString::from(name);
        self._create_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                ft_create(&mut conn, &opts).await
            });
            let result: Result<()> = task.await;
            let _ = handle.update(cx, |this, cx| {
                this.creating_index = false;
                match result {
                    Ok(()) => {
                        info!(index = created_name.as_ref(), "FT.CREATE succeeded");
                        // Close the form, then refresh the index list
                        // and auto-select the freshly created index so
                        // the user can verify the schema landed.
                        this.create_form = None;
                        this.refresh_indexes(cx);
                        this.select_index(created_name, cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Issue the query for the current mode. Reads input entities at
    /// call time and snapshots their values into the spawned task; no
    /// references escape the closure.
    fn run(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.running_query {
            return;
        }
        let Some(index) = self.selected_index.clone() else {
            self.error = Some(i18n_search(cx, "no_index_selected"));
            cx.notify();
            return;
        };
        let server_id = self.server_state.read(cx).server_id().to_string();
        let db = self.server_state.read(cx).db();
        if server_id.is_empty() {
            return;
        }

        let raw_query = self.query_input.read(cx).value().to_string();
        // RediSearch needs a non-empty query; the universal "match all"
        // value is `*`. Surface that as the default so an empty input
        // doesn't return an error.
        let query = if raw_query.trim().is_empty() {
            "*".to_string()
        } else {
            raw_query.trim().to_string()
        };

        let mode = self.mode;
        let offset = parse_u32(&self.limit_offset_input.read(cx).value()).unwrap_or(0);
        let count = parse_u32(&self.limit_count_input.read(cx).value()).unwrap_or(DEFAULT_LIMIT_COUNT);

        self.running_query = true;
        self.error = None;

        match mode {
            SearchMode::Search => {
                let return_fields = split_csv(&self.return_input.read(cx).value());
                let highlight_fields = split_csv(&self.highlight_fields_input.read(cx).value());
                let open = self.highlight_open_input.read(cx).value().to_string();
                let close = self.highlight_close_input.read(cx).value().to_string();
                let highlight_open = if highlight_fields.is_empty() || open.is_empty() {
                    None
                } else {
                    Some(SharedString::from(open))
                };
                let highlight_close = if highlight_fields.is_empty() || close.is_empty() {
                    None
                } else {
                    Some(SharedString::from(close))
                };
                let opts = SearchOptions {
                    limit: (offset, count),
                    return_fields,
                    highlight_fields,
                    highlight_open,
                    highlight_close,
                };
                let index_for_task = index.clone();
                self._query_task = Some(cx.spawn(async move |handle, cx| {
                    let task = cx.background_spawn(async move {
                        let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                        ft_search(&mut conn, index_for_task.as_ref(), &query, &opts).await
                    });
                    let result: Result<SearchResult> = task.await;
                    let _ = handle.update(cx, |this, cx| {
                        this.running_query = false;
                        match result {
                            Ok(r) => {
                                this.last_result = Some(LastResult::Search(r));
                                this.error = None;
                            }
                            Err(e) => {
                                this.error = Some(e.to_string().into());
                            }
                        }
                        cx.notify();
                    });
                }));
            }
            SearchMode::Aggregate => {
                let group_by = split_csv(&self.groupby_input.read(cx).value());
                let reducer_args = split_csv(&self.reducer_args_input.read(cx).value());
                let alias_str = self.reducer_alias_input.read(cx).value().to_string();
                let alias = if alias_str.trim().is_empty() {
                    None
                } else {
                    Some(SharedString::from(alias_str.trim().to_string()))
                };
                let reducer = Some(ReducerSpec {
                    func: Some(self.reducer_fn.clone()),
                    args: reducer_args,
                    alias,
                });
                let opts = AggregateOptions {
                    group_by,
                    reducer,
                    limit: Some((offset, count)),
                };
                let index_for_task = index.clone();
                self._query_task = Some(cx.spawn(async move |handle, cx| {
                    let task = cx.background_spawn(async move {
                        let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                        ft_aggregate(&mut conn, index_for_task.as_ref(), &query, &opts).await
                    });
                    let result: Result<AggregateResult> = task.await;
                    let _ = handle.update(cx, |this, cx| {
                        this.running_query = false;
                        match result {
                            Ok(r) => {
                                this.last_result = Some(LastResult::Aggregate(r));
                                this.error = None;
                            }
                            Err(e) => {
                                this.error = Some(e.to_string().into());
                            }
                        }
                        cx.notify();
                    });
                }));
            }
        }
        cx.notify();
    }
}

/// Split a "comma or whitespace separated" user string into trimmed,
/// non-empty tokens. Used for RETURN / HIGHLIGHT / GROUPBY inputs.
fn split_csv(s: &str) -> Vec<SharedString> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| SharedString::from(t.to_string()))
        .collect()
}

fn parse_u32(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

impl gpui::Render for ZedisSearchManager {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Each sub-render takes `&mut Context<Self>` and the returned
        // element has a lifetime tied to that borrow. Detach via
        // `into_any_element()` so we can compose them in one builder
        // chain without overlapping mutable borrows.
        let header = self.render_header(cx).into_any_element();

        // Create-index form takeover: when the form is open it
        // replaces the normal browser body. Cancel returns to the
        // browser.
        if self.create_form.is_some() {
            let form_panel = self.render_create_panel(cx).into_any_element();
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(form_panel),
                )
                .into_any_element();
        }
        // Add-field form: same takeover pattern, smaller form.
        if self.add_field_form.is_some() {
            let form_panel = self.render_add_field_panel(cx).into_any_element();
            return v_flex()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(form_panel),
                )
                .into_any_element();
        }

        let body: gpui::AnyElement = if self.module_unsupported {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_search(cx, "module_unsupported")).text_color(muted))
                .into_any_element()
        } else if self.loading_indexes && self.indexes.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_common(cx, "loading")).text_color(muted))
                .into_any_element()
        } else if self.indexes.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(Label::new(i18n_search(cx, "no_indexes")).text_color(muted))
                .into_any_element()
        } else {
            let schema = self.render_schema_panel(cx).into_any_element();
            let query = self.render_query_bar(window, cx).into_any_element();
            let options = self.render_options_bar(cx).into_any_element();
            let results = self.render_results(cx).into_any_element();
            v_flex()
                .size_full()
                .child(schema)
                .child(query)
                .child(options)
                .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(results))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().w_full().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }
}

impl ZedisSearchManager {
    fn render_header(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = i18n_search(cx, "title");
        let index_count_label = if self.indexes.is_empty() {
            SharedString::default()
        } else {
            SharedString::from(format!("({})", self.indexes.len()))
        };
        let current_label = self
            .selected_index
            .clone()
            .unwrap_or_else(|| i18n_search(cx, "pick_index"));
        let indexes = self.indexes.clone();

        h_flex()
            .items_center()
            .justify_between()
            .px_4()
            .h(px(40.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("search-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n_common(cx, "back_to_editor"))
                            .on_click(|_, _w, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
                                });
                            }),
                    )
                    .child(Icon::new(IconName::Search))
                    .child(Label::new(title).text_color(cx.theme().foreground))
                    .child(Label::new(index_count_label).text_color(muted).text_sm()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        // Index picker — DropdownButton's main button shows
                        // the current selection (or a placeholder), and the
                        // chevron opens the list of indexes from FT._LIST.
                        // Actions carry the index into `indexes` rather
                        // than the name itself so the derived Action enum
                        // stays Copy.
                        DropdownButton::new("search-index-picker")
                            .button(
                                Button::new("search-index-current")
                                    .outline()
                                    .small()
                                    .label(current_label),
                            )
                            .dropdown_menu(move |menu, _w, _cx| {
                                let mut menu = menu;
                                for (idx, name) in indexes.iter().enumerate() {
                                    let label = name.clone();
                                    menu = menu.menu_element(
                                        Box::new(SearchManagerAction::SelectIndex(idx as u32)),
                                        move |_w, _cx| Label::new(label.clone()),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        // + New Index — opens the structured create form.
                        // Always available (also when there are zero
                        // indexes) so the empty-state isn't a dead end.
                        Button::new("search-new-index")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip(i18n_search(cx, "create_tooltip"))
                            .disabled(self.creating_index)
                            .on_click(cx.listener(|this, _, w, cx| this.open_create_dialog(w, cx))),
                    )
                    .child(
                        Button::new("search-refresh")
                            .outline()
                            .small()
                            .icon(Icon::new(CustomIconName::RotateCw))
                            .tooltip(i18n_search(cx, "refresh_tooltip"))
                            .on_click(cx.listener(|this, _, _w, cx| this.refresh_indexes(cx))),
                    )
                    .child(
                        Button::new("search-run")
                            .small()
                            .primary()
                            .icon(IconName::Search)
                            .label(i18n_search(cx, "run"))
                            .disabled(self.running_query || self.selected_index.is_none())
                            .on_click(cx.listener(|this, _, w, cx| this.run(w, cx))),
                    ),
            )
            .on_action(cx.listener(|this, action: &SearchManagerAction, _w, cx| match action {
                SearchManagerAction::SelectIndex(idx) => {
                    if let Some(name) = this.indexes.get(*idx as usize).cloned() {
                        this.select_index(name, cx);
                    }
                }
                SearchManagerAction::SetReducer(idx) => {
                    let all = ReducerFn::all();
                    if let Some(r) = all.get(*idx as usize) {
                        this.reducer_fn = r.clone();
                        cx.notify();
                    }
                }
            }))
    }

    /// Compact schema panel: one row per indexed field with type chip
    /// + flags. Becomes a "no schema" hint when no index is selected yet.
    fn render_schema_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let body = if let Some(info) = &self.index_info {
            let key_type = info.key_type.clone();
            let prefixes = info.prefixes.clone();
            let num_docs = info.num_docs;
            let is_indexing = info.indexing;
            let failures = info.indexing_failures;
            let theme_yellow = cx.theme().yellow;
            let theme_red = cx.theme().red;
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(info.fields.len());
            for f in &info.fields {
                rows.push(self.render_schema_row(f.clone(), cx).into_any_element());
            }
            let dropping = self.dropping_index;
            let altering = self.altering_index;
            v_flex()
                .gap_1()
                .p_2()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(i18n_search(cx, "schema_label")).text_sm().text_color(muted))
                        .when(!key_type.is_empty(), |this| {
                            this.child(self.chip(key_type, cx.theme().muted, cx).into_any_element())
                        })
                        .when(!prefixes.is_empty(), |this| {
                            // Quote each prefix so the trailing colon
                            // doesn't visually merge with the next label
                            // ("prefix: user: 0 docs" → "prefix: \"user:\"").
                            let joined = prefixes
                                .iter()
                                .map(|s| format!("\"{}\"", s.as_ref()))
                                .collect::<Vec<_>>()
                                .join(", ");
                            this.child(
                                Label::new(format!("{}: {joined}", i18n_search(cx, "prefix_label")))
                                    .text_xs()
                                    .text_color(muted),
                            )
                        })
                        .child(
                            Label::new(format!("{num_docs} {}", i18n_search(cx, "docs_unit")))
                                .text_xs()
                                .text_color(muted),
                        )
                        // While RediSearch is backfilling, num_docs lags
                        // reality — surface that state so the user
                        // doesn't think the index is broken.
                        .when(is_indexing, |this| {
                            this.child(
                                self.chip(i18n_search(cx, "indexing_chip"), theme_yellow, cx)
                                    .into_any_element(),
                            )
                        })
                        // hash_indexing_failures is the direct signal
                        // for "keys matched the prefix but were the
                        // wrong storage type". Show count + tooltip.
                        .when(failures > 0, |this| {
                            this.child(
                                self.chip(
                                    SharedString::from(format!("{} {}", failures, i18n_search(cx, "failures_chip"))),
                                    theme_red,
                                    cx,
                                )
                                .into_any_element(),
                            )
                        })
                        // Schema actions pinned to the right: ALTER ADD
                        // (incremental) and DROPINDEX (destructive).
                        // Disabled while the corresponding task is
                        // running so users can't double-click their
                        // way into races.
                        .child(div().flex_1())
                        .child(
                            Button::new("search-alter-add-field")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .tooltip(i18n_search(cx, "alter_add_tooltip"))
                                .disabled(altering)
                                .on_click(cx.listener(|this, _, w, cx| this.open_add_field_form(w, cx))),
                        )
                        .child(
                            Button::new("search-drop-index")
                                .ghost()
                                .small()
                                .icon(IconName::CircleX)
                                .tooltip(i18n_search(cx, "drop_tooltip"))
                                .disabled(dropping)
                                .on_click(cx.listener(|this, _, w, cx| this.confirm_drop_index(w, cx))),
                        ),
                )
                .child(v_flex().gap_1().children(rows))
                .into_any_element()
        } else if self.loading_info {
            div()
                .p_4()
                .child(Label::new(i18n_common(cx, "loading")).text_color(muted))
                .into_any_element()
        } else {
            div()
                .p_4()
                .child(Label::new(i18n_search(cx, "pick_index")).text_color(muted))
                .into_any_element()
        };

        div()
            .h(px(SCHEMA_PANEL_HEIGHT))
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .overflow_y_scrollbar()
            .child(body)
    }

    fn render_schema_row(&self, field: FieldSchema, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let kind = field.kind();
        let kind_color = match kind {
            crate::connection::FieldKind::Text => cx.theme().blue,
            crate::connection::FieldKind::Numeric => cx.theme().green,
            crate::connection::FieldKind::Tag => cx.theme().yellow,
            crate::connection::FieldKind::Geo | crate::connection::FieldKind::GeoShape => cx.theme().cyan,
            crate::connection::FieldKind::Vector => cx.theme().magenta,
            crate::connection::FieldKind::Unknown(_) => cx.theme().muted,
        };
        let mut flag_chips: Vec<gpui::AnyElement> = Vec::new();
        if field.sortable {
            flag_chips.push(self.chip("SORTABLE".into(), cx.theme().muted, cx).into_any_element());
        }
        if field.no_stem {
            flag_chips.push(self.chip("NOSTEM".into(), cx.theme().muted, cx).into_any_element());
        }
        if field.no_index {
            flag_chips.push(self.chip("NOINDEX".into(), cx.theme().muted, cx).into_any_element());
        }
        if let Some(w) = field.weight {
            flag_chips.push(
                Label::new(format!("weight={w}"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
            );
        }
        if let Some(sep) = field.separator.clone() {
            flag_chips.push(
                Label::new(format!("sep={sep}"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
            );
        }
        h_flex()
            .gap_2()
            .items_center()
            .px_2()
            .child(self.chip(field.kind_str.clone(), kind_color, cx).into_any_element())
            .child(Label::new(field.name.clone()).text_sm())
            .children(flag_chips)
    }

    /// Generic colored "pill" used for type chips and key-type chips.
    /// Kept inline because the rest of the codebase doesn't have a
    /// reusable chip component yet.
    fn chip(&self, text: SharedString, color: gpui::Hsla, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let bg = color.opacity(0.18);
        let _ = cx;
        div()
            .px_2()
            .rounded_sm()
            .bg(bg)
            .child(Label::new(text).text_xs().text_color(color))
    }

    fn render_query_bar(&self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Build field hint chips so users see what they can query.
        let mut hint_chips: Vec<gpui::AnyElement> = Vec::new();
        if let Some(info) = &self.index_info {
            for field in info.fields.iter().take(8) {
                hint_chips.push(
                    Label::new(format!("@{}", field.name))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .into_any_element(),
                );
            }
        }
        let muted = cx.theme().muted_foreground;
        let mode_search_selected = self.mode == SearchMode::Search;
        let mode_aggregate_selected = self.mode == SearchMode::Aggregate;
        h_flex()
            .w_full()
            .gap_2()
            .px_2()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .child(
                // Mode toggle: two buttons acting as a radio. The active
                // one switches from outline → primary so the selection is
                // visually obvious without a dedicated SegmentedControl.
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("search-mode-search")
                            .small()
                            .when(mode_search_selected, |b| b.primary())
                            .when(!mode_search_selected, |b| b.outline())
                            .label(i18n_search(cx, "mode_search"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.mode = SearchMode::Search;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("search-mode-aggregate")
                            .small()
                            .when(mode_aggregate_selected, |b| b.primary())
                            .when(!mode_aggregate_selected, |b| b.outline())
                            .label(i18n_search(cx, "mode_aggregate"))
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.mode = SearchMode::Aggregate;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(Input::new(&self.query_input).small())
                    .when(!hint_chips.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Label::new(i18n_search(cx, "fields_hint")).text_xs().text_color(muted))
                                .children(hint_chips),
                        )
                    }),
            )
    }

    fn render_options_bar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mode = self.mode;
        // Shared LIMIT row used by both modes.
        let limit_row = h_flex()
            .gap_2()
            .items_center()
            .child(Label::new(i18n_search(cx, "limit_label")).text_xs().text_color(muted))
            .child(Input::new(&self.limit_offset_input).small().w(px(70.0)))
            .child(Label::new("/").text_color(muted))
            .child(Input::new(&self.limit_count_input).small().w(px(70.0)));

        let mode_specific = match mode {
            SearchMode::Search => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_search(cx, "return_label")).text_xs().text_color(muted))
                        .child(Input::new(&self.return_input).small().flex_1()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Label::new(i18n_search(cx, "highlight_label"))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(Input::new(&self.highlight_fields_input).small().flex_1())
                        .child(Label::new(i18n_search(cx, "tags_label")).text_xs().text_color(muted))
                        .child(Input::new(&self.highlight_open_input).small().w(px(72.0)))
                        .child(Input::new(&self.highlight_close_input).small().w(px(72.0))),
                )
                .into_any_element(),
            SearchMode::Aggregate => {
                let reducer_label = i18n_search(cx, "reducer_label");
                let current_reducer_label: SharedString = self.reducer_fn.as_str().to_string().into();
                let arity = self.reducer_fn.arity();
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(i18n_search(cx, "groupby_label")).text_xs().text_color(muted))
                            .child(Input::new(&self.groupby_input).small().flex_1()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(reducer_label).text_xs().text_color(muted))
                            .child(
                                DropdownButton::new("search-reducer-picker")
                                    .button(
                                        Button::new("search-reducer-current")
                                            .outline()
                                            .small()
                                            .label(current_reducer_label),
                                    )
                                    .dropdown_menu(move |menu, _w, _cx| {
                                        let mut menu = menu;
                                        for (idx, r) in ReducerFn::all().iter().enumerate() {
                                            let label: SharedString = r.as_str().to_string().into();
                                            menu = menu.menu_element(
                                                Box::new(SearchManagerAction::SetReducer(idx as u32)),
                                                move |_w, _cx| Label::new(label.clone()),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .when(arity > 0, |this| {
                                this.child(Input::new(&self.reducer_args_input).small().flex_1())
                            })
                            .child(Label::new(i18n_search(cx, "alias_label")).text_xs().text_color(muted))
                            .child(Input::new(&self.reducer_alias_input).small().w(px(120.0))),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .px_2()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(limit_row)
            .child(mode_specific)
    }

    /// Full-panel create-index form. Replaces the schema/query/results
    /// stack while open. Mutations route through view methods so all
    /// changes hit the standard `cx.notify` redraw path — no shared
    /// `Rc<RefCell<…>>` plumbing needed.
    fn render_create_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.create_form.as_ref() else {
            return div().into_any_element();
        };
        let name_input = form.name.clone();
        let prefixes_input = form.prefixes.clone();
        let on_json = form.on_json;
        let creating = self.creating_index;

        let mut field_rows: Vec<gpui::AnyElement> = Vec::with_capacity(form.fields.len());
        let row_count = form.fields.len();
        for f in &form.fields {
            field_rows.push(self.render_create_field_row(f, row_count, cx).into_any_element());
        }

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            // Index name
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_name_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&name_input).appearance(true)),
            )
            // ON HASH | ON JSON toggle
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_on_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("create-on-hash")
                                    .small()
                                    .when(!on_json, |b| b.primary())
                                    .when(on_json, |b| b.outline())
                                    .label("ON HASH")
                                    .on_click(cx.listener(|this, _, _w, cx| this.set_create_key_type(false, cx))),
                            )
                            .child(
                                Button::new("create-on-json")
                                    .small()
                                    .when(on_json, |b| b.primary())
                                    .when(!on_json, |b| b.outline())
                                    .label("ON JSON")
                                    .on_click(cx.listener(|this, _, _w, cx| this.set_create_key_type(true, cx))),
                            ),
                    ),
            )
            // Prefixes
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(i18n_search(cx, "create_prefixes_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Input::new(&prefixes_input).appearance(true)),
            )
            // Schema fields
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .justify_between()
                            .child(
                                Label::new(i18n_search(cx, "create_fields_label"))
                                    .text_xs()
                                    .text_color(muted),
                            )
                            .child(
                                Button::new("create-add-field")
                                    .small()
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label(i18n_search(cx, "create_add_field"))
                                    .on_click(cx.listener(|this, _, w, cx| this.add_create_field(w, cx))),
                            ),
                    )
                    .child(v_flex().gap_2().children(field_rows)),
            )
            // Footer buttons
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("create-cancel")
                            .small()
                            .outline()
                            .disabled(creating)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_create_dialog(cx))),
                    )
                    .child(
                        Button::new("create-submit")
                            .small()
                            .primary()
                            .disabled(creating)
                            .label(i18n_search(cx, "create"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit_create_index(cx))),
                    ),
            )
            .into_any_element()
    }

    /// Minimal one-field form used by `FT.ALTER … SCHEMA ADD`. Mirrors
    /// the layout of a single row from `render_create_field_row` but
    /// in panel form with its own submit/cancel buttons.
    fn render_add_field_panel(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.add_field_form.as_ref() else {
            return div().into_any_element();
        };
        let index = self.selected_index.clone().unwrap_or_default();
        let altering = self.altering_index;
        let name_input = form.name.clone();
        let current_type = form.field_type.clone();
        let is_text = current_type.as_ref() == "TEXT";
        let sortable = form.sortable;
        let no_stem = form.no_stem;
        let no_index = form.no_index;

        let type_chips = h_flex().gap_1().children(CREATE_FIELD_TYPES.iter().map(|t| {
            let t = *t;
            let selected = current_type.as_ref() == t;
            let prefix: &'static str = match t {
                "TEXT" => "alter-type-text",
                "NUMERIC" => "alter-type-numeric",
                "TAG" => "alter-type-tag",
                "GEO" => "alter-type-geo",
                _ => "alter-type-other",
            };
            Button::new(prefix)
                .small()
                .when(selected, |b| b.primary())
                .when(!selected, |b| b.outline())
                .label(t)
                .on_click(cx.listener(move |this, _, _w, cx| this.set_add_field_type(t, cx)))
                .into_any_element()
        }));

        let error_banner: Option<gpui::AnyElement> = self.error.as_ref().map(|e| {
            div()
                .px_3()
                .py_2()
                .bg(cx.theme().red.opacity(0.15))
                .child(Label::new(e.clone()).text_color(cx.theme().red).text_xs())
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .when_some(error_banner, |this, banner| this.child(banner))
            .child(
                Label::new(format!("{}: {}", i18n_search(cx, "alter_target_label"), index))
                    .text_sm()
                    .text_color(muted),
            )
            .child(Label::new(i18n_search(cx, "alter_hint")).text_xs().text_color(muted))
            .child(Input::new(&name_input).appearance(true))
            .child(type_chips)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("alter-sortable")
                            .small()
                            .when(sortable, |b| b.primary())
                            .when(!sortable, |b| b.outline())
                            .label("SORTABLE")
                            .on_click(
                                cx.listener(|this, _, _w, cx| {
                                    this.toggle_add_field_flag(CreateFieldFlag::Sortable, cx)
                                }),
                            ),
                    )
                    .when(is_text, |this| {
                        this.child(
                            Button::new("alter-nostem")
                                .small()
                                .when(no_stem, |b| b.primary())
                                .when(!no_stem, |b| b.outline())
                                .label("NOSTEM")
                                .on_click(cx.listener(|this, _, _w, cx| {
                                    this.toggle_add_field_flag(CreateFieldFlag::NoStem, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("alter-noindex")
                            .small()
                            .when(no_index, |b| b.primary())
                            .when(!no_index, |b| b.outline())
                            .label("NOINDEX")
                            .on_click(
                                cx.listener(|this, _, _w, cx| this.toggle_add_field_flag(CreateFieldFlag::NoIndex, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("alter-cancel")
                            .small()
                            .outline()
                            .disabled(altering)
                            .label(i18n_common(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _w, cx| this.close_add_field_form(cx))),
                    )
                    .child(
                        Button::new("alter-submit")
                            .small()
                            .primary()
                            .disabled(altering)
                            .label(i18n_search(cx, "alter_add"))
                            .on_click(cx.listener(|this, _, _w, cx| this.submit_add_field(cx))),
                    ),
            )
            .into_any_element()
    }

    /// One row of the schema field list inside the create dialog.
    fn render_create_field_row(
        &self,
        row: &CreateFieldRow,
        total_rows: usize,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let id = row.id;
        let current_type = row.field_type.clone();
        let is_text = current_type.as_ref() == "TEXT";
        let name_input = row.name.clone();
        let sortable = row.sortable;
        let no_stem = row.no_stem;
        let no_index = row.no_index;
        let muted = cx.theme().muted_foreground;
        let can_remove = total_rows > 1;

        // Type-chip row. Inline buttons feel lighter than a popup menu
        // for a 4-option choice.
        let type_chips = h_flex().gap_1().children(CREATE_FIELD_TYPES.iter().map(|t| {
            let t = *t;
            let selected = current_type.as_ref() == t;
            // ElementId requires the tuple's second element to be a
            // primitive (u32/u64/usize), so we can't compose
            // `(static_prefix, "row_id-TYPE_NAME")`. Use one static
            // prefix per type and key by row id only.
            let prefix: &'static str = match t {
                "TEXT" => "create-type-text",
                "NUMERIC" => "create-type-numeric",
                "TAG" => "create-type-tag",
                "GEO" => "create-type-geo",
                _ => "create-type-other",
            };
            Button::new((prefix, id as u32))
                .small()
                .when(selected, |b| b.primary())
                .when(!selected, |b| b.outline())
                .label(t)
                .on_click(cx.listener(move |this, _, _w, cx| this.set_create_field_type(id, t, cx)))
                .into_any_element()
        }));

        h_flex()
            .gap_2()
            .items_center()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_sm()
            .child(Input::new(&name_input).appearance(true).flex_1())
            .child(type_chips)
            .child(
                // Toggle chips for the boolean flags. NOSTEM is gated to
                // TEXT (it's a no-op on other types and Redis rejects it).
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("create-sortable", id as u32))
                            .small()
                            .when(sortable, |b| b.primary())
                            .when(!sortable, |b| b.outline())
                            .label("SORTABLE")
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.toggle_create_field_flag(id, CreateFieldFlag::Sortable, cx)
                            })),
                    )
                    .when(is_text, |this| {
                        this.child(
                            Button::new(("create-nostem", id as u32))
                                .small()
                                .when(no_stem, |b| b.primary())
                                .when(!no_stem, |b| b.outline())
                                .label("NOSTEM")
                                .on_click(cx.listener(move |this, _, _w, cx| {
                                    this.toggle_create_field_flag(id, CreateFieldFlag::NoStem, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(("create-noindex", id as u32))
                            .small()
                            .when(no_index, |b| b.primary())
                            .when(!no_index, |b| b.outline())
                            .label("NOINDEX")
                            .on_click(cx.listener(move |this, _, _w, cx| {
                                this.toggle_create_field_flag(id, CreateFieldFlag::NoIndex, cx)
                            })),
                    ),
            )
            .child(
                Button::new(("create-remove", id as u32))
                    .small()
                    .ghost()
                    .icon(IconName::CircleX)
                    .disabled(!can_remove)
                    .tooltip(i18n_search(cx, "create_remove_field"))
                    .on_click(cx.listener(move |this, _, _w, cx| this.remove_create_field(id, cx))),
            )
            .child(div().w(px(0.0)).child(Label::new("").text_color(muted))) // spacer
    }

    fn render_results(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        if let Some(err) = &self.error {
            return div()
                .p_4()
                .child(Label::new(err.clone()).text_color(cx.theme().red))
                .into_any_element();
        }
        if self.running_query && self.last_result.is_none() {
            return div()
                .p_4()
                .child(Label::new(i18n_common(cx, "loading")).text_color(muted))
                .into_any_element();
        }
        match &self.last_result {
            Some(LastResult::Search(r)) => self.render_search_result(r.clone(), cx).into_any_element(),
            Some(LastResult::Aggregate(r)) => self.render_aggregate_result(r.clone(), cx).into_any_element(),
            None => div()
                .p_4()
                .child(Label::new(i18n_search(cx, "run_hint")).text_color(muted))
                .into_any_element(),
        }
    }

    fn render_search_result(&self, r: SearchResult, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(r.hits.len());
        for hit in &r.hits {
            let id = hit.doc_id.clone();
            let mut field_lines: Vec<gpui::AnyElement> = Vec::new();
            for (k, v) in &hit.fields {
                field_lines.push(
                    h_flex()
                        .gap_2()
                        .child(Label::new(k.clone()).text_xs().text_color(muted))
                        .child(Label::new(v.clone()).text_sm().whitespace_normal())
                        .into_any_element(),
                );
            }
            rows.push(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Label::new(id).text_sm().text_color(cx.theme().foreground))
                    .child(v_flex().gap_1().children(field_lines))
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Label::new(format!("{} {}", i18n_search(cx, "total_label"), r.total))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        Label::new(format!("{} {}", i18n_search(cx, "returned_label"), r.hits.len()))
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .children(rows)
    }

    fn render_aggregate_result(&self, r: AggregateResult, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(r.rows.len());
        for row in &r.rows {
            let mut cells: Vec<gpui::AnyElement> = Vec::with_capacity(row.len());
            for (k, v) in row {
                cells.push(
                    h_flex()
                        .gap_1()
                        .child(Label::new(k.clone()).text_xs().text_color(muted))
                        .child(Label::new(v.clone()).text_sm())
                        .into_any_element(),
                );
            }
            rows.push(
                h_flex()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(cells)
                    .into_any_element(),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex().gap_2().px_3().py_1().bg(cx.theme().muted.opacity(0.4)).child(
                    Label::new(format!("{} {}", i18n_search(cx, "rows_label"), r.rows.len()))
                        .text_xs()
                        .text_color(muted),
                ),
            )
            .children(rows)
    }
}

/// View-private action enum for dropdown items. Variants carry indices
/// (not strings) so the derive macro doesn't need to handle non-Copy
/// payloads — easier and dodges schemars surface-area issues.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize, JsonSchema, Action)]
enum SearchManagerAction {
    SelectIndex(u32),
    SetReducer(u32),
}

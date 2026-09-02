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
        AggregateOptions, AggregateResult, CreateFieldSpec, CreateIndexOptions, FieldKind, FieldSchema, IndexInfo,
        ReducerFn, ReducerSpec, SearchOptions, SearchResult, ft_aggregate, ft_alter_add, ft_create, ft_dropindex,
        ft_explain, ft_info, ft_list, ft_profile, ft_search, get_connection_manager,
    },
    error::Error,
    helpers::get_mono_font_family,
    states::{
        ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, dialog_button_props,
        i18n_common, i18n_search,
    },
    views::open_key_in_editor,
};
use gpui::{Action, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    spinner::Spinner,
    v_flex,
};
use rust_i18n::t;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::info;
use zedis_core::search_params::{ParamKind, encode_param, is_vector_param, param_names};
use zedis_ui::ZedisDialog;

mod render;

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

/// Content of the inline plan panel above the results — FT.EXPLAIN's
/// execution plan or FT.PROFILE's timing tree.
struct PlanOutput {
    /// `true` for FT.PROFILE (drives the panel title).
    profile: bool,
    text: SharedString,
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

/// One `PARAMS` binding: the `$name` a query references, how its text is
/// encoded, and the input that holds the text.
struct ParamRow {
    name: String,
    kind: ParamKind,
    value: Entity<InputState>,
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
    /// SORTBY field name (empty = no sort clause).
    sort_by_input: Entity<InputState>,
    /// DIALECT version string (empty = omit / server default).
    dialect_input: Entity<InputState>,
    /// `PARAMS` rows, one per `$name` ever seen in the query bar. Rows
    /// outlive their placeholder so retyping `$BLOB` never loses a pasted
    /// vector; only the names the query currently references are shown
    /// and sent (`active_params`).
    params: Vec<ParamRow>,

    reducer_fn: ReducerFn,
    /// When SORTBY is set: `true` = DESC, `false` = ASC.
    sort_desc: bool,
    /// Collapse the schema inspector to free space for results.
    schema_collapsed: bool,
    last_result: Option<LastResult>,
    /// Inline FT.EXPLAIN / FT.PROFILE output; `None` hides the panel.
    plan: Option<PlanOutput>,
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
    _plan_task: Option<Task<()>>,
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
            cx.subscribe_in(&query_input, window, |this, _state, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.run(window, cx),
                InputEvent::Change => this.sync_params(window, cx),
                _ => {}
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
        let sort_by_input = cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "sortby_placeholder")));
        let dialect_input = cx.new(|cx| InputState::new(window, cx).default_value("2"));

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
            sort_by_input,
            dialect_input,
            params: Vec::new(),
            reducer_fn: ReducerFn::Count,
            sort_desc: false,
            schema_collapsed: false,
            last_result: None,
            plan: None,
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
            _plan_task: None,
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
                        this.indexes = listing.names.into_iter().map(Into::into).collect();
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
        self.plan = None;
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
            let result: Result<IndexInfo> = task.await.map_err(Into::into);
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
                        let result: Result<()> = task.await.map_err(Into::into);
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
            name: SharedString::from(name).to_string(),
            field_type: form.field_type.clone().to_string(),
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
            let result: Result<()> = task.await.map_err(Into::into);
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
                name: SharedString::from(fname).to_string(),
                field_type: f.field_type.clone().to_string(),
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
            index: SharedString::from(name.clone()).to_string(),
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
            let result: Result<()> = task.await.map_err(Into::into);
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
        let dialect = parse_u32(&self.dialect_input.read(cx).value());
        let params = match self.collect_params(&query, cx) {
            Ok(params) => params,
            Err(message) => {
                self.error = Some(message);
                cx.notify();
                return;
            }
        };

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
                    Some(open)
                };
                let highlight_close = if highlight_fields.is_empty() || close.is_empty() {
                    None
                } else {
                    Some(close)
                };
                let sort_raw = self.sort_by_input.read(cx).value().to_string();
                let sort_by = {
                    let t = sort_raw.trim();
                    if t.is_empty() { None } else { Some(t.to_string()) }
                };
                let opts = SearchOptions {
                    limit: (offset, count),
                    return_fields,
                    highlight_fields,
                    highlight_open,
                    highlight_close,
                    sort_by,
                    sort_desc: self.sort_desc,
                    dialect,
                    params,
                };
                let index_for_task = index.clone();
                self._query_task = Some(cx.spawn(async move |handle, cx| {
                    let task = cx.background_spawn(async move {
                        let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                        ft_search(&mut conn, index_for_task.as_ref(), &query, &opts).await
                    });
                    let result: Result<SearchResult> = task.await.map_err(Into::into);
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
                    Some(alias_str.trim().to_string())
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
                    dialect,
                    params,
                };
                let index_for_task = index.clone();
                self._query_task = Some(cx.spawn(async move |handle, cx| {
                    let task = cx.background_spawn(async move {
                        let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                        ft_aggregate(&mut conn, index_for_task.as_ref(), &query, &opts).await
                    });
                    let result: Result<AggregateResult> = task.await.map_err(Into::into);
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

    /// FT.EXPLAIN (execution plan) or FT.PROFILE (run + timing tree) for
    /// the current query — the query builder's companion diagnostics.
    /// Output lands in the inline plan panel above the results.
    fn run_plan(&mut self, profile: bool, cx: &mut gpui::Context<Self>) {
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
        let query = if raw_query.trim().is_empty() {
            "*".to_string()
        } else {
            raw_query.trim().to_string()
        };
        let dialect = parse_u32(&self.dialect_input.read(cx).value());
        let aggregate = self.mode == SearchMode::Aggregate;
        let params = match self.collect_params(&query, cx) {
            Ok(params) => params,
            Err(message) => {
                self.error = Some(message);
                cx.notify();
                return;
            }
        };

        self.running_query = true;
        self.error = None;
        self._plan_task = Some(cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                if profile {
                    ft_profile(&mut conn, index.as_ref(), aggregate, &query, &params, dialect).await
                } else {
                    ft_explain(&mut conn, index.as_ref(), &query, &params, dialect).await
                }
            });
            let result: Result<String> = task.await.map_err(Into::into);
            let _ = handle.update(cx, |this, cx| {
                this.running_query = false;
                match result {
                    Ok(text) => {
                        this.plan = Some(PlanOutput {
                            profile,
                            text: text.into(),
                        });
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn close_plan(&mut self, cx: &mut gpui::Context<Self>) {
        self.plan = None;
        cx.notify();
    }

    /// Insert a type-aware query fragment for `field` into the query bar
    /// (e.g. `@price:[  ]` for NUMERIC, `@brand:{}` for TAG).
    fn insert_field_query(&mut self, field: &FieldSchema, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let snippet = field_query_snippet(field);
        self.query_input.update(cx, |state, cx| {
            let cur = state.value().to_string();
            let next = if cur.trim().is_empty() {
                snippet
            } else {
                format!("{} {}", cur.trim_end(), snippet)
            };
            state.set_value(SharedString::from(next), window, cx);
            state.focus(window, cx);
        });
        self.sync_params(window, cx);
        cx.notify();
    }

    /// Replace the query bar with `query` and optionally run immediately.
    fn apply_example_query(&mut self, query: &str, run: bool, window: &mut Window, cx: &mut gpui::Context<Self>) {
        self.query_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(query.to_string()), window, cx);
        });
        self.sync_params(window, cx);
        if run {
            self.run(window, cx);
        } else {
            cx.notify();
        }
    }

    /// Move the LIMIT offset by `pages` pages (−1 / +1) and re-run.
    fn page_by(&mut self, pages: i32, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let offset = parse_u32(&self.limit_offset_input.read(cx).value()).unwrap_or(0);
        let count = parse_u32(&self.limit_count_input.read(cx).value())
            .unwrap_or(DEFAULT_LIMIT_COUNT)
            .max(1);
        let step = count.saturating_mul(pages.unsigned_abs());
        let mut next = if pages < 0 {
            offset.saturating_sub(step)
        } else {
            offset.saturating_add(step)
        };
        if let Some(LastResult::Search(r)) = &self.last_result
            && r.total > 0
        {
            let max_off = ((r.total - 1) / u64::from(count)) * u64::from(count);
            next = next.min(max_off.min(u64::from(u32::MAX)) as u32);
        }
        self.limit_offset_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(next.to_string()), window, cx);
        });
        self.run(window, cx);
    }

    fn open_hit_key(&mut self, doc_id: SharedString, cx: &mut gpui::Context<Self>) {
        open_key_in_editor(&self.server_state, doc_id, cx);
    }

    fn toggle_schema_collapsed(&mut self, cx: &mut gpui::Context<Self>) {
        self.schema_collapsed = !self.schema_collapsed;
        cx.notify();
    }

    fn toggle_sort_desc(&mut self, cx: &mut gpui::Context<Self>) {
        self.sort_desc = !self.sort_desc;
        cx.notify();
    }

    /// Example (label, query) pairs derived from the current schema, plus `*`.
    /// Keep one value row per `$name` the query references. New names get
    /// a row (vector slots — `KNN k @f $name`, `VECTOR_RANGE r $name` —
    /// default to FLOAT32, anything else to TEXT); names that vanished keep
    /// their row while it holds text, and are pruned once empty so typing
    /// `$BLOB` letter by letter leaves no `$B` / `$BL` litter behind.
    fn sync_params(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let query = self.query_input.read(cx).value().to_string();
        let names = param_names(&query);
        let before = self.params.len();
        self.params
            .retain(|row| names.contains(&row.name) || !row.value.read(cx).value().trim().is_empty());
        for name in names {
            if self.params.iter().any(|row| row.name == name) {
                continue;
            }
            let kind = if is_vector_param(&query, &name) {
                ParamKind::Float32
            } else {
                ParamKind::Text
            };
            let value =
                cx.new(|cx| InputState::new(window, cx).placeholder(i18n_search(cx, "params_value_placeholder")));
            self.params.push(ParamRow { name, kind, value });
        }
        if self.params.len() != before {
            cx.notify();
        }
    }

    /// The rows the query currently references, in placeholder order.
    fn active_params(&self, query: &str) -> Vec<&ParamRow> {
        param_names(query)
            .into_iter()
            .filter_map(|name| self.params.iter().find(|row| row.name == name))
            .collect()
    }

    /// Encode every active `$name` for the wire; the first missing or
    /// malformed value aborts with a message naming the parameter, so the
    /// server never sees a half-bound query.
    fn collect_params(&self, query: &str, cx: &gpui::App) -> Result<Vec<(String, Vec<u8>)>, SharedString> {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let mut out = Vec::new();
        for row in self.active_params(query) {
            let text = row.value.read(cx).value().to_string();
            if text.trim().is_empty() {
                return Err(t!("search.params_missing", name = row.name.as_str(), locale = locale)
                    .to_string()
                    .into());
            }
            let bytes = encode_param(row.kind, &text).map_err(|reason| {
                SharedString::from(
                    t!(
                        "search.params_invalid",
                        name = row.name.as_str(),
                        reason = reason,
                        locale = locale
                    )
                    .to_string(),
                )
            })?;
            out.push((row.name.clone(), bytes));
        }
        Ok(out)
    }

    fn set_param_kind(&mut self, name: &str, kind: ParamKind, cx: &mut gpui::Context<Self>) {
        if let Some(row) = self.params.iter_mut().find(|row| row.name == name) {
            row.kind = kind;
            cx.notify();
        }
    }

    fn example_queries(&self) -> Vec<(SharedString, String)> {
        let mut out = vec![("*".into(), "*".to_string())];
        let Some(info) = &self.index_info else {
            return out;
        };
        for field in info.fields.iter().take(4) {
            let q = field_query_example(field);
            let label: SharedString = format!("@{}", field.name).into();
            out.push((label, q));
        }
        out
    }
}

/// Type-aware fragment for chip-click insert (cursor-friendly placeholders).
fn field_query_snippet(field: &FieldSchema) -> String {
    match field.kind() {
        FieldKind::Numeric => format!("@{}:[0 100]", field.name),
        FieldKind::Tag => format!("@{}:{{tag}}", field.name),
        FieldKind::Text => format!("@{}:term", field.name),
        FieldKind::Geo => format!("@{}:[0 0 1 km]", field.name),
        FieldKind::Vector => format!("*=>[KNN 10 @{} $BLOB]", field.name),
        FieldKind::GeoShape | FieldKind::Unknown(_) => format!("@{}", field.name),
    }
}

/// Slightly cleaner example string for empty-state buttons.
fn field_query_example(field: &FieldSchema) -> String {
    match field.kind() {
        FieldKind::Numeric => format!("@{}:[0 1000]", field.name),
        FieldKind::Tag => format!("@{}:{{*}}", field.name),
        FieldKind::Text => format!("@{}:*", field.name),
        FieldKind::Geo => format!("@{}:[0 0 10 km]", field.name),
        FieldKind::Vector => format!("*=>[KNN 10 @{} $BLOB]", field.name),
        FieldKind::GeoShape | FieldKind::Unknown(_) => format!("@{}", field.name),
    }
}

/// Split a "comma or whitespace separated" user string into trimmed,
/// non-empty tokens. Used for RETURN / HIGHLIGHT / GROUPBY inputs.
fn split_csv(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
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
            let plan = self.render_plan_panel(cx).map(|p| p.into_any_element());
            let results = self.render_results(cx).into_any_element();
            v_flex()
                .size_full()
                .child(schema)
                .child(query)
                .child(options)
                .children(plan)
                .child(div().flex_1().w_full().min_h_0().overflow_y_scrollbar().child(results))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .child(header)
            .child(div().flex_1().w_full().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }
}

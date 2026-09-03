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

//! Redis 8 Vector Set (`V*`) viewer with interactive KNN search.
//!
//! Rendered by [`crate::views::ZedisEditor`] when the selected key
//! reports TYPE `vectorset`. Like the other module-type viewers it
//! self-gates by the key's TYPE and owns its own fetch.
//!
//! The initial load shows `VINFO` / `VCARD` / `VDIM` metadata plus a
//! `VRANDMEMBER` sample of element names. The KNN panel runs
//! `VSIM key ELE <element> WITHSCORES` and renders the ranked nearest
//! neighbours with their similarity scores; clicking any sampled
//! element (or a neighbour) re-runs the search on it, so the HNSW graph
//! can be explored hop by hop. A `FILTER` expression narrows every
//! search to elements whose attributes match (with `FILTER-EF` as the
//! candidate budget), and on 8.2+ `WITHATTRIBS` brings each neighbour's
//! attributes back inline so a filtered result shows why it matched. The
//! queried element also shows its `VGETATTR` attributes, its dequantized
//! `VEMB` components (copyable), and offers `VSETATTR` (edit) and `VREM`
//! (remove) — `VADD` stays out: pasting a whole float vector by hand is
//! the terminal's job.

use crate::helpers::get_mono_font_family;
use crate::{
    assets::CustomIconName,
    connection::{Capability, RedisAsyncConn, floors, get_connection_manager},
    error::Error,
    states::{ZedisServerState, dialog_button_props, i18n_common, i18n_vector_set},
};
use gpui::{App, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    label::Label,
    notification::Notification,
    v_flex,
};
use redis::{Cmd, Value, cmd};
use tracing::info;
use zedis_ui::ZedisDialog;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Starting sizes for the `VSIM` neighbour list and the `VRANDMEMBER`
/// sample. Both grow by doubling via their "load more" buttons, up to
/// [`GROW_MAX`] — neither command pages, so "more" means re-asking with
/// a larger COUNT.
const DEFAULT_KNN_COUNT: i64 = 10;
const DEFAULT_SAMPLE_CAP: i64 = 50;
const GROW_MAX: i64 = 1_000;

/// How many vector components are shown inline before an ellipsis.
const VECTOR_INLINE_COMPONENTS: usize = 16;

/// One `VSIM` hit: the element, its similarity and — with `WITHATTRIBS`
/// (Redis 8.2+) — its attribute JSON, so a filtered result shows why it
/// matched.
#[derive(Clone, Debug, PartialEq)]
struct Neighbour {
    element: SharedString,
    score: f64,
    attrs: Option<SharedString>,
}

/// Everything a `VSIM` run needs beyond the element: the COUNT, the
/// optional `FILTER` expression with its `FILTER-EF` candidate budget, and
/// whether the server understands `WITHATTRIBS`.
#[derive(Clone, Default)]
struct SimOptions {
    count: i64,
    filter: Option<String>,
    filter_ef: Option<i64>,
    with_attribs: bool,
}

/// What one KNN round hands back: the ranked neighbours plus the queried
/// element's own attributes (`VGETATTR`) and dequantized vector (`VEMB`).
struct SimResult {
    neighbours: Vec<Neighbour>,
    attrs: Option<String>,
    vector: Option<Vec<f64>>,
}

#[derive(Clone, Default)]
struct VectorSetData {
    info: Vec<(String, String)>,
    card: i64,
    dim: i64,
    sample: Vec<SharedString>,
    /// Element the `neighbours` were computed for, and the ranked results
    /// — seeded from the first sample element on load, then driven by the
    /// search box / clicks.
    queried: Option<SharedString>,
    neighbours: Vec<Neighbour>,
    /// `VGETATTR` of the queried element — `None` when it carries no
    /// attributes. Shown under the neighbours header and edited via
    /// `VSETATTR`.
    queried_attrs: Option<SharedString>,
    /// `VEMB` of the queried element — the stored vector as the server
    /// dequantizes it (int8 by default, so an approximation of what was
    /// added).
    queried_vector: Option<Vec<f64>>,
}

pub struct ZedisVectorSetEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    query_input: Entity<InputState>,
    /// `FILTER` expression over element attributes; empty sends none.
    filter_input: Entity<InputState>,
    /// `FILTER-EF` candidate budget; empty leaves the server default.
    filter_ef_input: Entity<InputState>,
    data: Option<VectorSetData>,
    error: Option<SharedString>,
    search_error: Option<SharedString>,
    loading: bool,
    searching: bool,
    /// Current `VSIM COUNT` — doubled by the neighbour list's "load more".
    knn_count: i64,
    /// Current `VRANDMEMBER` count — doubled by the sample's "load more".
    sample_cap: i64,
    load_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    pending_notification: Option<Notification>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisVectorSetEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new vector set editor view");
        let query_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_vector_set(cx, "search_placeholder"))
        });

        let filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_vector_set(cx, "filter_placeholder"))
        });
        let filter_ef_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_vector_set(cx, "filter_ef_placeholder")));

        let mut subscriptions = Vec::new();
        // Enter in the search box runs KNN for the typed element.
        subscriptions.push(
            cx.subscribe_in(&query_input, window, |this, state, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let element = state.read(cx).value().to_string();
                    this.run_search(element.into(), cx);
                }
            }),
        );
        // Enter in the filter boxes re-runs the current element with them.
        for input in [&filter_input, &filter_ef_input] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _state, event, _window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.rerun_current(cx);
                }
            }));
        }

        let mut this = Self {
            server_state,
            key,
            query_input,
            filter_input,
            filter_ef_input,
            data: None,
            error: None,
            search_error: None,
            loading: false,
            searching: false,
            knn_count: DEFAULT_KNN_COUNT,
            sample_cap: DEFAULT_SAMPLE_CAP,
            load_task: None,
            search_task: None,
            pending_notification: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    /// The current search options, read straight from the inputs.
    fn sim_options(&self, cx: &App) -> SimOptions {
        let filter = self.filter_input.read(cx).value().trim().to_string();
        let filter_ef = self
            .filter_ef_input
            .read(cx)
            .value()
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0);
        SimOptions {
            count: self.knn_count,
            filter: (!filter.is_empty()).then_some(filter),
            filter_ef,
            with_attribs: self.server_state.read(cx).supports(floors::VSIM_WITHATTRIBS),
        }
    }

    /// Re-run KNN for the element already queried — after a filter edit.
    fn rerun_current(&mut self, cx: &mut Context<Self>) {
        if let Some(queried) = self.data.as_ref().and_then(|d| d.queried.clone()) {
            self.run_search(queried, cx);
        }
    }

    /// Put the queried element's full vector on the clipboard, comma
    /// separated — the shape the search panel's PARAMS editor and `VADD
    /// VALUES` both take.
    fn copy_vector(&mut self, cx: &mut Context<Self>) {
        let Some(vector) = self.data.as_ref().and_then(|d| d.queried_vector.clone()) else {
            return;
        };
        let text = vector.iter().map(f64::to_string).collect::<Vec<_>>().join(", ");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.pending_notification = Some(Notification::info(i18n_vector_set(cx, "vector_copied")));
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        if key.is_empty() {
            return;
        }
        let sample_cap = self.sample_cap;
        let sim = self.sim_options(cx);
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_vector_set(server_id, db, key, sample_cap, sim).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(data) => {
                        this.data = Some(data);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Run `VSIM` for `element` and replace the neighbour list.
    fn run_search(&mut self, element: SharedString, cx: &mut Context<Self>) {
        if element.trim().is_empty() {
            return;
        }
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.searching = true;
        self.search_error = None;
        if let Some(data) = self.data.as_mut() {
            data.queried = Some(element.clone());
        }
        cx.notify();
        let element_str = element.to_string();
        let sim = self.sim_options(cx);
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_neighbours(server_id, db, key, element_str, sim).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(found) => {
                        if let Some(data) = this.data.as_mut() {
                            data.neighbours = found.neighbours;
                            data.queried_attrs = found.attrs.map(SharedString::from);
                            data.queried_vector = found.vector;
                        }
                        this.search_error = None;
                    }
                    Err(e) => this.search_error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// `VREM` the currently queried element, then reload — the sample,
    /// cardinality and neighbours all change. Element-level removal is
    /// direct (no confirm), matching hash-field / stream-entry deletes.
    fn remove_queried(&mut self, cx: &mut Context<Self>) {
        let Some(element) = self.data.as_ref().and_then(|d| d.queried.clone()) else {
            return;
        };
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.searching = true;
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result: Result<()> = async {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: i64 = cmd("VREM")
                    .arg(&key)
                    .arg(element.as_ref())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(()) => this.load(cx),
                    Err(e) => this.search_error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Edit the queried element's attributes: a JSON textarea seeded with
    /// the current `VGETATTR` value. Empty clears them (`VSETATTR ""`).
    /// The server accepts any string, but a non-JSON attribute silently
    /// breaks `FILTER` queries later — so invalid JSON keeps the dialog
    /// open instead of being written.
    fn open_attrs_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(element) = self.data.as_ref().and_then(|d| d.queried.clone()) else {
            return;
        };
        let current = self
            .data
            .as_ref()
            .and_then(|d| d.queried_attrs.clone())
            .unwrap_or_default();
        let attrs_state = cx.new(|cx| TextareaState::new(window, cx).default_value(current).rows(6));
        let hint = i18n_vector_set(cx, "attrs_hint");
        let body_attrs = attrs_state.clone();
        let submit_attrs = attrs_state.clone();
        let view = cx.entity().downgrade();

        ZedisDialog::new(i18n_vector_set(cx, "attrs_title"))
            .w(px(480.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || {
                v_flex()
                    .gap_2()
                    .w_full()
                    .child(Textarea::new(&body_attrs))
                    .child(Label::new(hint.clone()).text_xs())
            })
            .on_ok(move |_, _window, cx| {
                let raw = submit_attrs.read(cx).value().trim().to_string();
                if !raw.is_empty() && serde_json::from_str::<serde_json::Value>(&raw).is_err() {
                    // Invalid JSON — keep the dialog open (the hint says why).
                    return false;
                }
                let element = element.clone();
                let _ = view.update(cx, |view, cx| view.apply_attrs(element, raw, cx));
                true
            })
            .open(window, cx);
    }

    /// `VSETATTR` then re-run the search so the attrs line refreshes.
    fn apply_attrs(&mut self, element: SharedString, json: String, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        self.searching = true;
        cx.notify();
        let element_for_refresh = element.clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result: Result<()> = async {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: i64 = cmd("VSETATTR")
                    .arg(&key)
                    .arg(element.as_ref())
                    .arg(&json)
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(()) => this.run_search(element_for_refresh, cx),
                    Err(e) => this.search_error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    /// Set the search box to `element` and run KNN — used when a sample
    /// element or a neighbour row is clicked.
    fn search_element(&mut self, element: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input
            .update(cx, |state, cx| state.set_value(element.clone(), window, cx));
        self.run_search(element, cx);
    }

    fn render_meta(&self, data: &VectorSetData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let chip = |label: SharedString, value: String| {
            h_flex()
                .gap_1()
                .items_baseline()
                .child(Label::new(label).text_xs().text_color(muted))
                .child(Label::new(value).text_xs().font_semibold())
        };
        let mut row = h_flex().w_full().flex_wrap().gap_x_4().gap_y_1().items_center();
        row = row.child(chip(i18n_vector_set(cx, "dimensions"), data.dim.to_string()));
        row = row.child(chip(i18n_vector_set(cx, "elements"), data.card.to_string()));
        for (k, v) in data.info.iter() {
            row = row.child(chip(SharedString::from(k.clone()), v.clone()));
        }
        row
    }

    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let element_row = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            // `.small()` matches the button beside it — the sizing pair
            // every other search bar uses (value-search, the probe row).
            .child(
                div()
                    .flex_1()
                    .child(Input::new(&self.query_input).appearance(true).small()),
            )
            .child(
                Button::new("vector-set-search")
                    .label(i18n_vector_set(cx, "search"))
                    .small()
                    .primary()
                    // `.loading()` renders its spinner in place of the icon, so a
                    // label-only button would just grey out (see CLAUDE.md).
                    .icon(Icon::new(IconName::Search))
                    .loading(self.searching)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        let element = this.query_input.read(cx).value().to_string();
                        this.run_search(element.into(), cx);
                    })),
            );
        // FILTER narrows every VSIM to elements whose attributes match;
        // FILTER-EF is the candidate budget that decides how hard the
        // server looks for matches.
        let filter_row = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .child(Input::new(&self.filter_input).appearance(true).small()),
            )
            .child(
                div()
                    .w(px(104.))
                    .child(Input::new(&self.filter_ef_input).appearance(true).small()),
            );
        v_flex().w_full().gap_1().child(element_row).child(filter_row).child(
            Label::new(i18n_vector_set(cx, "filter_hint"))
                .text_xs()
                .text_color(muted)
                .whitespace_normal(),
        )
    }

    fn render_neighbours(&self, data: &VectorSetData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut col = v_flex().w_full().gap_1();
        let mut header = h_flex()
            .gap_2()
            .items_baseline()
            .child(Label::new(i18n_vector_set(cx, "neighbours")).text_sm().font_semibold());
        if let Some(queried) = data.queried.as_ref() {
            header = header.child(Label::new(queried.clone()).text_xs().text_color(muted));
        }
        header = header.child(div().flex_1());
        // Element actions for the queried element: edit its attributes,
        // remove it. Hidden in read-only mode alongside every other write.
        if data.queried.is_some() && self.server_state.read(cx).can(Capability::MutateContainer) {
            header = header
                .child(
                    Button::new("vector-set-edit-attrs")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(CustomIconName::FilePenLine))
                        .tooltip(i18n_vector_set(cx, "attrs_edit_tooltip"))
                        .disabled(self.searching)
                        .on_click(cx.listener(|this, _, window, cx| this.open_attrs_dialog(window, cx))),
                )
                .child(
                    Button::new("vector-set-remove")
                        .xsmall()
                        .danger()
                        .icon(IconName::Close)
                        .tooltip(i18n_vector_set(cx, "remove_tooltip"))
                        .disabled(self.searching)
                        .on_click(cx.listener(|this, _, _window, cx| this.remove_queried(cx))),
                );
        }
        col = col.child(header);
        // The queried element's attributes (VGETATTR) — the metadata that
        // FILTER expressions match against.
        if data.queried.is_some() {
            let attrs = data
                .queried_attrs
                .clone()
                .unwrap_or_else(|| i18n_vector_set(cx, "attrs_none"));
            col = col.child(
                h_flex()
                    .gap_1()
                    .items_baseline()
                    .child(
                        Label::new(i18n_vector_set(cx, "attrs_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(Label::new(attrs).text_xs().text_color(muted).whitespace_normal()),
            );
        }
        // The queried element's own vector (VEMB) — a glance at the
        // components, the full list one click away.
        if let Some(vector) = data.queried_vector.as_ref() {
            let mut text = vector
                .iter()
                .take(VECTOR_INLINE_COMPONENTS)
                .map(|v| format!("{v:.4}"))
                .collect::<Vec<_>>()
                .join(", ");
            if vector.len() > VECTOR_INLINE_COMPONENTS {
                text.push_str(&format!(", … (+{})", vector.len() - VECTOR_INLINE_COMPONENTS));
            }
            col = col.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Label::new(i18n_vector_set(cx, "vector_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        div().flex_1().min_w_0().overflow_hidden().child(
                            Label::new(text)
                                .text_xs()
                                .text_color(muted)
                                .whitespace_nowrap()
                                .text_ellipsis(),
                        ),
                    )
                    .child(
                        Button::new("vector-set-copy-vector")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Copy))
                            .tooltip(i18n_vector_set(cx, "vector_copy_tooltip"))
                            .on_click(cx.listener(|this, _, _window, cx| this.copy_vector(cx))),
                    ),
            );
        }

        if let Some(err) = self.search_error.clone() {
            return col.child(Label::new(err).text_xs().text_color(cx.theme().danger));
        }
        for (rank, neighbour) in data.neighbours.iter().enumerate() {
            let el = neighbour.element.clone();
            let (element, score) = (&neighbour.element, neighbour.score);
            col = col.child(
                h_flex()
                    .id(("vector-set-neighbour", rank))
                    .w_full()
                    .gap_3()
                    .items_baseline()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_active))
                    .rounded_md()
                    .px_1()
                    .child(
                        div()
                            .min_w(px(28.))
                            .child(Label::new(format!("{}.", rank + 1)).text_xs().text_color(muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(element.clone()).text_xs().font_semibold()),
                    )
                    .child(Label::new(format!("{score:.4}")).text_xs().text_color(muted))
                    // WITHATTRIBS (8.2+): the attributes the FILTER matched on.
                    .when_some(neighbour.attrs.clone(), |this, attrs| {
                        this.child(
                            div().max_w(px(360.)).overflow_hidden().child(
                                Label::new(attrs)
                                    .text_xs()
                                    .text_color(muted)
                                    .whitespace_nowrap()
                                    .text_ellipsis(),
                            ),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.search_element(el.clone(), window, cx))),
            );
        }
        // A short page under a FILTER is not an error: the candidate
        // budget ran out before COUNT matches were found.
        let filter_active = !self.filter_input.read(cx).value().trim().is_empty();
        if filter_active && data.queried.is_some() && (data.neighbours.len() as i64) < self.knn_count {
            col = col.child(
                Label::new(i18n_vector_set(cx, "filter_short_hint"))
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal(),
            );
        }
        // A full page suggests more neighbours exist — VSIM doesn't page,
        // so "more" re-runs the query with a doubled COUNT.
        let maybe_more =
            data.queried.is_some() && data.neighbours.len() as i64 >= self.knn_count && self.knn_count < GROW_MAX;
        if maybe_more {
            col = col.child(
                Button::new("vector-set-more-neighbours")
                    .xsmall()
                    .ghost()
                    .label(i18n_vector_set(cx, "load_more"))
                    .disabled(self.searching)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.knn_count = (this.knn_count * 2).min(GROW_MAX);
                        if let Some(queried) = this.data.as_ref().and_then(|d| d.queried.clone()) {
                            this.run_search(queried, cx);
                        }
                    })),
            );
        }
        col
    }

    fn render_sample(&self, data: &VectorSetData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut wrap = h_flex().w_full().flex_wrap().gap_2().child(
            Label::new(i18n_vector_set(cx, "sample"))
                .text_sm()
                .font_semibold()
                .w_full(),
        );
        for (ix, element) in data.sample.iter().enumerate() {
            let el = element.clone();
            wrap = wrap.child(
                div()
                    .id(("vector-set-sample", ix))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_active))
                    .child(Label::new(element.clone()).text_xs().text_color(muted))
                    .on_click(cx.listener(move |this, _, window, cx| this.search_element(el.clone(), window, cx))),
            );
        }
        // More elements exist than the random sample shows — re-sample
        // with a doubled count (VRANDMEMBER has no paging either).
        if (data.sample.len() as i64) < data.card && self.sample_cap < GROW_MAX {
            wrap = wrap.child(
                Button::new("vector-set-more-sample")
                    .xsmall()
                    .ghost()
                    .label(i18n_vector_set(cx, "load_more"))
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.sample_cap = (this.sample_cap * 2).min(GROW_MAX);
                        this.load(cx);
                    })),
            );
        }
        wrap
    }
}

impl Render for ZedisVectorSetEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let muted = cx.theme().muted_foreground;
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Label::new(i18n_vector_set(cx, "title")).font_semibold())
            .child(
                div()
                    .px_1p5()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .child(Label::new("VSET").text_xs().text_color(muted)),
            );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if let Some(data) = self.data.as_ref() {
            let mut col = v_flex()
                .w_full()
                .gap_4()
                .child(self.render_meta(data, cx))
                .child(self.render_search_bar(cx))
                .child(self.render_neighbours(data, cx));
            if !data.sample.is_empty() {
                col = col.child(self.render_sample(data, cx));
            }
            col.into_any_element()
        } else {
            div()
                .p_4()
                .child(Label::new(i18n_vector_set(cx, "loading")).text_sm().text_color(muted))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .gap_3()
            .p_3()
            .child(header)
            .child(body)
    }
}

/// Initial load: `VINFO` + `VCARD` + `VDIM` + a `VRANDMEMBER` sample,
/// seeding the neighbour panel with the first sample element.
async fn fetch_vector_set(
    server_id: String,
    db: usize,
    key: String,
    sample_cap: i64,
    sim: SimOptions,
) -> Result<VectorSetData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let info_raw: Value = cmd("VINFO").arg(&key).query_async(&mut conn).await?;
    let info = info_pairs_display(&info_raw);
    let card: i64 = cmd("VCARD").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let dim: i64 = cmd("VDIM").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let sample: Vec<String> = cmd("VRANDMEMBER")
        .arg(&key)
        .arg(sample_cap)
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    let sample: Vec<SharedString> = sample.into_iter().map(SharedString::from).collect();

    let mut data = VectorSetData {
        info,
        card,
        dim,
        sample,
        ..Default::default()
    };
    if let Some(first) = data.sample.first().cloned()
        && let Ok(found) = run_vsim(&mut conn, &key, first.as_ref(), &sim).await
    {
        data.queried_attrs = found.attrs.map(Into::into);
        data.queried_vector = found.vector;
        data.queried = Some(first);
        data.neighbours = found.neighbours;
    }
    Ok(data)
}

/// `VGETATTR key element` — `None` for no attributes (nil reply) or any
/// error (attrs are decoration; a failed read must not fail the search).
async fn fetch_attrs(conn: &mut RedisAsyncConn, key: &str, element: &str) -> Option<String> {
    cmd("VGETATTR")
        .arg(key)
        .arg(element)
        .query_async::<Option<String>>(conn)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// `VEMB key element` — the stored vector as the server dequantizes it.
/// Decoration like the attributes: any failure just hides the row.
async fn fetch_vector(conn: &mut RedisAsyncConn, key: &str, element: &str) -> Option<Vec<f64>> {
    let raw: Value = cmd("VEMB").arg(key).arg(element).query_async(conn).await.ok()?;
    let Value::Array(items) = raw else {
        return None;
    };
    let components: Vec<f64> = items.iter().filter_map(value_to_f64).collect();
    (!components.is_empty()).then_some(components)
}

/// `VSIM key ELE elem WITHSCORES [WITHATTRIBS] COUNT n [FILTER expr
/// [FILTER-EF n]]` — FILTER-EF only means something next to a FILTER.
fn vsim_cmd(key: &str, element: &str, opts: &SimOptions) -> Cmd {
    let mut c = cmd("VSIM");
    c.arg(key).arg("ELE").arg(element).arg("WITHSCORES");
    if opts.with_attribs {
        c.arg("WITHATTRIBS");
    }
    c.arg("COUNT").arg(opts.count);
    if let Some(filter) = &opts.filter {
        c.arg("FILTER").arg(filter.as_str());
        if let Some(ef) = opts.filter_ef {
            c.arg("FILTER-EF").arg(ef);
        }
    }
    c
}

/// One KNN round on an open connection.
async fn run_vsim(conn: &mut RedisAsyncConn, key: &str, element: &str, opts: &SimOptions) -> Result<SimResult> {
    let raw: Value = vsim_cmd(key, element, opts).query_async(conn).await?;
    let neighbours = parse_neighbours(&raw, opts.with_attribs);
    let attrs = fetch_attrs(conn, key, element).await;
    let vector = fetch_vector(conn, key, element).await;
    Ok(SimResult {
        neighbours,
        attrs,
        vector,
    })
}

async fn fetch_neighbours(
    server_id: String,
    db: usize,
    key: String,
    element: String,
    sim: SimOptions,
) -> Result<SimResult> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    run_vsim(&mut conn, &key, &element, &sim).await
}

/// Parse a `WITHSCORES [WITHATTRIBS]` reply. RESP3 is a map from element
/// to either the score or `[score, attrs]`; RESP2 is a flat array of
/// pairs, or of triples with `WITHATTRIBS` (nil for an element without
/// attributes).
fn parse_neighbours(value: &Value, with_attribs: bool) -> Vec<Neighbour> {
    let attrs_of = |v: Option<&Value>| {
        v.and_then(value_to_string)
            .filter(|s| !s.is_empty())
            .map(SharedString::from)
    };
    match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| {
                let element = SharedString::from(value_to_string(k)?);
                let (score, attrs) = match v {
                    Value::Array(items) => (value_to_f64(items.first()?)?, attrs_of(items.get(1))),
                    scalar => (value_to_f64(scalar)?, None),
                };
                Some(Neighbour { element, score, attrs })
            })
            .collect(),
        Value::Array(items) => {
            let stride = if with_attribs { 3 } else { 2 };
            items
                .chunks(stride)
                .filter_map(|chunk| {
                    let element = SharedString::from(value_to_string(chunk.first()?)?);
                    let score = value_to_f64(chunk.get(1)?)?;
                    Some(Neighbour {
                        element,
                        score,
                        attrs: attrs_of(chunk.get(2)),
                    })
                })
                .collect()
        }
        _ => vec![],
    }
}

/// Flatten a `VINFO` reply (RESP3 map or RESP2 flat array) into
/// display-ready `(field, value)` pairs.
fn info_pairs_display(value: &Value) -> Vec<(String, String)> {
    let pairs: Vec<(String, &Value)> = match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| value_to_string(k).map(|s| (s, v)))
            .collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|chunk| {
                let key = chunk.first()?;
                let val = chunk.get(1)?;
                value_to_string(key).map(|s| (s, val))
            })
            .collect(),
        _ => vec![],
    };
    pairs.into_iter().map(|(k, v)| (k, value_to_display(v))).collect()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Double(d) => Some(*d),
        Value::Int(i) => Some(*i as f64),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).trim().parse().ok(),
        Value::SimpleString(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn value_to_display(value: &Value) -> String {
    match value {
        Value::Int(i) => i.to_string(),
        Value::Double(d) => format!("{d}"),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::SimpleString(s) => s.clone(),
        Value::Boolean(b) => b.to_string(),
        Value::Nil => "—".to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Arg;

    fn words(c: &Cmd) -> Vec<String> {
        c.args_iter()
            .map(|a| match a {
                Arg::Simple(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => String::new(),
            })
            .collect()
    }

    fn bs(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn vsim_cmd_spells_filter_and_attribs() {
        let opts = SimOptions {
            count: 5,
            filter: Some(".year > 2000".to_string()),
            filter_ef: Some(500),
            with_attribs: true,
        };
        assert_eq!(
            words(&vsim_cmd("k", "e", &opts)),
            [
                "VSIM",
                "k",
                "ELE",
                "e",
                "WITHSCORES",
                "WITHATTRIBS",
                "COUNT",
                "5",
                "FILTER",
                ".year > 2000",
                "FILTER-EF",
                "500"
            ]
        );
        let plain = SimOptions {
            count: 10,
            ..Default::default()
        };
        assert_eq!(
            words(&vsim_cmd("k", "e", &plain)),
            ["VSIM", "k", "ELE", "e", "WITHSCORES", "COUNT", "10"]
        );
        // FILTER-EF without a FILTER is meaningless — never sent alone.
        let ef_only = SimOptions {
            count: 10,
            filter_ef: Some(9),
            ..Default::default()
        };
        assert!(!words(&vsim_cmd("k", "e", &ef_only)).iter().any(|w| w == "FILTER-EF"));
    }

    #[test]
    fn neighbours_parse_both_transports() {
        // RESP2 triples; nil attrs for an element that has none.
        let resp2 = Value::Array(vec![bs("a"), bs("1"), bs(r#"{"y":1}"#), bs("b"), bs("0.5"), Value::Nil]);
        let n = parse_neighbours(&resp2, true);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].attrs.as_deref(), Some(r#"{"y":1}"#));
        assert_eq!(n[1].attrs, None);
        assert_eq!(n[1].score, 0.5);
        // RESP3 map: element → [score, attrs].
        let resp3 = Value::Map(vec![
            (bs("a"), Value::Array(vec![Value::Double(1.0), bs(r#"{"y":1}"#)])),
            (bs("b"), Value::Array(vec![Value::Double(0.5), Value::Nil])),
        ]);
        let n = parse_neighbours(&resp3, true);
        assert_eq!(n[0].attrs.as_deref(), Some(r#"{"y":1}"#));
        assert_eq!(n[1].attrs, None);
        // Without WITHATTRIBS: pairs, or plain scores in the map.
        let n = parse_neighbours(&Value::Array(vec![bs("a"), bs("0.9")]), false);
        assert_eq!((n[0].element.as_ref(), n[0].score), ("a", 0.9));
        let n = parse_neighbours(&Value::Map(vec![(bs("a"), Value::Double(0.9))]), false);
        assert_eq!(n[0].score, 0.9);
        assert_eq!(n[0].attrs, None);
    }
}

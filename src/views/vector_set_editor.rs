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
//! can be explored hop by hop. Read-only — no `VADD` / `VREM`.

use crate::helpers::get_mono_font_family;
use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ZedisServerState, i18n_vector_set},
};
use gpui::{Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex,
};
use redis::{Value, cmd};
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// How many neighbours `VSIM` returns, and how many elements the
/// `VRANDMEMBER` sample shows.
const KNN_COUNT: i64 = 10;
const SAMPLE_CAP: i64 = 50;

#[derive(Clone, Default)]
struct VectorSetData {
    info: Vec<(String, String)>,
    card: i64,
    dim: i64,
    sample: Vec<SharedString>,
    /// Element the `neighbours` were computed for, and the ranked
    /// `(element, score)` results — seeded from the first sample element
    /// on load, then driven by the search box / clicks.
    queried: Option<SharedString>,
    neighbours: Vec<(SharedString, f64)>,
}

pub struct ZedisVectorSetEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    query_input: Entity<InputState>,
    data: Option<VectorSetData>,
    error: Option<SharedString>,
    search_error: Option<SharedString>,
    loading: bool,
    searching: bool,
    load_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
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

        let mut this = Self {
            server_state,
            key,
            query_input,
            data: None,
            error: None,
            search_error: None,
            loading: false,
            searching: false,
            load_task: None,
            search_task: None,
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        if key.is_empty() {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_vector_set(server_id, db, key).await;
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
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_neighbours(server_id, db, key, element_str).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(neighbours) => {
                        if let Some(data) = this.data.as_mut() {
                            data.neighbours = neighbours;
                        }
                        this.search_error = None;
                    }
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
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().flex_1().child(Input::new(&self.query_input).appearance(true)))
            .child(
                Button::new("vector-set-search")
                    .label(i18n_vector_set(cx, "search"))
                    .small()
                    .primary()
                    .loading(self.searching)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        let element = this.query_input.read(cx).value().to_string();
                        this.run_search(element.into(), cx);
                    })),
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
        col = col.child(header);

        if let Some(err) = self.search_error.clone() {
            return col.child(Label::new(err).text_xs().text_color(cx.theme().danger));
        }
        for (rank, (element, score)) in data.neighbours.iter().enumerate() {
            let el = element.clone();
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
                    .on_click(cx.listener(move |this, _, window, cx| this.search_element(el.clone(), window, cx))),
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
        wrap
    }
}

impl Render for ZedisVectorSetEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
async fn fetch_vector_set(server_id: String, db: usize, key: String) -> Result<VectorSetData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let info_raw: Value = cmd("VINFO").arg(&key).query_async(&mut conn).await?;
    let info = info_pairs_display(&info_raw);
    let card: i64 = cmd("VCARD").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let dim: i64 = cmd("VDIM").arg(&key).query_async(&mut conn).await.unwrap_or(0);
    let sample: Vec<String> = cmd("VRANDMEMBER")
        .arg(&key)
        .arg(SAMPLE_CAP)
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
        && let Ok(raw) = cmd("VSIM")
            .arg(&key)
            .arg("ELE")
            .arg(first.as_ref())
            .arg("WITHSCORES")
            .arg("COUNT")
            .arg(KNN_COUNT)
            .query_async::<Value>(&mut conn)
            .await
    {
        data.queried = Some(first);
        data.neighbours = parse_scored(&raw);
    }
    Ok(data)
}

/// `VSIM key ELE <element> WITHSCORES COUNT n` → ranked neighbours.
async fn fetch_neighbours(
    server_id: String,
    db: usize,
    key: String,
    element: String,
) -> Result<Vec<(SharedString, f64)>> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
    let raw: Value = cmd("VSIM")
        .arg(&key)
        .arg("ELE")
        .arg(&element)
        .arg("WITHSCORES")
        .arg("COUNT")
        .arg(KNN_COUNT)
        .query_async(&mut conn)
        .await?;
    Ok(parse_scored(&raw))
}

/// Parse a `WITHSCORES` reply (RESP3 map or RESP2 flat `[member, score, …]`).
fn parse_scored(value: &Value) -> Vec<(SharedString, f64)> {
    match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| Some((SharedString::from(value_to_string(k)?), value_to_f64(v)?)))
            .collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|chunk| {
                let member = value_to_string(chunk.first()?)?;
                let score = value_to_f64(chunk.get(1)?)?;
                Some((SharedString::from(member), score))
            })
            .collect(),
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

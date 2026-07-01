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

//! RedisBloom probabilistic-structure viewer.
//!
//! Rendered by [`crate::views::ZedisEditor`] for the five RedisBloom
//! module types — Bloom filter (`MBbloom--`), Cuckoo filter
//! (`MBbloomCF`), Count-Min Sketch (`CMSk-TYPE`), Top-K (`TopK-TYPE`)
//! and t-digest (`TDIS-TYPE`) — which previously fell back to the raw
//! bytes editor. Like the time series viewer it self-gates by the key's
//! TYPE (the module must be loaded for a key to resolve to one of these)
//! and owns its own fetch.
//!
//! Every kind renders its `*.INFO` reply as a stat table. Top-K also
//! lists its current top items (`TOPK.LIST … WITHCOUNT`) and t-digest
//! adds min / max / p50 / p90 / p99 (`TDIGEST.MIN` / `MAX` /
//! `QUANTILE`). The viewer is read-only.

use crate::helpers::get_mono_font_family;
use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ProbKind, ZedisServerState, i18n_probabilistic},
};
use gpui::{Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, StyledExt, h_flex, label::Label, v_flex};
use redis::{Value, cmd};
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// i18n sub-key for the kind's display name.
fn kind_i18n_key(kind: ProbKind) -> &'static str {
    match kind {
        ProbKind::Bloom => "bloom",
        ProbKind::Cuckoo => "cuckoo",
        ProbKind::CountMinSketch => "count_min",
        ProbKind::TopK => "topk",
        ProbKind::TDigest => "tdigest",
    }
}

#[derive(Clone, Default)]
struct ProbData {
    /// Flattened `*.INFO` reply, rendered as a stat table.
    info: Vec<(String, String)>,
    /// Top-K only: `(item, count)` from `TOPK.LIST … WITHCOUNT`.
    top_items: Vec<(String, i64)>,
    /// t-digest only: `(label, value)` for min / max / pNN.
    quantiles: Vec<(SharedString, f64)>,
}

pub struct ZedisProbabilisticEditor {
    server_state: Entity<ZedisServerState>,
    key: SharedString,
    kind: ProbKind,
    data: Option<ProbData>,
    error: Option<SharedString>,
    loading: bool,
    /// In-flight fetch; dropped (and thereby cancelled) when the editor
    /// is recreated for a new key.
    load_task: Option<Task<()>>,
}

impl ZedisProbabilisticEditor {
    pub fn new(
        server_state: Entity<ZedisServerState>,
        kind: ProbKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new probabilistic editor view");
        let mut this = Self {
            server_state,
            key,
            kind,
            data: None,
            error: None,
            loading: false,
            load_task: None,
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
        let kind = self.kind;
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_probabilistic(server_id, db, key, kind).await;
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

    fn render_info(&self, data: &ProbData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows = v_flex().w_full().gap_1();
        for (k, v) in data.info.iter() {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_baseline()
                    .child(
                        div()
                            .min_w(px(180.))
                            .child(Label::new(k.clone()).text_xs().text_color(muted)),
                    )
                    .child(Label::new(v.clone()).text_xs().font_semibold()),
            );
        }
        rows
    }

    fn render_top_items(&self, data: &ProbData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut rows = v_flex().w_full().gap_1().child(
            Label::new(i18n_probabilistic(cx, "top_items"))
                .text_sm()
                .font_semibold(),
        );
        for (rank, (item, count)) in data.top_items.iter().enumerate() {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_baseline()
                    .child(
                        div()
                            .min_w(px(28.))
                            .child(Label::new(format!("{}.", rank + 1)).text_xs().text_color(muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(item.clone()).text_xs().font_semibold()),
                    )
                    .child(Label::new(count.to_string()).text_xs().text_color(muted)),
            );
        }
        rows
    }

    fn render_quantiles(&self, data: &ProbData, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut row = h_flex().w_full().flex_wrap().gap_x_4().gap_y_1().items_center().child(
            Label::new(i18n_probabilistic(cx, "quantiles"))
                .text_sm()
                .font_semibold(),
        );
        for (label, value) in data.quantiles.iter() {
            row = row.child(
                h_flex()
                    .gap_1()
                    .items_baseline()
                    .child(Label::new(label.clone()).text_xs().text_color(muted))
                    .child(Label::new(format!("{value:.4}")).text_xs().font_semibold()),
            );
        }
        row
    }
}

impl Render for ZedisProbabilisticEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = i18n_probabilistic(cx, kind_i18n_key(self.kind));
        let header = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(Label::new(title).font_semibold())
            .child(
                div()
                    .px_1p5()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .child(Label::new(self.kind.prefix()).text_xs().text_color(muted)),
            );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if let Some(data) = self.data.as_ref() {
            if data.info.is_empty() && data.top_items.is_empty() && data.quantiles.is_empty() {
                div()
                    .p_4()
                    .child(
                        Label::new(i18n_probabilistic(cx, "no_data"))
                            .text_sm()
                            .text_color(muted),
                    )
                    .into_any_element()
            } else {
                let mut col = v_flex().w_full().gap_4();
                col = col.child(self.render_info(data, cx));
                if !data.top_items.is_empty() {
                    col = col.child(self.render_top_items(data, cx));
                }
                if !data.quantiles.is_empty() {
                    col = col.child(self.render_quantiles(data, cx));
                }
                col.into_any_element()
            }
        } else {
            div()
                .p_4()
                .child(
                    Label::new(i18n_probabilistic(cx, "loading"))
                        .text_sm()
                        .text_color(muted),
                )
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

/// Fetch the `*.INFO` stats plus the per-kind extras (Top-K list /
/// t-digest quantiles). Only `*.INFO` is fatal on error; the extras are
/// best-effort.
async fn fetch_probabilistic(server_id: String, db: usize, key: String, kind: ProbKind) -> Result<ProbData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let info_cmd = format!("{}.INFO", kind.prefix());
    let info_raw: Value = cmd(info_cmd.as_str()).arg(&key).query_async(&mut conn).await?;
    let mut data = ProbData {
        info: info_pairs_display(&info_raw),
        ..Default::default()
    };

    match kind {
        ProbKind::TopK => {
            if let Ok(raw) = cmd("TOPK.LIST")
                .arg(&key)
                .arg("WITHCOUNT")
                .query_async::<Value>(&mut conn)
                .await
            {
                data.top_items = parse_topk_list(&raw);
            }
        }
        ProbKind::TDigest => {
            let mut quantiles = Vec::new();
            if let Ok(v) = cmd("TDIGEST.MIN").arg(&key).query_async::<f64>(&mut conn).await
                && v.is_finite()
            {
                quantiles.push((SharedString::from("min"), v));
            }
            if let Ok(v) = cmd("TDIGEST.MAX").arg(&key).query_async::<f64>(&mut conn).await
                && v.is_finite()
            {
                quantiles.push((SharedString::from("max"), v));
            }
            if let Ok(vs) = cmd("TDIGEST.QUANTILE")
                .arg(&key)
                .arg(0.5)
                .arg(0.9)
                .arg(0.99)
                .query_async::<Vec<f64>>(&mut conn)
                .await
            {
                for (label, value) in [("p50", vs.first()), ("p90", vs.get(1)), ("p99", vs.get(2))] {
                    if let Some(value) = value.copied().filter(|x| x.is_finite()) {
                        quantiles.push((SharedString::from(label), value));
                    }
                }
            }
            data.quantiles = quantiles;
        }
        _ => {}
    }

    Ok(data)
}

/// Flatten a `*.INFO` reply (RESP3 map or RESP2 flat array) into
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

/// Parse `TOPK.LIST key WITHCOUNT` (a flat `[item, count, …]` array).
fn parse_topk_list(value: &Value) -> Vec<(String, i64)> {
    let Value::Array(items) = value else {
        return vec![];
    };
    items
        .chunks(2)
        .filter_map(|chunk| {
            let item = value_to_string(chunk.first()?)?;
            let count = value_to_i64(chunk.get(1)?)?;
            Some((item, count))
        })
        .collect()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

fn value_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(i) => Some(*i),
        Value::Double(d) => Some(*d as i64),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).trim().parse().ok(),
        Value::SimpleString(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Stringify any `*.INFO` value for display.
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

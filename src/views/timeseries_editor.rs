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

//! RedisTimeSeries (`TS.*`) value viewer.
//!
//! Rendered by [`crate::views::ZedisEditor`] whenever the selected key
//! reports the module type `TSDB-TYPE` (mapped to
//! [`crate::states::KeyType::TimeSeries`]). No separate module/version
//! gate is needed: a key only resolves to this type when the
//! `timeseries` module is loaded, so the viewer self-gates by the key's
//! existence — mirroring the RedisJSON (`ReJSON-RL`) dispatch path.
//!
//! Data is fetched directly through the pooled connection (the metrics
//! view's self-owned-task pattern) rather than through `ServerState`
//! ops: `TS.INFO` for metadata and `TS.RANGE` (server-side `AVG`
//! aggregation, bucketed to ~[`TARGET_POINTS`]) for the line itself, so
//! a multi-million-sample series never ships every point to the UI.

use crate::helpers::get_mono_font_family;
use crate::{
    connection::get_connection_manager,
    error::Error,
    states::{ZedisServerState, i18n_timeseries},
    views::{ChartParams, format_timestamp_ms, make_line_canvas},
};
use gpui::{Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    v_flex,
};
use redis::{Value, cmd};
use std::sync::Arc;
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Target number of points to draw. The range is bucketed with a
/// server-side `AVG` aggregation so the chart stays readable (and the
/// payload bounded) regardless of how dense the underlying series is.
const TARGET_POINTS: i64 = 240;
const CHART_HEIGHT: f32 = 320.;

/// Selectable look-back windows for the chart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TsRange {
    M15,
    H1,
    H6,
    D1,
    D7,
    All,
}

impl TsRange {
    const ALL: [TsRange; 6] = [
        TsRange::M15,
        TsRange::H1,
        TsRange::H6,
        TsRange::D1,
        TsRange::D7,
        TsRange::All,
    ];

    /// Window length in milliseconds, or `None` for the full history.
    fn window_ms(self) -> Option<i64> {
        match self {
            TsRange::M15 => Some(15 * 60 * 1000),
            TsRange::H1 => Some(60 * 60 * 1000),
            TsRange::H6 => Some(6 * 60 * 60 * 1000),
            TsRange::D1 => Some(24 * 60 * 60 * 1000),
            TsRange::D7 => Some(7 * 24 * 60 * 60 * 1000),
            TsRange::All => None,
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            TsRange::M15 => "range_15m",
            TsRange::H1 => "range_1h",
            TsRange::H6 => "range_6h",
            TsRange::D1 => "range_24h",
            TsRange::D7 => "range_7d",
            TsRange::All => "range_all",
        }
    }
}

/// Metadata parsed from `TS.INFO` (best effort — missing fields stay 0).
#[derive(Clone, Default)]
struct TsInfo {
    total_samples: i64,
    memory_usage: i64,
    first_ts: i64,
    last_ts: i64,
    retention_ms: i64,
    chunk_count: i64,
    labels: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct TsData {
    info: TsInfo,
    samples: Vec<(i64, f64)>,
}

pub struct ZedisTimeSeriesEditor {
    server_state: Entity<ZedisServerState>,
    /// Key snapshotted at construction. The editor is recreated per key
    /// (taken on `ValueLoaded` by `ZedisEditor`), so this never goes
    /// stale relative to the live selection.
    key: SharedString,
    range: TsRange,
    data: Option<TsData>,
    error: Option<SharedString>,
    loading: bool,
    /// In-flight fetch. Dropping it (new key, range switch, teardown)
    /// cancels the previous request.
    load_task: Option<Task<()>>,
}

impl ZedisTimeSeriesEditor {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = server_state.read(cx).key().unwrap_or_default();
        info!("Creating new time series editor view");
        let mut this = Self {
            server_state,
            key,
            range: TsRange::All,
            data: None,
            error: None,
            loading: false,
            load_task: None,
        };
        this.load(cx);
        this
    }

    /// Kick off a background `TS.INFO` + `TS.RANGE` fetch for the current
    /// key and range, replacing any in-flight request.
    fn load(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let key = self.key.to_string();
        if key.is_empty() {
            return;
        }
        let range = self.range;
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_timeseries(server_id, db, key, range).await;
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

    fn set_range(&mut self, range: TsRange, cx: &mut Context<Self>) {
        if self.range == range {
            return;
        }
        self.range = range;
        self.load(cx);
    }

    fn render_range_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.range;
        let mut bar = h_flex().gap_1().items_center();
        for r in TsRange::ALL {
            let label = i18n_timeseries(cx, r.i18n_key());
            let mut button = Button::new(("ts-range", r as usize)).label(label).small();
            button = if r == current { button.primary() } else { button.ghost() };
            bar = bar.child(button.on_click(cx.listener(move |this, _, _, cx| this.set_range(r, cx))));
        }
        bar
    }

    fn render_meta(&self, info: &TsInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let chip = |label: SharedString, value: String| {
            h_flex()
                .gap_1()
                .items_baseline()
                .child(Label::new(label).text_xs().text_color(muted))
                .child(Label::new(value).text_xs().font_semibold())
        };

        let mut row = h_flex().flex_wrap().gap_x_4().gap_y_1().items_center();
        row = row.child(chip(i18n_timeseries(cx, "samples"), info.total_samples.to_string()));
        if info.memory_usage > 0 {
            let mem = humansize::format_size(info.memory_usage as u64, humansize::DECIMAL);
            row = row.child(chip(i18n_timeseries(cx, "memory"), mem));
        }
        row = row.child(chip(i18n_timeseries(cx, "retention"), human_ms(info.retention_ms)));
        if info.chunk_count > 0 {
            row = row.child(chip(i18n_timeseries(cx, "chunks"), info.chunk_count.to_string()));
        }
        for (k, v) in info.labels.iter() {
            row = row.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .child(Label::new(format!("{k}={v}")).text_xs().text_color(muted)),
            );
        }
        row
    }

    fn render_chart(&self, data: &TsData, cx: &mut Context<Self>) -> impl IntoElement {
        let dates: Vec<SharedString> = data.samples.iter().map(|(ts, _)| format_timestamp_ms(*ts)).collect();
        let values: Vec<f64> = data.samples.iter().map(|(_, v)| *v).collect();
        let max = values.iter().copied().fold(0.0_f64, f64::max);
        let y_max = if max <= 0.0 { 1.0 } else { max * 1.1 };
        let tick_margin = (dates.len() / 6).max(1);

        let params = ChartParams {
            dates: Arc::new(dates),
            y_max,
            y_format: Box::new(|v: f64| format!("{v:.2}")),
            tick_margin,
            border: cx.theme().border,
            muted_fg: cx.theme().muted_foreground,
        };
        let chart = make_line_canvas(params, Arc::new(values), cx.theme().chart_1, false);
        v_flex().w_full().h(px(CHART_HEIGHT)).child(chart)
    }
}

impl Render for ZedisTimeSeriesEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(Label::new(i18n_timeseries(cx, "title")).font_semibold())
            .child(self.render_range_bar(cx));

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if let Some(data) = self.data.as_ref() {
            if data.samples.is_empty() {
                div()
                    .p_4()
                    .child(Label::new(i18n_timeseries(cx, "no_data")).text_sm().text_color(muted))
                    .into_any_element()
            } else {
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(self.render_meta(&data.info, cx))
                    .child(self.render_chart(data, cx))
                    .into_any_element()
            }
        } else {
            div()
                .p_4()
                .child(Label::new(i18n_timeseries(cx, "loading")).text_sm().text_color(muted))
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

/// Fetch `TS.INFO` (metadata) and a bucketed `TS.RANGE` (the line) for
/// `key` over the selected window.
async fn fetch_timeseries(server_id: String, db: usize, key: String, range: TsRange) -> Result<TsData> {
    let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

    let info_raw: Value = cmd("TS.INFO").arg(&key).query_async(&mut conn).await?;
    let info = parse_ts_info(&info_raw);

    // No samples (or an unreadable INFO) → render the empty state.
    if info.total_samples <= 0 || info.last_ts <= 0 {
        return Ok(TsData { info, samples: vec![] });
    }

    let to = info.last_ts;
    let from = match range.window_ms() {
        Some(window) => (to - window).max(info.first_ts),
        None => info.first_ts,
    }
    .min(to);

    // Bucket so the returned point count is ~TARGET_POINTS regardless of
    // sample density; skip aggregation for short/sparse spans.
    let span = (to - from).max(1);
    let bucket = (span / TARGET_POINTS).max(1);

    let mut range_cmd = cmd("TS.RANGE");
    range_cmd.arg(&key).arg(from).arg(to);
    if bucket > 1 {
        range_cmd.arg("AGGREGATION").arg("avg").arg(bucket);
    }
    let samples: Vec<(i64, f64)> = range_cmd.query_async(&mut conn).await?;

    Ok(TsData { info, samples })
}

/// Flatten a `TS.INFO` reply into `(field, value)` pairs, tolerating
/// both the RESP3 map and the RESP2 flat-array encodings.
fn ts_info_pairs(value: &Value) -> Vec<(String, &Value)> {
    match value {
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
    }
}

fn parse_ts_info(value: &Value) -> TsInfo {
    let pairs = ts_info_pairs(value);
    let find = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| *v);

    let mut info = TsInfo::default();
    if let Some(v) = find("totalSamples") {
        info.total_samples = value_to_i64(v).unwrap_or(0);
    }
    if let Some(v) = find("memoryUsage") {
        info.memory_usage = value_to_i64(v).unwrap_or(0);
    }
    if let Some(v) = find("firstTimestamp") {
        info.first_ts = value_to_i64(v).unwrap_or(0);
    }
    if let Some(v) = find("lastTimestamp") {
        info.last_ts = value_to_i64(v).unwrap_or(0);
    }
    if let Some(v) = find("retentionTime") {
        info.retention_ms = value_to_i64(v).unwrap_or(0);
    }
    if let Some(v) = find("chunkCount") {
        info.chunk_count = value_to_i64(v).unwrap_or(0);
    }
    if let Some(Value::Array(items)) = find("labels") {
        for item in items {
            if let Value::Array(kv) = item
                && let (Some(k), Some(v)) = (
                    kv.first().and_then(value_to_string),
                    kv.get(1).and_then(value_to_string),
                )
            {
                info.labels.push((k, v));
            }
        }
    }
    info
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

/// Compact humanization of a millisecond duration; `0` (or less) means
/// "no retention limit".
fn human_ms(ms: i64) -> String {
    if ms <= 0 {
        return "∞".to_string();
    }
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

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

use crate::assets::CustomIconName;
use crate::connection::{ServerCommand, get_server};
use crate::helpers::has_file_dialogs;
use crate::helpers::{build_csv, format_unix_millis_with, get_mono_font_family, pacing};
use crate::states::{RedisMetrics, ServerView, get_metrics_cache, load_persisted_metrics};
use crate::states::{
    ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, content_area_width, i18n_common, i18n_metrics,
};
use crate::views::{
    ChartParams, ChartSeries, ServerReport, export_to_file, make_bounded_line_chart, make_line_chart,
    make_series_chart, open_server_report_dialog,
};
use core::f64;
use gpui::{
    App, Entity, Pixels, SharedString, Subscription, Task, Window, div, linear_color_stop, linear_gradient, prelude::*,
    px,
};
use gpui_kit::component::h_flex;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::Arc;
use zedis_ui::ZedisSkeletonLoading;

const TIME_FORMAT: &str = "%H:%M:%S";
const CHART_CARD_HEIGHT: Pixels = px(300.);
const BYTES_TO_MB: f64 = 1_000_000.;

/// Chart-ready series, computed once per heartbeat in
/// [`convert_metrics_to_chart_data`] and rendered by borrowing: each
/// `render_*_chart` just bumps these `Arc`s instead of re-flattening and
/// re-collecting a fresh `Vec` every frame. `dates` is the shared x-axis —
/// one allocation reused by all eight charts.
#[derive(Debug, Clone)]
struct MetricsChartData {
    dates: Arc<Vec<SharedString>>,
    max_cpu_percent: f64,
    min_cpu_percent: f64,
    cpu_sys: Arc<Vec<f64>>,
    cpu_user: Arc<Vec<f64>>,
    max_memory: f64,
    min_memory: f64,
    memory: Arc<Vec<f64>>,
    min_latency_ms: f64,
    max_latency_ms: f64,
    latency: Arc<Vec<f64>>,
    max_connected_clients: f64,
    min_connected_clients: f64,
    connected_clients: Arc<Vec<f64>>,
    max_total_commands_processed: f64,
    min_total_commands_processed: f64,
    total_commands_processed: Arc<Vec<f64>>,
    max_net_kbps: f64,
    min_net_kbps: f64,
    input_kbps: Arc<Vec<f64>>,
    output_kbps: Arc<Vec<f64>>,
    max_blocked_clients: f64,
    blocked_clients: Arc<Vec<f64>>,
    max_fragmentation: f64,
    min_fragmentation: f64,
    fragmentation: Arc<Vec<f64>>,
    max_key_hit_rate: f64,
    min_key_hit_rate: f64,
    key_hit_rate: Arc<Vec<f64>>,
    max_evicted_keys: f64,
    min_evicted_keys: f64,
    evicted_keys: Arc<Vec<f64>>,
}

/// Chart time window: `Live` renders the in-memory 2s-resolution cache
/// (session-only, the pre-existing behavior); the other ranges load the
/// persisted 1/min samples from redb, so they survive restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricsRange {
    Live,
    LastHour,
    LastDay,
    LastWeek,
}

impl MetricsRange {
    const ALL: [MetricsRange; 4] = [
        MetricsRange::Live,
        MetricsRange::LastHour,
        MetricsRange::LastDay,
        MetricsRange::LastWeek,
    ];
    /// Whether the panel offers this window. All of them on the desktop.
    #[cfg(not(target_family = "wasm"))]
    const fn offered(self) -> bool {
        true
    }
    /// Only `Live` in the browser: the history windows read samples the
    /// browser build never writes (`states/server/stat/persist.rs`).
    #[cfg(target_family = "wasm")]
    fn offered(self) -> bool {
        self == MetricsRange::Live
    }
    fn button_id(self) -> &'static str {
        match self {
            MetricsRange::Live => "metrics-range-live",
            MetricsRange::LastHour => "metrics-range-1h",
            MetricsRange::LastDay => "metrics-range-24h",
            MetricsRange::LastWeek => "metrics-range-7d",
        }
    }
    fn label_key(self) -> &'static str {
        match self {
            MetricsRange::Live => "range_live",
            MetricsRange::LastHour => "range_1h",
            MetricsRange::LastDay => "range_24h",
            MetricsRange::LastWeek => "range_7d",
        }
    }
    fn duration_ms(self) -> i64 {
        match self {
            MetricsRange::Live => 0,
            MetricsRange::LastHour => 60 * 60 * 1000,
            MetricsRange::LastDay => 24 * 60 * 60 * 1000,
            MetricsRange::LastWeek => 7 * 24 * 60 * 60 * 1000,
        }
    }
    /// X-axis label format: seconds only make sense for the live window,
    /// and a week of labels needs the date to stay readable.
    fn time_format(self) -> &'static str {
        match self {
            MetricsRange::Live => TIME_FORMAT,
            MetricsRange::LastHour | MetricsRange::LastDay => "%H:%M",
            MetricsRange::LastWeek => "%m-%d %H:%M",
        }
    }
    /// Decimation budget: enough bars to show the trend without turning
    /// the band chart into a smear.
    fn max_points(self) -> usize {
        match self {
            MetricsRange::Live => 0,
            MetricsRange::LastHour => 120,
            MetricsRange::LastDay => 144,
            MetricsRange::LastWeek => 168,
        }
    }
}

pub struct ZedisMetrics {
    title: SharedString,
    server_state: Entity<ZedisServerState>,
    server_id: String,
    range: MetricsRange,
    latest_metrics: Option<RedisMetrics>,
    metrics_chart_data: MetricsChartData,
    /// Label every how-many-th sample on a chart's x axis. Worked out in
    /// `render`, where the width of a chart is known ([`tick_margin_for`]).
    tick_margin: usize,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn format_timestamp_ms_as(ts_ms: i64, fmt: &str) -> SharedString {
    format_unix_millis_with(ts_ms, fmt)
        .map(SharedString::from)
        .unwrap_or_else(|| "--".into())
}

pub(crate) fn format_timestamp_ms(ts_ms: i64) -> SharedString {
    format_timestamp_ms_as(ts_ms, TIME_FORMAT)
}

/// The floor a chart's maximum is given so an all-zero series still has an
/// axis; at or under it the series is drawn on 0–1 (see `chart_params`).
const FLAT_SERIES_MAX: f64 = 0.01;

/// The width a chart on the metrics page has to draw its x axis in: the
/// content area split into `columns`, less the page's and the card's
/// paddings and the y axis' labels.
fn chart_plot_width(content_width: f32, columns: u16) -> f32 {
    const PAGE_PADDING: f32 = 16.;
    const GRID_GAP: f32 = 8.;
    const CARD_PADDING_AND_Y_AXIS: f32 = 88.;
    let columns = f32::from(columns.max(1));
    let card = (content_width - PAGE_PADDING - GRID_GAP * (columns - 1.)) / columns;
    (card - CARD_PADDING_AND_Y_AXIS).max(0.)
}

/// Label every how-many-th of `samples` so the labels — `label_chars` wide,
/// in the axis' small mono face — fit in `chart_width` with a gap between
/// them. Scales with the UI font size, which the axis labels follow.
fn tick_margin_for(samples: usize, label_chars: usize, chart_width: f32, font_px: f32) -> usize {
    const CHAR_WIDTH: f32 = 6.;
    const LABEL_GAP: f32 = 20.;
    let label_width = (label_chars as f32 * CHAR_WIDTH + LABEL_GAP) * font_px / 14.;
    let fits = (chart_width / label_width).floor().max(1.);
    ((samples as f32 / fits).ceil() as usize).max(1)
}

fn convert_metrics_to_chart_data(history_metrics: Vec<RedisMetrics>, time_format: &str) -> MetricsChartData {
    let mut prev_metrics = RedisMetrics::default();
    let n = history_metrics.len();

    let mut dates = Vec::with_capacity(n);

    let mut cpu_sys = Vec::with_capacity(n);
    let mut cpu_user = Vec::with_capacity(n);
    // `<f64>::MIN`, not `f64::MIN`: bare `f64::MIN` resolves to the
    // deprecated `std::f64` module constant, which the nightly toolchain the
    // browser build uses warns about (stable does not). The angle brackets
    // name the primitive, so the associated constant wins — don't simplify.
    let mut max_cpu_percent = <f64>::MIN;
    let mut min_cpu_percent = <f64>::MAX;

    let mut memory = Vec::with_capacity(n);
    let mut max_memory = <f64>::MIN;
    let mut min_memory = <f64>::MAX;

    let mut latency = Vec::with_capacity(n);
    let mut min_latency_ms = <f64>::MAX;
    let mut max_latency_ms = <f64>::MIN;

    let mut connected_clients = Vec::with_capacity(n);
    let mut max_connected_clients = <f64>::MIN;
    let mut min_connected_clients = <f64>::MAX;

    let mut total_commands_processed = Vec::with_capacity(n);
    let mut max_total_commands_processed = <f64>::MIN;
    let mut min_total_commands_processed = <f64>::MAX;

    let mut input_kbps = Vec::with_capacity(n);
    let mut output_kbps = Vec::with_capacity(n);
    let mut max_net_kbps = <f64>::MIN;
    let mut min_net_kbps = <f64>::MAX;

    let mut blocked_clients = Vec::with_capacity(n);
    let mut max_blocked_clients = <f64>::MIN;

    let mut fragmentation = Vec::with_capacity(n);
    let mut max_fragmentation = <f64>::MIN;
    let mut min_fragmentation = <f64>::MAX;

    let mut key_hit_rate = Vec::with_capacity(n);
    let mut max_key_hit_rate = <f64>::MIN;
    let mut min_key_hit_rate = <f64>::MAX;

    let mut evicted_keys = Vec::with_capacity(n);
    let mut max_evicted_keys = <f64>::MIN;
    let mut min_evicted_keys = <f64>::MAX;

    for metrics in history_metrics.iter() {
        let duration_ms = if prev_metrics.timestamp_ms != 0 {
            metrics.timestamp_ms - prev_metrics.timestamp_ms
        } else {
            0
        };
        if duration_ms <= 0 {
            prev_metrics = *metrics;
            continue;
        }

        dates.push(format_timestamp_ms_as(metrics.timestamp_ms, time_format));
        let delta_time = (duration_ms as f64) / 1000.;

        // Counters (CPU time, commands, hits, evictions) reset when the
        // server restarts. Persisted history spans restarts, so deltas are
        // clamped at zero instead of wrapping into absurd spikes.
        let used_cpu_sys_percent = ((metrics.used_cpu_sys - prev_metrics.used_cpu_sys) / delta_time * 100.).max(0.);
        let used_cpu_user_percent = ((metrics.used_cpu_user - prev_metrics.used_cpu_user) / delta_time * 100.).max(0.);
        max_cpu_percent = max_cpu_percent.max(used_cpu_sys_percent.max(used_cpu_user_percent));
        min_cpu_percent = min_cpu_percent.min(used_cpu_sys_percent.min(used_cpu_user_percent));
        cpu_sys.push(used_cpu_sys_percent);
        cpu_user.push(used_cpu_user_percent);

        let used_memory = metrics.used_memory as f64 / BYTES_TO_MB;
        max_memory = max_memory.max(used_memory);
        min_memory = min_memory.min(used_memory);
        memory.push(used_memory);

        let latency_ms = metrics.latency_ms as f64;
        max_latency_ms = max_latency_ms.max(latency_ms);
        min_latency_ms = min_latency_ms.min(latency_ms);
        latency.push(latency_ms);

        let clients = metrics.connected_clients as f64;
        max_connected_clients = max_connected_clients.max(clients);
        min_connected_clients = min_connected_clients.min(clients);
        connected_clients.push(clients);

        // Per second: samples come every 2s in front and every 30s in a
        // background tab, and a count per sample drew each switch between
        // the two as a spike (or a drop) fifteen times its size.
        let processed = metrics
            .total_commands_processed
            .saturating_sub(prev_metrics.total_commands_processed) as f64
            / delta_time;
        max_total_commands_processed = max_total_commands_processed.max(processed);
        min_total_commands_processed = min_total_commands_processed.min(processed);
        total_commands_processed.push(processed);

        // One axis for both directions, so in and out compare by eye. A
        // rate below zero is a server saying it keeps none (`net_rate_label`)
        // and is drawn as the flat line it is, not as a dip under the axis.
        let input = metrics.instantaneous_input_kbps.max(0.);
        let output = metrics.instantaneous_output_kbps.max(0.);
        max_net_kbps = max_net_kbps.max(input.max(output));
        min_net_kbps = min_net_kbps.min(input.min(output));
        input_kbps.push(input);
        output_kbps.push(output);

        let blocked = metrics.blocked_clients as f64;
        max_blocked_clients = max_blocked_clients.max(blocked);
        blocked_clients.push(blocked);

        let ratio = metrics.mem_fragmentation_ratio;
        max_fragmentation = max_fragmentation.max(ratio);
        min_fragmentation = min_fragmentation.min(ratio);
        fragmentation.push(ratio);

        let keyspace_hits = metrics.keyspace_hits.saturating_sub(prev_metrics.keyspace_hits);
        let keyspace_misses = metrics.keyspace_misses.saturating_sub(prev_metrics.keyspace_misses);
        let keyspace_total = keyspace_hits + keyspace_misses;
        let rate = if keyspace_total > 0 {
            keyspace_hits as f64 / keyspace_total as f64 * 100.
        } else {
            100.
        };
        max_key_hit_rate = max_key_hit_rate.max(rate);
        min_key_hit_rate = min_key_hit_rate.min(rate);
        key_hit_rate.push(rate);

        let evicted = metrics.evicted_keys.saturating_sub(prev_metrics.evicted_keys) as f64;
        max_evicted_keys = max_evicted_keys.max(evicted);
        min_evicted_keys = min_evicted_keys.min(evicted);
        evicted_keys.push(evicted);

        prev_metrics = *metrics;
    }

    MetricsChartData {
        dates: Arc::new(dates),
        cpu_sys: Arc::new(cpu_sys),
        cpu_user: Arc::new(cpu_user),
        max_cpu_percent,
        min_cpu_percent,
        memory: Arc::new(memory),
        max_memory,
        min_memory,
        latency: Arc::new(latency),
        min_latency_ms,
        max_latency_ms,
        connected_clients: Arc::new(connected_clients),
        max_connected_clients,
        min_connected_clients,
        total_commands_processed: Arc::new(total_commands_processed),
        max_total_commands_processed,
        min_total_commands_processed,
        input_kbps: Arc::new(input_kbps),
        output_kbps: Arc::new(output_kbps),
        max_net_kbps,
        min_net_kbps,
        blocked_clients: Arc::new(blocked_clients),
        max_blocked_clients,
        fragmentation: Arc::new(fragmentation),
        max_fragmentation,
        min_fragmentation,
        key_hit_rate: Arc::new(key_hit_rate),
        min_key_hit_rate,
        max_key_hit_rate,
        evicted_keys: Arc::new(evicted_keys),
        max_evicted_keys,
        min_evicted_keys,
    }
}

impl ZedisMetrics {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_id = server_state.read(cx).server_id().to_string();
        let title = Self::title_for(&server_state, cx);
        let metrics_history = get_metrics_cache().list_metrics(&server_id);
        let latest_metrics = metrics_history.last().copied();
        let metrics_chart_data = convert_metrics_to_chart_data(metrics_history, TIME_FORMAT);

        let mut this = Self {
            title,
            server_state,
            server_id,
            range: MetricsRange::Live,
            latest_metrics,
            metrics_chart_data,
            tick_margin: 1,
            heartbeat_task: None,
            _subscriptions: vec![],
        };
        this.start_heartbeat(cx);
        this
    }

    /// `name - type(masters)`, or `--` while the tab has no connection yet.
    /// The count of masters — the field it used to print is their addresses.
    fn title_for(server_state: &Entity<ZedisServerState>, cx: &App) -> SharedString {
        let state = server_state.read(cx);
        let name = get_server(state.server_id())
            .map(|server| server.name)
            .unwrap_or_else(|_| "--".to_string());
        format!(
            "{name} - {}({})",
            state.nodes_description().server_type,
            state.nodes().0
        )
        .into()
    }

    /// Switch the chart window. `Live` re-renders from the in-memory cache
    /// immediately; history ranges load the persisted samples off the UI
    /// thread (redb read + JSON decode) and apply on completion, dropping
    /// the result if the user has already switched again.
    fn set_range(&mut self, range: MetricsRange, cx: &mut Context<Self>) {
        if self.range == range {
            return;
        }
        self.range = range;
        if range == MetricsRange::Live {
            let history = get_metrics_cache().list_metrics(&self.server_id);
            self.metrics_chart_data = convert_metrics_to_chart_data(history, TIME_FORMAT);
            cx.notify();
            return;
        }
        let server_id = self.server_id.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let history = cx
                .background_spawn(
                    async move { load_persisted_metrics(&server_id, range.duration_ms(), range.max_points()) },
                )
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.range != range {
                    return;
                }
                state.metrics_chart_data = convert_metrics_to_chart_data(history, range.time_format());
                cx.notify();
            });
        })
        .detach();
    }
    /// Start the heartbeat task
    fn start_heartbeat(&mut self, cx: &mut Context<Self>) {
        self.heartbeat_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(pacing::HEARTBEAT_INTERVAL).await;
                let _ = this.update(cx, |state, cx| state.tick(cx));
            }
        }));
    }

    /// One heartbeat: pull the latest samples for the tab's connection.
    /// The connection is read each time rather than once at construction —
    /// a restored tab connects after its route (and this view) is up, so a
    /// view built against an empty id would otherwise load forever.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id != self.server_id {
            self.server_id = server_id;
            self.range = MetricsRange::Live;
        }
        // Every beat: the topology lands after the switch that changed the
        // id, and a title made then kept saying `(0)`.
        self.title = Self::title_for(&self.server_state, cx);
        if self.server_id.is_empty() {
            return;
        }
        let metrics_history = get_metrics_cache().list_metrics(&self.server_id);
        self.latest_metrics = metrics_history.last().copied();
        // Stat cards stay live in every range; the charts only follow the
        // heartbeat in the Live window — a history window is a frozen
        // snapshot until re-selected.
        if self.range == MetricsRange::Live {
            self.metrics_chart_data = convert_metrics_to_chart_data(metrics_history, TIME_FORMAT);
        }
        cx.notify();
    }
    fn render_chart_card<E: IntoElement>(
        &self,
        cx: &mut Context<Self>,
        label: impl Into<SharedString>,
        chart: E,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .h(CHART_CARD_HEIGHT)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .p_4()
            .child(div().font_semibold().child(label.into()).mb_2())
            .child(chart)
    }

    fn chart_params(
        &self,
        id: &'static str,
        dates: Arc<Vec<SharedString>>,
        y_max: f64,
        y_format: impl Fn(f64) -> String + 'static,
    ) -> ChartParams {
        // A series that never left zero arrives with its floor for a maximum,
        // and an axis from 0 to that reads "0" on every tick. It gets 0–1 and
        // decimals instead (two: the ticks fall on quarters): a flat line on
        // a scale, not five zeros.
        let flat = y_max <= FLAT_SERIES_MAX;
        let y_format: Box<dyn Fn(f64) -> String> = if flat {
            Box::new(|value| format!("{value:.2}"))
        } else {
            Box::new(y_format)
        };
        ChartParams {
            id: id.into(),
            y_min: 0.0,
            dates,
            y_max: if flat { 1.0 } else { y_max },
            y_format,
            tick_margin: self.tick_margin,
        }
    }

    fn render_stat_card(&self, cx: &mut Context<Self>, label: SharedString, value: String) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .p_4()
            .child(Label::new(label).text_sm().text_color(theme.muted_foreground))
            .child(Label::new(value).font_semibold())
    }

    fn render_stat_cards(&self, columns: u16, cx: &mut Context<Self>) -> impl IntoElement {
        let m = match self.latest_metrics {
            Some(m) => m,
            None => return div().into_any_element(),
        };

        let memory = if m.used_memory == 0 {
            "--".to_string()
        } else {
            humansize::format_size(m.used_memory, humansize::FormatSizeOptions::default().decimal_places(0))
        };

        let clients = format!("{} / {}", m.connected_clients, m.blocked_clients);

        let ops = format!("{} ops/s", m.instantaneous_ops_per_sec);

        let latency = format!("{} ms", m.latency_ms);

        let total = m.keyspace_hits + m.keyspace_misses;
        let hit_rate = if total > 0 {
            format!("{:.1}%", m.keyspace_hits as f64 / total as f64 * 100.)
        } else {
            "100%".to_string()
        };

        let net = net_rate_label(m.instantaneous_input_kbps, m.instantaneous_output_kbps);

        // 0 means INFO did not report it (a proxy), not a perfect ratio.
        let fragmentation = if m.mem_fragmentation_ratio > 0. {
            format!("{:.2}", m.mem_fragmentation_ratio)
        } else {
            "--".to_string()
        };

        let evicted = m.evicted_keys.to_string();

        div()
            .col_span_full()
            .w_full()
            .grid()
            .gap_2()
            .grid_cols(columns * 2)
            .child(self.render_stat_card(cx, i18n_metrics(cx, "memory"), memory))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "clients"), clients))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "ops"), ops))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "latency"), latency))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "hit_rate"), hit_rate))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "net"), net))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "fragmentation"), fragmentation))
            .child(self.render_stat_card(cx, i18n_metrics(cx, "evicted_keys"), evicted))
            .into_any_element()
    }

    fn render_cpu_usage_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.2}% - {:.2}%",
            i18n_metrics(cx, "cpu_usage"),
            self.metrics_chart_data.min_cpu_percent,
            self.metrics_chart_data.max_cpu_percent
        );
        let dates = self.metrics_chart_data.dates.clone();
        let sys_values = self.metrics_chart_data.cpu_sys.clone();
        let user_values = self.metrics_chart_data.cpu_user.clone();
        let max_val = self.metrics_chart_data.max_cpu_percent.max(0.01);
        let chart_1 = cx.theme().chart_1;
        let chart_2 = cx.theme().chart_2;
        let bg = cx.theme().background;
        let chart = make_series_chart(
            self.chart_params("metrics-cpu", dates, max_val, |v| format!("{:.1}%", v)),
            vec![
                ChartSeries {
                    name: i18n_metrics(cx, "series_system"),
                    values: sys_values,
                    stroke: chart_1,
                    fill: Some(linear_gradient(
                        0.,
                        linear_color_stop(chart_1.opacity(0.4), 1.),
                        linear_color_stop(bg.opacity(0.3), 0.),
                    )),
                },
                ChartSeries {
                    name: i18n_metrics(cx, "series_user"),
                    values: user_values,
                    stroke: chart_2,
                    fill: Some(linear_gradient(
                        0.,
                        linear_color_stop(chart_2.opacity(0.4), 1.),
                        linear_color_stop(bg.opacity(0.3), 0.),
                    )),
                },
            ],
            false,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_memory_usage_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0}MB - {:.0}MB",
            i18n_metrics(cx, "memory_usage"),
            self.metrics_chart_data.min_memory,
            self.metrics_chart_data.max_memory
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.memory.clone();
        // A line on an axis drawn around the samples: as bars from zero, a
        // steady 500 MB was a solid wall and a 5 MB climb did not show.
        let (y_min, y_max) = memory_axis(self.metrics_chart_data.min_memory, self.metrics_chart_data.max_memory);
        let mut params = self.chart_params("metrics-memory", dates, y_max, |v| format!("{:.0}", v));
        params.y_min = y_min;
        let chart = make_line_chart(params, values, cx.theme().chart_2);
        self.render_chart_card(cx, label, chart)
    }

    fn render_latency_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0}ms - {:.0}ms",
            i18n_metrics(cx, "latency"),
            self.metrics_chart_data.min_latency_ms,
            self.metrics_chart_data.max_latency_ms
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.latency.clone();
        let max_val = self.metrics_chart_data.max_latency_ms.max(0.01);
        let stroke = cx.theme().chart_2;
        let chart = make_line_chart(
            self.chart_params("metrics-latency", dates, max_val, |v| format!("{:.0}", v)),
            values,
            stroke,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_connected_clients_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0} - {:.0} / {:.0}",
            i18n_metrics(cx, "clients_chart"),
            self.metrics_chart_data.min_connected_clients,
            self.metrics_chart_data.max_connected_clients,
            self.metrics_chart_data.max_blocked_clients.max(0.)
        );
        let dates = self.metrics_chart_data.dates.clone();
        let connected = self.metrics_chart_data.connected_clients.clone();
        let blocked = self.metrics_chart_data.blocked_clients.clone();
        // Blocked clients are a subset of connected, so one axis fits both.
        let max_val = self.metrics_chart_data.max_connected_clients.max(0.01);
        let chart_1 = cx.theme().chart_1;
        let chart_2 = cx.theme().chart_2;
        let chart = make_series_chart(
            self.chart_params("metrics-clients", dates, max_val, |v| format!("{:.0}", v)),
            vec![
                ChartSeries {
                    name: i18n_metrics(cx, "series_connected"),
                    values: connected,
                    stroke: chart_2,
                    fill: None,
                },
                ChartSeries {
                    name: i18n_metrics(cx, "series_blocked"),
                    values: blocked,
                    stroke: chart_1,
                    fill: None,
                },
            ],
            true,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_fragmentation_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.2} - {:.2}",
            i18n_metrics(cx, "fragmentation_ratio"),
            self.metrics_chart_data.min_fragmentation,
            self.metrics_chart_data.max_fragmentation
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.fragmentation.clone();
        let max_val = self.metrics_chart_data.max_fragmentation.max(0.01);
        let stroke = cx.theme().chart_2;
        let chart = make_line_chart(
            self.chart_params("metrics-fragmentation", dates, max_val, |v| format!("{:.2}", v)),
            values,
            stroke,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_total_commands_processed_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0} - {:.0} /s",
            i18n_metrics(cx, "total_commands_processed"),
            self.metrics_chart_data.min_total_commands_processed,
            self.metrics_chart_data.max_total_commands_processed
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.total_commands_processed.clone();
        let max_val = self.metrics_chart_data.max_total_commands_processed.max(0.01);
        let stroke = cx.theme().chart_2;
        let chart = make_line_chart(
            self.chart_params("metrics-commands", dates, max_val, |v| format!("{:.0}", v)),
            values,
            stroke,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_net_kbps_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0} - {:.0}",
            i18n_metrics(cx, "net_kbps"),
            self.metrics_chart_data.min_net_kbps,
            self.metrics_chart_data.max_net_kbps
        );
        let dates = self.metrics_chart_data.dates.clone();
        let input = self.metrics_chart_data.input_kbps.clone();
        let output = self.metrics_chart_data.output_kbps.clone();
        let max_val = self.metrics_chart_data.max_net_kbps.max(0.01);
        let chart_1 = cx.theme().chart_1;
        let chart_2 = cx.theme().chart_2;
        let chart = make_series_chart(
            self.chart_params("metrics-net", dates, max_val, |v| format!("{:.0}", v)),
            vec![
                ChartSeries {
                    name: i18n_metrics(cx, "series_in"),
                    values: input,
                    stroke: chart_1,
                    fill: Some(chart_1.opacity(0.4).into()),
                },
                ChartSeries {
                    name: i18n_metrics(cx, "series_out"),
                    values: output,
                    stroke: chart_2,
                    fill: Some(chart_2.opacity(0.4).into()),
                },
            ],
            false,
        );
        self.render_chart_card(cx, label, chart)
    }

    /// Export the samples of the shown range — every persisted one, not the
    /// decimated set the charts draw — as CSV, one row per sample.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let range = self.range;
        if range == MetricsRange::Live {
            let samples = get_metrics_cache().list_metrics(&self.server_id);
            self.finish_export(samples, cx);
            return;
        }
        let server_id = self.server_id.clone();
        cx.spawn(async move |this, cx| {
            let samples = cx
                .background_spawn(async move { load_persisted_metrics(&server_id, range.duration_ms(), usize::MAX) })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_export(samples, cx));
        })
        .detach();
    }

    fn finish_export(&mut self, samples: Vec<RedisMetrics>, cx: &mut Context<Self>) {
        if samples.is_empty() {
            let message = i18n_metrics(cx, "export_empty");
            self.server_state
                .update(cx, |state, cx| state.emit_info_notification(message, cx));
            return;
        }
        let server = get_server(&self.server_id)
            .map(|server| server.name)
            .unwrap_or_else(|_| self.server_id.clone());
        let server: String = server
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let suggested = format!(
            "metrics-{server}-{}.csv",
            self.range.label_key().trim_start_matches("range_")
        );
        let csv = metrics_csv(&samples);
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            &suggested,
            i18n_common(cx, "csv_exported"),
            i18n_common(cx, "csv_export_failed"),
        );
    }

    fn render_key_hit_rate_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0}% - {:.0}%",
            i18n_metrics(cx, "key_hit_rate"),
            self.metrics_chart_data.min_key_hit_rate,
            self.metrics_chart_data.max_key_hit_rate
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.key_hit_rate.clone();
        // A line on a fixed 0–100% axis. As bars from zero, a rate that sat
        // at 100% — the usual one — was a solid block that said nothing.
        let stroke = cx.theme().chart_2;
        let chart = make_bounded_line_chart(
            self.chart_params("metrics-hit-rate", dates, 100.0, |v| format!("{:.0}%", v)),
            values,
            stroke,
        );
        self.render_chart_card(cx, label, chart)
    }

    fn render_evicted_keys_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{}: {:.0} - {:.0}",
            i18n_metrics(cx, "evicted_keys"),
            self.metrics_chart_data.min_evicted_keys,
            self.metrics_chart_data.max_evicted_keys
        );
        let dates = self.metrics_chart_data.dates.clone();
        let values = self.metrics_chart_data.evicted_keys.clone();
        let max_val = self.metrics_chart_data.max_evicted_keys.max(0.01);
        let chart_2 = cx.theme().chart_2;
        let chart = make_series_chart(
            self.chart_params("metrics-evicted", dates, max_val, |v| format!("{:.0}", v)),
            vec![ChartSeries {
                name: i18n_metrics(cx, "evicted_keys"),
                values,
                stroke: chart_2,
                fill: Some(chart_2.opacity(0.4).into()),
            }],
            false,
        );
        self.render_chart_card(cx, label, chart)
    }
}

impl Render for ZedisMetrics {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_width = window.viewport_size().width;
        let columns = if window_width > px(1200.) { 2 } else { 1 };
        // As many x labels as a chart is wide enough for: a fixed ten ran
        // into each other at the default size and overlapped when zoomed.
        let dates = &self.metrics_chart_data.dates;
        let label_chars = dates.iter().map(|date| date.chars().count()).max().unwrap_or(0);
        let chart_width = chart_plot_width(content_area_width(window, cx).as_f32(), columns);
        self.tick_margin = tick_margin_for(dates.len(), label_chars, chart_width, cx.theme().font_size.as_f32());
        if self.latest_metrics.is_none() {
            return ZedisSkeletonLoading::new()
                .text(i18n_common(cx, "loading"))
                .into_any_element();
        }
        let has_chart_data = !self.metrics_chart_data.dates.is_empty();
        let memory_report = self
            .server_state
            .read(cx)
            .command_block(ServerCommand::MemoryDoctor)
            .is_none();
        div()
            .size_full()
            .font_family(get_mono_font_family())
            .p_2()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .grid()
                    .gap_2()
                    .grid_cols(columns)
                    .items_start()
                    .justify_start()
                    .child(
                        h_flex()
                            .items_center()
                            .col_span_full()
                            .justify_between()
                            .px_2()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("metrics-back")
                                            .ghost()
                                            .small()
                                            .icon(IconName::ArrowLeft)
                                            .tooltip(back_to_editor_tooltip(cx))
                                            .on_click(|_, _w, cx| {
                                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                                    store.update(cx, |state, cx| {
                                                        state.go_to_view(ServerView::Editor, cx)
                                                    });
                                                });
                                            }),
                                    )
                                    .child(Label::new(self.title.clone())),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .when(memory_report, |this| {
                                        this.child(
                                            Button::new("metrics-memory-report")
                                                .ghost()
                                                .small()
                                                .icon(CustomIconName::MemoryStick)
                                                .tooltip(i18n_metrics(cx, "memory_report_tooltip"))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    open_server_report_dialog(
                                                        this.server_state.clone(),
                                                        ServerReport::Memory,
                                                        window,
                                                        cx,
                                                    )
                                                })),
                                        )
                                    })
                                    .when(has_file_dialogs(), |this| {
                                        this.child(
                                            Button::new("metrics-export-csv")
                                                .ghost()
                                                .small()
                                                .icon(CustomIconName::Download)
                                                .tooltip(i18n_metrics(cx, "export_tooltip"))
                                                .on_click(cx.listener(|this, _, _window, cx| this.export_csv(cx))),
                                        )
                                    })
                                    .child(
                                        h_flex().gap_1().children(
                                            MetricsRange::ALL.into_iter().filter(|range| range.offered()).map(
                                                |range| {
                                                    let selected = self.range == range;
                                                    let button = Button::new(range.button_id())
                                                        .xsmall()
                                                        .label(i18n_metrics(cx, range.label_key()));
                                                    let button =
                                                        if selected { button.primary() } else { button.ghost() };
                                                    button.on_click(cx.listener(move |this, _, _window, cx| {
                                                        this.set_range(range, cx)
                                                    }))
                                                },
                                            ),
                                        ),
                                    ),
                            ),
                    )
                    .child(self.render_stat_cards(columns, cx))
                    .when(has_chart_data, |this| {
                        this.child(self.render_cpu_usage_chart(cx))
                            .child(self.render_memory_usage_chart(cx))
                            .child(self.render_fragmentation_chart(cx))
                            .child(self.render_latency_chart(cx))
                            .child(self.render_connected_clients_chart(cx))
                            .child(self.render_net_kbps_chart(cx))
                            .child(self.render_total_commands_processed_chart(cx))
                            .child(self.render_key_hit_rate_chart(cx))
                            .child(self.render_evicted_keys_chart(cx))
                    }),
            )
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

/// One row per sample, the raw INFO numbers plus a readable time.
fn metrics_csv(samples: &[RedisMetrics]) -> String {
    let rows: Vec<Vec<String>> = samples
        .iter()
        .map(|m| {
            vec![
                format_timestamp_ms_as(m.timestamp_ms, "%Y-%m-%d %H:%M:%S").to_string(),
                m.timestamp_ms.to_string(),
                m.latency_ms.to_string(),
                m.connected_clients.to_string(),
                m.blocked_clients.to_string(),
                m.rejected_connections.to_string(),
                m.used_memory.to_string(),
                m.used_memory_rss.to_string(),
                m.mem_fragmentation_ratio.to_string(),
                m.total_connections_received.to_string(),
                m.total_commands_processed.to_string(),
                m.instantaneous_ops_per_sec.to_string(),
                m.instantaneous_input_kbps.to_string(),
                m.instantaneous_output_kbps.to_string(),
                m.keyspace_hits.to_string(),
                m.keyspace_misses.to_string(),
                m.expired_keys.to_string(),
                m.evicted_keys.to_string(),
                m.used_cpu_sys.to_string(),
                m.used_cpu_user.to_string(),
            ]
        })
        .collect();
    build_csv(
        &[
            "time",
            "timestamp_ms",
            "latency_ms",
            "connected_clients",
            "blocked_clients",
            "rejected_connections",
            "used_memory",
            "used_memory_rss",
            "mem_fragmentation_ratio",
            "total_connections_received",
            "total_commands_processed",
            "instantaneous_ops_per_sec",
            "instantaneous_input_kbps",
            "instantaneous_output_kbps",
            "keyspace_hits",
            "keyspace_misses",
            "expired_keys",
            "evicted_keys",
            "used_cpu_sys",
            "used_cpu_user",
        ],
        &rows,
    )
}

/// The y axis of the memory chart, in the samples' own unit (MB): their
/// range and a margin either side — a quarter of the range, and never less
/// than 2% of the peak or one unit, so a flat series is a line across the
/// middle rather than one drawn on the frame. Never below zero; `(0, 1)`
/// while there is no sample.
fn memory_axis(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() || min > max {
        return (0.0, 1.0);
    }
    let margin = ((max - min) * 0.25).max(max * 0.02).max(1.0);
    ((min - margin).max(0.0), max + margin)
}

/// The Net In / Out card. A negative rate is not traffic: Dragonfly answers
/// `instantaneous_input_kbps:-1` for a counter it does not keep, and the card
/// read "-1.0 / -1.0 KB/s". Unmeasured is "--", as the fragmentation card
/// beside it says for a ratio the server did not report.
fn net_rate_label(input_kbps: f64, output_kbps: f64) -> String {
    if input_kbps < 0. || output_kbps < 0. {
        return "--".to_string();
    }
    format!("{input_kbps:.1} / {output_kbps:.1} KB/s")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The x labels are as many as the chart has room for, and fewer when
    /// the font is larger — a fixed ten ran into each other.
    #[test]
    fn a_rate_the_server_does_not_measure_is_not_shown_as_minus_one() {
        assert_eq!(net_rate_label(1.25, 30.), "1.2 / 30.0 KB/s");
        assert_eq!(net_rate_label(0., 0.), "0.0 / 0.0 KB/s");
        // Dragonfly's `instantaneous_*_kbps:-1`.
        assert_eq!(net_rate_label(-1., -1.), "--");
    }

    #[test]
    fn x_labels_are_thinned_to_what_the_chart_is_wide_enough_for() {
        // Two columns in a 1280px window with the sidebar open.
        let width = chart_plot_width(1100., 2);
        assert!((440.0..460.0).contains(&width), "{width}");
        // "22:13:20" — eight characters: six labels fit, so 60 samples are
        // labelled every tenth.
        assert_eq!(tick_margin_for(60, 8, width, 14.), 10);
        // Three zoom steps up the labels are wider: fewer of them.
        assert!(tick_margin_for(60, 8, width, 17.) > 10);
        // One column: twice the room, twice the labels.
        assert!(tick_margin_for(60, 8, chart_plot_width(1100., 1), 14.) < 10);
        // Never zero — the chart would label nothing — however few samples
        // or little room there is.
        assert_eq!(tick_margin_for(3, 8, width, 14.), 1);
        assert_eq!(tick_margin_for(0, 0, 0., 14.), 1);
        assert_eq!(tick_margin_for(60, 8, 0., 14.), 60);
    }

    #[test]
    fn the_memory_axis_frames_the_samples_instead_of_starting_at_zero() {
        // Steady: a band around the value, the line in its middle.
        let (low, high) = memory_axis(508.0, 508.0);
        assert!(
            low > 490.0 && low < 508.0 && high > 508.0 && high < 526.0,
            "{low}..{high}"
        );
        // Moving: the range plus a quarter of it either side.
        assert_eq!(memory_axis(100.0, 500.0), (0.0, 600.0));
        assert_eq!(memory_axis(400.0, 500.0), (375.0, 525.0));
        // Small numbers never go below zero; no samples is still an axis.
        assert_eq!(memory_axis(0.5, 0.5).0, 0.0);
        assert_eq!(memory_axis(f64::MAX, f64::MIN), (0.0, 1.0));
    }

    #[test]
    fn the_csv_has_one_row_per_sample_with_the_raw_numbers() {
        let samples = vec![
            RedisMetrics {
                timestamp_ms: 1_700_000_000_000,
                used_memory: 1024,
                mem_fragmentation_ratio: 1.25,
                blocked_clients: 2,
                ..Default::default()
            },
            RedisMetrics {
                timestamp_ms: 1_700_000_060_000,
                used_memory: 2048,
                ..Default::default()
            },
        ];
        let csv = metrics_csv(&samples);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("time,timestamp_ms,latency_ms,connected_clients,blocked_clients"));
        assert!(lines[1].contains(",1700000000000,0,0,2,0,1024,0,1.25,"), "{}", lines[1]);
        assert!(lines[2].contains(",1700000060000,0,0,0,0,2048,0,0,"), "{}", lines[2]);
    }
}

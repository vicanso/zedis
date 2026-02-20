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

use crate::components::SkeletonLoading;
use crate::connection::get_server;
use crate::states::ZedisServerState;
use crate::states::{RedisMetrics, get_metrics_cache};
use chrono::{Local, LocalResult, TimeZone};
use core::f64;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, linear_color_stop, linear_gradient, prelude::*, px};
use gpui_component::chart::{AreaChart, BarChart, LineChart};
use gpui_component::h_flex;
use gpui_component::{ActiveTheme, StyledExt, label::Label, scroll::ScrollableElement, v_flex};
use std::time::Duration;

const TIME_FORMAT: &str = "%H:%M:%S";
const CHART_CARD_HEIGHT: f32 = 300.;
const HEARTBEAT_INTERVAL_SECS: u64 = 2;
const BYTES_TO_MB: f64 = 1_000_000.;

#[derive(Debug, Clone)]
struct MetricsCpu {
    date: SharedString,
    used_cpu_sys_percent: f64,
    used_cpu_user_percent: f64,
}
#[derive(Debug, Clone)]
struct MetricsMemory {
    date: SharedString,
    used_memory: f64,
}

#[derive(Debug, Clone)]
struct MetricsLatency {
    date: SharedString,
    latency_ms: f64,
}

#[derive(Debug, Clone)]
struct MetricsConnectedClients {
    date: SharedString,
    connected_clients: f64,
}

#[derive(Debug, Clone)]
struct MetricsTotalCommandsProcessed {
    date: SharedString,
    total_commands_processed: f64,
}

#[derive(Debug, Clone)]
struct MetricsOutputKbps {
    date: SharedString,
    output_kbps: f64,
}

#[derive(Debug, Clone)]
struct MetricsKeyHitRate {
    date: SharedString,
    key_hit_rate: f64,
}

#[derive(Debug, Clone)]
struct MetricsEvictedKeys {
    date: SharedString,
    evicted_keys: f64,
}

#[derive(Debug, Clone)]
struct MetricsChartData {
    max_cpu_percent: f64,
    min_cpu_percent: f64,
    cpu: Vec<MetricsCpu>,
    max_memory: f64,
    min_memory: f64,
    memory: Vec<MetricsMemory>,
    min_latency_ms: f64,
    max_latency_ms: f64,
    latency: Vec<MetricsLatency>,
    max_connected_clients: f64,
    min_connected_clients: f64,
    connected_clients: Vec<MetricsConnectedClients>,
    max_total_commands_processed: f64,
    min_total_commands_processed: f64,
    total_commands_processed: Vec<MetricsTotalCommandsProcessed>,
    max_output_kbps: f64,
    min_output_kbps: f64,
    output_kbps: Vec<MetricsOutputKbps>,
    max_key_hit_rate: f64,
    min_key_hit_rate: f64,
    key_hit_rate: Vec<MetricsKeyHitRate>,
    max_evicted_keys: f64,
    min_evicted_keys: f64,
    evicted_keys: Vec<MetricsEvictedKeys>,
}

pub struct ZedisMetrics {
    title: SharedString,
    metrics_chart_data: MetricsChartData,
    tick_margin: usize,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn format_timestamp_ms(ts_ms: i64) -> SharedString {
    match Local.timestamp_millis_opt(ts_ms) {
        LocalResult::Single(dt) => dt.format(TIME_FORMAT).to_string().into(),
        _ => "--".into(),
    }
}

fn convert_metrics_to_chart_data(history_metrics: Vec<RedisMetrics>) -> (MetricsChartData, usize) {
    let mut prev_metrics = RedisMetrics::default();
    let n = history_metrics.len();

    let mut cpu_list = Vec::with_capacity(n);
    let mut max_cpu_percent = f64::MIN;
    let mut min_cpu_percent = f64::MAX;

    let mut memory_list = Vec::with_capacity(n);
    let mut max_memory = f64::MIN;
    let mut min_memory = f64::MAX;

    let mut latency_list = Vec::with_capacity(n);
    let mut min_latency_ms = f64::MAX;
    let mut max_latency_ms = f64::MIN;

    let mut connected_clients_list = Vec::with_capacity(n);
    let mut max_connected_clients = f64::MIN;
    let mut min_connected_clients = f64::MAX;

    let mut total_commands_processed_list = Vec::with_capacity(n);
    let mut max_total_commands_processed = f64::MIN;
    let mut min_total_commands_processed = f64::MAX;

    let mut output_kbps_list = Vec::with_capacity(n);
    let mut max_output_kbps = f64::MIN;
    let mut min_output_kbps = f64::MAX;

    let mut key_hit_rate_list = Vec::with_capacity(n);
    let mut max_key_hit_rate = f64::MIN;
    let mut min_key_hit_rate = f64::MAX;

    let mut evicted_keys_list = Vec::with_capacity(n);
    let mut max_evicted_keys = f64::MIN;
    let mut min_evicted_keys = f64::MAX;

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

        let date = format_timestamp_ms(metrics.timestamp_ms);
        let delta_time = (duration_ms as f64) / 1000.;
        let used_cpu_sys_percent = (metrics.used_cpu_sys - prev_metrics.used_cpu_sys) / delta_time * 100.;
        let used_cpu_user_percent = (metrics.used_cpu_user - prev_metrics.used_cpu_user) / delta_time * 100.;

        let cpu_high = used_cpu_sys_percent.max(used_cpu_user_percent);
        let cpu_low = used_cpu_sys_percent.min(used_cpu_user_percent);
        max_cpu_percent = max_cpu_percent.max(cpu_high);
        min_cpu_percent = min_cpu_percent.min(cpu_low);

        cpu_list.push(MetricsCpu {
            date: date.clone(),
            used_cpu_sys_percent,
            used_cpu_user_percent,
        });

        let used_memory = metrics.used_memory as f64 / BYTES_TO_MB;
        max_memory = max_memory.max(used_memory);
        min_memory = min_memory.min(used_memory);
        memory_list.push(MetricsMemory {
            date: date.clone(),
            used_memory,
        });

        let latency_ms = metrics.latency_ms as f64;
        max_latency_ms = max_latency_ms.max(latency_ms);
        min_latency_ms = min_latency_ms.min(latency_ms);
        latency_list.push(MetricsLatency {
            date: date.clone(),
            latency_ms,
        });

        let clients = metrics.connected_clients as f64;
        max_connected_clients = max_connected_clients.max(clients);
        min_connected_clients = min_connected_clients.min(clients);
        connected_clients_list.push(MetricsConnectedClients {
            date: date.clone(),
            connected_clients: clients,
        });

        let processed = (metrics.total_commands_processed - prev_metrics.total_commands_processed) as f64;
        max_total_commands_processed = max_total_commands_processed.max(processed);
        min_total_commands_processed = min_total_commands_processed.min(processed);
        total_commands_processed_list.push(MetricsTotalCommandsProcessed {
            date: date.clone(),
            total_commands_processed: processed,
        });

        let output = metrics.instantaneous_output_kbps;
        max_output_kbps = max_output_kbps.max(output);
        min_output_kbps = min_output_kbps.min(output);
        output_kbps_list.push(MetricsOutputKbps {
            date: date.clone(),
            output_kbps: output,
        });

        let keyspace_hits = metrics.keyspace_hits - prev_metrics.keyspace_hits;
        let keyspace_misses = metrics.keyspace_misses - prev_metrics.keyspace_misses;
        let keyspace_total = keyspace_hits + keyspace_misses;
        let rate = if keyspace_total > 0 {
            keyspace_hits as f64 / keyspace_total as f64 * 100.
        } else {
            100.
        };
        max_key_hit_rate = max_key_hit_rate.max(rate);
        min_key_hit_rate = min_key_hit_rate.min(rate);
        key_hit_rate_list.push(MetricsKeyHitRate {
            date: date.clone(),
            key_hit_rate: rate,
        });

        let evicted_keys = (metrics.evicted_keys - prev_metrics.evicted_keys) as f64;
        max_evicted_keys = max_evicted_keys.max(evicted_keys);
        min_evicted_keys = min_evicted_keys.min(evicted_keys);
        evicted_keys_list.push(MetricsEvictedKeys {
            date: date.clone(),
            evicted_keys,
        });

        prev_metrics = *metrics;
    }

    let mut tick_margin = n / 10;
    if !tick_margin.is_multiple_of(10) {
        tick_margin += 1;
    }

    (
        MetricsChartData {
            cpu: cpu_list,
            max_cpu_percent,
            min_cpu_percent,
            memory: memory_list,
            max_memory,
            min_memory,
            latency: latency_list,
            min_latency_ms,
            max_latency_ms,
            connected_clients: connected_clients_list,
            max_connected_clients,
            min_connected_clients,
            total_commands_processed: total_commands_processed_list,
            max_total_commands_processed,
            min_total_commands_processed,
            output_kbps: output_kbps_list,
            max_output_kbps,
            min_output_kbps,
            key_hit_rate: key_hit_rate_list,
            min_key_hit_rate,
            max_key_hit_rate,
            evicted_keys: evicted_keys_list,
            max_evicted_keys,
            min_evicted_keys,
        },
        tick_margin.max(1),
    )
}

impl ZedisMetrics {
    pub fn new(server_state: Entity<ZedisServerState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id();
        let name = if let Ok(server) = get_server(server_id) {
            server.name
        } else {
            "--".to_string()
        };
        let nodes_description = state.nodes_description();
        let title = format!(
            "{name} - {}({})",
            nodes_description.server_type, nodes_description.master_nodes
        )
        .into();
        let (metrics_chart_data, tick_margin) =
            convert_metrics_to_chart_data(get_metrics_cache().list_metrics(server_id));

        let mut this = Self {
            title,
            metrics_chart_data,
            tick_margin,
            heartbeat_task: None,
            _subscriptions: vec![],
        };
        this.start_heartbeat(server_id.to_string(), cx);
        this
    }
    /// Start the heartbeat task
    fn start_heartbeat(&mut self, server_id: String, cx: &mut Context<Self>) {
        // start task
        self.heartbeat_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(HEARTBEAT_INTERVAL_SECS))
                    .await;
                let metrics_history = get_metrics_cache().list_metrics(&server_id);
                let _ = this.update(cx, |state, cx| {
                    let (metrics_chart_data, tick_margin) = convert_metrics_to_chart_data(metrics_history);
                    state.metrics_chart_data = metrics_chart_data;
                    state.tick_margin = tick_margin;
                    cx.notify();
                });
            }
        }));
    }
    fn render_chart_card<E: IntoElement>(
        &self,
        cx: &mut Context<Self>,
        label: impl Into<SharedString>,
        chart: E,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .h(px(CHART_CARD_HEIGHT))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .p_4()
            .child(div().font_semibold().child(label.into()).mb_2())
            .child(chart)
    }

    fn render_cpu_usage_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "CPU Usage: {:.2}% - {:.2}%",
            self.metrics_chart_data.min_cpu_percent, self.metrics_chart_data.max_cpu_percent
        );
        self.render_chart_card(
            cx,
            label,
            AreaChart::new(self.metrics_chart_data.cpu.clone())
                .x(|d| d.date.clone())
                .y(|d| d.used_cpu_sys_percent)
                .stroke(cx.theme().chart_1)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(cx.theme().chart_1.opacity(0.4), 1.),
                    linear_color_stop(cx.theme().background.opacity(0.3), 0.),
                ))
                .y(|d| d.used_cpu_user_percent)
                .stroke(cx.theme().chart_2)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(cx.theme().chart_2.opacity(0.4), 1.),
                    linear_color_stop(cx.theme().background.opacity(0.3), 0.),
                ))
                .tick_margin(self.tick_margin),
        )
    }

    fn render_memory_usage_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Memory Usage: {:.0}MB - {:.0}MB",
            self.metrics_chart_data.min_memory, self.metrics_chart_data.max_memory
        );
        self.render_chart_card(
            cx,
            label,
            BarChart::new(self.metrics_chart_data.memory.clone())
                .x(|d| d.date.clone())
                .y(|d| d.used_memory)
                .tick_margin(self.tick_margin),
        )
    }

    fn render_latency_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Latency: {:.0}ms - {:.0}ms",
            self.metrics_chart_data.min_latency_ms, self.metrics_chart_data.max_latency_ms
        );
        self.render_chart_card(
            cx,
            label,
            LineChart::new(self.metrics_chart_data.latency.clone())
                .x(|d| d.date.clone())
                .y(|d| d.latency_ms)
                .tick_margin(self.tick_margin),
        )
    }

    fn render_connected_clients_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Connected Clients: {:.0} - {:.0}",
            self.metrics_chart_data.min_connected_clients, self.metrics_chart_data.max_connected_clients
        );
        self.render_chart_card(
            cx,
            label,
            LineChart::new(self.metrics_chart_data.connected_clients.clone())
                .x(|d| d.date.clone())
                .y(|d| d.connected_clients)
                .tick_margin(self.tick_margin)
                .step_after(),
        )
    }
    fn render_total_commands_processed_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Total Commands Processed: {:.0} - {:.0}",
            self.metrics_chart_data.min_total_commands_processed, self.metrics_chart_data.max_total_commands_processed
        );
        self.render_chart_card(
            cx,
            label,
            LineChart::new(self.metrics_chart_data.total_commands_processed.clone())
                .x(|d| d.date.clone())
                .y(|d| d.total_commands_processed)
                .tick_margin(self.tick_margin),
        )
    }
    fn render_output_kbps_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Output KBPS: {:.0} - {:.0}",
            self.metrics_chart_data.min_output_kbps, self.metrics_chart_data.max_output_kbps
        );
        self.render_chart_card(
            cx,
            label,
            AreaChart::new(self.metrics_chart_data.output_kbps.clone())
                .x(|d| d.date.clone())
                .y(|d| d.output_kbps)
                .tick_margin(self.tick_margin),
        )
    }
    fn render_key_hit_rate_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Key Hit Rate: {:.0}% - {:.0}%",
            self.metrics_chart_data.min_key_hit_rate, self.metrics_chart_data.max_key_hit_rate
        );
        self.render_chart_card(
            cx,
            label,
            BarChart::new(self.metrics_chart_data.key_hit_rate.clone())
                .x(|d| d.date.clone())
                .y(|d| d.key_hit_rate)
                .tick_margin(self.tick_margin),
        )
    }
    fn render_evicted_keys_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "Evicted Keys: {:.0} - {:.0}",
            self.metrics_chart_data.min_evicted_keys, self.metrics_chart_data.max_evicted_keys
        );
        self.render_chart_card(
            cx,
            label,
            AreaChart::new(self.metrics_chart_data.evicted_keys.clone())
                .x(|d| d.date.clone())
                .y(|d| d.evicted_keys)
                .tick_margin(self.tick_margin),
        )
    }
}

impl Render for ZedisMetrics {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_width = window.viewport_size().width;
        let columns = if window_width > px(1200.) { 2 } else { 1 };
        if self.metrics_chart_data.cpu.is_empty() {
            return SkeletonLoading::new().into_any_element();
        }
        let time_range = if let Some(first) = self.metrics_chart_data.cpu.first()
            && let Some(last) = self.metrics_chart_data.cpu.last()
        {
            format!("{} - {}", first.date, last.date)
        } else {
            "".to_string()
        };
        div()
            .size_full()
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
                            .col_span_full()
                            .justify_between()
                            .px_2()
                            .child(Label::new(self.title.clone()))
                            .child(Label::new(time_range)),
                    )
                    .child(self.render_cpu_usage_chart(cx))
                    .child(self.render_memory_usage_chart(cx))
                    .child(self.render_latency_chart(cx))
                    .child(self.render_connected_clients_chart(cx))
                    .child(self.render_output_kbps_chart(cx))
                    .child(self.render_total_commands_processed_chart(cx))
                    .child(self.render_key_hit_rate_chart(cx))
                    .child(self.render_evicted_keys_chart(cx)),
            )
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

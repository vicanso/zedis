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

use crate::connection::get_server;
use crate::states::ZedisServerState;
use crate::states::{RedisMetrics, get_metrics_cache};
use chrono::{Local, LocalResult, TimeZone};
use core::f64;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, linear_color_stop, linear_gradient, prelude::*, px};
use gpui_component::chart::{AreaChart, BarChart};
use gpui_component::{ActiveTheme, StyledExt, label::Label, v_flex};
use std::time::Duration;

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
struct MetricsChartData {
    max_cpu_percent: f64,
    min_cpu_percent: f64,
    cpu: Vec<MetricsCpu>,
    max_memory: f64,
    min_memory: f64,
    memory: Vec<MetricsMemory>,
}

pub struct ZedisMetrics {
    title: SharedString,
    metrics_chart_data: MetricsChartData,
    heartbeat_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn convert_metrics_to_chart_data(history_metrics: Vec<RedisMetrics>) -> MetricsChartData {
    let mut prev_metrics = RedisMetrics::default();

    let mut cpu = Vec::with_capacity(history_metrics.len());
    let mut max_cpu_percent = f64::MIN;
    let mut min_cpu_percent = f64::MAX;

    let mut memory = Vec::with_capacity(history_metrics.len());
    let mut max_memory = f64::MIN;
    let mut min_memory = f64::MAX;
    for metrics in history_metrics.iter() {
        let date: SharedString = if let LocalResult::Single(date) = Local.timestamp_millis_opt(metrics.timestamp_ms) {
            date.format("%H:%M:%S").to_string().into()
        } else {
            "--".to_string().into()
        };
        let mut duration_ms = 0;
        if prev_metrics.timestamp_ms != 0 {
            duration_ms = metrics.timestamp_ms - prev_metrics.timestamp_ms;
        }
        if duration_ms <= 0 {
            prev_metrics = *metrics;
            continue;
        }
        let (used_cpu_sys_percent, used_cpu_user_percent) = {
            let delta_time = (duration_ms as f64) / 1000.;
            let used_cpu_sys = metrics.used_cpu_sys - prev_metrics.used_cpu_sys;
            let used_cpu_user = metrics.used_cpu_user - prev_metrics.used_cpu_user;
            (used_cpu_sys / delta_time * 100., used_cpu_user / delta_time * 100.)
        };
        if used_cpu_sys_percent > max_cpu_percent {
            max_cpu_percent = used_cpu_sys_percent;
        }
        if used_cpu_sys_percent < min_cpu_percent {
            min_cpu_percent = used_cpu_sys_percent;
        }
        cpu.push(MetricsCpu {
            date: date.clone(),
            used_cpu_sys_percent,
            used_cpu_user_percent,
        });
        let used_memory = (metrics.used_memory / 1_000_000) as f64;

        if used_memory > max_memory {
            max_memory = used_memory;
        }
        if used_memory < min_memory {
            min_memory = used_memory;
        }
        memory.push(MetricsMemory {
            date: date.clone(),
            used_memory,
        });

        prev_metrics = *metrics;
    }

    MetricsChartData {
        cpu,
        max_cpu_percent,
        min_cpu_percent,
        memory,
        max_memory,
        min_memory,
    }
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
        let metrics_chart_data = convert_metrics_to_chart_data(get_metrics_cache().list_metrics(server_id));

        let mut this = Self {
            title,
            metrics_chart_data,
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
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let metrics_history = get_metrics_cache().list_metrics(&server_id);
                let _ = this.update(cx, |state, cx| {
                    state.metrics_chart_data = convert_metrics_to_chart_data(metrics_history);
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
            .h(px(400.))
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
                .y(|d| d.used_cpu_user_percent)
                .stroke(cx.theme().chart_1)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(cx.theme().chart_1.opacity(0.4), 1.),
                    linear_color_stop(cx.theme().background.opacity(0.3), 0.),
                ))
                .y(|d| d.used_cpu_sys_percent)
                .stroke(cx.theme().chart_2)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(cx.theme().chart_2.opacity(0.4), 1.),
                    linear_color_stop(cx.theme().background.opacity(0.3), 0.),
                ))
                .tick_margin(8),
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
                .tick_margin(8),
        )
    }
}

impl Render for ZedisMetrics {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .m_2()
            .grid()
            .gap_2()
            .grid_cols(2)
            .justify_start()
            .child(Label::new(self.title.clone()).col_span_full())
            .child(self.render_cpu_usage_chart(cx))
            .child(self.render_memory_usage_chart(cx))
    }
}

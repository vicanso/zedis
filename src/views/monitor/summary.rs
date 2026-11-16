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

use super::{COLUMN_KEYS, ZedisMonitor};
use crate::helpers::{build_csv, group_thousands};
use crate::states::ZedisGlobalStore;
use crate::states::{i18n_common, i18n_monitor};
use crate::views::export_to_file;
use gpui::{AnyElement, Context, Div, Hsla, SharedString, Task, Window, div, prelude::*, px};
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, label::Label, tooltip::Tooltip, v_flex};
use std::time::Duration;
use web_time::Instant;
use zedis_core::tally::{MonitorTally, Tally};

/// Rows each list of a sample's summary shows.
const SUMMARY_ROWS: usize = 15;
const SUMMARY_EXPORT_MAX_ROWS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SampleCap {
    Seconds(u64),
    Commands(u64),
}

/// A sample being taken: its counts, its duration or command-count cap, and
/// the timer that ends it (dropped, and so cancelled, when it ends early).
pub(super) struct Sampling {
    pub(super) tally: MonitorTally,
    pub(super) cap: SampleCap,
    pub(super) started: Instant,
    pub(super) _timer: Task<()>,
}

/// What a finished sample counted, and over how long.
pub(super) struct SampleResult {
    pub(super) tally: MonitorTally,
    pub(super) elapsed: Duration,
    /// Ended before its time or command budget — Stop, the rate guard, a dropped link.
    pub(super) cut_short: bool,
}

pub(super) fn format_csv_export(rows: &[Vec<SharedString>]) -> String {
    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|cells| cells.iter().take(COLUMN_KEYS.len()).map(|c| c.to_string()).collect())
        .collect();
    build_csv(&COLUMN_KEYS, &data)
}

pub(super) fn build_summary_csv(sample: &SampleResult) -> String {
    let total = sample.tally.total();
    let headers = ["dimension", "name", "count", "share_pct"];
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut append_dim = |dim: &str, tally: &Tally| {
        for (name, count) in tally.top(SUMMARY_EXPORT_MAX_ROWS) {
            rows.push(vec![dim.to_string(), name, count.to_string(), share_of(count, total)]);
        }
        if tally.other() > 0 {
            rows.push(vec![
                dim.to_string(),
                "(other)".to_string(),
                tally.other().to_string(),
                share_of(tally.other(), total),
            ]);
        }
    };
    append_dim("command_prefix", &sample.tally.command_prefixes);
    append_dim("client_command", &sample.tally.client_commands);
    append_dim("client", &sample.tally.clients);
    append_dim("command", &sample.tally.commands);
    append_dim("prefix", &sample.tally.prefixes);
    build_csv(&headers, &rows)
}

pub(super) fn build_summary_json(sample: &SampleResult) -> String {
    let total = sample.tally.total();
    let elapsed_secs = sample.elapsed.as_secs_f64();
    let rate_per_sec = if elapsed_secs > 0.0 {
        total as f64 / elapsed_secs
    } else {
        0.0
    };
    let dim_to_json = |tally: &Tally| -> serde_json::Value {
        let mut items: Vec<serde_json::Value> = tally
            .top(SUMMARY_EXPORT_MAX_ROWS)
            .into_iter()
            .map(|(name, count)| {
                serde_json::json!({
                    "name": name,
                    "count": count,
                    "share_pct": if total == 0 { 0.0 } else { (count as f64 * 1000.0 / total as f64).round() / 10.0 },
                })
            })
            .collect();
        if tally.other() > 0 {
            let count = tally.other();
            items.push(serde_json::json!({
                "name": "(other)",
                "count": count,
                "share_pct": if total == 0 { 0.0 } else { (count as f64 * 1000.0 / total as f64).round() / 10.0 },
            }));
        }
        serde_json::Value::Array(items)
    };
    let doc = serde_json::json!({
        "total_commands": total,
        "elapsed_secs": (elapsed_secs * 100.0).round() / 100.0,
        "rate_per_sec": (rate_per_sec * 10.0).round() / 10.0,
        "cut_short": sample.cut_short,
        "command_prefixes": dim_to_json(&sample.tally.command_prefixes),
        "client_commands": dim_to_json(&sample.tally.client_commands),
        "clients": dim_to_json(&sample.tally.clients),
        "commands": dim_to_json(&sample.tally.commands),
        "prefixes": dim_to_json(&sample.tally.prefixes),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// A share of the sample, in percent.
fn share_of(count: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    format!("{:.1}%", count as f64 * 100.0 / total as f64)
}

/// One row of a summary list: the count and its share, the name after them.
fn summary_row(count: String, share: String, muted: Hsla) -> Div {
    h_flex()
        .w_full()
        .gap_3()
        .child(div().w(px(72.)).flex_none().text_right().child(count))
        .child(div().w(px(56.)).flex_none().text_right().text_color(muted).child(share))
}

impl ZedisMonitor {
    /// Close a sample in progress into its result, and show the summary.
    pub(super) fn end_sampling(&mut self) {
        let Some(sampling) = self.sampling.take() else {
            return;
        };
        let elapsed = sampling.started.elapsed();
        let cut_short = match sampling.cap {
            // A little short of the time is the timer's own scheduling.
            SampleCap::Seconds(seconds) => elapsed + Duration::from_millis(500) < Duration::from_secs(seconds),
            SampleCap::Commands(max_cmds) => sampling.tally.total() < max_cmds,
        };
        self.sample = Some(SampleResult {
            tally: sampling.tally,
            elapsed,
            cut_short,
        });
        self.show_summary = true;
    }

    pub(super) fn export_csv(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible_rows_snapshot(cx);
        if rows.is_empty() {
            return;
        }
        let csv = format_csv_export(&rows);
        self.save_export(csv.into_bytes(), "monitor.csv", true, cx);
    }

    pub(super) fn export_json(&mut self, cx: &mut Context<Self>) {
        let rows = self.visible_rows_snapshot(cx);
        if rows.is_empty() {
            return;
        }
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|cells| {
                let object: serde_json::Map<String, serde_json::Value> = COLUMN_KEYS
                    .iter()
                    .zip(cells)
                    .map(|(key, cell)| (key.to_string(), serde_json::Value::String(cell.to_string())))
                    .collect();
                serde_json::Value::Object(object)
            })
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Array(values)).unwrap_or_default();
        self.save_export(json.into_bytes(), "monitor.json", false, cx);
    }

    pub(super) fn export_summary_csv(&mut self, cx: &mut Context<Self>) {
        let Some(sample) = &self.sample else {
            return;
        };
        let csv = build_summary_csv(sample);
        self.save_export(csv.into_bytes(), "monitor-summary.csv", true, cx);
    }

    pub(super) fn export_summary_json(&mut self, cx: &mut Context<Self>) {
        let Some(sample) = &self.sample else {
            return;
        };
        let json = build_summary_json(sample);
        self.save_export(json.into_bytes(), "monitor-summary.json", false, cx);
    }

    /// Shared save flow for the exports — mirrors the slow-log panel.
    fn save_export(&mut self, bytes: Vec<u8>, suggested: &'static str, is_csv: bool, cx: &mut Context<Self>) {
        let server_state = self.server_state.clone();
        let (success_key, error_key) = if is_csv {
            ("csv_exported", "csv_export_failed")
        } else {
            ("json_exported", "json_export_failed")
        };
        let success = i18n_common(cx, success_key);
        let error = i18n_common(cx, error_key);
        export_to_file(cx, server_state, bytes, suggested, success, error);
    }

    /// Show the lines a summary row stands for: its name in the keyword
    /// box — a prefix without its `*`, which the filter would take literally.
    fn show_lines_for(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let term = name.strip_suffix('*').unwrap_or(&name).to_string();
        self.keyword_state
            .update(cx, |state, cx| state.set_value(term, window, cx));
        self.handle_filter(cx);
        self.show_summary = false;
        cx.notify();
    }

    /// The finished sample: how many commands over how long, then who sent
    /// them, what they were and which keys they touched, the most first.
    pub(super) fn render_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(sample) = &self.sample else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let total = sample.tally.total();
        let seconds = sample.elapsed.as_secs_f64().max(0.001);
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let header = rust_i18n::t!(
            "monitor.summary_header",
            total = group_thousands(total),
            secs = format!("{seconds:.1}"),
            rate = group_thousands((total as f64 / seconds).round() as u64),
            locale = locale
        )
        .to_string();
        let filter_tooltip = i18n_monitor(cx, "summary_show_lines");
        let others = i18n_monitor(cx, "summary_others");
        let column = |id: &'static str, title: SharedString, tally: &Tally| -> AnyElement {
            let mut rows: Vec<AnyElement> = tally
                .top(SUMMARY_ROWS)
                .into_iter()
                .enumerate()
                .map(|(ix, (name, count))| {
                    let name = SharedString::from(name);
                    let target = name.clone();
                    summary_row(group_thousands(count), share_of(count, total), muted)
                        .child(
                            div()
                                .id((id, ix))
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .hover(|style| style.underline())
                                .tooltip({
                                    let tip = filter_tooltip.clone();
                                    move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show_lines_for(target.clone(), window, cx)
                                }))
                                .child(name),
                        )
                        .into_any_element()
                })
                .collect();
            if tally.other() > 0 {
                rows.push(
                    summary_row(group_thousands(tally.other()), share_of(tally.other(), total), muted)
                        .child(Label::new(others.clone()).text_color(muted))
                        .into_any_element(),
                );
            }
            v_flex()
                .flex_1()
                .min_w(px(260.))
                .gap_1()
                .child(Label::new(title).font_semibold().pb_1())
                .children(rows)
                .into_any_element()
        };
        let lists = h_flex()
            .w_full()
            .items_start()
            .flex_wrap()
            .gap_6()
            .child(column(
                "summary-client",
                i18n_monitor(cx, "summary_clients"),
                &sample.tally.clients,
            ))
            .child(column(
                "summary-command",
                i18n_monitor(cx, "summary_commands"),
                &sample.tally.commands,
            ))
            .child(column(
                "summary-prefix",
                i18n_monitor(cx, "summary_prefixes"),
                &sample.tally.prefixes,
            ))
            .child(column(
                "summary-command-prefix",
                i18n_monitor(cx, "summary_command_prefixes"),
                &sample.tally.command_prefixes,
            ))
            .child(column(
                "summary-client-command",
                i18n_monitor(cx, "summary_client_commands"),
                &sample.tally.client_commands,
            ));
        div()
            .id("monitor-summary")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .text_sm()
                    .child(Label::new(header).font_semibold())
                    .when(sample.cut_short, |this| {
                        this.child(
                            Label::new(i18n_monitor(cx, "summary_cut_short"))
                                .text_xs()
                                .text_color(cx.theme().warning),
                        )
                    })
                    .child(
                        Label::new(i18n_monitor(cx, "summary_no_duration"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .map(|this| {
                        if total == 0 {
                            this.child(Label::new(i18n_monitor(cx, "summary_empty")).text_color(muted))
                        } else {
                            this.child(lists)
                        }
                    }),
            )
            .into_any_element()
    }
}

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

use super::{
    BenchmarkHistoryEntry, CLEAR_BUTTON_ID, EXPORT_BUTTON_ID, RUN_BUTTON_ID, STOP_BUTTON_ID, ZedisBenchmark,
    format_delta_pct,
};
use crate::assets::CustomIconName;
use crate::connection::Capability;
use crate::helpers::{get_mono_font_family, group_thousands, saves_files};
use crate::states::{ServerView, ZedisGlobalStore, back_to_editor_tooltip, i18n_benchmark, i18n_common};
use crate::views::{ChartParams, ValueLabel, make_bar_chart, make_line_chart};
use gpui::{AnyElement, Context, Entity, Hsla, Render, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::Arc;
use zedis_core::benchmark::{
    BenchmarkComparison, BenchmarkPreset, PIPELINE_CHOICES, compare_summaries, format_latency_f64_us, format_latency_us,
};
use zedis_ui::panel_header;

impl ZedisBenchmark {
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let has_runs = !self.runs.is_empty();
        panel_header()
            .justify_between()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("bench-back-btn")
                            .small()
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _window, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    )
                    .child(Icon::new(CustomIconName::Zap).text_color(theme.primary))
                    .child(Label::new(i18n_benchmark(cx, "title")).text_base().font_semibold())
                    .child(
                        Label::new(i18n_benchmark(cx, "subtitle"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(saves_files(), |this| {
                        this.child(
                            Button::new(EXPORT_BUTTON_ID)
                                .small()
                                .outline()
                                .icon(Icon::new(CustomIconName::Download))
                                .label(i18n_benchmark(cx, "export_csv"))
                                .disabled(!has_runs || self.running)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.export_csv(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(CLEAR_BUTTON_ID)
                            .small()
                            .outline()
                            .icon(Icon::new(CustomIconName::Eraser))
                            .label(i18n_benchmark(cx, "clear_history"))
                            .disabled(!has_runs || self.running)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.clear_history(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.server_state.read(cx);
        let can_write = state.can(Capability::CreateKey);
        let single_db = state.databases() == 1;
        let uses_payload = self.preset.uses_payload();

        let mut preset_row = h_flex().gap_1().items_center().flex_wrap();
        preset_row = preset_row.child(
            Label::new(i18n_benchmark(cx, "preset"))
                .text_xs()
                .text_color(theme.muted_foreground)
                .mr_1(),
        );
        for preset in BenchmarkPreset::ALL {
            let active = self.preset == preset;
            preset_row = preset_row.child(
                Button::new(SharedString::from(format!("bench-preset-{}", preset.label())))
                    .xsmall()
                    .outline()
                    .selected(active)
                    .disabled(self.running)
                    .label(preset.label())
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.preset = preset;
                        cx.notify();
                    })),
            );
        }

        let mut pipe_row = h_flex().gap_1().items_center().flex_wrap();
        pipe_row = pipe_row.child(
            Label::new(i18n_benchmark(cx, "pipeline"))
                .text_xs()
                .text_color(theme.muted_foreground)
                .mr_1(),
        );
        for p in PIPELINE_CHOICES {
            let active = self.pipeline == p;
            pipe_row = pipe_row.child(
                Button::new(SharedString::from(format!("bench-pipe-{p}")))
                    .xsmall()
                    .outline()
                    .selected(active)
                    .disabled(self.running)
                    .label(format!("P={p}"))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.pipeline = p;
                        cx.notify();
                    })),
            );
        }

        let action_btn = if self.running {
            Button::new(STOP_BUTTON_ID)
                .small()
                .danger()
                .icon(IconName::Square)
                .label(i18n_benchmark(cx, "stop"))
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.stop_benchmark(cx);
                }))
        } else {
            Button::new(RUN_BUTTON_ID)
                .small()
                .primary()
                .icon(Icon::new(CustomIconName::Zap))
                .label(i18n_benchmark(cx, "run"))
                .disabled(!can_write)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.request_run(window, cx);
                }))
        };

        let knob = |label: SharedString, input: &Entity<InputState>, width: f32, disabled: bool| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Label::new(label).text_xs().text_color(theme.muted_foreground))
                .child(div().w(px(width)).child(Input::new(input).xsmall().disabled(disabled)))
        };

        v_flex()
            .w_full()
            .px_4()
            .py_2p5()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .flex_wrap()
                            .child(preset_row)
                            .child(pipe_row),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(!can_write, |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(Icon::new(CustomIconName::Lock).text_xs().text_color(theme.warning))
                                        .child(
                                            Label::new(i18n_benchmark(cx, "readonly_hint"))
                                                .text_xs()
                                                .text_color(theme.warning),
                                        ),
                                )
                            })
                            .child(action_btn),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_center()
                    .flex_wrap()
                    .child(knob(
                        i18n_benchmark(cx, "target_db"),
                        &self.db_input,
                        64.0,
                        self.running || single_db,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "concurrency"),
                        &self.concurrency_input,
                        72.0,
                        self.running,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "payload_bytes"),
                        &self.payload_input,
                        88.0,
                        self.running || !uses_payload,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "keyspace"),
                        &self.keyspace_input,
                        96.0,
                        self.running,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "duration_secs"),
                        &self.duration_input,
                        68.0,
                        self.running,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "key_prefix"),
                        &self.prefix_input,
                        132.0,
                        self.running,
                    ))
                    .child(
                        Checkbox::new("bench-keep-keys")
                            .label(i18n_benchmark(cx, "keep_keys"))
                            .checked(self.keep_keys)
                            .disabled(self.running)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.keep_keys = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div().id("bench-cleanup-note").child(
                            Label::new(self.cleanup_note(cx))
                                .text_xs()
                                .text_color(theme.muted_foreground),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_live_banner(&self, cx: &Context<Self>) -> AnyElement {
        let Some(progress) = &self.live_progress else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let pct = if progress.duration_ms > 0 {
            ((progress.elapsed_ms as f64 / progress.duration_ms as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let prefix = self.active_prefix.clone().unwrap_or_else(|| "zedis:bench:".into());
        let target = self.active_target_label.clone().unwrap_or_default();

        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_1p5()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(i18n_benchmark(cx, "running_status"))
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.primary),
                            )
                            .child(Label::new(target).text_xs().text_color(theme.foreground))
                            .child(
                                Label::new(format!("{prefix}*"))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .child(
                                Label::new(format!(
                                    "{} ops/s",
                                    group_thousands(progress.current_ops_per_sec.round() as u64)
                                ))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(theme.success),
                            )
                            .child(
                                Label::new(format!("P99: {}", format_latency_us(progress.current_p99_us)))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.foreground),
                            )
                            .child(
                                Label::new(format!(
                                    "{:.1}s / {:.1}s ({pct:.0}%)",
                                    progress.elapsed_ms as f64 / 1000.0,
                                    progress.duration_ms as f64 / 1000.0
                                ))
                                .text_xs()
                                .font_family(mono)
                                .text_color(theme.muted_foreground),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_headline_cards(
        &self,
        entry: &BenchmarkHistoryEntry,
        baseline: Option<&BenchmarkHistoryEntry>,
        comparison: Option<&BenchmarkComparison>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let s = &entry.summary;

        let card = |title: SharedString, value: String, sub: Option<(String, Hsla)>| {
            v_flex()
                .min_w(px(145.))
                .flex_1()
                .p_3()
                .gap_1()
                .rounded(px(6.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(Label::new(title).text_xs().text_color(theme.muted_foreground))
                .child(
                    Label::new(value)
                        .text_base()
                        .font_family(mono.clone())
                        .text_color(theme.foreground),
                )
                .when_some(sub, |this, (text, color)| {
                    this.child(Label::new(text).text_xs().font_family(mono.clone()).text_color(color))
                })
        };

        let throughput_delta = comparison.map(|cmp| {
            let d = cmp.ops_per_sec_delta_pct;
            let color = if d >= 0.0 { theme.success } else { theme.warning };
            (
                format!("{} vs #{}", format_delta_pct(d), baseline.map_or(0, |b| b.id)),
                color,
            )
        });
        let latency_delta = |d: f64| {
            let color = if d <= 0.0 { theme.success } else { theme.warning };
            (
                format!("{} vs #{}", format_delta_pct(d), baseline.map_or(0, |b| b.id)),
                color,
            )
        };

        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(format!("#{} · {}", entry.id, entry.server_label))
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(theme.foreground),
                            )
                            .child(
                                Label::new(entry.config.summary_label())
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                Label::new(format!("{}*", entry.prefix))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .when_some(baseline, |this, base| {
                        this.child(
                            Label::new(format!(
                                "{}: #{} ({} · {})",
                                i18n_benchmark(cx, "comparing_against"),
                                base.id,
                                base.server_label,
                                base.config.summary_label()
                            ))
                            .text_xs()
                            .text_color(theme.primary),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2p5()
                    .flex_wrap()
                    .child(card(
                        i18n_benchmark(cx, "ops_per_sec"),
                        format!("{}/s", group_thousands(s.ops_per_sec.round() as u64)),
                        throughput_delta,
                    ))
                    .child(card(
                        i18n_benchmark(cx, "avg_latency"),
                        format_latency_f64_us(s.avg_us),
                        comparison.map(|c| latency_delta(c.avg_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p50_latency"),
                        format_latency_us(s.p50_us),
                        comparison.map(|c| latency_delta(c.p50_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p95_latency"),
                        format_latency_us(s.p95_us),
                        comparison.map(|c| latency_delta(c.p95_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p99_latency"),
                        format_latency_us(s.p99_us),
                        comparison.map(|c| latency_delta(c.p99_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "min_max_latency"),
                        format!("{} / {}", format_latency_us(s.min_us), format_latency_us(s.max_us)),
                        Some((format!("{:.2}s", s.elapsed_ms as f64 / 1000.0), theme.muted_foreground)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "total_ops"),
                        group_thousands(s.total_ops),
                        Some((
                            format!(
                                "{} {}: {}",
                                group_thousands(s.total_batches),
                                i18n_benchmark(cx, "errors"),
                                s.errors
                            ),
                            if s.errors > 0 {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            },
                        )),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "cleaned_keys"),
                        group_thousands(s.cleaned_keys),
                        Some((
                            i18n_benchmark(
                                cx,
                                if s.cancelled {
                                    "status_cancelled"
                                } else if entry.config.keep_keys {
                                    "status_kept"
                                } else {
                                    "status_cleaned"
                                },
                            )
                            .to_string(),
                            theme.success,
                        )),
                    )),
            )
            .into_any_element()
    }

    fn render_charts(&self, entry: &BenchmarkHistoryEntry, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let s = &entry.summary;

        // 1. Latency distribution bar chart
        let hist_labels: Vec<SharedString> = s.buckets.iter().map(|b| SharedString::from(b.label.clone())).collect();
        let hist_values: Vec<f64> = s.buckets.iter().map(|b| b.count as f64).collect();
        let hist_max = (hist_values.iter().copied().fold(0.0_f64, f64::max) * 1.1).max(1.0);
        let hist_params = ChartParams {
            id: "bench-latency-hist".into(),
            y_min: 0.0,
            dates: Arc::new(hist_labels),
            y_max: hist_max,
            y_label: ValueLabel::decimals(0),
            tick_margin: 1,
        };
        let hist_chart = make_bar_chart(hist_params, Arc::new(hist_values), theme.chart_1);

        let mut bucket_chips = h_flex().gap_2().flex_wrap();
        for b in &s.buckets {
            if b.count > 0 {
                bucket_chips = bucket_chips.child(
                    h_flex()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded(cx.theme().radius)
                        .bg(theme.secondary)
                        .child(Label::new(b.label.clone()).text_xs().text_color(theme.muted_foreground))
                        .child(
                            Label::new(format!("{} ({:.1}%)", group_thousands(b.count), b.pct))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(theme.foreground),
                        ),
                );
            }
        }

        // 2. Throughput timeline line chart
        let tl_dates: Vec<SharedString> = s
            .timeline
            .iter()
            .map(|pt| SharedString::from(format!("{:.1}s", pt.elapsed_ms as f64 / 1000.0)))
            .collect();
        let tl_values: Vec<f64> = s.timeline.iter().map(|pt| pt.ops_per_sec).collect();
        let tl_max = (tl_values.iter().copied().fold(0.01_f64, f64::max) * 1.1).max(1.0);
        let tl_tick_margin = (s.timeline.len() / 4).max(1);
        let tl_params = ChartParams {
            id: "bench-throughput-line".into(),
            y_min: 0.0,
            dates: Arc::new(tl_dates),
            y_max: tl_max,
            y_label: ValueLabel::decimals(0).unit("/s"),
            tick_margin: tl_tick_margin,
        };
        let tl_chart = make_line_chart(tl_params, Arc::new(tl_values), theme.chart_2);

        h_flex()
            .w_full()
            .gap_3()
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(360.))
                    .p_3()
                    .gap_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        Label::new(i18n_benchmark(cx, "latency_histogram"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(div().w_full().h(px(180.)).child(hist_chart))
                    .child(bucket_chips),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(360.))
                    .p_3()
                    .gap_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        Label::new(i18n_benchmark(cx, "throughput_timeline"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(div().w_full().h(px(180.)).child(tl_chart)),
            )
            .into_any_element()
    }

    fn render_history_table(&self, cx: &mut Context<Self>) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted_fg = cx.theme().muted_foreground;
        let secondary_bg = cx.theme().secondary;
        let hover_bg = cx.theme().list_hover;
        let border_col = cx.theme().border;
        let mono = get_mono_font_family();
        let selected_id = self.selected_entry().map(|e| e.id);
        let baseline = self.baseline_id.and_then(|id| self.runs.iter().find(|r| r.id == id));

        let header_cell = |label: SharedString, width: f32| {
            div()
                .w(px(width))
                .flex_none()
                .child(Label::new(label).text_xs().text_color(muted_fg))
        };

        let mut list = v_flex().w_full().gap_1();
        for run in &self.runs {
            let run_id = run.id;
            let is_selected = selected_id == Some(run_id);
            let is_baseline = self.baseline_id == Some(run_id);
            let cmp = baseline
                .filter(|b| b.id != run_id)
                .map(|b| compare_summaries(&b.summary, &run.summary));

            let ops_str = match &cmp {
                Some(c) => format!(
                    "{}/s ({})",
                    group_thousands(run.summary.ops_per_sec.round() as u64),
                    format_delta_pct(c.ops_per_sec_delta_pct)
                ),
                None => format!("{}/s", group_thousands(run.summary.ops_per_sec.round() as u64)),
            };
            let p99_str = match &cmp {
                Some(c) => format!(
                    "{} ({})",
                    format_latency_us(run.summary.p99_us),
                    format_delta_pct(c.p99_us_delta_pct)
                ),
                None => format_latency_us(run.summary.p99_us),
            };
            let btn_label = i18n_benchmark(cx, if is_baseline { "baseline_active" } else { "set_baseline" });

            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("bench-history-row-{run_id}")))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .when(is_selected, |this| this.bg(secondary_bg))
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.select_run(run_id, cx);
                    }))
                    .child(
                        div().w(px(56.)).flex_none().child(
                            Label::new(format!("#{run_id} {}", run.timestamp))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted_fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(160.))
                            .flex_none()
                            .overflow_hidden()
                            .child(Label::new(run.server_label.clone()).text_xs().text_color(fg)),
                    )
                    .child(
                        div().flex_1().min_w(px(180.)).overflow_hidden().child(
                            Label::new(run.config.summary_label())
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .child(Label::new(ops_str).text_xs().font_family(mono.clone()).text_color(fg)),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_f64_us(run.summary.avg_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_us(run.summary.p50_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_us(run.summary.p95_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .child(Label::new(p99_str).text_xs().font_family(mono.clone()).text_color(fg)),
                    )
                    .child(
                        div().w(px(96.)).flex_none().child(
                            Button::new(SharedString::from(format!("bench-base-btn-{run_id}")))
                                .xsmall()
                                .outline()
                                .selected(is_baseline)
                                .label(btn_label)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_baseline(run_id, cx);
                                })),
                        ),
                    ),
            );
        }

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(px(6.))
            .border_1()
            .border_color(border_col)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        Label::new(i18n_benchmark(cx, "history_title"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(
                        Label::new(i18n_benchmark(cx, "history_hint"))
                            .text_xs()
                            .text_color(muted_fg),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(border_col)
                    .child(header_cell("#".into(), 56.0))
                    .child(header_cell(i18n_benchmark(cx, "col_server"), 160.0))
                    .child(
                        div().flex_1().min_w(px(180.)).child(
                            Label::new(i18n_benchmark(cx, "col_config"))
                                .text_xs()
                                .text_color(muted_fg),
                        ),
                    )
                    .child(header_cell(i18n_benchmark(cx, "ops_per_sec"), 140.0))
                    .child(header_cell(i18n_benchmark(cx, "avg_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p50_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p95_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p99_latency"), 130.0))
                    .child(header_cell(i18n_benchmark(cx, "col_baseline"), 96.0)),
            )
            .child(list)
            .into_any_element()
    }
}

impl Render for ZedisBenchmark {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_session();
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let muted_fg = cx.theme().muted_foreground;
        let border_col = cx.theme().border;
        let danger_bg = cx.theme().danger;
        let danger_fg = cx.theme().danger_foreground;

        let selected = self.selected_entry().cloned();
        let baseline = self.baseline_entry().cloned();
        let comparison = selected
            .as_ref()
            .zip(baseline.as_ref())
            .map(|(sel, base)| compare_summaries(&base.summary, &sel.summary));

        let body = if let Some(entry) = selected.as_ref() {
            let cards = self.render_headline_cards(entry, baseline.as_ref(), comparison.as_ref(), cx);
            let charts = self.render_charts(entry, cx);
            let table = self.render_history_table(cx);
            v_flex()
                .w_full()
                .p_4()
                .gap_4()
                .child(cards)
                .child(charts)
                .child(table)
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .p_8()
                .gap_2()
                .child(Icon::new(CustomIconName::Zap).size(px(32.)).text_color(muted_fg))
                .child(
                    Label::new(i18n_benchmark(cx, "empty_title"))
                        .text_sm()
                        .font_semibold()
                        .text_color(fg),
                )
                .child(
                    Label::new(i18n_benchmark(cx, "empty_description"))
                        .text_xs()
                        .text_color(muted_fg),
                )
                .into_any_element()
        };

        let header = self.render_header(cx);
        let controls = self.render_controls(cx);
        let live_banner = self.render_live_banner(cx);

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            .child(controls)
            .child(live_banner)
            .when_some(self.error.clone(), move |this, err| {
                this.child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(danger_bg)
                        .child(Label::new(err).text_xs().text_color(danger_fg)),
                )
            })
            .child(
                div().flex_1().w_full().relative().child(
                    div()
                        .id("benchmark-scroll")
                        .absolute()
                        .inset_0()
                        .size_full()
                        .track_scroll(&self.scroll)
                        .overflow_y_scrollbar()
                        .child(body),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_1p5()
                    .border_t_1()
                    .border_color(border_col)
                    .justify_between()
                    .items_center()
                    .child(
                        Label::new(i18n_common(cx, "back_to_editor"))
                            .text_xs()
                            .text_color(muted_fg),
                    ),
            )
    }
}

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

//! `TS.MRANGE` explorer: many time series at once, selected by label.
//!
//! The key editor's chart answers "what did *this* series do". A series set
//! organised by labels — `host`, `region`, `env` — is built to answer "what
//! did *all of these* do", and that question has no key to open, which is
//! why this is a tool page rather than another editor tab.
//!
//! Overlaying several series only means anything if their points line up, so
//! the query always downsamples: `AGGREGATION` puts every series on the same
//! bucket boundaries, and [`align_series`] then walks the union of those
//! buckets. Without it each line would be drawn against its own x-axis and
//! the picture would be confidently wrong.

use crate::connection::{TS_AGGREGATORS, TsMRange, TsSeries, get_connection_manager, has_positive_matcher, ts_mrange};
use crate::error::Error;
use crate::helpers::{get_mono_font_family, unix_ts_millis};
use crate::states::{ZedisServerState, content_area_width, i18n_common, i18n_timeseries};
use crate::views::{ChartParams, format_timestamp_ms, make_line_canvas};
use gpui::{Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{DataTable, TableState},
    v_flex,
};
use std::sync::Arc;
use zedis_ui::{TextColumn, ZedisTextTable};

type Result<T, E = Error> = std::result::Result<T, E>;

/// How many series get a line. Beyond this the chart is noise, so the rest
/// stay in the table where their numbers are still readable.
const MAX_PLOTTED: usize = 8;
/// Buckets per query. The chart has a few hundred pixels of width; asking
/// for more points than that spends bandwidth to draw the same picture.
const MAX_BUCKETS: u64 = 400;
const CHART_HEIGHT: f32 = 260.0;

/// A shared time axis and one value row per series, aligned onto it.
///
/// `TS.MRANGE … AGGREGATION` already snaps every series to the same bucket
/// boundaries, so the axis is the union of the timestamps that came back. A
/// series with no sample in a bucket carries its previous value forward —
/// which is what a metric with a gap means, and what the line would draw
/// anyway; leading gaps stay absent rather than being invented as zero.
fn align_series(series: &[TsSeries]) -> (Vec<i64>, Vec<Vec<f64>>) {
    let mut axis: Vec<i64> = series
        .iter()
        .flat_map(|s| s.samples.iter().map(|(ts, _)| *ts))
        .collect();
    axis.sort_unstable();
    axis.dedup();
    if axis.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let rows = series
        .iter()
        .map(|s| {
            let mut row = Vec::with_capacity(axis.len());
            let mut cursor = 0usize;
            let mut last = 0.0_f64;
            let mut seen = false;
            for stamp in &axis {
                while cursor < s.samples.len() && s.samples[cursor].0 <= *stamp {
                    last = s.samples[cursor].1;
                    seen = true;
                    cursor += 1;
                }
                // Before this series' first sample there is nothing to carry,
                // so the line starts flat at its own first value instead of
                // dropping to zero and inventing a change.
                row.push(if seen {
                    last
                } else {
                    s.samples.first().map(|(_, v)| *v).unwrap_or(0.0)
                });
            }
            row
        })
        .collect();
    (axis, rows)
}

/// A series' one-line summary for the table.
fn summarize(series: &TsSeries) -> (String, String, String) {
    let values: Vec<f64> = series.samples.iter().map(|(_, v)| *v).collect();
    if values.is_empty() {
        return ("—".to_string(), "—".to_string(), "—".to_string());
    }
    let last = values.last().copied().unwrap_or(0.0);
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (format!("{last:.3}"), format!("{min:.3}"), format!("{max:.3}"))
}

/// Query window, mirroring the single-series chart's range bar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Window_ {
    H1,
    H6,
    D1,
    D7,
}

impl Window_ {
    const ALL: [Window_; 4] = [Window_::H1, Window_::H6, Window_::D1, Window_::D7];

    fn span_ms(self) -> i64 {
        match self {
            Window_::H1 => 3_600_000,
            Window_::H6 => 6 * 3_600_000,
            Window_::D1 => 86_400_000,
            Window_::D7 => 7 * 86_400_000,
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Window_::H1 => "range_1h",
            Window_::H6 => "range_6h",
            Window_::D1 => "range_24h",
            Window_::D7 => "range_7d",
        }
    }
}

pub struct ZedisTimeSeriesExplorer {
    server_state: Entity<ZedisServerState>,
    /// Label matchers, whitespace separated (`env=prod host!=a`).
    filter_input: Entity<InputState>,
    aggregation_input: Entity<InputState>,
    window: Window_,
    series: Vec<TsSeries>,
    table_state: Option<Entity<TableState<ZedisTextTable>>>,
    error: Option<SharedString>,
    loading: bool,
    load_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisTimeSeriesExplorer {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n_timeseries(cx, "explorer_filter_placeholder")));
        let aggregation_input = cx.new(|cx| InputState::new(window, cx).default_value("avg"));
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&filter_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.load(cx);
            }
        }));
        Self {
            server_state,
            filter_input,
            aggregation_input,
            window: Window_::H1,
            series: Vec::new(),
            table_state: None,
            error: None,
            loading: false,
            load_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// The filters as typed, split on whitespace. A matcher with a quoted or
    /// spaced value is out of scope: RedisTimeSeries label values cannot
    /// contain spaces either.
    fn filters(&self, cx: &gpui::App) -> Vec<String> {
        self.filter_input
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let filters = self.filters(cx);
        if !has_positive_matcher(&filters) {
            // The server refuses a query that only excludes; saying so here
            // is a sentence about the form rather than a relayed error.
            self.error = Some(i18n_timeseries(cx, "explorer_needs_match"));
            self.series.clear();
            self.table_state = None;
            cx.notify();
            return;
        }
        let aggregator = self.aggregation_input.read(cx).value().trim().to_lowercase();
        let aggregator = if TS_AGGREGATORS.contains(&aggregator.as_str()) {
            aggregator
        } else {
            "avg".to_string()
        };
        let state = self.server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();
        let span_ms = self.window.span_ms();
        let now = unix_ts_millis();
        // One bucket per plotted point: the chart cannot show more, and every
        // series comes back on the same boundaries so the lines line up.
        let bucket_ms = (span_ms / MAX_BUCKETS as i64).max(1);
        let query = TsMRange {
            from_ms: Some(now - span_ms),
            to_ms: Some(now),
            filters,
            aggregation: Some((aggregator, bucket_ms)),
            count: Some(MAX_BUCKETS),
        };
        self.loading = true;
        self.error = None;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result: Result<Vec<TsSeries>> = async {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                Ok(ts_mrange(&mut conn, &query).await?)
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(series) => {
                        this.series = series;
                        this.error = None;
                        this.table_state = None;
                    }
                    Err(e) => this.error = Some(SharedString::from(e.to_string())),
                }
                cx.notify();
            });
        }));
    }

    fn set_window(&mut self, window: Window_, cx: &mut Context<Self>) {
        if self.window == window {
            return;
        }
        self.window = window;
        if !self.series.is_empty() || self.error.is_some() {
            self.load(cx);
        }
    }

    fn build_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content_width = content_area_width(window, cx).as_f32();
        let numeric_w = 110.0;
        let labels_w = 240.0;
        let key_w = (content_width - labels_w - numeric_w * 4.0 - 40.).max(160.);
        let columns = vec![
            TextColumn::new("key", i18n_timeseries(cx, "explorer_col_key"), key_w).sortable(),
            TextColumn::new("labels", i18n_timeseries(cx, "explorer_col_labels"), labels_w),
            TextColumn::new("samples", i18n_timeseries(cx, "explorer_col_samples"), numeric_w)
                .sortable()
                .numeric(),
            TextColumn::new("last", i18n_timeseries(cx, "explorer_col_last"), numeric_w)
                .sortable()
                .numeric(),
            TextColumn::new("min", i18n_timeseries(cx, "explorer_col_min"), numeric_w)
                .sortable()
                .numeric(),
            TextColumn::new("max", i18n_timeseries(cx, "explorer_col_max"), numeric_w)
                .sortable()
                .numeric(),
        ];
        let rows: Vec<Vec<SharedString>> = self
            .series
            .iter()
            .map(|s| {
                let (last, min, max) = summarize(s);
                let labels = s
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                vec![
                    s.key.clone().into(),
                    labels.into(),
                    s.samples.len().to_string().into(),
                    last.into(),
                    min.into(),
                    max.into(),
                ]
            })
            .collect();
        let mut table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
            .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"));
        table.set_rows(rows);
        self.table_state = Some(cx.new(|cx| TableState::new(table, window, cx)));
    }

    /// Every plotted series on one axis, drawn as overlaid canvases.
    ///
    /// Each `make_line_canvas` paints inside its own bounds, so stacking
    /// them absolutely in a positioned parent overlays the lines; they share
    /// one `y_max` and one date axis, which is the whole reason the query
    /// aggregates.
    fn render_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let plotted: Vec<TsSeries> = self.series.iter().take(MAX_PLOTTED).cloned().collect();
        let (axis, rows) = align_series(&plotted);
        if axis.is_empty() {
            return div().into_any_element();
        }
        let dates: Arc<Vec<SharedString>> = Arc::new(axis.iter().map(|ts| format_timestamp_ms(*ts)).collect());
        let max = rows.iter().flat_map(|row| row.iter().copied()).fold(0.0_f64, f64::max);
        let y_max = if max <= 0.0 { 1.0 } else { max * 1.1 };
        let tick_margin = (dates.len() / 6).max(1);
        let palette = [
            cx.theme().chart_1,
            cx.theme().chart_2,
            cx.theme().chart_3,
            cx.theme().chart_4,
            cx.theme().chart_5,
        ];
        let mut stack = div().relative().w_full().h(px(CHART_HEIGHT));
        for (index, row) in rows.into_iter().enumerate() {
            let params = ChartParams {
                dates: dates.clone(),
                y_max,
                y_format: Box::new(|v: f64| format!("{v:.2}")),
                tick_margin,
                border: cx.theme().border,
                muted_fg: cx.theme().muted_foreground,
            };
            let stroke = palette[index % palette.len()];
            stack = stack.child(div().absolute().inset_0().child(make_line_canvas(
                params,
                Arc::new(row),
                stroke,
                false,
            )));
        }
        stack.into_any_element()
    }

    /// Which colour belongs to which series — an overlay without a legend is
    /// a picture of nothing in particular.
    fn render_legend(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = [
            cx.theme().chart_1,
            cx.theme().chart_2,
            cx.theme().chart_3,
            cx.theme().chart_4,
            cx.theme().chart_5,
        ];
        let mut legend = h_flex().flex_wrap().gap_x_3().gap_y_1().items_center();
        for (index, series) in self.series.iter().take(MAX_PLOTTED).enumerate() {
            legend = legend.child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(10.))
                            .h(px(3.))
                            .rounded_sm()
                            .bg(palette[index % palette.len()]),
                    )
                    .child(Label::new(series.key.clone()).text_xs()),
            );
        }
        legend
    }
}

impl Render for ZedisTimeSeriesExplorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.table_state.is_none() && !self.series.is_empty() {
            self.build_table(window, cx);
        }
        let muted = cx.theme().muted_foreground;

        let mut range_bar = h_flex().gap_1().items_center();
        for option in Window_::ALL {
            let label = i18n_timeseries(cx, option.i18n_key());
            let button = Button::new(("ts-explorer-range", option as usize)).small().label(label);
            let button = if option == self.window {
                button.primary()
            } else {
                button.ghost()
            };
            range_bar = range_bar.child(button.on_click(cx.listener(move |this, _, _, cx| {
                this.set_window(option, cx);
            })));
        }

        let bar = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().flex_1().child(Input::new(&self.filter_input).small().h(px(32.))))
            .child(
                Label::new(i18n_timeseries(cx, "explorer_aggregation"))
                    .text_xs()
                    .text_color(muted),
            )
            .child(Input::new(&self.aggregation_input).small().w(px(90.)).h(px(32.)))
            .child(range_bar)
            .child(
                Button::new("ts-explorer-run")
                    .small()
                    .primary()
                    .loading(self.loading)
                    .disabled(self.loading)
                    .icon(IconName::Search)
                    .label(i18n_timeseries(cx, "explorer_run"))
                    .on_click(cx.listener(|this, _, _window, cx| this.load(cx))),
            );

        let body = if let Some(error) = self.error.clone() {
            div()
                .p_4()
                .child(Label::new(error).text_sm().text_color(cx.theme().danger))
                .into_any_element()
        } else if self.series.is_empty() {
            div()
                .p_4()
                .child(
                    Label::new(i18n_timeseries(cx, "explorer_empty"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .flex_1()
                .gap_3()
                .child(self.render_legend(cx))
                .child(self.render_chart(cx))
                // Beyond the plotted few the table is where the numbers stay
                // readable, so it always lists every match.
                .when(self.series.len() > MAX_PLOTTED, |this| {
                    this.child(
                        Label::new(format!("{} / {}", MAX_PLOTTED, self.series.len()))
                            .text_xs()
                            .text_color(muted),
                    )
                })
                .when_some(self.table_state.clone(), |this, table| {
                    this.child(div().flex_1().w_full().child(DataTable::new(&table).bordered(false)))
                })
                .into_any_element()
        };

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .gap_3()
            .p_3()
            .child(bar)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{align_series, summarize};
    use crate::connection::TsSeries;

    fn series(key: &str, samples: &[(i64, f64)]) -> TsSeries {
        TsSeries {
            key: key.to_string(),
            labels: Vec::new(),
            samples: samples.to_vec(),
        }
    }

    #[test]
    fn the_axis_is_the_union_of_every_series_timestamps() {
        let (axis, rows) = align_series(&[series("a", &[(1000, 1.0), (3000, 3.0)]), series("b", &[(2000, 20.0)])]);
        assert_eq!(axis, vec![1000, 2000, 3000]);
        assert_eq!(rows.len(), 2);
        // A gap carries the previous value forward rather than dropping to
        // zero, which would draw a change that never happened.
        assert_eq!(rows[0], vec![1.0, 1.0, 3.0]);
    }

    #[test]
    fn a_series_that_starts_late_holds_its_first_value_rather_than_zero() {
        let (_, rows) = align_series(&[
            series("early", &[(1000, 1.0), (2000, 2.0)]),
            series("late", &[(2000, 50.0)]),
        ]);
        // Before its own first sample the late series is flat at 50, not 0 —
        // a zero would read as a collapse that never occurred.
        assert_eq!(rows[1], vec![50.0, 50.0]);
    }

    #[test]
    fn nothing_to_plot_yields_no_axis() {
        let (axis, rows) = align_series(&[]);
        assert!(axis.is_empty() && rows.is_empty());
        let (axis, _) = align_series(&[series("empty", &[])]);
        assert!(axis.is_empty());
    }

    #[test]
    fn the_summary_reports_last_min_and_max() {
        let (last, min, max) = summarize(&series("s", &[(1, 5.0), (2, 1.0), (3, 3.0)]));
        assert_eq!((last.as_str(), min.as_str(), max.as_str()), ("3.000", "1.000", "5.000"));
        let (last, min, max) = summarize(&series("s", &[]));
        assert_eq!((last.as_str(), min.as_str(), max.as_str()), ("—", "—", "—"));
    }
}

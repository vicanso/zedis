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

//! The charts the panels draw — metrics, memory analysis, latency, time
//! series — as gpui-kit's chart components, all built from one shape of
//! input ([`ChartParams`]) so the axes, the ticks and the hover tooltip read
//! the same in every panel.

use gpui::{Background, ElementId, Hsla, IntoElement, SharedString, transparent_black};
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use std::{rc::Rc, sync::Arc};

/// What every chart takes: which one it is, the x labels, the y range and
/// how its values read.
pub(crate) struct ChartParams {
    /// Unique among the charts one view draws. Every chart is built in this
    /// file — one construction site for all of them, which is the id a chart
    /// takes by default — and a chart keys its hover state and its path cache
    /// on its id, so two charts sharing one would trade them.
    pub id: ElementId,
    /// One label per sample, in order.
    pub dates: Arc<Vec<SharedString>>,
    /// The bottom of the y axis: 0 for the counts and sizes most charts
    /// draw, the lowest sample for a series that can go negative. A bar chart
    /// takes its range from its bars and ignores this and `y_max`.
    pub y_min: f64,
    pub y_max: f64,
    /// A value as the axis labels and the tooltip show it.
    pub y_format: Box<dyn Fn(f64) -> String>,
    /// Label every `tick_margin`-th sample on the x axis.
    pub tick_margin: usize,
}

/// One series of a chart that draws several (see [`make_series_chart`]).
pub(crate) struct ChartSeries {
    /// Its row in the hover tooltip.
    pub name: SharedString,
    pub values: Arc<Vec<f64>>,
    pub stroke: Hsla,
    /// The area under the line; `None` draws the line alone.
    pub fill: Option<Background>,
}

/// The y axis for a series: from 0, or from below its lowest sample when
/// that is negative (a temperature, a balance) — the axis started at 0 and
/// drew those under it — to a little above its highest.
pub(crate) fn value_range(values: &[f64]) -> (f64, f64) {
    let max = values.iter().copied().fold(0.0_f64, f64::max);
    let min = values.iter().copied().fold(0.0_f64, f64::min);
    let y_max = if max <= 0.0 {
        if min < 0.0 { 0.0 } else { 1.0 }
    } else {
        max * 1.1
    };
    let y_min = if min < 0.0 { min * 1.1 } else { 0.0 };
    (y_min, y_max)
}

/// A sample on a chart's x axis: where it sits, and what its label reads.
///
/// Equal only to itself — the index decides — so a label that repeats
/// (`12:00:01` twice within a second, a day-long window's two `12:00`s) is
/// still two points and two bars. A bar chart places its bars by value, and
/// two equal labels would have drawn one bar over the other.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Sample {
    index: usize,
    label: SharedString,
}

impl From<Sample> for SharedString {
    fn from(sample: Sample) -> Self {
        sample.label
    }
}

/// The x accessor of a chart whose data is its sample indices.
fn sample_at(dates: &Arc<Vec<SharedString>>) -> impl Fn(&usize) -> Sample + 'static {
    let dates = dates.clone();
    move |index| Sample {
        index: *index,
        label: dates.get(*index).cloned().unwrap_or_default(),
    }
}

/// The value accessor of a series at a sample index.
fn value_at(values: Arc<Vec<f64>>) -> impl Fn(&usize) -> f64 + 'static {
    move |index| values.get(*index).copied().unwrap_or_default()
}

/// One series as a line.
pub(crate) fn make_line_chart(params: ChartParams, values: Arc<Vec<f64>>, stroke: Hsla) -> impl IntoElement {
    line_chart(params, values, stroke, false)
}

/// One series as a line whose axis ends exactly at `y_max`: for a value with
/// a hard ceiling — a percentage. A chart keeps a little room above its
/// domain and labels it, which on a 0–100 axis reads "105%".
pub(crate) fn make_bounded_line_chart(params: ChartParams, values: Arc<Vec<f64>>, stroke: Hsla) -> impl IntoElement {
    line_chart(params, values, stroke, true)
}

fn line_chart(params: ChartParams, values: Arc<Vec<f64>>, stroke: Hsla, bounded: bool) -> impl IntoElement {
    let len = params.dates.len().min(values.len());
    let format: Rc<dyn Fn(f64) -> String> = Rc::from(params.y_format);
    let tooltip = format.clone();
    let chart = LineChart::new(0..len)
        .id(params.id)
        .x(sample_at(&params.dates))
        .y(value_at(values))
        .stroke(stroke)
        .tick_margin(params.tick_margin)
        .y_domain(params.y_min, params.y_max)
        .y_axis(true)
        .y_tick_format(move |value| format(value))
        .tooltip_value(move |_, value| tooltip(value).into());
    if bounded { chart.y_padding(0., 0.) } else { chart }
}

/// Several series on one set of axes, each with its own colour and, when it
/// has one, the area under it — the hover tooltip has a row for each.
///
/// Also the multi-line chart: `LineChart` draws one series, and overlaying
/// several would draw the axes and the grid once per series, darker with each
/// one. An area chart's series share one frame, are drawn over each other from
/// the baseline rather than stacked, and a series without a fill is a line
/// (1px, where a `LineChart`'s is 2px).
pub(crate) fn make_series_chart(params: ChartParams, series: Vec<ChartSeries>, step_after: bool) -> impl IntoElement {
    let format: Rc<dyn Fn(f64) -> String> = Rc::from(params.y_format);
    let tooltip = format.clone();
    let mut chart = AreaChart::new(0..params.dates.len())
        .id(params.id)
        .x(sample_at(&params.dates))
        .tick_margin(params.tick_margin)
        .y_domain(params.y_min, params.y_max)
        .y_axis(true)
        .y_tick_format(move |value| format(value))
        .tooltip_value(move |_, _, value| tooltip(value).into());
    for series in series {
        chart = chart
            .y(value_at(series.values))
            .name(series.name)
            .stroke(series.stroke)
            .fill(series.fill.unwrap_or_else(|| transparent_black().into()));
        // A curve per series; with none pushed they all take the default.
        if step_after {
            chart = chart.step_after();
        }
    }
    chart
}

/// The ticks gpui-kit's bar chart draws on its value axis unless told
/// otherwise.
const DEFAULT_VALUE_TICKS: usize = 5;

/// How many ticks the value axis of a bar chart of counts carries.
///
/// A bar chart's axis runs from zero to its tallest bar and its ticks are
/// spaced evenly between the two — five by default, which puts them on whole
/// numbers only when the tallest bar is a multiple of four. Everywhere else
/// they fall between, and a label that rounds them names a place it is not
/// at: with a tallest bar of 5 the ticks are 0, 1.25, 2.5, 3.75 and 5, drawn
/// as "0, 1, 2, 4, 5", so a bar of one key ended four fifths of the way to
/// the line that said "1" (#175). The count here divides the tallest bar
/// into whole steps: one per unit while that is few enough to read, else the
/// first of four, five, three, six or two that goes into it evenly. Where
/// none does (11, 13, 17…) it stays at five and [`tick_label`] says which
/// of them can be named.
fn count_axis_ticks(tallest: f64) -> usize {
    if !(tallest >= 1.0 && tallest.fract() == 0.0) {
        return DEFAULT_VALUE_TICKS;
    }
    let tallest = tallest as u64;
    if tallest <= 8 {
        return tallest as usize + 1;
    }
    [4u64, 5, 3, 6, 2]
        .into_iter()
        .find(|steps| tallest.is_multiple_of(*steps))
        .map_or(DEFAULT_VALUE_TICKS, |steps| steps as usize + 1)
}

/// The tallest bar from which a rounded tick label is as good as an exact
/// one. A tick is at most half a key from a whole number, and on an axis of
/// a hundred that is half a percent of its height — about a pixel.
const ROUNDING_UNSEEN_FROM: f64 = 100.0;

/// The label of a tick on a count axis: its number where it is one, nothing
/// where it falls between two. The question is asked of the axis before the
/// tick — past [`ROUNDING_UNSEEN_FROM`] every tick is labelled, below it only
/// the whole ones — so an axis never names its quarters and skips its middle.
/// "Whole" allows for the chart's own arithmetic, which places ticks in `f32`.
fn tick_label(value: f64, tallest: f64, format: &dyn Fn(f64) -> String) -> String {
    let whole = value.round();
    if tallest >= ROUNDING_UNSEEN_FROM || (value - whole).abs() < 1e-3 {
        format(whole)
    } else {
        String::new()
    }
}

/// One series as bars, one per sample. The value axis runs from zero to the
/// tallest bar, so `y_min` / `y_max` are not read.
///
/// Bars here are counts — keys in a size bucket, keys in a TTL bucket — so
/// where every bar is a whole number the axis is ticked in whole numbers
/// ([`count_axis_ticks`]). A series with fractions keeps the default axis.
pub(crate) fn make_bar_chart(params: ChartParams, values: Arc<Vec<f64>>, fill: Hsla) -> impl IntoElement {
    let len = params.dates.len().min(values.len());
    let format: Rc<dyn Fn(f64) -> String> = Rc::from(params.y_format);
    let tooltip = format.clone();
    let drawn = &values[..len];
    let tallest = drawn.iter().copied().fold(0.0_f64, f64::max);
    let counts = drawn.iter().all(|value| *value >= 0.0 && value.fract() == 0.0);
    let ticks = if counts {
        count_axis_ticks(tallest)
    } else {
        DEFAULT_VALUE_TICKS
    };
    BarChart::new(0..len)
        .id(params.id)
        .band(sample_at(&params.dates))
        .value(value_at(values))
        .fill(move |_, _, _, _| fill)
        .tick_margin(params.tick_margin)
        .value_axis(true)
        .value_tick_count(ticks)
        .value_tick_format(move |value| {
            if counts {
                tick_label(value, tallest, format.as_ref())
            } else {
                format(value)
            }
        })
        .tooltip_value(move |_, value| tooltip(value).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tick of a count axis is a whole number where the tallest bar
    /// can be divided into whole steps, which is where the labels used to
    /// name places they were not at (#175).
    #[test]
    fn a_count_axis_is_ticked_in_whole_numbers() {
        // One tick per unit while that is few: 0 and 1; 0 to 5.
        assert_eq!(count_axis_ticks(1.0), 2);
        assert_eq!(count_axis_ticks(5.0), 6);
        assert_eq!(count_axis_ticks(8.0), 9);
        // Past that, the first even division: 9 in threes, 20 in fives,
        // 22 in elevens.
        assert_eq!(count_axis_ticks(9.0), 4);
        assert_eq!(count_axis_ticks(20.0), 5);
        assert_eq!(count_axis_ticks(22.0), 3);
        for tallest in [1.0, 5.0, 8.0, 9.0, 12.0, 20.0, 22.0, 100.0] {
            let ticks = count_axis_ticks(tallest);
            let spacing: f64 = tallest / (ticks - 1) as f64;
            assert_eq!(spacing.fract(), 0.0, "{tallest} in {ticks} ticks");
        }
        // Nothing divides 11 evenly, and nothing is drawn of a fraction or
        // an empty chart: the default five.
        assert_eq!(count_axis_ticks(11.0), 5);
        assert_eq!(count_axis_ticks(2.5), 5);
        assert_eq!(count_axis_ticks(0.0), 5);
    }

    /// A tick between two whole numbers has no label — a rounded one is the
    /// name of somewhere else — until the axis is long enough that half a
    /// key cannot be seen, and then all of them have one.
    #[test]
    fn a_tick_between_two_counts_is_not_labelled_as_one_of_them() {
        let plain = |value: f64| format!("{value:.0}");
        // Tallest bar 11, five ticks: 0, 2.75, 5.5, 8.25, 11.
        assert_eq!(tick_label(11.0, 11.0, &plain), "11");
        assert_eq!(tick_label(8.25, 11.0, &plain), "");
        assert_eq!(tick_label(5.5, 11.0, &plain), "");
        assert_eq!(tick_label(0.0, 11.0, &plain), "0");
        // The chart's own f32 arithmetic.
        assert_eq!(tick_label(2.999_999_8, 5.0, &plain), "3");
        // One answer per axis: 53 names neither its quarters nor its middle,
        // 101 names both.
        assert_eq!(tick_label(13.25, 53.0, &plain), "");
        assert_eq!(tick_label(26.5, 53.0, &plain), "");
        assert_eq!(tick_label(25.25, 101.0, &plain), "25");
        assert_eq!(tick_label(50.5, 101.0, &plain), "51");
        assert_eq!(tick_label(25_000.75, 100_003.0, &plain), "25001");
    }

    #[test]
    fn a_negative_series_gets_an_axis_below_zero() {
        assert_eq!(value_range(&[1.0, 10.0]), (0.0, 11.0));
        let (low, high) = value_range(&[-5.0, 2.0]);
        assert!(low < -5.0 && high > 2.0, "{low}..{high}");
        let (low, high) = value_range(&[-5.0, -1.0]);
        assert!(low < -5.0 && high == 0.0, "{low}..{high}");
        assert_eq!(value_range(&[]), (0.0, 1.0), "nothing to draw still has an axis");
    }

    #[test]
    fn two_samples_that_read_the_same_are_still_two() {
        let dates = Arc::new(vec![SharedString::from("12:00:01"), SharedString::from("12:00:01")]);
        let at = sample_at(&dates);
        let (first, second) = (at(&0), at(&1));
        assert!(first != second, "equal labels, different samples");
        assert_eq!(SharedString::from(first), SharedString::from(second));
        // Past the labels: an empty one, not a panic.
        assert_eq!(SharedString::from(at(&5)), SharedString::default());
    }
}

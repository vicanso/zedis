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

/// Redis Memory Analysis viewer.
///
/// Samples keys from the database, groups by prefix and displays two tables:
/// 1. Top 20 prefix groups by estimated memory (keys containing the separator)
/// 2. Top 20 single keys by memory / freq / idletime (keys without the separator)
use crate::assets::CustomIconName;
use crate::connection::{HeatMetric, HeatProbe, KeyMemoryUsage, get_connection_manager};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::format_duration;
use crate::states::{Route, ZedisGlobalStore, ZedisServerState, get_metrics_cache, i18n_common, i18n_memory_analysis};
use crate::views::{ChartParams, format_timestamp_ms, make_line_canvas};
use gpui::{ClipboardItem, Edges, Entity, Pixels, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, error};
use zedis_ui::ZedisDivider;

/// Maximum rows kept per table.
const TOP_N: usize = 20;

/// Default table row height (Medium size in gpui-component).
const TABLE_ROW_HEIGHT: f32 = 32.;

/// Section title bar height (py_1p5 padding + text).
const SECTION_TITLE_HEIGHT: f32 = 30.;

const DEFAULT_SCAN_COUNT: u64 = 100;

/// Calculate the pixel height needed for a DataTable with the given row count.
/// Includes 1 header row + data rows.
fn table_height(row_count: usize) -> Pixels {
    px(((row_count + 1) as f32) * TABLE_ROW_HEIGHT)
}

// ─── Row types ───────────────────────────────────────────────────────────────

/// A row in the prefix-group table.
#[derive(Clone, Debug)]
struct PrefixRow {
    /// e.g. "user:*"
    prefix: SharedString,
    /// Estimated key count (sampled × 1/ratio)
    key_count: u64,
    /// Display key count (with "~" prefix)
    display_key_count: SharedString,
    /// Estimated memory in bytes
    memory_bytes: u64,
    /// Human-readable estimated memory (with "~" prefix)
    memory: SharedString,
    /// Comma-separated key types
    types: SharedString,
    /// Display average TTL (with "~" prefix)
    avg_ttl: SharedString,
    /// Average TTL in seconds (-1 means no expiry)
    avg_ttl_secs: f64,
}

/// A row in the single-key table.
#[derive(Clone, Debug)]
struct SingleKeyRow {
    /// Full key name
    key: SharedString,
    /// Actual memory in bytes
    memory_bytes: u64,
    /// Human-readable memory
    memory: SharedString,
    /// Key type
    key_type: SharedString,
    /// TTL display string
    ttl: SharedString,
    /// TTL in seconds for sorting (-1 = no expiry, -2 = not exists)
    ttl_secs: i64,
    /// Heat metric (FREQ counter / IDLETIME seconds / unknown).
    heat: HeatMetric,
    /// Pre-formatted heat cell (e.g. "12 hits", "3m idle", "—").
    heat_display: SharedString,
}

/// Comparable signed key for the heat metric — higher = hotter so the
/// default descending sort puts hot keys at the top regardless of which
/// metric is in play.
fn heat_sort_key(heat: HeatMetric) -> i64 {
    match heat {
        HeatMetric::Freq(v) => v as i64,
        HeatMetric::IdleTime(v) => -(v as i64),
        HeatMetric::None => i64::MIN,
    }
}

fn format_heat(heat: HeatMetric) -> SharedString {
    match heat {
        HeatMetric::None => "—".into(),
        HeatMetric::Freq(v) => format!("{v} hits").into(),
        HeatMetric::IdleTime(secs) => {
            if secs == 0 {
                "0s idle".into()
            } else {
                format!("{} idle", format_duration(Duration::from_secs(secs))).into()
            }
        }
    }
}

/// What to sort the TopN single-key list by. Hot/Cold only meaningful when
/// the heat metric is available; the toggle hides those when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum SortMode {
    #[default]
    Size,
    /// FREQ desc (LFU) or IDLETIME asc (LRU) — most-active first.
    Hot,
    /// Inverse of Hot. Cold keys = eviction candidates.
    Cold,
}

// ─── Column constants ────────────────────────────────────────────────────────

const COL_PREFIX: &str = "prefix";
const COL_KEY_COUNT: &str = "key_count";
const COL_MEMORY: &str = "memory";
const COL_TYPES: &str = "types";
const COL_AVG_TTL: &str = "avg_ttl";
const COL_KEY: &str = "key";
const COL_KEY_TYPE: &str = "key_type";
const COL_TTL: &str = "ttl";
const COL_HEAT: &str = "heat";

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn make_paddings() -> Option<Edges<Pixels>> {
    Some(Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    })
}

fn format_memory(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::FormatSizeOptions::default().decimal_places(2))
}

fn format_ttl(avg_secs: f64) -> String {
    if avg_secs < 0.0 {
        return "Perm".to_string();
    }
    format_duration(Duration::from_secs(avg_secs as u64))
}

fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

fn render_copy_cell(
    row_ix: usize,
    col_ix: usize,
    value: SharedString,
    column: &Column,
    id_prefix: &'static str,
    copied_message: SharedString,
) -> impl IntoElement {
    // This is the only necessary string allocation.
    // It serves as a globally unique Group identifier for the hover state.
    let group_name: SharedString = format!("{id_prefix}-td-{row_ix}-{col_ix}").into();

    h_flex()
        .size_full()
        .when_some(column.paddings, |this, paddings| this.paddings(paddings))
        .group(group_name.clone())
        .overflow_hidden()
        .child(
            Label::new(value.clone())
                .text_align(column.align)
                .text_ellipsis()
                .flex_1()
                // Essential for text_ellipsis to work inside a flex container
                .min_w_0(),
        )
        .child(
            div()
                // Clever trick: Reuse the group_name (SharedString) combined with a usize index.
                // This perfectly matches GPUI's `impl From<(SharedString, usize)> for ElementId`.
                // It requires zero extra heap allocation and guarantees absolute uniqueness!
                .id((group_name.clone(), 0_usize))
                .invisible()
                .group_hover(group_name.clone(), |style| style.visible())
                .flex_none()
                // Stop event propagation to prevent triggering row selection events
                .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                .child(
                    // Reuse the same group_name, but with index 1 to distinguish the Button's ID
                    Button::new((group_name.clone(), 1_usize))
                        .ghost()
                        .icon(IconName::Copy)
                        .on_click(move |_, window, cx: &mut gpui::App| {
                            cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                            window.push_notification(Notification::info(copied_message.clone()), cx);
                        }),
                ),
        )
}

const TYPE_KEY_WIDTH: f32 = 140.;
const MEMORY_KEY_WIDTH: f32 = 200.;
const COUNT_KEY_WIDTH: f32 = 150.;
const TTL_KEY_WIDTH: f32 = 120.;
const HEAT_KEY_WIDTH: f32 = 130.;

// ─── Prefix table delegate ───────────────────────────────────────────────────

struct PrefixTableDelegate {
    rows: Vec<PrefixRow>,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
}

impl PrefixTableDelegate {
    fn new(rows: Vec<PrefixRow>, window: &mut Window, _cx: &mut gpui::App) -> Self {
        let content_width = (window.viewport_size().width - SIDEBAR_WIDTH).as_f32();

        // Use padding offsets to prevent horizontal scrollbars
        let padding_offset = 16.0;
        let scrollbar_offset = 10.0;
        let prefix_w = content_width
            - COUNT_KEY_WIDTH
            - MEMORY_KEY_WIDTH
            - TYPE_KEY_WIDTH
            - TTL_KEY_WIDTH
            - padding_offset
            - scrollbar_offset;

        let column_keys = vec![COL_PREFIX, COL_KEY_COUNT, COL_MEMORY, COL_AVG_TTL, COL_TYPES];
        let widths = [
            prefix_w,
            COUNT_KEY_WIDTH,
            MEMORY_KEY_WIDTH,
            TTL_KEY_WIDTH,
            TYPE_KEY_WIDTH,
        ];

        let columns = column_keys
            .clone()
            .into_iter()
            .zip(widths)
            .map(|(key, w)| {
                let mut c = Column::new(key, SharedString::default()).width(w).sortable();
                c.paddings = make_paddings();
                c
            })
            .collect();

        Self {
            rows,
            columns,
            column_keys,
        }
    }
}

impl TableDelegate for PrefixTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _cx: &gpui::App) -> Column {
        self.columns[ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.as_ref();
        self.rows.sort_by(|a, b| {
            let ord = match key {
                COL_PREFIX => a.prefix.cmp(&b.prefix),
                COL_KEY_COUNT => a.key_count.cmp(&b.key_count),
                COL_MEMORY => a.memory_bytes.cmp(&b.memory_bytes),
                COL_AVG_TTL => a
                    .avg_ttl_secs
                    .partial_cmp(&b.avg_ttl_secs)
                    .unwrap_or(std::cmp::Ordering::Equal),
                COL_TYPES => a.types.cmp(&b.types),
                _ => std::cmp::Ordering::Equal,
            };
            if matches!(sort, ColumnSort::Ascending) {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        div()
            .size_full()
            .when_some(col.paddings, |this, p| this.paddings(p))
            .child(
                Label::new(i18n_memory_analysis(cx, self.column_keys[col_ix]))
                    .text_align(col.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        let value: SharedString = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => r.prefix.clone(),
                1 => r.display_key_count.clone(),
                2 => r.memory.clone(),
                3 => r.avg_ttl.clone(),
                4 => r.types.clone(),
                _ => "--".into(),
            })
            .unwrap_or_else(|| "--".into());

        // Uses our highly optimized render_copy_cell function
        render_copy_cell(
            row_ix,
            col_ix,
            value,
            col,
            "prefix",
            i18n_common(cx, "copied_to_clipboard"),
        )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _: &mut Window, _: &mut gpui::Context<TableState<Self>>) {}
}

// ─── Single-key table delegate ───────────────────────────────────────────────

struct SingleKeyTableDelegate {
    rows: Vec<SingleKeyRow>,
    columns: Vec<Column>,
    column_keys: Vec<&'static str>,
}

impl SingleKeyTableDelegate {
    fn new(rows: Vec<SingleKeyRow>, window: &mut Window, _cx: &mut gpui::App) -> Self {
        let content_width = (window.viewport_size().width - SIDEBAR_WIDTH).as_f32();

        let padding_offset = 16.0;
        let scrollbar_offset = 10.0;
        let key_w = content_width
            - MEMORY_KEY_WIDTH
            - TTL_KEY_WIDTH
            - TYPE_KEY_WIDTH
            - HEAT_KEY_WIDTH
            - padding_offset
            - scrollbar_offset;

        let column_keys = vec![COL_KEY, COL_MEMORY, COL_TTL, COL_KEY_TYPE, COL_HEAT];
        let widths = [key_w, MEMORY_KEY_WIDTH, TTL_KEY_WIDTH, TYPE_KEY_WIDTH, HEAT_KEY_WIDTH];

        let columns = column_keys
            .clone()
            .into_iter()
            .zip(widths)
            .map(|(key, w)| {
                let mut c = Column::new(key, SharedString::default()).width(w).sortable();

                c.paddings = make_paddings();
                c
            })
            .collect();

        Self {
            rows,
            columns,
            column_keys,
        }
    }
}

impl TableDelegate for SingleKeyTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _cx: &gpui::App) -> Column {
        self.columns[ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut gpui::Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.as_ref();
        self.rows.sort_by(|a, b| {
            let ord = match key {
                COL_KEY => a.key.cmp(&b.key),
                COL_MEMORY => a.memory_bytes.cmp(&b.memory_bytes),
                COL_TTL => a.ttl_secs.cmp(&b.ttl_secs),
                COL_KEY_TYPE => a.key_type.cmp(&b.key_type),
                COL_HEAT => heat_sort_key(a.heat).cmp(&heat_sort_key(b.heat)),
                _ => std::cmp::Ordering::Equal,
            };
            if matches!(sort, ColumnSort::Ascending) {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        div()
            .size_full()
            .when_some(col.paddings, |this, p| this.paddings(p))
            .child(
                Label::new(i18n_memory_analysis(cx, self.column_keys[col_ix]))
                    .text_align(col.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        let value: SharedString = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => r.key.clone(),
                1 => r.memory.clone(),
                2 => r.ttl.clone(),
                3 => r.key_type.clone(),
                4 => r.heat_display.clone(),
                _ => "--".into(),
            })
            .unwrap_or_else(|| "--".into());
        render_copy_cell(
            row_ix,
            col_ix,
            value,
            col,
            "singlekey",
            i18n_common(cx, "copied_to_clipboard"),
        )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }
    fn load_more_threshold(&self) -> usize {
        0
    }
    fn load_more(&mut self, _: &mut Window, _: &mut gpui::Context<TableState<Self>>) {}
}

// ─── Accumulator ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct PrefixStats {
    key_count: u64,
    memory_bytes: u64,
    types: std::collections::HashSet<String>,
    /// Sum of TTL values (only keys with TTL > 0).
    ttl_sum: i64,
    /// Count of keys that have a TTL (TTL > 0).
    ttl_count: u64,
    /// Count of keys with no expiry (TTL == -1).
    perm_count: u64,
}

/// Keeps a capped top-N collection sorted by an i64 ranking descending.
/// Generic over both the row type and the ranking metric so we can run
/// parallel pickers for "biggest", "hottest", "coldest" off the same scan.
struct TopN<T> {
    items: Vec<T>,
    limit: usize,
    /// Minimum ranking score in the current list (for fast rejection).
    min_score: i64,
}

impl<T> TopN<T> {
    fn new(limit: usize) -> Self {
        Self {
            items: Vec::with_capacity(limit + 1),
            limit,
            min_score: i64::MIN,
        }
    }

    /// Cheap pre-check before constructing a row. Avoids building keys we
    /// know would be evicted immediately.
    fn should_insert(&self, score: i64) -> bool {
        self.items.len() < self.limit || score > self.min_score
    }

    fn insert(&mut self, item: T, get_score: impl Fn(&T) -> i64) {
        let val = get_score(&item);
        if self.items.len() < self.limit || val > self.min_score {
            let pos = self
                .items
                .binary_search_by_key(&std::cmp::Reverse(val), |b| std::cmp::Reverse(get_score(b)))
                .unwrap_or_else(|e| e);

            if pos < self.limit {
                self.items.insert(pos, item);
                if self.items.len() > self.limit {
                    self.items.truncate(self.limit);
                }
                self.min_score = self.items.last().map(&get_score).unwrap_or(i64::MIN);
            }
        }
    }
}

// ─── Row builders ────────────────────────────────────────────────────────────

fn build_prefix_rows(prefix_map: &HashMap<String, PrefixStats>, ratio: f32, key_separator: &str) -> Vec<PrefixRow> {
    let scale = if ratio > 0.0 { 1.0 / ratio } else { 1.0 };

    //  Determine if we are sampling to prepend the "~" indicator
    let is_sampled = ratio > 0.0 && ratio < 1.0;
    let est_prefix = if is_sampled { "~" } else { "" };

    let mut rows: Vec<PrefixRow> = prefix_map
        .iter()
        .map(|(prefix, stats)| {
            // Raw numeric values for internal logic and sorting
            let est_count = (stats.key_count as f32 * scale) as u64;
            let est_mem = (stats.memory_bytes as f32 * scale) as u64;

            let mut types: Vec<&String> = stats.types.iter().collect();
            types.sort();

            let avg_ttl_secs = if stats.ttl_count > 0 {
                stats.ttl_sum as f64 / stats.ttl_count as f64
            } else {
                -1.0
            };

            PrefixRow {
                prefix: format!("{prefix}{key_separator}*").into(),

                // Keep raw values for TableDelegate's perform_sort
                key_count: est_count,
                memory_bytes: est_mem,
                avg_ttl_secs,

                // Pre-format all display strings here (Zero-Allocation trick)
                // Add the "~" prefix and format with thousands separators
                display_key_count: format!("{est_prefix}{}", format_thousands(est_count)).into(),

                // Add the "~" prefix to the human-readable memory
                memory: format!("{est_prefix}{}", format_memory(est_mem)).into(),

                types: types.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ").into(),
                avg_ttl: format_ttl(avg_ttl_secs).into(),
            }
        })
        .collect();

    // Sort descending by memory usage
    rows.sort_by_key(|b| std::cmp::Reverse(b.memory_bytes));

    // Truncate to keep the UI snappy
    rows.truncate(TOP_N);

    rows
}
/// Three parallel top-N pickers fed by one scan: biggest by memory, hottest
/// (FREQ desc / IDLETIME asc), coldest (inverse of hottest). Hot/Cold are
/// only fed when the heat metric is available; otherwise they stay empty.
struct SingleKeyTopGroups {
    by_size: TopN<SingleKeyRow>,
    hottest: TopN<SingleKeyRow>,
    coldest: TopN<SingleKeyRow>,
}

impl SingleKeyTopGroups {
    fn new(limit: usize) -> Self {
        Self {
            by_size: TopN::new(limit),
            hottest: TopN::new(limit),
            coldest: TopN::new(limit),
        }
    }

    fn rows_for(&self, mode: SortMode) -> Vec<SingleKeyRow> {
        match mode {
            SortMode::Size => self.by_size.items.clone(),
            SortMode::Hot => self.hottest.items.clone(),
            SortMode::Cold => self.coldest.items.clone(),
        }
    }
}

// ─── Analysis status ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Default)]
enum AnalysisStatus {
    #[default]
    Idle,
    Running,
    Finished,
}

// ─── Main component ──────────────────────────────────────────────────────────

pub struct ZedisMemoryAnalysis {
    server_state: Entity<ZedisServerState>,
    prefix_table: Entity<TableState<PrefixTableDelegate>>,
    single_table: Entity<TableState<SingleKeyTableDelegate>>,
    status: AnalysisStatus,
    prefix_count: usize,
    single_count: usize,
    progress: SharedString,
    analysis_task: Option<Task<()>>,
    /// Database key count fetched on load.
    dbsize: Option<u64>,
    /// User-editable sample ratio (0.0–1.0).
    ratio: f32,
    ratio_input_state: Entity<InputState>,
    /// True when ratio changed programmatically and InputState needs sync.
    ratio_dirty: bool,
    /// User-editable scan count per round.
    scan_count: u64,
    scan_count_input_state: Entity<InputState>,
    /// Estimated Redis commands.
    est_commands: u64,
    /// `maxmemory-policy` reported by the server (`allkeys-lfu`, `allkeys-lru`,
    /// `noeviction`, ...). Empty when not detected.
    policy: SharedString,
    /// Heat probe to use for the next analysis run, derived from `policy`.
    heat: HeatProbe,
    /// User-selected ranking mode for the single-key table.
    sort_mode: SortMode,
    /// Cached group of top-N selectors so toggling Size/Hot/Cold doesn't
    /// re-run the scan.
    single_groups: SingleKeyTopGroups,
    _subscriptions: Vec<Subscription>,
}

impl ZedisMemoryAnalysis {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let prefix_table = cx.new(|cx| TableState::new(PrefixTableDelegate::new(Vec::new(), window, cx), window, cx));
        let single_table =
            cx.new(|cx| TableState::new(SingleKeyTableDelegate::new(Vec::new(), window, cx), window, cx));

        let ratio_input_state = cx.new(|cx| InputState::new(window, cx).default_value("1".to_string()));
        let scan_count_input_state =
            cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_SCAN_COUNT.to_string()));

        let dbsize = server_state.read(cx).dbsize();

        // Listen for ratio input blur to update ratio
        subscriptions.push(
            cx.subscribe_in(&ratio_input_state, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    let text = state.read(cx).value();
                    if let Ok(v) = text.parse::<f32>() {
                        let v = v.clamp(0.001, 1.0);
                        this.ratio = v;
                        this.update_est_commands();
                        cx.notify();
                    }
                }
            }),
        );

        // Listen for scan count input blur to update scan_count
        subscriptions.push(
            cx.subscribe_in(&scan_count_input_state, window, |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    let text = state.read(cx).value();
                    if let Ok(v) = text.parse::<u64>() {
                        let v = v.clamp(10, 10000);
                        this.scan_count = v;
                        this.update_est_commands();
                        cx.notify();
                    }
                }
            }),
        );

        let mut this = Self {
            policy: SharedString::default(),
            heat: HeatProbe::None,
            sort_mode: SortMode::Size,
            single_groups: SingleKeyTopGroups::new(TOP_N),
            server_state,
            prefix_table,
            single_table,
            status: AnalysisStatus::Idle,
            prefix_count: 0,
            single_count: 0,
            progress: SharedString::default(),
            analysis_task: None,
            dbsize,
            ratio: 1.0,
            ratio_input_state,
            ratio_dirty: false,
            scan_count: DEFAULT_SCAN_COUNT,
            scan_count_input_state,
            est_commands: 0,
            _subscriptions: subscriptions,
        };
        this.update_est_commands();
        this
    }

    fn update_est_commands(&mut self) {
        if let Some(dbsize) = self.dbsize {
            let scan_count = self.scan_count.max(1);
            let sampled_keys = (dbsize as f32 * self.ratio) as u64;
            // SCAN rounds to iterate all keys (count=scan_count per round)
            let scan_rounds = if dbsize > 0 { dbsize / scan_count + 1 } else { 0 };
            // Each round needs commands (TYPE + MEMORY USAGE + TTL) sent via pipeline
            self.est_commands = (sampled_keys / scan_count) + scan_rounds;
        }
    }
    fn stop_analysis(&mut self, cx: &mut gpui::Context<Self>) {
        self.analysis_task.take();
        self.status = if self.prefix_count > 0 || self.single_count > 0 {
            AnalysisStatus::Finished
        } else {
            AnalysisStatus::Idle
        };
        cx.notify();
    }

    fn start_analysis(&mut self, cx: &mut gpui::Context<Self>) {
        self.status = AnalysisStatus::Running;
        self.progress = "0%".into();
        self.prefix_count = 0;
        self.single_count = 0;
        self.single_groups = SingleKeyTopGroups::new(TOP_N);

        self.prefix_table.update(cx, |s, _| s.delegate_mut().rows.clear());
        self.single_table.update(cx, |s, _| s.delegate_mut().rows.clear());

        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        let prefix_table = self.prefix_table.clone();
        let single_table = self.single_table.clone();
        let key_separator = cx.global::<ZedisGlobalStore>().read(cx).key_separator().to_string();
        let ratio = self.ratio;
        let dbsize = self.dbsize.unwrap_or(0);
        let scan_count = self.scan_count;

        self.analysis_task = Some(cx.spawn(async move |handle, cx| {
            debug!(dbsize, ratio, "Memory analysis: using sample ratio");

            // Detect maxmemory-policy once at the start of the run. If the
            // server changes policy mid-scan we'd see mixed FREQ/IDLETIME
            // results — accept that as a one-frame visual glitch rather than
            // re-fetching every round.
            let (policy, heat) = match cx
                .background_spawn({
                    let server_id = server_id.clone();
                    async move {
                        let client = get_connection_manager().get_client(&server_id, db).await?;
                        let p = client.maxmemory_policy().await?;
                        Ok::<String, Error>(p)
                    }
                })
                .await
            {
                Ok(p) => {
                    let h = HeatProbe::from_policy(&p);
                    (p, h)
                }
                Err(_) => (String::new(), HeatProbe::None),
            };
            let policy_shared: SharedString = policy.clone().into();
            let _ = handle.update(cx, |this, _| {
                this.policy = policy_shared.clone();
                this.heat = heat;
                // Hot/Cold modes only meaningful with a heat probe — fall back
                // to Size if the user previously selected a hot/cold view on
                // a server that doesn't expose either metric.
                if heat == HeatProbe::None && this.sort_mode != SortMode::Size {
                    this.sort_mode = SortMode::Size;
                }
            });

            let mut prefix_map: HashMap<String, PrefixStats> = HashMap::new();
            let mut single_groups: SingleKeyTopGroups = SingleKeyTopGroups::new(TOP_N);
            let mut cursors: Option<Vec<u64>> = None;
            let mut analysis_count: u64 = 0;
            let redis_process_ratio = 0.5;
            let min_sleep = Duration::from_micros(500);
            let max_sleep = Duration::from_millis(20);

            loop {
                let scan_task = cx.background_spawn({
                    let server_id = server_id.clone();
                    let cursors_clone = cursors.clone();
                    async move {
                        let start = Instant::now();
                        let client = get_connection_manager().get_client(&server_id, db).await?;
                        let (count, new_cursors, keys_memory_usage) = client
                            .sample_scan_memory_usage(ratio, scan_count, cursors_clone, heat)
                            .await?;
                        let base_sleep = start.elapsed().mul_f64(redis_process_ratio);
                        let sleep_duration = base_sleep.clamp(min_sleep, max_sleep);
                        smol::Timer::after(sleep_duration).await;
                        Ok::<(u64, Vec<u64>, Vec<KeyMemoryUsage>), Error>((count, new_cursors, keys_memory_usage))
                    }
                });

                let (count, new_cursors, keys_memory_usage) = match scan_task.await {
                    Ok(result) => result,
                    Err(e) => {
                        error!(error = %e, "Failed to sample scan for memory analysis");
                        break;
                    }
                };
                analysis_count += count;

                if keys_memory_usage.is_empty() && new_cursors.iter().all(|c| *c == 0) {
                    break;
                }

                // Classify and accumulate from the already-fetched data
                for item in &keys_memory_usage {
                    let key = &item.key;
                    let memory = item.memory_usage;
                    let ttl = item.ttl;
                    let key_type = &item.key_type;

                    if let Some(pos) = key.find(&key_separator) {
                        let prefix = &key[..pos];
                        let stats = prefix_map.entry(prefix.to_string()).or_default();
                        stats.key_count += 1;
                        stats.memory_bytes += memory;
                        if ttl > 0 {
                            stats.ttl_sum += ttl;
                            stats.ttl_count += 1;
                        } else if ttl == -1 {
                            stats.perm_count += 1;
                        }
                        if !key_type.is_empty() && key_type != "none" {
                            stats.types.insert(key_type.clone());
                        }
                    }

                    let memory_score = memory as i64;
                    let heat_score = heat_sort_key(item.heat);
                    let row_template = || SingleKeyRow {
                        key: key.clone(),
                        memory_bytes: memory,
                        memory: format_memory(memory).into(),
                        key_type: SharedString::from(key_type.clone()),
                        ttl: format_ttl(ttl as f64).into(),
                        ttl_secs: ttl,
                        heat: item.heat,
                        heat_display: format_heat(item.heat),
                    };
                    if single_groups.by_size.should_insert(memory_score) {
                        single_groups.by_size.insert(row_template(), |r| r.memory_bytes as i64);
                    }
                    if heat != HeatProbe::None && item.heat != HeatMetric::None {
                        if single_groups.hottest.should_insert(heat_score) {
                            single_groups.hottest.insert(row_template(), |r| heat_sort_key(r.heat));
                        }
                        // Coldest = inverse score — flip the sign so the same
                        // descending TopN logic gives us the bottom-N hot list.
                        let cold_score = -heat_score;
                        if single_groups.coldest.should_insert(cold_score) {
                            single_groups.coldest.insert(row_template(), |r| -heat_sort_key(r.heat));
                        }
                    }
                }

                // Update progress
                let pct = if analysis_count > 0 && dbsize > 0 {
                    ((analysis_count as f32 / dbsize as f32) * 100.0).min(99.0) as u32
                } else {
                    99
                };
                let progress_text: SharedString = format!("{}%", pct).into();
                let prefix_rows = build_prefix_rows(&prefix_map, ratio, &key_separator);
                let pc = prefix_rows.len();
                let groups_snapshot = SingleKeyTopGroups {
                    by_size: TopN {
                        items: single_groups.by_size.items.clone(),
                        limit: single_groups.by_size.limit,
                        min_score: single_groups.by_size.min_score,
                    },
                    hottest: TopN {
                        items: single_groups.hottest.items.clone(),
                        limit: single_groups.hottest.limit,
                        min_score: single_groups.hottest.min_score,
                    },
                    coldest: TopN {
                        items: single_groups.coldest.items.clone(),
                        limit: single_groups.coldest.limit,
                        min_score: single_groups.coldest.min_score,
                    },
                };
                let _ = handle.update(cx, |this, cx| {
                    this.progress = progress_text;
                    this.prefix_count = pc;
                    this.single_groups = groups_snapshot;
                    let mode = this.sort_mode;
                    let single_rows = this.single_groups.rows_for(mode);
                    this.single_count = single_rows.len();
                    prefix_table.update(cx, |s, _| s.delegate_mut().rows = prefix_rows);
                    single_table.update(cx, |s, _| s.delegate_mut().rows = single_rows);
                    cx.notify();
                });

                if new_cursors.iter().all(|c| *c == 0) {
                    break;
                }

                cursors = Some(new_cursors);
            }

            // Final update
            let prefix_rows = build_prefix_rows(&prefix_map, ratio, &key_separator);
            let pc = prefix_rows.len();
            let final_groups = single_groups;
            let _ = handle.update(cx, |this, cx| {
                this.status = AnalysisStatus::Finished;
                this.progress = "100%".into();
                this.prefix_count = pc;
                this.single_groups = final_groups;
                let mode = this.sort_mode;
                let single_rows = this.single_groups.rows_for(mode);
                this.single_count = single_rows.len();
                prefix_table.update(cx, |s, _| s.delegate_mut().rows = prefix_rows);
                single_table.update(cx, |s, _| s.delegate_mut().rows = single_rows);
                cx.notify();
            });
        }));

        cx.notify();
    }

    fn set_sort_mode(&mut self, mode: SortMode, cx: &mut gpui::Context<Self>) {
        if self.sort_mode == mode {
            return;
        }
        self.sort_mode = mode;
        let rows = self.single_groups.rows_for(mode);
        self.single_count = rows.len();
        self.single_table.update(cx, |s, _| s.delegate_mut().rows = rows);
        cx.notify();
    }

    fn render_toolbar_functions(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let is_running = self.status == AnalysisStatus::Running;
        let is_idle = self.status == AnalysisStatus::Idle;
        let stat_item = |cx: &mut gpui::Context<Self>, key: &'static str, value: SharedString| {
            h_flex()
                .gap_1()
                .child(
                    Label::new(i18n_memory_analysis(cx, key))
                        .text_color(cx.theme().muted_foreground)
                        .text_sm(),
                )
                .child(Label::new(value).text_sm().font_weight(gpui::FontWeight::MEDIUM))
        };

        ZedisDivider::new()
            .gap_4()
            // Read-only data information display
            .child(
                h_flex()
                    .gap_4() // Use moderate spacing inside the data group
                    .items_center()
                    // DB Size
                    .when_some(self.dbsize, |this, dbsize| {
                        this.child(stat_item(cx, "dbsize", format_thousands(dbsize).into()))
                    })
                    // Estimated commands
                    .when(self.est_commands > 0, |this| {
                        this.child(stat_item(
                            cx,
                            "est_commands",
                            format!("~{}", format_thousands(self.est_commands)).into(),
                        ))
                    })
                    // Active maxmemory-policy chip — explains which heat
                    // metric the Heat column is showing.
                    .when(!self.policy.is_empty(), |this| {
                        this.child(stat_item(cx, "policy", self.policy.clone()))
                    })
                    // Progress
                    .when(!is_idle, |this| {
                        this.child(stat_item(cx, "progress", self.progress.clone()))
                    }),
            )
            // Sort-mode toggle for the single-key TopN table.
            .child({
                let heat_available = self.heat != HeatProbe::None;
                let mode = self.sort_mode;
                let make = |id: &'static str, key: &'static str, target: SortMode, enabled: bool| {
                    let active = mode == target;
                    Button::new(id)
                        .small()
                        .when(active, |b| b.primary())
                        .when(!active, |b| b.outline())
                        .disabled(!enabled)
                        .label(i18n_memory_analysis(cx, key))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.set_sort_mode(target, cx);
                        }))
                };
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(i18n_memory_analysis(cx, "rank_by"))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    )
                    .child(make("sort-mode-size", "rank_size", SortMode::Size, true))
                    .child(make("sort-mode-hot", "rank_hot", SortMode::Hot, heat_available))
                    .child(make("sort-mode-cold", "rank_cold", SortMode::Cold, heat_available))
            })
            // ─── User interaction operation area ───
            .child(
                h_flex()
                    .gap_3() // Input box and button are closely related, spacing is smaller
                    .items_center()
                    // Scan Count input
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(i18n_memory_analysis(cx, "scan_count"))
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            )
                            .child(
                                Input::new(&self.scan_count_input_state)
                                    .small()
                                    .w(px(70.))
                                    .disabled(is_running),
                            ),
                    )
                    // Sample Ratio input
                    .when_some(self.dbsize, |this, _| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Label::new(i18n_memory_analysis(cx, "sample_ratio"))
                                        .text_color(cx.theme().muted_foreground)
                                        .text_sm(),
                                )
                                .child(
                                    Input::new(&self.ratio_input_state)
                                        .small()
                                        .w(px(70.))
                                        .disabled(is_running),
                                ),
                        )
                    })
                    // Start / Stop Button
                    .child(if is_running {
                        Button::new("stop-analysis")
                            .danger()
                            .small()
                            .label(i18n_memory_analysis(cx, "stop"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.stop_analysis(cx);
                            }))
                    } else {
                        Button::new("start-analysis")
                            .primary()
                            .small()
                            .disabled(self.dbsize.is_none())
                            .label(i18n_memory_analysis(cx, "start"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.start_analysis(cx);
                            }))
                    }),
            )
    }
    /// Pull the metrics history kept by the status bar heartbeat and
    /// render a fragmentation-ratio line chart. Returns `None` when
    /// there are fewer than two data points — a single sample isn't
    /// a "trend" yet, so don't waste vertical space.
    ///
    /// Color encodes severity using BOTH the ratio and the absolute
    /// wasted bytes (RSS - used). At very small dataset sizes the
    /// ratio is noisy (jemalloc fixed overhead is ~100MB regardless
    /// of `used_memory`) so a 6× ratio on a 20MB DB is normal, not
    /// a fire — we keep it green until the absolute waste is big
    /// enough to be a real cost.
    ///
    /// - `< FRAG_FLOOR_BYTES` waste → always green (noise floor)
    /// - waste ≥ floor AND ratio ≥ 2.0 → red
    /// - waste ≥ floor AND ratio ≥ 1.5 → yellow
    /// - otherwise → green
    fn render_fragmentation_chart(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        // 200MB. Below this much absolute overhead, jemalloc's fixed
        // costs dominate and the ratio carries no signal. Any modern
        // server can absorb a few hundred MB of allocator slack.
        const FRAG_FLOOR_BYTES: i64 = 200 * 1024 * 1024;

        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return None;
        }
        let history = get_metrics_cache().list_metrics(&server_id);
        // Filter out zero ratios — INFO emits 0 before sampling finishes.
        let samples: Vec<(i64, f64, i64)> = history
            .iter()
            .filter(|m| m.mem_fragmentation_ratio > 0.0)
            .map(|m| {
                // fragmentation_bytes = RSS - used; saturating sub
                // because in rare cases RSS can momentarily be
                // smaller than used (RSS lags by one sampling tick).
                let frag_bytes = (m.used_memory_rss as i64).saturating_sub(m.used_memory as i64);
                (m.timestamp_ms, m.mem_fragmentation_ratio, frag_bytes)
            })
            .collect();
        if samples.len() < 2 {
            return None;
        }
        let dates: Vec<SharedString> = samples.iter().map(|(ts, _, _)| format_timestamp_ms(*ts)).collect();
        let values: Vec<f64> = samples.iter().map(|(_, v, _)| *v).collect();
        let latest_ratio = *values.last().unwrap_or(&1.0);
        let latest_frag_bytes = samples.last().map(|(_, _, b)| *b).unwrap_or(0);
        // Pad y_max slightly above the peak so the line doesn't touch
        // the top edge; clamp the floor at 2.0 so a flat-healthy chart
        // still has room for a future spike.
        let raw_max = values.iter().cloned().fold(f64::MIN, f64::max);
        let y_max = (raw_max * 1.1).max(2.0);

        let theme = cx.theme();
        // Severity needs BOTH a bad ratio AND a meaningful absolute
        // waste — see the constant doc above for why.
        let stroke = if latest_frag_bytes < FRAG_FLOOR_BYTES {
            theme.green
        } else if latest_ratio >= 2.0 {
            theme.red
        } else if latest_ratio >= 1.5 {
            theme.yellow
        } else {
            theme.green
        };
        // Format the absolute waste so users can sanity-check the
        // ratio. "6× ratio · 100MB waste" is much less scary than
        // just "6× ratio" alone.
        let waste_str = if latest_frag_bytes > 0 {
            humansize::format_size(
                latest_frag_bytes as u64,
                humansize::FormatSizeOptions::default().decimal_places(0),
            )
        } else {
            "0 B".to_string()
        };
        let label_text = format!(
            "{} · {}: {:.2}× ({} {})",
            i18n_memory_analysis(cx, "fragmentation_chart_title"),
            i18n_memory_analysis(cx, "fragmentation_chart_latest"),
            latest_ratio,
            waste_str,
            i18n_memory_analysis(cx, "fragmentation_chart_waste"),
        );

        // Aim for at most ~5 X-axis labels. Lower than the metrics
        // view's ~10 because this chart sits in a body that's the
        // user's full content width *but* can shrink with the window.
        // On a 600px-wide window with 100+ samples, 8 labels still
        // produced <5px gaps between adjacent HH:MM:SS strings — the
        // first label's right edge overlapped the second's left edge.
        // 5 labels gives comfortable spacing even at narrow widths.
        const TARGET_X_LABELS: usize = 5;
        let tick_margin = samples.len().div_ceil(TARGET_X_LABELS).max(1);
        let params = ChartParams {
            dates,
            y_max,
            y_format: Box::new(|v| format!("{v:.2}")),
            tick_margin,
            border: theme.border,
            muted_fg: theme.muted_foreground,
        };
        let chart = make_line_canvas(params, values, stroke, false);

        Some(
            v_flex()
                // `w_full` is critical — without it the card collapses
                // to the label's natural width (~200px) and the canvas
                // inherits that, jamming HH:MM:SS x-axis labels on top
                // of each other. `flex_none` prevents vertical squeeze
                // when the body has many siblings.
                .w_full()
                .flex_none()
                .h(px(180.0))
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius_lg)
                .p_3()
                .child(div().font_semibold().child(label_text).mb_2())
                .child(chart)
                .into_any_element(),
        )
    }

    fn render_table_section(
        &mut self,
        title_key: &'static str,
        count: usize,
        table_view: impl IntoElement,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .h(px(SECTION_TITLE_HEIGHT))
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new(i18n_memory_analysis(cx, title_key))
                            .text_color(cx.theme().foreground)
                            .text_sm(),
                    )
                    .child(
                        Label::new(format!("(Top {})", count))
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                    ),
            )
            .child(div().w_full().h(table_height(count)).child(table_view))
    }
}

impl gpui::Render for ZedisMemoryAnalysis {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // Sync ratio InputState when changed programmatically
        if self.ratio_dirty {
            self.ratio_dirty = false;
            let ratio_text = format!("{:.4}", self.ratio);
            self.ratio_input_state
                .update(cx, |s, cx| s.set_value(ratio_text, window, cx));
        }

        let is_running = self.status == AnalysisStatus::Running;
        let has_prefix = self.prefix_count > 0;
        let has_single = self.single_count > 0;
        let has_data = has_prefix || has_single;

        v_flex()
            .size_full()
            .overflow_hidden()
            .gap_2()
            // ── Toolbar ──
            .child(
                h_flex()
                    .w_full()
                    .h(px(40.))
                    .px_4()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("memory-analysis-back")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip(i18n_common(cx, "back_to_editor"))
                                    .on_click(|_, _w, cx| {
                                        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                            store.update(cx, |state, cx| state.go_to(Route::Editor, cx));
                                        });
                                    }),
                            )
                            .child(Icon::new(CustomIconName::MemoryStick))
                            .child(Label::new(i18n_memory_analysis(cx, "title")).text_color(cx.theme().foreground)),
                    )
                    .child(self.render_toolbar_functions(cx)),
            )
            // ── Body ──
            .child({
                let mut body = v_flex()
                    .flex_1()
                    .w_full()
                    .p_2()
                    .min_h_0()
                    .gap_2()
                    .id("memory-analysis-body")
                    .overflow_y_scroll();

                // Fragmentation trend chart (pulls from METRICS_CACHE
                // populated by the status_bar heartbeat). Always
                // attempted — even before the user clicks "Analyse",
                // the chart shows the running mem_fragmentation_ratio
                // so it doubles as ambient diagnostic.
                if let Some(chart) = self.render_fragmentation_chart(cx) {
                    body = body.child(chart);
                }

                if !has_data && !is_running {
                    body = body.child(div().size_full().flex().items_center().justify_center().child(
                        Label::new(i18n_memory_analysis(cx, "no_data")).text_color(cx.theme().muted_foreground),
                    ));
                }

                // Apply the closure to render the prefix table
                if has_prefix {
                    let table = DataTable::new(&self.prefix_table)
                        .stripe(true)
                        .bordered(true)
                        .scrollbar_visible(false, false);

                    body = body.child(self.render_table_section(
                        "prefix_table_title",
                        self.prefix_count,
                        table,
                        window,
                        cx,
                    ));
                }

                // Apply the closure to render the single key table
                if has_single {
                    let table = DataTable::new(&self.single_table)
                        .stripe(true)
                        .bordered(true)
                        .scrollbar_visible(false, false);

                    body = body.child(self.render_table_section(
                        "single_table_title",
                        self.single_count,
                        table,
                        window,
                        cx,
                    ));
                }

                body
            })
            .into_any_element()
    }
}

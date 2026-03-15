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
/// 2. Top 20 single keys by memory (keys without the separator)
use crate::assets::CustomIconName;
use crate::connection::{KeyMemoryUsage, get_connection_manager};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::format_duration;
use crate::states::{ZedisGlobalStore, ZedisServerState, i18n_common, i18n_memory_analysis};
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
use std::time::Duration;
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
        let key_w =
            content_width - MEMORY_KEY_WIDTH - TTL_KEY_WIDTH - TYPE_KEY_WIDTH - padding_offset - scrollbar_offset;

        let column_keys = vec![COL_KEY, COL_MEMORY, COL_TTL, COL_KEY_TYPE];
        let widths = [key_w, MEMORY_KEY_WIDTH, TTL_KEY_WIDTH, TYPE_KEY_WIDTH];

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

/// Keeps a capped top-N collection sorted by memory descending.
struct TopN<T> {
    items: Vec<T>,
    limit: usize,
    /// Minimum memory_bytes in the current list (for fast rejection).
    min_memory: u64,
}

impl<T> TopN<T> {
    fn new(limit: usize) -> Self {
        Self {
            items: Vec::with_capacity(limit + 1),
            limit,
            min_memory: 0,
        }
    }

    fn should_insert(&self, memory_bytes: u64) -> bool {
        self.items.len() < self.limit || memory_bytes > self.min_memory
    }

    fn insert(&mut self, item: T, get_mem: impl Fn(&T) -> u64) {
        let val = get_mem(&item);
        if self.items.len() < self.limit || val > self.min_memory {
            let pos = self
                .items
                .binary_search_by_key(&std::cmp::Reverse(val), |b| std::cmp::Reverse(get_mem(b)))
                .unwrap_or_else(|e| e);

            if pos < self.limit {
                self.items.insert(pos, item);
                if self.items.len() > self.limit {
                    self.items.truncate(self.limit);
                }
                self.min_memory = self.items.last().map(&get_mem).unwrap_or(0);
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
    rows.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));

    // Truncate to keep the UI snappy
    rows.truncate(TOP_N);

    rows
}
fn build_single_rows(top: &TopN<SingleKeyRow>) -> Vec<SingleKeyRow> {
    top.items.clone()
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

            let mut prefix_map: HashMap<String, PrefixStats> = HashMap::new();
            let mut single_top: TopN<SingleKeyRow> = TopN::new(TOP_N);
            let mut cursors: Option<Vec<u64>> = None;
            let mut analysis_count: u64 = 0;

            loop {
                let scan_task = cx.background_spawn({
                    let server_id = server_id.clone();
                    let cursors_clone = cursors.clone();
                    async move {
                        let client = get_connection_manager().get_client(&server_id, db).await?;
                        let (count, new_cursors, keys_memory_usage) = client
                            .sample_scan_memory_usage(ratio, scan_count, cursors_clone)
                            .await?;
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
                    if single_top.should_insert(memory) {
                        let row = SingleKeyRow {
                            key: key.clone(),
                            memory_bytes: memory,
                            memory: format_memory(memory).into(),
                            key_type: SharedString::from(key_type.clone()),
                            ttl: format_ttl(ttl as f64).into(),
                            ttl_secs: ttl,
                        };
                        single_top.insert(row, |r| r.memory_bytes);
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
                let single_rows = build_single_rows(&single_top);
                let pc = prefix_rows.len();
                let sc = single_rows.len();
                let _ = handle.update(cx, |this, cx| {
                    this.progress = progress_text;
                    this.prefix_count = pc;
                    this.single_count = sc;
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
            let single_rows = build_single_rows(&single_top);
            let pc = prefix_rows.len();
            let sc = single_rows.len();
            let _ = handle.update(cx, |this, cx| {
                this.status = AnalysisStatus::Finished;
                this.progress = "100%".into();
                this.prefix_count = pc;
                this.single_count = sc;
                prefix_table.update(cx, |s, _| s.delegate_mut().rows = prefix_rows);
                single_table.update(cx, |s, _| s.delegate_mut().rows = single_rows);
                cx.notify();
            });
        }));

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
                    // Progress
                    .when(!is_idle, |this| {
                        this.child(stat_item(cx, "progress", self.progress.clone()))
                    }),
            )
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

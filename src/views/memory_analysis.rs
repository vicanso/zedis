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
use crate::connection::{HeatMetric, HeatProbe, KeyMemoryUsage, get_connection_manager};
use crate::constants::SIDEBAR_WIDTH;
use crate::error::Error;
use crate::helpers::{AiEndpoint, analyze_report, format_duration, get_mono_font_family, group_thousands};
use crate::states::{
    ServerView, ZedisGlobalStore, ZedisServerState, get_metrics_cache, i18n_common, i18n_memory_analysis,
};
use crate::views::{ChartParams, format_timestamp_ms, make_bar_canvas, make_line_canvas};
/// Redis Memory Analysis viewer.
///
/// Samples keys from the database, groups by prefix and displays two tables:
/// 1. Top 20 prefix groups by estimated memory (keys containing the separator)
/// 2. Top 20 single keys by memory / freq / idletime (keys without the separator)
use crate::views::{open_key_in_editor, search_keys_in_tree};
use gpui::{ClipboardItem, Edges, Entity, Pixels, SharedString, Subscription, Task, Window, div, prelude::*, px, rems};
use gpui_component::button::ButtonVariants;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error};
use zedis_ui::{ZedisDivider, help_popover};

mod render;
/// Maximum rows kept per table.
mod rules;
mod stats;
mod tables;

use rules::*;
use stats::*;
use tables::*;

const TOP_N: usize = 20;

/// Default table row height (Medium size in gpui-component).
const TABLE_ROW_HEIGHT: f32 = 32.;

/// Section title bar height (py_1p5 padding + text).
const SECTION_TITLE_HEIGHT: f32 = 30.;

const DEFAULT_SCAN_COUNT: u64 = 100;

/// Default target number of keys to *probe* (`MEMORY USAGE`/`TTL`) per run. On
/// a larger DB the default sample ratio is scaled down so roughly this many
/// keys get probed — the keyspace is still fully traversed, but the expensive
/// per-key probes (and the load/time they cost) stay bounded. Users can still
/// override the ratio up to 100%.
const DEFAULT_SAMPLE_TARGET: u64 = 50_000;

/// Default sampling ratio for a database of `dbsize` keys: 100% up to
/// [`DEFAULT_SAMPLE_TARGET`], then `DEFAULT_SAMPLE_TARGET / dbsize` rounded
/// **up** to one decimal place — keeps the default a clean tenth and errs
/// toward more coverage, so the smallest default is 0.1 (10%).
fn default_sample_ratio(dbsize: Option<u64>) -> f32 {
    match dbsize {
        Some(n) if n > DEFAULT_SAMPLE_TARGET => {
            let raw = DEFAULT_SAMPLE_TARGET as f32 / n as f32;
            ((raw * 10.0).ceil() / 10.0).clamp(0.1, 1.0)
        }
        _ => 1.0,
    }
}

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
    /// Estimated count of keys with no expiry (TTL == -1) under this prefix.
    /// A high value on a cache prefix is a memory-leak red flag.
    perm_count: u64,
    /// Display permanent-key count (with "~" prefix when sampled)
    perm_display: SharedString,
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
const COL_PERM_COUNT: &str = "perm_count";
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

#[derive(Clone, Debug, PartialEq, Default)]
enum AnalysisStatus {
    #[default]
    Idle,
    Running,
    Finished,
    /// A sampling SCAN round errored mid-run; `scan_error` holds the
    /// message. Any rows accumulated before the failure are still shown.
    Error,
}

/// State of the optional "AI analysis" request that sends the current
/// report to a user-configured OpenAI-compatible endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum AiStatus {
    /// No request made yet — the result panel is hidden.
    #[default]
    Idle,
    /// Request in flight.
    Running,
    /// Completed; `ai_output` holds the model's Markdown reply.
    Done,
    /// Failed; `ai_output` holds a human-readable error message.
    Error,
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
    /// Error message when `status == Error` (a sampling SCAN failed
    /// mid-run). `None` otherwise. Mirrors `ai_output` for the AI path.
    scan_error: Option<SharedString>,
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
    /// Sampled TTL distribution. Populated by the existing SCAN loop
    /// (no extra Redis round-trip — `KeyMemoryUsage::ttl` is already
    /// in the pipeline). Reset on each `start_analysis`.
    ttl_histogram: TtlHistogram,
    /// Offline rule-engine findings, recomputed locally each time a scan
    /// finishes (no Redis round-trip, no external AI). Empty while a scan
    /// is running or when the keyspace is healthy.
    recommendations: Vec<Recommendation>,
    /// Status of the optional AI analysis request.
    ai_status: AiStatus,
    /// Model reply (Markdown) when `ai_status == Done`, or the error
    /// message when `ai_status == Error`. `None` while Idle/Running.
    ai_output: Option<SharedString>,
    /// Handle to the in-flight AI request; dropping it cancels the
    /// foreground update (the background HTTP call still finishes but
    /// its result is discarded).
    ai_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisMemoryAnalysis {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let prefix_table = cx.new(|cx| {
            TableState::new(
                PrefixTableDelegate::new(Vec::new(), server_state.clone(), window, cx),
                window,
                cx,
            )
        });
        let single_table = cx.new(|cx| {
            TableState::new(
                SingleKeyTableDelegate::new(Vec::new(), server_state.clone(), window, cx),
                window,
                cx,
            )
        });

        let dbsize = server_state.read(cx).dbsize();
        // Large DBs default to a sampled ratio so we don't `MEMORY USAGE`
        // every key; small DBs stay at 100%. See `default_sample_ratio`.
        let default_ratio = default_sample_ratio(dbsize);
        let ratio_default_text = if default_ratio >= 1.0 {
            "1".to_string()
        } else {
            format!("{default_ratio:.2}")
        };

        let ratio_input_state = cx.new(|cx| InputState::new(window, cx).default_value(ratio_default_text));
        let scan_count_input_state =
            cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_SCAN_COUNT.to_string()));

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
            scan_error: None,
            analysis_task: None,
            dbsize,
            ratio: default_ratio,
            ratio_input_state,
            ratio_dirty: false,
            scan_count: DEFAULT_SCAN_COUNT,
            scan_count_input_state,
            est_commands: 0,
            ttl_histogram: TtlHistogram::default(),
            recommendations: Vec::new(),
            ai_status: AiStatus::Idle,
            ai_output: None,
            ai_task: None,
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

    /// Drop the AI result panel and cancel any in-flight request.
    fn clear_ai_result(&mut self, cx: &mut gpui::Context<Self>) {
        self.ai_task.take();
        self.ai_status = AiStatus::Idle;
        self.ai_output = None;
        cx.notify();
    }

    /// Send the current analysis report to the configured AI endpoint
    /// and render its advice. No-op (with a notification) when the
    /// endpoint is not configured or there is nothing to analyze.
    fn start_ai_analysis(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.ai_status == AiStatus::Running {
            return;
        }

        let store = cx.global::<ZedisGlobalStore>().read(cx);
        if !store.ai_configured() {
            window.push_notification(Notification::warning(i18n_memory_analysis(cx, "ai_not_configured")), cx);
            return;
        }
        let endpoint = AiEndpoint {
            base_url: store.ai_base_url(),
            api_key: store.ai_api_key(),
            model: store.ai_model(),
        };
        // Ask the model to reply in the app's current UI language.
        let locale = store.locale().to_string();

        // Build the Markdown report from the freshly computed rows. Only
        // key names/sizes/TTLs are sent — never key values.
        let prefix_rows = self.prefix_table.read(cx).delegate().rows.clone();
        let single_rows = self.single_table.read(cx).delegate().rows.clone();
        if prefix_rows.is_empty() && single_rows.is_empty() && self.ttl_histogram.total() == 0 {
            window.push_notification(Notification::warning(i18n_memory_analysis(cx, "ai_no_data")), cx);
            return;
        }
        let report = build_markdown_report(
            self.dbsize,
            &self.policy,
            self.ratio,
            &prefix_rows,
            &single_rows,
            &self.ttl_histogram,
        );

        self.ai_status = AiStatus::Running;
        self.ai_output = None;
        cx.notify();

        self.ai_task = Some(cx.spawn(async move |handle, cx| {
            // `analyze_report` is blocking (ureq) — keep it on the
            // background pool so the UI thread stays responsive.
            let result = cx
                .background_spawn(async move { analyze_report(&endpoint, &report, &locale) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(markdown) => {
                        this.ai_status = AiStatus::Done;
                        this.ai_output = Some(markdown.into());
                    }
                    Err(e) => {
                        error!(error = %e, "AI memory analysis failed");
                        this.ai_status = AiStatus::Error;
                        this.ai_output = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn start_analysis(&mut self, cx: &mut gpui::Context<Self>) {
        // A fresh scan invalidates any previous AI advice.
        self.clear_ai_result(cx);
        self.status = AnalysisStatus::Running;
        self.scan_error = None;
        self.progress = "0%".into();
        self.prefix_count = 0;
        self.single_count = 0;
        self.single_groups = SingleKeyTopGroups::new(TOP_N);
        // Reset histogram so a re-run starts from zero. The TTL tab
        // re-renders each round via the snapshot below, so leaving
        // stale data here would briefly show the old bars on top of
        // a partial new run.
        self.ttl_histogram = TtlHistogram::default();
        // Stale advice from a previous run is worse than none — clear it so
        // the panel hides until the fresh scan completes and recomputes.
        self.recommendations.clear();

        self.prefix_table.update(cx, |s, _| s.delegate_mut().rows.clear());
        self.single_table.update(cx, |s, _| s.delegate_mut().rows.clear());

        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let db = server_state.db();
        let prefix_table = self.prefix_table.clone();
        let single_table = self.single_table.clone();
        let key_separator = self.server_state.read(cx).key_separator().to_string();
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
            // Local TTL histogram populated alongside the existing
            // accumulators. Snapshotted each round so the UI updates
            // progressively — same pattern as `single_groups`.
            let mut ttl_histogram: TtlHistogram = TtlHistogram::default();
            let mut cursors: Option<Vec<u64>> = None;
            let mut analysis_count: u64 = 0;
            // Set when a SCAN round errors; carried out of the loop so the
            // final update surfaces the failure instead of a fake "100%".
            let mut scan_error: Option<SharedString> = None;
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
                        scan_error = Some(e.to_string().into());
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

                    // TTL distribution — uses the same `ttl` already
                    // pulled by the SCAN pipeline. Cheap per-item op.
                    ttl_histogram.add(ttl);

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
                        key: key.clone().into(),
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
                let ttl_snapshot = ttl_histogram.clone();
                let _ = handle.update(cx, |this, cx| {
                    this.progress = progress_text;
                    this.prefix_count = pc;
                    this.single_groups = groups_snapshot;
                    this.ttl_histogram = ttl_snapshot;
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
            let final_histogram = ttl_histogram;
            let _ = handle.update(cx, |this, cx| {
                if let Some(err) = scan_error {
                    // Surface the failure instead of a fake "100% / Finished".
                    // `progress` keeps the last percentage the scan reached,
                    // and any rows accumulated before the error still render.
                    this.status = AnalysisStatus::Error;
                    this.scan_error = Some(err);
                    this.recommendations.clear();
                } else {
                    this.status = AnalysisStatus::Finished;
                    this.progress = "100%".into();
                    // Offline rule engine — free + instant, runs on the
                    // just-computed aggregates before they're moved into
                    // `this` below. Big-key detection uses `by_size` (the
                    // global biggest keys), independent of the table's sort.
                    let frag = latest_fragmentation(this.server_state.read(cx).server_id());
                    this.recommendations = build_recommendations(
                        &this.policy,
                        &prefix_rows,
                        &final_groups.by_size.items,
                        &final_histogram,
                        frag,
                    );
                }
                this.prefix_count = pc;
                this.single_groups = final_groups;
                this.ttl_histogram = final_histogram;
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
}

#[cfg(test)]
mod tests {
    use super::{
        PrefixRow, RecoKind, RecoSeverity, SingleKeyRow, TtlHistogram, build_markdown_report, build_recommendations,
        format_memory, format_thousands, md_cell,
    };
    use crate::connection::HeatMetric;

    const MIB: u64 = 1024 * 1024;

    fn single_key(key: &str, key_type: &str, bytes: u64) -> SingleKeyRow {
        SingleKeyRow {
            key: key.to_string().into(),
            memory_bytes: bytes,
            memory: format_memory(bytes).into(),
            key_type: key_type.to_string().into(),
            ttl: "Perm".into(),
            ttl_secs: -1,
            heat: HeatMetric::None,
            heat_display: "—".into(),
        }
    }

    fn prefix_row(prefix: &str, key_count: u64, memory_bytes: u64, types: &str) -> PrefixRow {
        PrefixRow {
            prefix: prefix.to_string().into(),
            key_count,
            display_key_count: format_thousands(key_count).into(),
            memory_bytes,
            memory: format_memory(memory_bytes).into(),
            types: types.to_string().into(),
            avg_ttl: "Perm".into(),
            avg_ttl_secs: -1.0,
            perm_count: 0,
            perm_display: "0".into(),
        }
    }

    #[test]
    fn md_cell_escapes_pipes_and_newlines() {
        assert_eq!(md_cell("a|b\nc"), "a\\|b c");
    }

    #[test]
    fn markdown_report_has_sections_and_overview() {
        let mut ttl = TtlHistogram::default();
        ttl.add(-1);
        ttl.add(30);
        let md = build_markdown_report(Some(1_234), "allkeys-lru", 1.0, &[], &[], &ttl);
        assert!(md.contains("# Redis Memory Analysis Report"));
        assert!(md.contains("Total keys (DBSIZE): 1,234"));
        assert!(md.contains("maxmemory-policy: `allkeys-lru`"));
        assert!(md.contains("## TTL distribution"));
        // No prefix/single rows supplied → those sections are omitted.
        assert!(!md.contains("Top key-prefix groups"));
        assert!(!md.contains("Top single keys"));
    }

    #[test]
    fn ttl_histogram_buckets_boundary_seconds() {
        // Each boundary lives in the strictly-less-than bucket; one
        // second past it tips into the next bucket up. This is the
        // semantic the UI labels imply ("<1m") and easy to get wrong.
        let mut h = TtlHistogram::default();
        h.add(0); // expires "now" — <1m
        h.add(59); // still <1m
        h.add(60); // boundary — <1h, not <1m
        h.add(3_599); // still <1h
        h.add(3_600); // boundary — <1d
        h.add(86_399); // still <1d
        h.add(86_400); // boundary — <7d
        h.add(7 * 86_400 - 1); // still <7d
        h.add(7 * 86_400); // boundary — ≥7d
        h.add(365 * 86_400); // ≥7d
        h.add(-1); // PERSIST
        h.add(-1);

        assert_eq!(h.lt_1m, 2);
        assert_eq!(h.lt_1h, 2);
        assert_eq!(h.lt_1d, 2);
        assert_eq!(h.lt_7d, 2);
        assert_eq!(h.gte_7d, 2);
        assert_eq!(h.no_ttl, 2);
        assert_eq!(h.total(), 12);
    }

    #[test]
    fn unexpected_negative_ttl_defaults_to_imminent() {
        // -2 is filtered upstream so we never see it; any other
        // unexpected negative falls into the <1m bucket rather than
        // crashing — preserves UI on noisy data.
        let mut h = TtlHistogram::default();
        h.add(-99);
        assert_eq!(h.lt_1m, 1);
        assert_eq!(h.total(), 1);
    }

    #[test]
    fn big_keys_are_tiered_and_capped() {
        let biggest = [
            single_key("a", "hash", 60 * MIB),   // ≥ crit
            single_key("b", "list", 6 * MIB),    // ≥ warn
            single_key("c", "set", 5 * MIB + 1), // ≥ warn
            single_key("d", "string", 1024),     // under the bar
        ];
        let recs = build_recommendations("allkeys-lru", &[], &biggest, &TtlHistogram::default(), None);
        let big: Vec<_> = recs
            .iter()
            .filter(|r| matches!(r.kind, RecoKind::BigKey { .. }))
            .collect();
        // Capped at 3 and the sub-threshold "d" is excluded anyway.
        assert_eq!(big.len(), 3);
        // Sorted most-urgent first, so the 60MiB critical leads.
        assert_eq!(big[0].severity, RecoSeverity::Critical);
        assert!(big[1..].iter().all(|r| r.severity == RecoSeverity::Warning));
    }

    #[test]
    fn unevictable_keys_only_under_volatile_policy() {
        let mut ttl = TtlHistogram::default();
        for _ in 0..60 {
            ttl.add(-1); // 60 with no TTL
        }
        for _ in 0..40 {
            ttl.add(30); // 40 with a TTL → 60% unevictable
        }
        let recs = build_recommendations("volatile-lru", &[], &[], &ttl, None);
        assert!(recs.iter().any(|r| {
            matches!(r.kind, RecoKind::UnevictableKeys { no_ttl_pct: 60, .. }) && r.severity == RecoSeverity::Critical
        }));
        // allkeys-* evicts regardless of TTL, so the same shape is fine there.
        let ok = build_recommendations("allkeys-lru", &[], &[], &ttl, None);
        assert!(!ok.iter().any(|r| matches!(r.kind, RecoKind::UnevictableKeys { .. })));
    }

    #[test]
    fn noeviction_policy_is_flagged() {
        let recs = build_recommendations("noeviction", &[], &[], &TtlHistogram::default(), None);
        assert!(
            recs.iter()
                .any(|r| r.kind == RecoKind::NoEvictionPolicy && r.severity == RecoSeverity::Info)
        );
    }

    #[test]
    fn fragmentation_needs_both_ratio_and_absolute_waste() {
        // High ratio but tiny waste → noise, not flagged.
        let small = build_recommendations("allkeys-lru", &[], &[], &TtlHistogram::default(), Some((3.0, 10 * MIB)));
        assert!(
            !small
                .iter()
                .any(|r| matches!(r.kind, RecoKind::HighFragmentation { .. }))
        );
        // High ratio AND meaningful waste → flagged.
        let real = build_recommendations(
            "allkeys-lru",
            &[],
            &[],
            &TtlHistogram::default(),
            Some((1.8, 400 * MIB)),
        );
        assert!(
            real.iter().any(|r| {
                matches!(r.kind, RecoKind::HighFragmentation { .. }) && r.severity == RecoSeverity::Warning
            })
        );
    }

    #[test]
    fn many_small_strings_suggest_a_hash() {
        let p = prefix_row("cache:*", 5_000, 5_000 * 50, "string"); // avg 50B
        let recs = build_recommendations("allkeys-lru", &[p], &[], &TtlHistogram::default(), None);
        assert!(recs.iter().any(|r| matches!(
            &r.kind,
            RecoKind::ManySmallStrings {
                keys: 5_000,
                avg_bytes: 50,
                ..
            }
        )));
        // A mixed-type prefix of the same shape is not a fold candidate.
        let mixed = prefix_row("cache:*", 5_000, 5_000 * 50, "hash, string");
        let none = build_recommendations("allkeys-lru", &[mixed], &[], &TtlHistogram::default(), None);
        assert!(!none.iter().any(|r| matches!(r.kind, RecoKind::ManySmallStrings { .. })));
    }

    #[test]
    fn dominant_prefix_needs_competition() {
        let big = prefix_row("a:*", 100, 9_000, "hash");
        let small = prefix_row("b:*", 100, 1_000, "hash");
        let recs = build_recommendations(
            "allkeys-lru",
            &[big.clone(), small],
            &[],
            &TtlHistogram::default(),
            None,
        );
        assert!(
            recs.iter()
                .any(|r| matches!(&r.kind, RecoKind::DominantPrefix { pct: 90, .. }))
        );
        // A lone prefix can't "dominate" — nothing to compare against.
        let solo = build_recommendations("allkeys-lru", &[big], &[], &TtlHistogram::default(), None);
        assert!(!solo.iter().any(|r| matches!(r.kind, RecoKind::DominantPrefix { .. })));
    }

    #[test]
    fn healthy_keyspace_yields_no_recommendations() {
        let recs = build_recommendations(
            "allkeys-lru",
            &[prefix_row("u:*", 10, 2_000, "hash")],
            &[single_key("u:1", "hash", 1024)],
            &TtlHistogram::default(),
            None,
        );
        assert!(recs.is_empty(), "unexpected findings: {recs:?}");
    }
}

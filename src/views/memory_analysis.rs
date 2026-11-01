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
use crate::connection::{CommandStatus, ServerCommand};
use crate::connection::{
    HeatMetric, HeatProbe, KeyMemoryUsage, ServerDb, key_size_distributions, maxmemory_policy, probe_pattern_fields,
    sample_memory_usage,
};
use crate::error::Error;
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{AiEndpoint, analyze_report};
use crate::helpers::{
    MemoryAnalysisAction, build_csv, format_duration, get_mono_font_family, group_thousands, unix_ts_millis,
};
use crate::states::{
    HINT_MEMORY_ANALYSIS, ReportFinding, ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState,
    back_to_editor_tooltip, content_area_width, get_metrics_cache, i18n_common, i18n_hints, i18n_memory_analysis,
    i18n_troubleshoot, update_app_state_and_save_quiet,
};
use crate::views::{ChartParams, ValueLabel, format_timestamp_ms, make_bar_chart, make_line_chart};
/// Redis Memory Analysis viewer.
///
/// Samples keys from the database, groups by prefix and displays two tables:
/// 1. Top 20 prefix groups by estimated memory (keys containing the separator)
/// 2. Top 20 single keys by memory / freq / idletime (keys without the separator)
use crate::views::{export_to_file, open_key_in_editor, search_keys_in_tree};
#[cfg(target_family = "wasm")]
use crate::{helpers::PickFailure, views::open_file};
use gpui::{
    ClipboardItem, Entity, Pixels, SharedString, Subscription, Task, Window, div, prelude::*, px, relative, rems,
};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt, WindowExt,
    button::Button,
    h_flex,
    label::Label,
    select::{Select, SelectEvent, SelectItem, SelectState},
    table::{DataTable, TableState},
    v_flex,
};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};
use web_time::Instant;
use zedis_core::keysizes::{KeysizesDist, KeysizesUnit};
use zedis_core::rdb::RdbParser;
use zedis_core::schema::{DataDictionary, SchemaAccumulator};
use zedis_ui::ZedisTextTable;
use zedis_ui::{ZedisDivider, fitted_table, help_popover, hint_banner};

mod export;
mod render;
mod report;
mod rules;
mod sampler;
mod schema_dialog;
mod stats;
mod tables;

use rules::*;
use schema_dialog::*;
use stats::*;
use tables::*;

/// Maximum rows kept per table.
const TOP_N: usize = 20;

/// Default table row height (Medium size in gpui-component).
const TABLE_ROW_HEIGHT: f32 = 32.;

/// Section title bar height (py_1p5 padding + text).
const SECTION_TITLE_HEIGHT: f32 = 30.;

const DEFAULT_SCAN_COUNT: u64 = 100;

/// Width of the "rank by" dropdown — fits the longest localized mode label.
const RANK_SELECT_WIDTH: f32 = 110.0;

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

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn format_memory(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::FormatSizeOptions::default().decimal_places(2))
}

/// A key that never expires, in the tables and the exported report. Not
/// "Perm": next to an ACL panel that reads as *permission*. (English, like
/// the rest of the report's own words — the sampler has no locale.)
const NO_TTL: &str = "No TTL";

fn format_ttl(avg_secs: f64) -> String {
    if avg_secs < 0.0 {
        return NO_TTL.to_string();
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
    prefix_table: Entity<TableState<ZedisTextTable>>,
    single_table: Entity<TableState<ZedisTextTable>>,
    /// The rows behind the two tables, kept for the AI report (the tables
    /// hold only their cells).
    prefix_rows: Vec<PrefixRow>,
    single_rows: Vec<SingleKeyRow>,
    /// Rows came from an offline RDB file: the tables' jump actions read
    /// this at click time and stay hidden, since there is no live key.
    offline: Rc<Cell<bool>>,
    /// Every prefix path of the finished run, one entry per level — what
    /// the drill-down re-slices. Empty while a run is in flight.
    prefix_map: HashMap<String, PrefixStats>,
    /// The path the prefix table is currently showing the children of;
    /// empty is the top level.
    prefix_root: SharedString,
    /// Whether the prefix rows can be drilled into — read by the table's
    /// action provider at click time, false until a run has finished and
    /// left its map behind.
    drillable: Rc<Cell<bool>>,
    /// `(type, encoding)` shares of the sample, and the rows built from
    /// them for the breakdown card.
    type_map: HashMap<(String, String), TypeStats>,
    type_rows: Vec<TypeShareRow>,
    /// Breakdown card mode: one row per `(type, encoding)` pair instead of
    /// one per type.
    type_by_encoding: bool,
    status: AnalysisStatus,
    prefix_count: usize,
    single_count: usize,
    progress: SharedString,
    /// Numeric twin of `progress` (0–100) driving the progress bar under
    /// the toolbar while a run is active.
    progress_value: u32,
    /// Error message when `status == Error` (a sampling SCAN failed
    /// mid-run). `None` otherwise. Mirrors `ai_output` for the AI path.
    scan_error: Option<SharedString>,
    analysis_task: Option<Task<()>>,
    /// The server and database the online analysis on screen scanned, so a
    /// switch away from them can drop it — and a reconnect to the same one
    /// does not.
    analyzed: Option<(String, usize)>,
    /// Database key count fetched on load.
    dbsize: Option<u64>,
    /// User-editable sample ratio (0.0–1.0).
    ratio: f32,
    /// The ratio the shown online run sampled at. The numbers on screen are
    /// scaled by it, not by whatever the ratio box says after the run —
    /// editing it re-scaled a finished analysis.
    run_ratio: Option<f32>,
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
    /// The "rank by" dropdown. Its item list depends on whether the server
    /// exposes a heat metric, so it is rebuilt whenever `heat` changes.
    sort_state: Entity<SelectState<Vec<SortOption>>>,
    /// Set when `heat` changed; the next render (which has a `Window`) rebuilds
    /// the dropdown's items and re-selects the active mode.
    should_rebuild_sort_items: Option<bool>,
    /// Cached group of top-N selectors so toggling Size/Hot/Cold doesn't
    /// re-run the scan.
    single_groups: SingleKeyTopGroups,
    /// Sampled TTL distribution. Populated by the existing SCAN loop
    /// (no extra Redis round-trip — `KeyMemoryUsage::ttl` is already
    /// in the pipeline). Reset on each `start_analysis`.
    ttl_histogram: TtlHistogram,
    /// Server-side per-type key-size histogram (`INFO keysizes`, Redis 8+)
    /// — exact whole-keyspace counts, unlike the sampled tables. Fetched
    /// ambiently (one tiny INFO) and refreshed on each run; empty on
    /// servers without the section, which hides the card.
    keysizes: Vec<KeysizesDist>,
    /// Selected type index into `keysizes` for the histogram card.
    keysizes_selected: usize,
    keysizes_task: Option<Task<()>>,
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
    /// File name of the RDB dump currently analyzed — `Some` marks the
    /// whole view as showing offline-file results (chips/rules that need
    /// the live server hide, sizes are serialized bytes, jump actions are
    /// suppressed). Cleared when an online scan starts.
    rdb_file: Option<SharedString>,
    /// First visit ever (HINT_MEMORY_ANALYSIS not yet dismissed) — show the
    /// one-time intro banner. Local so closing it repaints without waiting
    /// for the async state save.
    show_first_visit_hint: bool,
    /// The fragmentation chart's series, rebuilt when a new sample lands
    /// rather than on every paint.
    frag_series: Option<FragSeries>,
    /// Collapsed key-pattern schema inference accumulator.
    schema: SchemaAccumulator,
    /// Background task probing top Hash/JSON/String pattern fields after an online scan.
    schema_probe_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The fragmentation chart's data, built from the heartbeat's history: up
/// to 1800 samples, each with a formatted time, which a scan's every round
/// of repaints used to rebuild.
struct FragSeries {
    /// The server and newest sample time it was built from — the history
    /// only grows at its end, so an unchanged newest sample is an unchanged
    /// history.
    source: (String, i64),
    dates: Arc<Vec<SharedString>>,
    values: Arc<Vec<f64>>,
    latest_frag_bytes: i64,
}

impl ZedisMemoryAnalysis {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let offline = Rc::new(Cell::new(false));
        let drillable = Rc::new(Cell::new(false));
        let view = cx.entity().downgrade();
        let prefix_table = cx.new(|cx| {
            TableState::new(
                prefix_table(
                    server_state.clone(),
                    offline.clone(),
                    drillable.clone(),
                    view,
                    window,
                    cx,
                ),
                window,
                cx,
            )
        });
        let single_table = cx.new(|cx| {
            TableState::new(
                single_key_table(server_state.clone(), offline.clone(), window, cx),
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

        // `dbsize` gates the Analyze button, and it only lands once the connection
        // has run DBSIZE (`ServerEvent::ServerInfoUpdated`). Reading it once at
        // construction is not enough: restoring the app straight onto this route
        // builds the view *before* the server finishes connecting, so `dbsize`
        // stayed `None` forever and the button was permanently dead. Track it.
        subscriptions.push(cx.subscribe(&server_state, |this, state, event, cx| {
            if !matches!(event, ServerEvent::ServerInfoUpdated | ServerEvent::ServerSelected(_)) {
                return;
            }
            // An online analysis belongs to the server it scanned. The view
            // outlives a server switch, and left alone the run went on
            // sending SCAN + MEMORY USAGE to the previous server while its
            // rows — and "open key" / "search prefix" — landed on the new
            // one. An RDB file's analysis names no server and stays.
            if matches!(event, ServerEvent::ServerSelected(_)) && this.rdb_file.is_none() {
                let now = {
                    let state = state.read(cx);
                    (state.server_id().to_string(), state.db())
                };
                if this.analyzed.as_ref().is_some_and(|analyzed| *analyzed != now) {
                    this.analysis_task.take();
                    this.analyzed = None;
                    this.reset_for_run(false, cx);
                    this.status = AnalysisStatus::Idle;
                }
            }
            let dbsize = state.read(cx).dbsize();
            if this.dbsize == dbsize {
                return;
            }
            // First time we learn the key count: re-derive the sampling default,
            // which was computed against an unknown size (and so defaulted to
            // 100%). A ratio the user has since typed is left alone.
            if this.dbsize.is_none() {
                this.ratio = default_sample_ratio(dbsize);
                this.ratio_dirty = true;
            }
            this.dbsize = dbsize;
            this.update_est_commands();
            // The connection is demonstrably up now — (re)pull the keysizes
            // card too, covering the restored-route case where construction
            // ran before the server finished connecting.
            this.fetch_keysizes(cx);
            cx.notify();
        }));

        // Heat is unknown until a scan reads `maxmemory-policy`, so the dropdown
        // starts with Size alone and gains Hot/Cold once the probe resolves.
        let sort_state = cx.new(|cx| SelectState::new(sort_options(false, cx), Some(IndexPath::new(0)), window, cx));
        subscriptions.push(cx.subscribe_in(
            &sort_state,
            window,
            |this, _state, event: &SelectEvent<Vec<SortOption>>, _window, cx| match event {
                SelectEvent::Confirm(value) => {
                    if let Some(mode) = *value {
                        this.set_sort_mode(mode, cx);
                    }
                }
            },
        ));

        let mut this = Self {
            policy: SharedString::default(),
            heat: HeatProbe::None,
            sort_mode: SortMode::Size,
            sort_state,
            should_rebuild_sort_items: None,
            single_groups: SingleKeyTopGroups::new(TOP_N),
            prefix_rows: Vec::new(),
            prefix_map: HashMap::new(),
            prefix_root: SharedString::default(),
            drillable,
            type_map: HashMap::new(),
            type_rows: Vec::new(),
            type_by_encoding: false,
            single_rows: Vec::new(),
            offline,
            server_state,
            prefix_table,
            single_table,
            status: AnalysisStatus::Idle,
            prefix_count: 0,
            single_count: 0,
            progress: SharedString::default(),
            progress_value: 0,
            scan_error: None,
            analysis_task: None,
            analyzed: None,
            dbsize,
            ratio: default_ratio,
            run_ratio: None,
            ratio_input_state,
            ratio_dirty: false,
            scan_count: DEFAULT_SCAN_COUNT,
            scan_count_input_state,
            est_commands: 0,
            ttl_histogram: TtlHistogram::default(),
            keysizes: Vec::new(),
            keysizes_selected: 0,
            keysizes_task: None,
            recommendations: Vec::new(),
            ai_status: AiStatus::Idle,
            ai_output: None,
            ai_task: None,
            rdb_file: None,
            show_first_visit_hint: !cx
                .global::<ZedisGlobalStore>()
                .read(cx)
                .hint_dismissed(HINT_MEMORY_ANALYSIS),
            frag_series: None,
            schema: SchemaAccumulator::new(),
            schema_probe_task: None,
            _subscriptions: subscriptions,
        };
        this.update_est_commands();
        this.fetch_keysizes(cx);
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
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisAccumulators, KeySample, NO_TTL, PrefixRow, RecoKind, RecoSeverity, SingleKeyRow, TtlHistogram,
        TypeStats, build_markdown_report, build_prefix_rows, build_recommendations, build_type_rows, format_memory,
        format_thousands, md_cell,
    };
    use crate::connection::{HeatMetric, HeatProbe};
    use std::collections::HashMap;

    const MIB: u64 = 1024 * 1024;

    fn single_key(key: &str, key_type: &str, bytes: u64) -> SingleKeyRow {
        SingleKeyRow {
            key: key.to_string().into(),
            memory_bytes: bytes,
            memory: format_memory(bytes).into(),
            key_type: key_type.to_string().into(),
            ttl: NO_TTL.into(),
            ttl_secs: -1,
            heat: HeatMetric::None,
            heat_display: "—".into(),
        }
    }

    fn prefix_row(prefix: &str, key_count: u64, memory_bytes: u64, types: &str) -> PrefixRow {
        PrefixRow {
            path: prefix.to_string().into(),
            has_children: false,
            prefix: prefix.to_string().into(),
            key_count,
            display_key_count: format_thousands(key_count).into(),
            memory_bytes,
            memory: format_memory(memory_bytes).into(),
            types: types.to_string().into(),
            avg_ttl: NO_TTL.into(),
            avg_ttl_secs: -1.0,
            perm_count: 0,
            perm_display: "0".into(),
        }
    }

    /// One key folded into the accumulators, all defaults but the shape
    /// under test.
    fn fold(acc: &mut AnalysisAccumulators, key: &str, bytes: u64) {
        acc.add(
            KeySample {
                key,
                memory_bytes: bytes,
                ttl: -1,
                key_type: "string",
                encoding: "embstr",
                heat: HeatMetric::None,
            },
            HeatProbe::None,
            ":",
        );
    }

    /// Every ancestor path is counted, so drilling into a prefix is a
    /// filter over the same map — and it stops at `MAX_PREFIX_DEPTH`.
    #[test]
    fn prefixes_accumulate_at_every_level_and_slice_by_root() {
        let mut acc = AnalysisAccumulators::new();
        for (key, bytes) in [
            ("user:session:eu:1", 100),
            ("user:session:us:2", 200),
            ("user:profile:3", 400),
            ("cache:page:4", 800),
            ("nosep", 1600),
        ] {
            fold(&mut acc, key, bytes);
        }

        let top = build_prefix_rows(&acc.prefix_map, 1.0, ":", "");
        assert_eq!(
            top.iter().map(|r| r.prefix.as_ref()).collect::<Vec<_>>(),
            vec!["cache:*", "user:*"],
            "ranked by memory, and a key without the separator joins no group"
        );
        let user = top.iter().find(|r| r.path == "user").expect("user row");
        assert_eq!(user.memory_bytes, 700, "every key below the prefix counts once");
        assert!(user.has_children);

        let level2 = build_prefix_rows(&acc.prefix_map, 1.0, ":", "user");
        assert_eq!(
            level2.iter().map(|r| r.prefix.as_ref()).collect::<Vec<_>>(),
            vec!["user:profile:*", "user:session:*"]
        );

        let level3 = build_prefix_rows(&acc.prefix_map, 1.0, ":", "user:session");
        assert_eq!(
            level3.iter().map(|r| r.prefix.as_ref()).collect::<Vec<_>>(),
            vec!["user:session:us:*", "user:session:eu:*"]
        );
        assert!(
            level3.iter().all(|r| !r.has_children),
            "the third level is the last one kept, so nothing drills deeper"
        );
        assert!(!acc.prefix_map.contains_key("user:session:us:2"));
    }

    #[test]
    fn type_rows_merge_encodings_and_rank_by_memory() {
        let mut map: HashMap<(String, String), TypeStats> = HashMap::new();
        map.insert(
            ("hash".to_string(), "listpack".to_string()),
            TypeStats {
                key_count: 10,
                memory_bytes: 100,
            },
        );
        map.insert(
            ("hash".to_string(), "hashtable".to_string()),
            TypeStats {
                key_count: 2,
                memory_bytes: 900,
            },
        );
        // A server that would not answer OBJECT ENCODING.
        map.insert(
            ("string".to_string(), String::new()),
            TypeStats {
                key_count: 5,
                memory_bytes: 500,
            },
        );

        let by_type = build_type_rows(&map, 1.0, false, "unknown");
        assert_eq!(
            by_type.iter().map(|r| r.label.as_ref()).collect::<Vec<_>>(),
            vec!["hash", "string"]
        );
        assert_eq!(by_type[0].key_count, 12);
        assert_eq!(by_type[0].memory_bytes, 1000);
        assert!((by_type[0].share_pct - 1000.0 / 15.0).abs() < 0.1);

        let by_encoding = build_type_rows(&map, 1.0, true, "unknown");
        assert_eq!(
            by_encoding.iter().map(|r| r.label.as_ref()).collect::<Vec<_>>(),
            vec!["hash · hashtable", "string · unknown", "hash · listpack"]
        );

        // Sampling scales the figures back up to the whole keyspace.
        let scaled = build_type_rows(&map, 0.5, false, "unknown");
        assert_eq!(scaled[0].memory_bytes, 2000);
        assert_eq!(scaled[0].key_count, 24);
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
        let md = build_markdown_report(Some(1_234), "allkeys-lru", 1.0, &[], &[], &ttl, &[]);
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
        let recs = build_recommendations("allkeys-lru", &[], &biggest, &TtlHistogram::default(), None, None);
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
        let recs = build_recommendations("volatile-lru", &[], &[], &ttl, None, None);
        assert!(recs.iter().any(|r| {
            matches!(r.kind, RecoKind::UnevictableKeys { no_ttl_pct: 60, .. }) && r.severity == RecoSeverity::Critical
        }));
        // allkeys-* evicts regardless of TTL, so the same shape is fine there.
        let ok = build_recommendations("allkeys-lru", &[], &[], &ttl, None, None);
        assert!(!ok.iter().any(|r| matches!(r.kind, RecoKind::UnevictableKeys { .. })));
    }

    #[test]
    fn noeviction_policy_is_flagged() {
        let recs = build_recommendations("noeviction", &[], &[], &TtlHistogram::default(), None, None);
        assert!(
            recs.iter()
                .any(|r| r.kind == RecoKind::NoEvictionPolicy && r.severity == RecoSeverity::Info)
        );
    }

    #[test]
    fn fragmentation_needs_both_ratio_and_absolute_waste() {
        // High ratio but tiny waste → noise, not flagged.
        let small = build_recommendations(
            "allkeys-lru",
            &[],
            &[],
            &TtlHistogram::default(),
            Some((3.0, 10 * MIB)),
            None,
        );
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
            None,
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
        let recs = build_recommendations("allkeys-lru", &[p], &[], &TtlHistogram::default(), None, None);
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
        let none = build_recommendations("allkeys-lru", &[mixed], &[], &TtlHistogram::default(), None, None);
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
            None,
        );
        assert!(
            recs.iter()
                .any(|r| matches!(&r.kind, RecoKind::DominantPrefix { pct: 90, .. }))
        );
        // A share of everything under a prefix, not of the table's top few:
        // the same two rows out of 100k in all dominate nothing.
        let of_all = build_recommendations(
            "allkeys-lru",
            &[big.clone(), prefix_row("b:*", 100, 1_000, "hash")],
            &[],
            &TtlHistogram::default(),
            None,
            Some(100_000),
        );
        assert!(!of_all.iter().any(|r| matches!(r.kind, RecoKind::DominantPrefix { .. })));
        // A lone prefix can't "dominate" — nothing to compare against.
        let solo = build_recommendations("allkeys-lru", &[big], &[], &TtlHistogram::default(), None, None);
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
            None,
        );
        assert!(recs.is_empty(), "unexpected findings: {recs:?}");
    }

    #[test]
    fn accumulators_feed_schema_and_build_data_dictionary() {
        let mut acc = AnalysisAccumulators::new();
        for (key, bytes) in [("user:1001:profile", 256), ("user:1002:profile", 512)] {
            acc.add(
                KeySample {
                    key,
                    memory_bytes: bytes,
                    ttl: 3600,
                    key_type: "hash",
                    encoding: "listpack",
                    heat: HeatMetric::None,
                },
                HeatProbe::None,
                ":",
            );
        }
        acc.schema.record_structure_fields(
            "user:{id}:profile",
            &[
                ("uid".to_string(), "integer".to_string()),
                ("email".to_string(), "string".to_string()),
            ],
        );
        let dict = acc.schema.finish(0.5, 20);
        assert_eq!(dict.patterns.len(), 1);
        assert_eq!(dict.patterns[0].pattern, "user:{id}:profile");
        assert_eq!(dict.patterns[0].estimated_keys, 4);
        assert_eq!(dict.patterns[0].fields.len(), 2);
        let md = dict.to_markdown("prod-redis (db 0)");
        assert!(md.contains("`user:{id}:profile`"));
        assert!(md.contains("uid: integer"));
    }
}

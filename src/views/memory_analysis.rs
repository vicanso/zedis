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
/// Redis Memory Analysis viewer.
///
/// Samples keys from the database, groups by prefix and displays two tables:
/// 1. Top 20 prefix groups by estimated memory (keys containing the separator)
/// 2. Top 20 single keys by memory / freq / idletime (keys without the separator)
use crate::states::{
    ServerView, ZedisGlobalStore, ZedisServerState, get_metrics_cache, i18n_common, i18n_memory_analysis,
};
use crate::views::{ChartParams, format_timestamp_ms, make_bar_canvas, make_line_canvas};
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
use zedis_ui::ZedisDivider;

/// Maximum rows kept per table.
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

/// Number of rows from each table included in the AI report. The tables
/// already hold at most ~20 rows, but cap defensively so the prompt stays
/// bounded regardless of upstream changes.
const REPORT_ROW_LIMIT: usize = 20;

/// Escape a value for use inside a single Markdown table cell: pipes
/// would break the column layout and newlines would break the row.
fn md_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

/// Compact Markdown styling for the AI panel. The library defaults size
/// headings up to `rems(2.0)` (~28px h1), which dwarfs the body text in
/// a side panel; shrink them to a gentle hierarchy and tighten the
/// inter-paragraph gap so an LLM reply full of `#`/`##` stays readable.
fn ai_markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, _base| match level {
            1 => px(18.),
            2 => px(16.),
            3 => px(15.),
            _ => px(14.),
        })
}

/// Render the current analysis into a Markdown report suitable for
/// submitting to an LLM. Pure over its inputs — no Redis access. Only
/// key *names*, sizes and TTLs are included (never key values).
fn build_markdown_report(
    dbsize: Option<u64>,
    policy: &str,
    ratio: f32,
    prefix_rows: &[PrefixRow],
    single_rows: &[SingleKeyRow],
    ttl: &TtlHistogram,
) -> String {
    let mut md = String::with_capacity(2048);
    md.push_str("# Redis Memory Analysis Report\n\n");

    md.push_str("## Overview\n\n");
    if let Some(size) = dbsize {
        md.push_str(&format!("- Total keys (DBSIZE): {}\n", format_thousands(size)));
    }
    md.push_str(&format!("- Sample ratio: {:.1}%\n", (ratio * 100.0).clamp(0.0, 100.0)));
    if !policy.is_empty() {
        md.push_str(&format!("- maxmemory-policy: `{policy}`\n"));
    }
    md.push('\n');

    let total = ttl.total();
    if total > 0 {
        md.push_str("## TTL distribution (sampled keys)\n\n");
        md.push_str("| Bucket | Keys | Percent |\n| --- | ---: | ---: |\n");
        let pct = |n: u64| -> String { format!("{:.1}%", n as f64 / total as f64 * 100.0) };
        let buckets = [
            ("< 1m", ttl.lt_1m),
            ("< 1h", ttl.lt_1h),
            ("< 1d", ttl.lt_1d),
            ("< 7d", ttl.lt_7d),
            (">= 7d", ttl.gte_7d),
            ("No expiry", ttl.no_ttl),
        ];
        for (label, count) in buckets {
            md.push_str(&format!("| {label} | {} | {} |\n", format_thousands(count), pct(count)));
        }
        md.push('\n');
    }

    if !prefix_rows.is_empty() {
        md.push_str("## Top key-prefix groups by estimated memory\n\n");
        md.push_str("| Prefix | Keys | Est. memory | Avg TTL | No-expiry keys | Types |\n");
        md.push_str("| --- | ---: | ---: | --- | ---: | --- |\n");
        for r in prefix_rows.iter().take(REPORT_ROW_LIMIT) {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                md_cell(&r.prefix),
                md_cell(&r.display_key_count),
                md_cell(&r.memory),
                md_cell(&r.avg_ttl),
                md_cell(&r.perm_display),
                md_cell(&r.types),
            ));
        }
        md.push('\n');
    }

    if !single_rows.is_empty() {
        md.push_str("## Top single keys by memory\n\n");
        md.push_str("| Key | Memory | Type | TTL | Heat |\n");
        md.push_str("| --- | ---: | --- | --- | --- |\n");
        for r in single_rows.iter().take(REPORT_ROW_LIMIT) {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                md_cell(&r.key),
                md_cell(&r.memory),
                md_cell(&r.key_type),
                md_cell(&r.ttl),
                md_cell(&r.heat_display),
            ));
        }
        md.push('\n');
    }

    md
}

// ─── Recommendations (offline rule engine) ───────────────────────────────────

/// Severity of a local recommendation. Variant order is the priority order:
/// deriving `Ord` lets a single `sort_by_key` surface critical items first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RecoSeverity {
    Critical,
    Warning,
    Info,
}

/// One finding from the offline rule engine. `kind` carries the
/// machine-readable facts (numbers, offending key/prefix); the renderer
/// turns it into localized title/detail text. Keeping the numbers out of
/// the i18n layer means the rules stay unit-testable without a running
/// app and the locale files need no `%{var}` placeholders.
#[derive(Clone, Debug, PartialEq)]
struct Recommendation {
    severity: RecoSeverity,
    kind: RecoKind,
}

#[derive(Clone, Debug, PartialEq)]
enum RecoKind {
    /// A single key large enough to risk O(N)/serialization latency on
    /// access, `DEL`, replication or migration.
    BigKey {
        key: SharedString,
        key_type: SharedString,
        bytes: u64,
    },
    /// `volatile-*` eviction policy but a large share of keys have no TTL —
    /// those keys can never be evicted, so the server can OOM under pressure.
    UnevictableKeys { no_ttl_pct: u8, policy: SharedString },
    /// `noeviction` policy — writes start failing once `maxmemory` is hit.
    NoEvictionPolicy,
    /// High allocator fragmentation with a meaningful absolute waste.
    HighFragmentation { ratio: f64, waste_bytes: u64 },
    /// A prefix holds many tiny `string` keys — folding them into one Hash
    /// removes the per-key overhead (dict entry, object header, expire slot).
    ManySmallStrings {
        prefix: SharedString,
        keys: u64,
        avg_bytes: u64,
    },
    /// One prefix dominates sampled memory — a hotspot worth reviewing.
    DominantPrefix { prefix: SharedString, pct: u8 },
}

/// Largest single key that is merely "worth noticing" vs an outright red
/// flag. A 5MiB collection makes O(N) ops (`HGETALL`, `SMEMBERS`, `DEL`) and
/// replication chunks visibly slower; 50MiB can stall the event loop on a
/// single command.
const BIG_KEY_WARN_BYTES: u64 = 5 * 1024 * 1024;
const BIG_KEY_CRIT_BYTES: u64 = 50 * 1024 * 1024;
/// Cap big-key findings so a pathological DB doesn't bury the other advice —
/// the input is already sorted biggest-first.
const BIG_KEY_MAX_FINDINGS: usize = 3;
/// Minimum sampled keys before TTL-distribution rules fire — below this the
/// percentages are too noisy to act on.
const RECO_MIN_TTL_SAMPLE: u64 = 50;
/// Share (%) of sampled keys with no TTL that turns a `volatile-*` policy
/// into an OOM hazard.
const UNEVICTABLE_PCT: u8 = 50;
/// A prefix with at least this many keys, all of type `string`, whose
/// average size is under [`SMALL_STRING_MAX_AVG`], is a fold-into-Hash
/// candidate.
const MANY_SMALL_MIN_KEYS: u64 = 1000;
const SMALL_STRING_MAX_AVG: u64 = 200;
/// A prefix holding at least this share (%) of summed sampled prefix memory
/// is flagged as a hotspot (needs ≥2 prefixes to be meaningful).
const DOMINANT_PREFIX_PCT: u8 = 60;
/// Fragmentation ratio above which the allocator is wasting enough to suggest
/// `activedefrag`/restart — paired with [`FRAG_FLOOR_BYTES`] so a tiny DB's
/// noisy ratio doesn't trip it.
const FRAG_RATIO_WARN: f64 = 1.5;
/// Below this much absolute waste the ratio carries no signal (jemalloc's
/// fixed overhead dominates). Mirrors the chart's `FRAG_FLOOR_BYTES`.
const FRAG_FLOOR_BYTES: u64 = 200 * 1024 * 1024;

/// Build the offline recommendation list from one analysis run. Pure over its
/// inputs (no Redis, no `cx`) so it is fully unit-testable. `prefix_rows` and
/// `biggest_keys` are the already-computed aggregates; `frag` is
/// `Some((ratio, waste_bytes))` when the status-bar heartbeat has a recent
/// fragmentation sample, else `None`.
fn build_recommendations(
    policy: &str,
    prefix_rows: &[PrefixRow],
    biggest_keys: &[SingleKeyRow],
    ttl: &TtlHistogram,
    frag: Option<(f64, u64)>,
) -> Vec<Recommendation> {
    let mut out = Vec::new();

    // ── Big single keys (input is sorted biggest-first) ──
    for row in biggest_keys.iter().take(BIG_KEY_MAX_FINDINGS) {
        let severity = if row.memory_bytes >= BIG_KEY_CRIT_BYTES {
            RecoSeverity::Critical
        } else if row.memory_bytes >= BIG_KEY_WARN_BYTES {
            RecoSeverity::Warning
        } else {
            // Once one is under the bar, every later (smaller) one is too.
            break;
        };
        out.push(Recommendation {
            severity,
            kind: RecoKind::BigKey {
                key: row.key.clone(),
                key_type: row.key_type.clone(),
                bytes: row.memory_bytes,
            },
        });
    }

    // ── Eviction-policy hazards ──
    let total = ttl.total();
    let policy_lc = policy.to_ascii_lowercase();
    if policy_lc == "noeviction" {
        out.push(Recommendation {
            severity: RecoSeverity::Info,
            kind: RecoKind::NoEvictionPolicy,
        });
    } else if policy_lc.starts_with("volatile-") && total >= RECO_MIN_TTL_SAMPLE {
        let pct = (ttl.no_ttl as f64 / total as f64 * 100.0).round() as u8;
        if pct >= UNEVICTABLE_PCT {
            out.push(Recommendation {
                severity: RecoSeverity::Critical,
                kind: RecoKind::UnevictableKeys {
                    no_ttl_pct: pct,
                    policy: policy.to_string().into(),
                },
            });
        }
    }

    // ── Fragmentation (both a bad ratio AND meaningful absolute waste) ──
    if let Some((ratio, waste)) = frag
        && ratio >= FRAG_RATIO_WARN
        && waste >= FRAG_FLOOR_BYTES
    {
        out.push(Recommendation {
            severity: RecoSeverity::Warning,
            kind: RecoKind::HighFragmentation {
                ratio,
                waste_bytes: waste,
            },
        });
    }

    // ── Prefix-design hints ──
    for row in prefix_rows {
        // `types` is the comma-joined set built in `build_prefix_rows`.
        let only_string = !row.types.is_empty() && row.types.split(", ").all(|t| t == "string");
        if only_string && row.key_count >= MANY_SMALL_MIN_KEYS {
            let avg = row.memory_bytes / row.key_count.max(1);
            if avg <= SMALL_STRING_MAX_AVG {
                out.push(Recommendation {
                    severity: RecoSeverity::Info,
                    kind: RecoKind::ManySmallStrings {
                        prefix: row.prefix.clone(),
                        keys: row.key_count,
                        avg_bytes: avg,
                    },
                });
            }
        }
    }
    // Dominant prefix — only meaningful when several prefixes compete.
    let total_prefix_mem: u64 = prefix_rows.iter().map(|r| r.memory_bytes).sum();
    if prefix_rows.len() >= 2
        && total_prefix_mem > 0
        && let Some(top) = prefix_rows.iter().max_by_key(|r| r.memory_bytes)
    {
        let pct = (top.memory_bytes as f64 / total_prefix_mem as f64 * 100.0).round() as u8;
        if pct >= DOMINANT_PREFIX_PCT {
            out.push(Recommendation {
                severity: RecoSeverity::Info,
                kind: RecoKind::DominantPrefix {
                    prefix: top.prefix.clone(),
                    pct,
                },
            });
        }
    }

    // Most-urgent first; stable so within a severity the discovery order
    // (big keys, then policy, then design hints) is preserved.
    out.sort_by_key(|r| r.severity);
    out
}

/// Most recent fragmentation sample from the status-bar heartbeat cache:
/// `(mem_fragmentation_ratio, wasted_bytes)` where waste = RSS − used.
/// `None` when no non-zero sample exists yet. Mirrors the filtering in
/// `render_fragmentation_chart` so the rule engine and the chart agree.
fn latest_fragmentation(server_id: &str) -> Option<(f64, u64)> {
    if server_id.is_empty() {
        return None;
    }
    get_metrics_cache()
        .list_metrics(server_id)
        .iter()
        .rev()
        .find(|m| m.mem_fragmentation_ratio > 0.0)
        .map(|m| {
            let waste = (m.used_memory_rss as i64).saturating_sub(m.used_memory as i64).max(0) as u64;
            (m.mem_fragmentation_ratio, waste)
        })
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
const PERM_KEY_WIDTH: f32 = 130.;

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
            - PERM_KEY_WIDTH
            - padding_offset
            - scrollbar_offset;

        let column_keys = vec![
            COL_PREFIX,
            COL_KEY_COUNT,
            COL_MEMORY,
            COL_AVG_TTL,
            COL_PERM_COUNT,
            COL_TYPES,
        ];
        let widths = [
            prefix_w,
            COUNT_KEY_WIDTH,
            MEMORY_KEY_WIDTH,
            TTL_KEY_WIDTH,
            PERM_KEY_WIDTH,
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
                COL_PERM_COUNT => a.perm_count.cmp(&b.perm_count),
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
                4 => r.perm_display.clone(),
                5 => r.types.clone(),
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
            let est_perm = (stats.perm_count as f32 * scale) as u64;

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
                perm_count: est_perm,

                // Pre-format all display strings here (Zero-Allocation trick)
                // Add the "~" prefix and format with thousands separators
                display_key_count: format!("{est_prefix}{}", format_thousands(est_count)).into(),

                // Add the "~" prefix to the human-readable memory
                memory: format!("{est_prefix}{}", format_memory(est_mem)).into(),

                types: types.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ").into(),
                avg_ttl: format_ttl(avg_ttl_secs).into(),

                // Add the "~" prefix to the estimated permanent-key count
                perm_display: format!("{est_prefix}{}", format_thousands(est_perm)).into(),
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

// ─── TTL distribution ────────────────────────────────────────────────────────

/// Histogram of how soon sampled keys are scheduled to expire. The
/// boundaries (1m / 1h / 1d / 7d) match what most caching workloads
/// care about — pinpointing "what's about to expire in this very
/// minute" vs "comfortably long-lived". Tight enough to be readable
/// in a 6-bar chart, loose enough that adjacent keys in the same
/// cache-tier collapse into the same bucket.
///
/// `-1` (no TTL / PERSIST) gets its own bucket because it's a
/// qualitatively different state — a memory-leak red flag on a cache
/// that should be evicting things.
///
/// `-2` (key vanished mid-SCAN) is filtered upstream in
/// `sample_scan_memory_usage`, so we never see it here.
#[derive(Clone, Debug, Default)]
struct TtlHistogram {
    pub lt_1m: u64,
    pub lt_1h: u64,
    pub lt_1d: u64,
    pub lt_7d: u64,
    pub gte_7d: u64,
    pub no_ttl: u64,
}

impl TtlHistogram {
    /// Bucket a single key's TTL into the histogram. Caller has already
    /// filtered `ttl == -2` so we only see live keys.
    fn add(&mut self, ttl_secs: i64) {
        const SEC_PER_MIN: i64 = 60;
        const SEC_PER_HOUR: i64 = 60 * 60;
        const SEC_PER_DAY: i64 = 24 * 60 * 60;
        const SEC_PER_WEEK: i64 = 7 * SEC_PER_DAY;
        match ttl_secs {
            -1 => self.no_ttl += 1,
            // Negative TTLs other than -1 shouldn't reach here, but
            // treat them defensively as "imminent" rather than panic.
            t if t < SEC_PER_MIN => self.lt_1m += 1,
            t if t < SEC_PER_HOUR => self.lt_1h += 1,
            t if t < SEC_PER_DAY => self.lt_1d += 1,
            t if t < SEC_PER_WEEK => self.lt_7d += 1,
            _ => self.gte_7d += 1,
        }
    }

    /// Total number of keys recorded — sum of all buckets. Used both
    /// as the divisor for percentage display and as the empty-state
    /// signal ("no samples yet").
    fn total(&self) -> u64 {
        self.lt_1m + self.lt_1h + self.lt_1d + self.lt_7d + self.gte_7d + self.no_ttl
    }
}

// ─── Analysis status ─────────────────────────────────────────────────────────

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

        let prefix_table = cx.new(|cx| TableState::new(PrefixTableDelegate::new(Vec::new(), window, cx), window, cx));
        let single_table =
            cx.new(|cx| TableState::new(SingleKeyTableDelegate::new(Vec::new(), window, cx), window, cx));

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

    fn render_toolbar_functions(&self, cx: &mut gpui::Context<Self>) -> ZedisDivider {
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
    /// Render the AI advice panel. Hidden until an AI request has been
    /// started; then shows a loading line, the model's Markdown advice,
    /// or an error message, with a button to dismiss it.
    fn render_ai_panel(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        if self.ai_status == AiStatus::Idle {
            return None;
        }
        // Theme colors must be copied out before the `cx.listener`
        // closure below (can't borrow `cx` across it).
        let border = cx.theme().border;
        let panel_bg = cx.theme().muted.opacity(0.4);
        let muted_fg = cx.theme().muted_foreground;
        let danger = cx.theme().red;

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::Bot))
                    .child(Label::new(i18n_memory_analysis(cx, "ai_panel_title")).font_semibold()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    // Copy the model's Markdown reply to the clipboard.
                    .when(self.ai_status == AiStatus::Done, |this| {
                        this.child(
                            Button::new("ai-copy-reply")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some(reply) = this.ai_output.clone() else {
                                        return;
                                    };
                                    cx.write_to_clipboard(ClipboardItem::new_string(reply.to_string()));
                                    window.push_notification(
                                        Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(
                        Button::new("ai-panel-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.clear_ai_result(cx);
                            })),
                    ),
            );

        let content = match self.ai_status {
            AiStatus::Running => Label::new(i18n_memory_analysis(cx, "ai_running"))
                .text_color(muted_fg)
                .into_any_element(),
            AiStatus::Done => {
                TextView::markdown("memory-analysis-ai-result", self.ai_output.clone().unwrap_or_default())
                    .style(ai_markdown_style())
                    .into_any_element()
            }
            AiStatus::Error => Label::new(self.ai_output.clone().unwrap_or_default())
                .text_color(danger)
                .into_any_element(),
            AiStatus::Idle => return None,
        };

        Some(
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(panel_bg)
                .child(header)
                .child(content)
                .into_any_element(),
        )
    }

    /// Map a recommendation to its localized `(title, subject, detail)`. The
    /// subject is the language-neutral concrete fact (key/prefix name, sizes,
    /// percentages) composed in Rust; title/detail come from the locale files.
    fn reco_text(&self, reco: &Recommendation, cx: &gpui::App) -> (SharedString, Option<SharedString>, SharedString) {
        let k = |key: &str| i18n_memory_analysis(cx, key);
        match &reco.kind {
            RecoKind::BigKey { key, key_type, bytes } => (
                k("reco_big_key_title"),
                Some(format!("{key} · {key_type} · {}", format_memory(*bytes)).into()),
                k("reco_big_key_detail"),
            ),
            RecoKind::UnevictableKeys { no_ttl_pct, policy } => (
                k("reco_unevictable_title"),
                Some(format!("{policy} · {no_ttl_pct}%").into()),
                k("reco_unevictable_detail"),
            ),
            RecoKind::NoEvictionPolicy => (
                k("reco_noeviction_title"),
                Some("noeviction".into()),
                k("reco_noeviction_detail"),
            ),
            RecoKind::HighFragmentation { ratio, waste_bytes } => (
                k("reco_fragmentation_title"),
                Some(format!("{ratio:.2}× · {}", format_memory(*waste_bytes)).into()),
                k("reco_fragmentation_detail"),
            ),
            RecoKind::ManySmallStrings {
                prefix,
                keys,
                avg_bytes,
            } => (
                k("reco_small_strings_title"),
                Some(
                    format!(
                        "{prefix} · {} · ~{}",
                        format_thousands(*keys),
                        format_memory(*avg_bytes)
                    )
                    .into(),
                ),
                k("reco_small_strings_detail"),
            ),
            RecoKind::DominantPrefix { prefix, pct } => (
                k("reco_dominant_prefix_title"),
                Some(format!("{prefix} · {pct}%").into()),
                k("reco_dominant_prefix_detail"),
            ),
        }
    }

    /// Flatten the current recommendations into a plain-text block for the
    /// clipboard — one localized `[SEVERITY] Title (subject)` line plus its
    /// detail per finding.
    fn recommendations_plaintext(&self, cx: &gpui::App) -> String {
        let mut s = String::new();
        for reco in &self.recommendations {
            let (title, subject, detail) = self.reco_text(reco, cx);
            let sev = match reco.severity {
                RecoSeverity::Critical => "[CRITICAL]",
                RecoSeverity::Warning => "[WARNING]",
                RecoSeverity::Info => "[INFO]",
            };
            s.push_str(sev);
            s.push(' ');
            s.push_str(&title);
            if let Some(sub) = subject {
                s.push_str(" (");
                s.push_str(&sub);
                s.push(')');
            }
            s.push('\n');
            s.push_str(&detail);
            s.push_str("\n\n");
        }
        s
    }

    /// The offline rule engine's verdict, shown automatically once a scan
    /// finishes. A green "healthy" line when there are no findings, otherwise
    /// a severity-colored list. Hidden entirely until a scan completes.
    fn render_recommendations_panel(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::AnyElement> {
        if self.status != AnalysisStatus::Finished {
            return None;
        }
        // Copy theme colors out before any `cx.listener` closure below.
        let border = cx.theme().border;
        let panel_bg = cx.theme().muted.opacity(0.4);
        let muted_fg = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let c_critical = cx.theme().danger;
        let c_warning = cx.theme().warning;
        let c_info = cx.theme().blue;
        let sev_color = move |s: RecoSeverity| match s {
            RecoSeverity::Critical => c_critical,
            RecoSeverity::Warning => c_warning,
            RecoSeverity::Info => c_info,
        };
        let sev_icon = |s: RecoSeverity| match s {
            RecoSeverity::Critical => IconName::CircleX,
            RecoSeverity::Warning => IconName::TriangleAlert,
            RecoSeverity::Info => IconName::Info,
        };

        let count = self.recommendations.len();
        // The AI deep-dive trigger lives here in the panel header (next to
        // Copy), not in the toolbar — it surfaces exactly when a finished
        // scan has data worth sending to the model.
        let has_data = self.prefix_count > 0 || self.single_count > 0;
        let ai_running = self.ai_status == AiStatus::Running;
        let header =
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(CustomIconName::ListCheck))
                        .child(Label::new(i18n_memory_analysis(cx, "reco_panel_title")).font_semibold())
                        .when(count > 0, |this| {
                            this.child(Label::new(format!("{count}")).text_xs().text_color(muted_fg))
                        }),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        // AI advice: send the report (key names / sizes / TTLs
                        // only) to the configured OpenAI-compatible endpoint.
                        .when(has_data, |this| {
                            this.child(
                                Button::new("reco-ai-analysis")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Bot)
                                    .disabled(ai_running)
                                    .label(if ai_running {
                                        i18n_memory_analysis(cx, "ai_analyzing")
                                    } else {
                                        i18n_memory_analysis(cx, "ai_analyze")
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_ai_analysis(window, cx);
                                    })),
                            )
                        })
                        .when(count > 0, |this| {
                            this.child(Button::new("reco-copy").ghost().small().icon(IconName::Copy).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let text = this.recommendations_plaintext(cx);
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    window.push_notification(
                                        Notification::info(i18n_common(cx, "copied_to_clipboard")),
                                        cx,
                                    );
                                }),
                            ))
                        }),
                );

        let body = if count == 0 {
            h_flex()
                .gap_2()
                .items_center()
                .child(Icon::new(CustomIconName::CircleCheckBig).text_color(green))
                .child(
                    Label::new(i18n_memory_analysis(cx, "reco_healthy"))
                        .text_sm()
                        .text_color(muted_fg),
                )
                .into_any_element()
        } else {
            let mut list = v_flex().w_full().gap_2();
            for reco in &self.recommendations {
                let (title, subject, detail) = self.reco_text(reco, cx);
                let color = sev_color(reco.severity);
                list = list.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(sev_icon(reco.severity)).text_color(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Label::new(title).font_medium().text_color(color))
                                        .when_some(subject, |this, sub| {
                                            this.child(Label::new(sub).text_xs().text_color(muted_fg))
                                        }),
                                )
                                .child(Label::new(detail).text_sm().text_color(muted_fg)),
                        ),
                );
            }
            list.into_any_element()
        };

        Some(
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(panel_bg)
                .child(header)
                .child(body)
                .into_any_element(),
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
            dates: Arc::new(dates),
            y_max,
            y_format: Box::new(|v| format!("{v:.2}")),
            tick_margin,
            border: theme.border,
            muted_fg: theme.muted_foreground,
        };
        let chart = make_line_canvas(params, Arc::new(values), stroke, false);

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

    /// Render the TTL distribution body — a 6-bar histogram plus a
    /// summary line. Bars share the canvas helpers used by the Metrics
    /// panel so visual styling stays consistent. `ratio` is folded into
    /// the summary so users see both the sampled count and an estimated
    /// total ("12,345 sampled → ~123,450 estimated") which matters when
    /// they ran with `ratio < 1.0` and the absolute bar height alone
    /// doesn't reveal cluster impact.
    fn render_ttl_histogram_body(&self, cx: &mut gpui::Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let h = &self.ttl_histogram;
        let total = h.total();

        // Bucket order = visual left→right = expiry urgency: imminent
        // first, "no TTL" last. Same i18n key naming pattern so the
        // localised label sits next to its raw bucket name in the source.
        let buckets: [(&'static str, u64); 6] = [
            ("ttl_bucket_lt_1m", h.lt_1m),
            ("ttl_bucket_lt_1h", h.lt_1h),
            ("ttl_bucket_lt_1d", h.lt_1d),
            ("ttl_bucket_lt_7d", h.lt_7d),
            ("ttl_bucket_gte_7d", h.gte_7d),
            ("ttl_bucket_no_ttl", h.no_ttl),
        ];

        let dates: Vec<SharedString> = buckets.iter().map(|(key, _)| i18n_memory_analysis(cx, key)).collect();
        let values: Vec<f64> = buckets.iter().map(|(_, count)| *count as f64).collect();

        // Pad y_max 10 % above the peak so the tallest bar doesn't
        // touch the top edge. Floor at 1.0 because zero values would
        // collapse the chart to a degenerate scale.
        let raw_max = values.iter().cloned().fold(0.0_f64, f64::max);
        let y_max = (raw_max * 1.1).max(1.0);

        // 6 buckets and the chart is usually wide → label every bar.
        let params = ChartParams {
            dates: Arc::new(dates),
            y_max,
            y_format: Box::new(|v| format!("{v:.0}")),
            tick_margin: 1,
            border: theme.border,
            muted_fg: muted,
        };

        // Pick fill colour by aggregate urgency: if the leftmost two
        // buckets (≤1h) dominate the histogram, paint amber to draw
        // the eye to the eviction cliff. Otherwise the standard chart_2.
        let imminent = h.lt_1m + h.lt_1h;
        let fill_color = if total > 0 && imminent * 2 > total {
            theme.yellow
        } else {
            theme.chart_2
        };
        let chart = make_bar_canvas(params, Arc::new(values), fill_color);

        // Summary line: sampled total + (if ratio<1) estimated full
        // population + no-TTL share (the "are we leaking?" signal).
        let summary_text: SharedString = {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
            let estimated = if self.ratio > 0.0 && self.ratio < 1.0 {
                ((total as f64) / self.ratio as f64) as u64
            } else {
                total
            };
            let no_ttl_pct = if total > 0 {
                (h.no_ttl as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            rust_i18n::t!(
                "memory_analysis.ttl_summary_label",
                sampled = group_thousands(total),
                estimated = group_thousands(estimated),
                no_ttl_pct = format!("{no_ttl_pct:.1}"),
                locale = locale
            )
            .to_string()
            .into()
        };

        // Dominant-bucket callout: which bucket has the most keys? Helps
        // users spot the "everyone expires in the same hour" landmine
        // at a glance without parsing every bar.
        let dominant_label: Option<SharedString> =
            buckets
                .iter()
                .max_by_key(|(_, c)| *c)
                .filter(|(_, c)| *c > 0)
                .map(|(key, count)| {
                    let bucket_name = i18n_memory_analysis(cx, key);
                    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                    rust_i18n::t!(
                        "memory_analysis.ttl_dominant_label",
                        bucket = bucket_name.as_ref(),
                        count = group_thousands(*count),
                        locale = locale
                    )
                    .to_string()
                    .into()
                });

        v_flex()
            .w_full()
            .flex_none()
            .gap_2()
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .h(px(220.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .p_3()
                    .child(
                        div()
                            .font_semibold()
                            .child(i18n_memory_analysis(cx, "ttl_histogram_title"))
                            .mb_2(),
                    )
                    .child(chart),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .px_2()
                    .child(Label::new(summary_text).text_sm().text_color(muted))
                    .when_some(dominant_label, |this, d| {
                        this.child(Label::new(d).text_xs().text_color(muted))
                    }),
            )
            .into_any_element()
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
        let has_ttl_data = self.ttl_histogram.total() > 0;

        // Lay the toolbar out as a single non-wrapping row inside a
        // horizontal scroll container. Modern IDEs (Zed included) keep dense
        // toolbars on one line and let the overflow scroll rather than
        // wrapping or stacking — it stays readable at any window width.
        let nav = h_flex()
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
                            store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                        });
                    }),
            )
            .child(Icon::new(CustomIconName::MemoryStick))
            .child(Label::new(i18n_memory_analysis(cx, "title")).text_color(cx.theme().foreground));
        let functions = self.render_toolbar_functions(cx);

        v_flex()
            .size_full()
            .overflow_hidden()
            .font_family(get_mono_font_family())
            .gap_2()
            // ── Toolbar: single row, horizontal-scroll on overflow ──
            // The h_flex is itself the scroll viewport (mirrors gpui-component's
            // tab_bar). `nav`/`functions` are `flex_none` so they keep their
            // natural width and overflow the row instead of being compressed —
            // that overflow is what the scroll container actually scrolls. The
            // `flex_1` spacer only grows when there is leftover space, pushing
            // the functions group to the right edge when everything fits.
            .child(
                h_flex()
                    .id("memory-analysis-toolbar")
                    .w_full()
                    .flex_none()
                    .h(px(40.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .overflow_x_scroll()
                    .child(nav.flex_none())
                    .child(div().flex_1())
                    .child(functions.flex_none()),
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

                // Offline recommendations — the local rule engine's verdict,
                // shown automatically once a scan finishes (no AI, no config).
                if let Some(panel) = self.render_recommendations_panel(cx) {
                    body = body.child(panel);
                }

                // AI advice panel — pinned to the top so the result is
                // visible immediately after the request completes.
                if let Some(panel) = self.render_ai_panel(cx) {
                    body = body.child(panel);
                }

                // Scan-failure banner — surfaces a SCAN error (which would
                // otherwise have been hidden behind a fake "100% / Finished")
                // while keeping any partial results visible below it.
                if self.status == AnalysisStatus::Error
                    && let Some(message) = self.scan_error.clone()
                {
                    let theme = cx.theme();
                    body = body.child(
                        div()
                            .w_full()
                            .p_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .bg(theme.danger.opacity(0.1))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(Icon::new(IconName::CircleX).text_color(theme.danger))
                                    .child(Label::new(message).text_sm().text_color(theme.danger)),
                            ),
                    );
                }

                // Combined dashboard — no tab selection. One SCAN feeds every
                // section, so they stack in a single scroll view: fragmentation
                // trend, TTL distribution, then the prefix and single-key tables.

                // Fragmentation trend chart (pulls from METRICS_CACHE populated
                // by the status_bar heartbeat). Always attempted — even before
                // the user clicks "Analyse" it shows the running
                // mem_fragmentation_ratio, so it doubles as ambient diagnostic.
                if let Some(chart) = self.render_fragmentation_chart(cx) {
                    body = body.child(chart);
                }

                // Unified empty state: nothing sampled yet and not running.
                if !has_data && !has_ttl_data && !is_running {
                    body = body.child(div().size_full().flex().items_center().justify_center().child(
                        Label::new(i18n_memory_analysis(cx, "no_data")).text_color(cx.theme().muted_foreground),
                    ));
                }

                // TTL distribution histogram (same scan — no extra round-trip).
                if has_ttl_data {
                    body = body.child(self.render_ttl_histogram_body(cx));
                }

                // Prefix groups table
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

                // Single keys table
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

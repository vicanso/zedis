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

//! Analysis outputs: the markdown report builder and the
//! threshold-based recommendation rules (big keys, unevictable
//! TTL share, fragmentation, dominant prefixes, ...).

use super::*;

/// Number of rows from each table included in the AI report. The tables
/// already hold at most ~20 rows, but cap defensively so the prompt stays
/// bounded regardless of upstream changes.
pub(super) const REPORT_ROW_LIMIT: usize = 20;

/// Escape a value for use inside a single Markdown table cell: pipes
/// would break the column layout and newlines would break the row.
pub(super) fn md_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

/// Compact Markdown styling for the AI panel. The library defaults size
/// headings up to `rems(2.0)` (~28px h1), which dwarfs the body text in
/// a side panel; shrink them to a gentle hierarchy and tighten the
/// inter-paragraph gap so an LLM reply full of `#`/`##` stays readable.
pub(super) fn ai_markdown_style() -> TextViewStyle {
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
pub(super) fn build_markdown_report(
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
pub(super) enum RecoSeverity {
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
pub(super) struct Recommendation {
    pub(super) severity: RecoSeverity,
    pub(super) kind: RecoKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum RecoKind {
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
pub(super) const BIG_KEY_WARN_BYTES: u64 = 5 * 1024 * 1024;
pub(super) const BIG_KEY_CRIT_BYTES: u64 = 50 * 1024 * 1024;
/// Cap big-key findings so a pathological DB doesn't bury the other advice —
/// the input is already sorted biggest-first.
pub(super) const BIG_KEY_MAX_FINDINGS: usize = 3;
/// Minimum sampled keys before TTL-distribution rules fire — below this the
/// percentages are too noisy to act on.
pub(super) const RECO_MIN_TTL_SAMPLE: u64 = 50;
/// Share (%) of sampled keys with no TTL that turns a `volatile-*` policy
/// into an OOM hazard.
pub(super) const UNEVICTABLE_PCT: u8 = 50;
/// A prefix with at least this many keys, all of type `string`, whose
/// average size is under [`SMALL_STRING_MAX_AVG`], is a fold-into-Hash
/// candidate.
pub(super) const MANY_SMALL_MIN_KEYS: u64 = 1000;
pub(super) const SMALL_STRING_MAX_AVG: u64 = 200;
/// A prefix holding at least this share (%) of summed sampled prefix memory
/// is flagged as a hotspot (needs ≥2 prefixes to be meaningful).
pub(super) const DOMINANT_PREFIX_PCT: u8 = 60;
/// Fragmentation ratio above which the allocator is wasting enough to suggest
/// `activedefrag`/restart — paired with [`FRAG_FLOOR_BYTES`] so a tiny DB's
/// noisy ratio doesn't trip it.
pub(super) const FRAG_RATIO_WARN: f64 = 1.5;
/// Below this much absolute waste the ratio carries no signal (jemalloc's
/// fixed overhead dominates). Mirrors the chart's `FRAG_FLOOR_BYTES`.
pub(super) const FRAG_FLOOR_BYTES: u64 = 200 * 1024 * 1024;

/// Build the offline recommendation list from one analysis run. Pure over its
/// inputs (no Redis, no `cx`) so it is fully unit-testable. `prefix_rows` and
/// `biggest_keys` are the already-computed aggregates; `frag` is
/// `Some((ratio, waste_bytes))` when the status-bar heartbeat has a recent
/// fragmentation sample, else `None`.
pub(super) fn build_recommendations(
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
pub(super) fn latest_fragmentation(server_id: &str) -> Option<(f64, u64)> {
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

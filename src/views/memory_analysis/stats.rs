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

//! Scan-pipeline accumulators: per-prefix stats, bounded top-N
//! collections and the TTL histogram fed by the sampling loop.

use super::*;

#[derive(Default)]
pub(super) struct PrefixStats {
    pub(super) key_count: u64,
    pub(super) memory_bytes: u64,
    pub(super) types: std::collections::HashSet<String>,
    /// Sum of TTL values (only keys with TTL > 0).
    pub(super) ttl_sum: i64,
    /// Count of keys that have a TTL (TTL > 0).
    pub(super) ttl_count: u64,
    /// Count of keys with no expiry (TTL == -1).
    pub(super) perm_count: u64,
}

/// Keeps a capped top-N collection sorted by an i64 ranking descending.
/// Generic over both the row type and the ranking metric so we can run
/// parallel pickers for "biggest", "hottest", "coldest" off the same scan.
#[derive(Clone)]
pub(super) struct TopN<T> {
    pub(super) items: Vec<T>,
    pub(super) limit: usize,
    /// Minimum ranking score in the current list (for fast rejection).
    pub(super) min_score: i64,
}

impl<T> TopN<T> {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            items: Vec::with_capacity(limit + 1),
            limit,
            min_score: i64::MIN,
        }
    }

    /// Cheap pre-check before constructing a row. Avoids building keys we
    /// know would be evicted immediately.
    pub(super) fn should_insert(&self, score: i64) -> bool {
        self.items.len() < self.limit || score > self.min_score
    }

    pub(super) fn insert(&mut self, item: T, get_score: impl Fn(&T) -> i64) {
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

pub(super) fn build_prefix_rows(
    prefix_map: &HashMap<String, PrefixStats>,
    ratio: f32,
    key_separator: &str,
) -> Vec<PrefixRow> {
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
#[derive(Clone)]
pub(super) struct SingleKeyTopGroups {
    pub(super) by_size: TopN<SingleKeyRow>,
    pub(super) hottest: TopN<SingleKeyRow>,
    pub(super) coldest: TopN<SingleKeyRow>,
}

impl SingleKeyTopGroups {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            by_size: TopN::new(limit),
            hottest: TopN::new(limit),
            coldest: TopN::new(limit),
        }
    }

    pub(super) fn rows_for(&self, mode: SortMode) -> Vec<SingleKeyRow> {
        match mode {
            SortMode::Size => self.by_size.items.clone(),
            SortMode::Hot => self.hottest.items.clone(),
            SortMode::Cold => self.coldest.items.clone(),
        }
    }
}

// ─── Shared fold ─────────────────────────────────────────────────────────────

/// One key's contribution to the analysis accumulators.
pub(super) struct KeySample<'a> {
    pub(super) key: &'a str,
    pub(super) memory_bytes: u64,
    /// Remaining TTL in seconds; `-1` = no expiry.
    pub(super) ttl: i64,
    pub(super) key_type: &'a str,
    pub(super) heat: HeatMetric,
}

/// The accumulators every analysis source folds into — the online SCAN
/// loop and the offline RDB parse share [`add`](Self::add) so the two
/// pipelines can't drift apart.
pub(super) struct AnalysisAccumulators {
    pub(super) prefix_map: HashMap<String, PrefixStats>,
    pub(super) single_groups: SingleKeyTopGroups,
    pub(super) ttl_histogram: TtlHistogram,
}

impl AnalysisAccumulators {
    pub(super) fn new() -> Self {
        Self {
            prefix_map: HashMap::new(),
            single_groups: SingleKeyTopGroups::new(TOP_N),
            ttl_histogram: TtlHistogram::default(),
        }
    }

    /// Classify one key into the prefix map, the top-N groups, and the
    /// TTL histogram. Hot/Cold groups are only fed when the source has a
    /// heat probe (the RDB file never does).
    pub(super) fn add(&mut self, sample: KeySample, heat_probe: HeatProbe, key_separator: &str) {
        let KeySample {
            key,
            memory_bytes: memory,
            ttl,
            key_type,
            heat,
        } = sample;

        self.ttl_histogram.add(ttl);

        if let Some(pos) = key.find(key_separator) {
            let prefix = &key[..pos];
            let stats = self.prefix_map.entry(prefix.to_string()).or_default();
            stats.key_count += 1;
            stats.memory_bytes += memory;
            if ttl > 0 {
                stats.ttl_sum += ttl;
                stats.ttl_count += 1;
            } else if ttl == -1 {
                stats.perm_count += 1;
            }
            if !key_type.is_empty() && key_type != "none" {
                stats.types.insert(key_type.to_string());
            }
        }

        let memory_score = memory as i64;
        let heat_score = heat_sort_key(heat);
        let row_template = || SingleKeyRow {
            key: key.to_string().into(),
            memory_bytes: memory,
            memory: format_memory(memory).into(),
            key_type: SharedString::from(key_type.to_string()),
            ttl: format_ttl(ttl as f64).into(),
            ttl_secs: ttl,
            heat,
            heat_display: format_heat(heat),
        };
        if self.single_groups.by_size.should_insert(memory_score) {
            self.single_groups
                .by_size
                .insert(row_template(), |r| r.memory_bytes as i64);
        }
        if heat_probe != HeatProbe::None && heat != HeatMetric::None {
            if self.single_groups.hottest.should_insert(heat_score) {
                self.single_groups
                    .hottest
                    .insert(row_template(), |r| heat_sort_key(r.heat));
            }
            // Coldest = inverse score — flip the sign so the same
            // descending TopN logic gives us the bottom-N hot list.
            let cold_score = -heat_score;
            if self.single_groups.coldest.should_insert(cold_score) {
                self.single_groups
                    .coldest
                    .insert(row_template(), |r| -heat_sort_key(r.heat));
            }
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
pub(super) struct TtlHistogram {
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
    pub(super) fn add(&mut self, ttl_secs: i64) {
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
    pub(super) fn total(&self) -> u64 {
        self.lt_1m + self.lt_1h + self.lt_1d + self.lt_7d + self.gte_7d + self.no_ttl
    }
}

// ─── Analysis status ─────────────────────────────────────────────────────────

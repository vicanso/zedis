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

//! Pure benchmark configuration, percentile calculation, latency histogram
//! bucketing, and run-to-run comparison math.
//!
//! Kept in `zedis-core` so all statistical aggregation and comparison logic
//! is testable without GPUI or a live Redis connection.

pub use web_time::Instant;

pub const MIN_CONCURRENCY: usize = 1;
pub const MAX_CONCURRENCY: usize = 64;
pub const DEFAULT_CONCURRENCY: usize = 8;

pub const MIN_PAYLOAD_SIZE: usize = 1;
pub const MAX_PAYLOAD_SIZE: usize = 1_048_576;
pub const DEFAULT_PAYLOAD_SIZE: usize = 256;

pub const MIN_KEYSPACE_SIZE: usize = 1;
pub const MAX_KEYSPACE_SIZE: usize = 1_000_000;
pub const DEFAULT_KEYSPACE_SIZE: usize = 10_000;

pub const MIN_DURATION_SECS: u64 = 1;
pub const MAX_DURATION_SECS: u64 = 60;
pub const DEFAULT_DURATION_SECS: u64 = 5;

pub const MIN_PIPELINE: usize = 1;
pub const MAX_PIPELINE: usize = 64;
pub const DEFAULT_PIPELINE: usize = 1;

pub const PIPELINE_CHOICES: [usize; 5] = [1, 4, 8, 16, 64];

/// Supported command presets for the built-in benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BenchmarkPreset {
    #[default]
    Set,
    Get,
    Incr,
    Lpush,
    Hset,
    Zadd,
}

impl BenchmarkPreset {
    pub const ALL: [BenchmarkPreset; 6] = [
        BenchmarkPreset::Set,
        BenchmarkPreset::Get,
        BenchmarkPreset::Incr,
        BenchmarkPreset::Lpush,
        BenchmarkPreset::Hset,
        BenchmarkPreset::Zadd,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            BenchmarkPreset::Set => "SET",
            BenchmarkPreset::Get => "GET",
            BenchmarkPreset::Incr => "INCR",
            BenchmarkPreset::Lpush => "LPUSH",
            BenchmarkPreset::Hset => "HSET",
            BenchmarkPreset::Zadd => "ZADD",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "SET" => Some(BenchmarkPreset::Set),
            "GET" => Some(BenchmarkPreset::Get),
            "INCR" => Some(BenchmarkPreset::Incr),
            "LPUSH" => Some(BenchmarkPreset::Lpush),
            "HSET" => Some(BenchmarkPreset::Hset),
            "ZADD" => Some(BenchmarkPreset::Zadd),
            _ => None,
        }
    }

    /// Whether the preset sends or reads a configurable byte payload (`INCR`
    /// operates on 64-bit integer counters and ignores payload size).
    pub const fn uses_payload(self) -> bool {
        !matches!(self, BenchmarkPreset::Incr)
    }
}

/// User-configurable parameters for a benchmark run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkConfig {
    pub preset: BenchmarkPreset,
    pub concurrency: usize,
    pub payload_size: usize,
    pub keyspace_size: usize,
    pub duration_secs: u64,
    pub pipeline: usize,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            preset: BenchmarkPreset::Set,
            concurrency: DEFAULT_CONCURRENCY,
            payload_size: DEFAULT_PAYLOAD_SIZE,
            keyspace_size: DEFAULT_KEYSPACE_SIZE,
            duration_secs: DEFAULT_DURATION_SECS,
            pipeline: DEFAULT_PIPELINE,
        }
    }
}

impl BenchmarkConfig {
    /// Returns a copy with every parameter clamped to its safe operating range.
    pub fn sanitized(&self) -> Self {
        Self {
            preset: self.preset,
            concurrency: self.concurrency.clamp(MIN_CONCURRENCY, MAX_CONCURRENCY),
            payload_size: self.payload_size.clamp(MIN_PAYLOAD_SIZE, MAX_PAYLOAD_SIZE),
            keyspace_size: self.keyspace_size.clamp(MIN_KEYSPACE_SIZE, MAX_KEYSPACE_SIZE),
            duration_secs: self.duration_secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS),
            pipeline: self.pipeline.clamp(MIN_PIPELINE, MAX_PIPELINE),
        }
    }

    /// Short human-readable summary of the configuration (e.g.
    /// `SET · c=8 · P=1 · 256B · 10000 keys · 5s`).
    pub fn summary_label(&self) -> String {
        let cfg = self.sanitized();
        if cfg.preset.uses_payload() {
            format!(
                "{} · c={} · P={} · {}B · {} keys · {}s",
                cfg.preset.label(),
                cfg.concurrency,
                cfg.pipeline,
                cfg.payload_size,
                cfg.keyspace_size,
                cfg.duration_secs
            )
        } else {
            format!(
                "{} · c={} · P={} · {} keys · {}s",
                cfg.preset.label(),
                cfg.concurrency,
                cfg.pipeline,
                cfg.keyspace_size,
                cfg.duration_secs
            )
        }
    }

    /// Preview line shown in the dangerous-command confirmation dialog.
    pub fn command_preview(&self, prefix: &str) -> String {
        let cfg = self.sanitized();
        format!(
            "redis-benchmark -t {} -c {} -P {} -d {} -r {} --duration {}s (prefix {}*)",
            cfg.preset.label().to_ascii_lowercase(),
            cfg.concurrency,
            cfg.pipeline,
            cfg.payload_size,
            cfg.keyspace_size,
            cfg.duration_secs,
            prefix
        )
    }
}

/// One bucket in the latency distribution histogram.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencyBucket {
    pub label: String,
    /// Upper bound in microseconds (`None` for the open-ended tail bucket).
    pub upper_us: Option<u64>,
    pub count: u64,
    /// Share of total recorded latency samples (`0.0..=100.0`).
    pub pct: f64,
}

/// One time-series sample captured during a benchmark run.
#[derive(Debug, Clone, PartialEq)]
pub struct ThroughputPoint {
    pub elapsed_ms: u64,
    pub ops_per_sec: f64,
    pub p99_us: u64,
}

/// Live progress snapshot emitted periodically while a benchmark is running.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkProgress {
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub total_ops: u64,
    pub current_ops_per_sec: f64,
    pub current_p99_us: u64,
}

/// Final aggregated metrics for a completed (or cancelled) benchmark run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BenchmarkSummary {
    pub total_ops: u64,
    pub total_batches: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub ops_per_sec: f64,
    pub min_us: u64,
    pub avg_us: f64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub buckets: Vec<LatencyBucket>,
    pub timeline: Vec<ThroughputPoint>,
    pub cleaned_keys: u64,
    pub cancelled: bool,
}

/// Relative delta (`(candidate - baseline) / baseline * 100.0`) between two
/// benchmark summaries.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BenchmarkComparison {
    /// Positive means candidate achieved higher throughput.
    pub ops_per_sec_delta_pct: f64,
    /// Negative means candidate achieved lower average latency.
    pub avg_us_delta_pct: f64,
    /// Negative means candidate achieved lower p50 latency.
    pub p50_us_delta_pct: f64,
    /// Negative means candidate achieved lower p95 latency.
    pub p95_us_delta_pct: f64,
    /// Negative means candidate achieved lower p99 latency.
    pub p99_us_delta_pct: f64,
}

const SUB_MS_HISTOGRAM_BOUNDS_US: &[(Option<u64>, &str)] = &[
    (Some(25), "≤25µs"),
    (Some(50), "25–50µs"),
    (Some(75), "50–75µs"),
    (Some(100), "75–100µs"),
    (Some(150), "100–150µs"),
    (Some(250), "150–250µs"),
    (Some(500), "250–500µs"),
    (None, ">500µs"),
];

const STANDARD_HISTOGRAM_BOUNDS_US: &[(Option<u64>, &str)] = &[
    (Some(100), "≤0.1ms"),
    (Some(250), "0.1–0.25ms"),
    (Some(500), "0.25–0.5ms"),
    (Some(1_000), "0.5–1ms"),
    (Some(2_000), "1–2ms"),
    (Some(5_000), "2–5ms"),
    (Some(10_000), "5–10ms"),
    (None, ">10ms"),
];

const HIGH_LATENCY_HISTOGRAM_BOUNDS_US: &[(Option<u64>, &str)] = &[
    (Some(500), "≤0.5ms"),
    (Some(1_000), "0.5–1ms"),
    (Some(2_000), "1–2ms"),
    (Some(5_000), "2–5ms"),
    (Some(10_000), "5–10ms"),
    (Some(25_000), "10–25ms"),
    (Some(50_000), "25–50ms"),
    (None, ">50ms"),
];

/// Nearest-rank percentile on an already-sorted slice of microsecond samples.
pub fn percentile_sorted(sorted_us: &[u64], quantile: f64) -> u64 {
    if sorted_us.is_empty() {
        return 0;
    }
    let q = quantile.clamp(0.0, 1.0);
    let rank = ((sorted_us.len() as f64) * q).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted_us.len() - 1);
    sorted_us[idx]
}

fn select_histogram_bounds(latencies_us: &[u64]) -> &'static [(Option<u64>, &'static str)] {
    if latencies_us.is_empty() {
        return STANDARD_HISTOGRAM_BOUNDS_US;
    }
    let is_sorted = latencies_us.windows(2).all(|w| w[0] <= w[1]);
    let p95 = if is_sorted {
        percentile_sorted(latencies_us, 0.95)
    } else {
        let mut copy = latencies_us.to_vec();
        copy.sort_unstable();
        percentile_sorted(&copy, 0.95)
    };
    if p95 <= 250 {
        SUB_MS_HISTOGRAM_BOUNDS_US
    } else if p95 <= 10_000 {
        STANDARD_HISTOGRAM_BOUNDS_US
    } else {
        HIGH_LATENCY_HISTOGRAM_BOUNDS_US
    }
}

/// Bucket microsecond latency samples into an adaptive 8-bucket latency
/// histogram (sub-millisecond `≤25µs..>500µs`, standard `≤0.1ms..>10ms`, or
/// high-latency `≤0.5ms..>50ms` based on the sample distribution's p95).
pub fn build_latency_histogram(latencies_us: &[u64]) -> Vec<LatencyBucket> {
    let bounds = select_histogram_bounds(latencies_us);
    let mut counts = vec![0u64; bounds.len()];
    for &sample in latencies_us {
        let slot = bounds
            .iter()
            .position(|(upper, _)| upper.is_none_or(|u| sample <= u))
            .unwrap_or(bounds.len() - 1);
        counts[slot] += 1;
    }
    let total = latencies_us.len() as f64;
    bounds
        .iter()
        .zip(counts)
        .map(|(&(upper_us, label), count)| LatencyBucket {
            label: label.to_string(),
            upper_us,
            count,
            pct: if total > 0.0 {
                (count as f64 / total) * 100.0
            } else {
                0.0
            },
        })
        .collect()
}

/// Input data for [`summarize_benchmark`].
pub struct BenchmarkRawOutcome {
    pub latencies_us: Vec<u64>,
    pub total_ops: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub timeline: Vec<ThroughputPoint>,
    pub cleaned_keys: u64,
    pub cancelled: bool,
}

/// Compute percentiles, throughput, and histogram buckets from raw latency
/// measurements.
pub fn summarize_benchmark(raw: BenchmarkRawOutcome) -> BenchmarkSummary {
    let mut sorted = raw.latencies_us;
    sorted.sort_unstable();

    let total_batches = sorted.len() as u64;
    let (min_us, max_us, avg_us, p50_us, p95_us, p99_us) = if sorted.is_empty() {
        (0, 0, 0.0, 0, 0, 0)
    } else {
        let sum: u128 = sorted.iter().map(|&v| u128::from(v)).sum();
        let avg = (sum as f64) / (sorted.len() as f64);
        (
            sorted[0],
            sorted[sorted.len() - 1],
            avg,
            percentile_sorted(&sorted, 0.50),
            percentile_sorted(&sorted, 0.95),
            percentile_sorted(&sorted, 0.99),
        )
    };

    let buckets = build_latency_histogram(&sorted);
    let ops_per_sec = if raw.elapsed_ms > 0 {
        (raw.total_ops as f64) * 1000.0 / (raw.elapsed_ms as f64)
    } else {
        0.0
    };

    BenchmarkSummary {
        total_ops: raw.total_ops,
        total_batches,
        errors: raw.errors,
        elapsed_ms: raw.elapsed_ms,
        ops_per_sec,
        min_us,
        avg_us,
        p50_us,
        p95_us,
        p99_us,
        max_us,
        buckets,
        timeline: raw.timeline,
        cleaned_keys: raw.cleaned_keys,
        cancelled: raw.cancelled,
    }
}

fn delta_pct(baseline: f64, candidate: f64) -> f64 {
    if baseline.abs() < f64::EPSILON {
        0.0
    } else {
        ((candidate - baseline) / baseline) * 100.0
    }
}

/// Compare `candidate` against `baseline`, returning percentage changes.
pub fn compare_summaries(baseline: &BenchmarkSummary, candidate: &BenchmarkSummary) -> BenchmarkComparison {
    BenchmarkComparison {
        ops_per_sec_delta_pct: delta_pct(baseline.ops_per_sec, candidate.ops_per_sec),
        avg_us_delta_pct: delta_pct(baseline.avg_us, candidate.avg_us),
        p50_us_delta_pct: delta_pct(baseline.p50_us as f64, candidate.p50_us as f64),
        p95_us_delta_pct: delta_pct(baseline.p95_us as f64, candidate.p95_us as f64),
        p99_us_delta_pct: delta_pct(baseline.p99_us as f64, candidate.p99_us as f64),
    }
}

/// Format a microsecond latency value as a compact string (`142 µs` or `1.28 ms`).
pub fn format_latency_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us} µs")
    } else {
        format!("{:.2} ms", us as f64 / 1_000.0)
    }
}

/// Format a floating-point microsecond latency value (`142.5 µs` or `1.28 ms`).
pub fn format_latency_f64_us(us: f64) -> String {
    if us < 1_000.0 {
        format!("{us:.1} µs")
    } else {
        format!("{:.2} ms", us / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_roundtrips_and_payload_flag() {
        for preset in BenchmarkPreset::ALL {
            assert_eq!(BenchmarkPreset::from_name(preset.label()), Some(preset));
            assert_eq!(
                BenchmarkPreset::from_name(&preset.label().to_ascii_lowercase()),
                Some(preset)
            );
        }
        assert_eq!(BenchmarkPreset::from_name("unknown"), None);
        assert!(BenchmarkPreset::Set.uses_payload());
        assert!(BenchmarkPreset::Get.uses_payload());
        assert!(!BenchmarkPreset::Incr.uses_payload());
    }

    #[test]
    fn config_sanitizes_out_of_range_knobs() {
        let raw = BenchmarkConfig {
            preset: BenchmarkPreset::Hset,
            concurrency: 0,
            payload_size: 99_999_999,
            keyspace_size: 0,
            duration_secs: 500,
            pipeline: 0,
        };
        let clean = raw.sanitized();
        assert_eq!(clean.concurrency, MIN_CONCURRENCY);
        assert_eq!(clean.payload_size, MAX_PAYLOAD_SIZE);
        assert_eq!(clean.keyspace_size, MIN_KEYSPACE_SIZE);
        assert_eq!(clean.duration_secs, MAX_DURATION_SECS);
        assert_eq!(clean.pipeline, MIN_PIPELINE);
    }

    #[test]
    fn percentiles_and_summary_handle_empty_and_populated_samples() {
        let empty = summarize_benchmark(BenchmarkRawOutcome {
            latencies_us: Vec::new(),
            total_ops: 0,
            errors: 0,
            elapsed_ms: 0,
            timeline: Vec::new(),
            cleaned_keys: 0,
            cancelled: false,
        });
        assert_eq!(empty.total_ops, 0);
        assert_eq!(empty.ops_per_sec, 0.0);
        assert_eq!(empty.p50_us, 0);
        assert_eq!(empty.p99_us, 0);
        assert_eq!(empty.buckets.len(), 8);
        assert!(empty.buckets.iter().all(|b| b.count == 0 && b.pct == 0.0));

        let samples: Vec<u64> = (1..=100).map(|i| i * 100).collect(); // 100us .. 10_000us
        let summary = summarize_benchmark(BenchmarkRawOutcome {
            latencies_us: samples,
            total_ops: 400,
            errors: 2,
            elapsed_ms: 2_000,
            timeline: vec![ThroughputPoint {
                elapsed_ms: 1_000,
                ops_per_sec: 200.0,
                p99_us: 9_900,
            }],
            cleaned_keys: 100,
            cancelled: false,
        });
        assert_eq!(summary.total_ops, 400);
        assert_eq!(summary.total_batches, 100);
        assert_eq!(summary.errors, 2);
        assert!((summary.ops_per_sec - 200.0).abs() < 1e-6);
        assert_eq!(summary.min_us, 100);
        assert_eq!(summary.max_us, 10_000);
        assert_eq!(summary.p50_us, 5_000);
        assert_eq!(summary.p95_us, 9_500);
        assert_eq!(summary.p99_us, 9_900);
        let bucket_sum: u64 = summary.buckets.iter().map(|b| b.count).sum();
        assert_eq!(bucket_sum, 100);
    }

    #[test]
    fn comparison_computes_expected_percentage_deltas() {
        let base = summarize_benchmark(BenchmarkRawOutcome {
            latencies_us: vec![200, 400, 600],
            total_ops: 1_000,
            errors: 0,
            elapsed_ms: 1_000,
            timeline: Vec::new(),
            cleaned_keys: 10,
            cancelled: false,
        });
        let cand = summarize_benchmark(BenchmarkRawOutcome {
            latencies_us: vec![100, 200, 300],
            total_ops: 1_500,
            errors: 0,
            elapsed_ms: 1_000,
            timeline: Vec::new(),
            cleaned_keys: 10,
            cancelled: false,
        });
        let cmp = compare_summaries(&base, &cand);
        assert!((cmp.ops_per_sec_delta_pct - 50.0).abs() < 1e-6);
        assert!((cmp.avg_us_delta_pct - (-50.0)).abs() < 1e-6);
        assert!((cmp.p50_us_delta_pct - (-50.0)).abs() < 1e-6);
    }

    #[test]
    fn adaptive_histogram_selects_sub_ms_standard_and_wan_tiers() {
        // Localhost sub-millisecond latencies (p95 <= 250us) -> sub-ms buckets
        let sub_ms = build_latency_histogram(&[20, 35, 60, 90, 140, 210]);
        assert_eq!(sub_ms[0].label, "≤25µs");
        assert_eq!(sub_ms[7].label, ">500µs");
        assert_eq!(sub_ms.iter().filter(|b| b.count > 0).count(), 6);

        // Standard LAN / pipelined latencies (250us < p95 <= 10ms) -> standard buckets
        let standard = build_latency_histogram(&[90, 210, 480, 950, 2_500]);
        assert_eq!(standard[0].label, "≤0.1ms");
        assert_eq!(standard[7].label, ">10ms");

        // High-latency WAN / cloud latencies (p95 > 10ms) -> WAN buckets
        let wan = build_latency_histogram(&[8_000, 12_000, 18_000, 32_000, 65_000]);
        assert_eq!(wan[0].label, "≤0.5ms");
        assert_eq!(wan[7].label, ">50ms");
        assert_eq!(wan.iter().filter(|b| b.count > 0).count(), 4);
    }
}

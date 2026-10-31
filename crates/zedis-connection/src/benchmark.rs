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

//! Built-in Redis benchmark runner (`GET`, `SET`, `INCR`, `LPUSH`, `HSET`,
//! `ZADD`, with configurable pipeline size, concurrency, payload size,
//! keyspace size, and duration).
//!
//! Every benchmark run writes only under a random throwaway key prefix
//! (`zedis:bench:<id>:`) and deletes all touched keys before returning —
//! including when cancelled mid-run.

use crate::bridge::BRIDGE_MAX_VALUE_BYTES;
#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::keyspace::delete_keys;
use crate::server_db::ServerDb;
use futures::future::join_all;
use redis::{Cmd, Pipeline, Value, cmd, pipe};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use uuid::Uuid;
use zedis_core::benchmark::{
    BenchmarkConfig, BenchmarkPreset, BenchmarkProgress, BenchmarkRawOutcome, BenchmarkSummary, Instant,
    ThroughputPoint, percentile_sorted, summarize_benchmark,
};

type Result<T, E = Error> = std::result::Result<T, E>;

const PROGRESS_INTERVAL_MS: u64 = 200;
const CLEANUP_BATCH_SIZE: usize = 1_000;
const MAX_GET_SEED_KEYS: usize = 256;
const MAX_GET_SEED_BYTES: usize = 8 * 1024 * 1024;

/// Completed (or cancelled) benchmark run with its configuration, throwaway
/// prefix, and statistical summary.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkRunResult {
    pub prefix: String,
    pub config: BenchmarkConfig,
    pub summary: BenchmarkSummary,
}

/// Generate a fresh random throwaway prefix (`zedis:bench:<12-hex>:`) for a
/// single benchmark run.
pub fn new_benchmark_prefix() -> String {
    let raw = Uuid::now_v7().simple().to_string();
    let short = &raw[raw.len().saturating_sub(12)..];
    format!("zedis:bench:{short}:")
}

/// Deterministic key name for index `index` under `prefix`.
///
/// When `pipeline > 1`, keys carry a 16-way cluster hash tag (`{s0}`..`{s15}`)
/// so every command inside a pipelined batch hashes to the same slot on
/// Redis Cluster while still spreading load across up to 16 slots.
pub fn benchmark_key_name(prefix: &str, pipeline: usize, keyspace_size: usize, index: usize) -> String {
    if pipeline <= 1 {
        format!("{prefix}{index}")
    } else {
        let shards = keyspace_size.clamp(1, 16);
        let shard = index % shards;
        format!("{prefix}{{s{shard}}}:{index}")
    }
}

/// Effective keyspace cardinality for a run: `GET` benchmarks pre-seed at most
/// [`MAX_GET_SEED_KEYS`] (and [`MAX_GET_SEED_BYTES`]) throwaway keys and read
/// exclusively from that seeded keyspace so every `GET` transfers a full
/// `payload_size`-byte reply rather than `Nil`.
fn effective_benchmark_keyspace(cfg: &BenchmarkConfig) -> usize {
    if cfg.preset == BenchmarkPreset::Get {
        let max_by_bytes = (MAX_GET_SEED_BYTES / cfg.payload_size.max(1)).clamp(16, MAX_GET_SEED_KEYS);
        cfg.keyspace_size.min(max_by_bytes).max(1)
    } else {
        cfg.keyspace_size.max(1)
    }
}

fn build_single_cmd(preset: BenchmarkPreset, key: &str, payload: &[u8], seq: usize) -> Cmd {
    match preset {
        BenchmarkPreset::Set => {
            let mut c = cmd("SET");
            c.arg(key).arg(payload);
            c
        }
        BenchmarkPreset::Get => {
            let mut c = cmd("GET");
            c.arg(key);
            c
        }
        BenchmarkPreset::Incr => {
            let mut c = cmd("INCR");
            c.arg(key);
            c
        }
        BenchmarkPreset::Lpush => {
            let mut c = cmd("LPUSH");
            c.arg(key).arg(payload);
            c
        }
        BenchmarkPreset::Hset => {
            let mut c = cmd("HSET");
            c.arg(key).arg(format!("f{}", seq % 16)).arg(payload);
            c
        }
        BenchmarkPreset::Zadd => {
            let mut c = cmd("ZADD");
            c.arg(key).arg(seq as f64).arg(payload);
            c
        }
    }
}

fn append_pipeline_cmd(p: &mut Pipeline, preset: BenchmarkPreset, key: &str, payload: &[u8], seq: usize) {
    p.add_command(build_single_cmd(preset, key, payload, seq));
}

#[cfg(not(target_family = "wasm"))]
async fn open_worker_connection(at: &ServerDb, fallback: &RedisAsyncConn) -> RedisAsyncConn {
    match at.dedicated_connection().await {
        Ok(conn) => conn,
        Err(_) => at.connection().await.unwrap_or_else(|_| fallback.clone()),
    }
}

#[cfg(target_family = "wasm")]
async fn open_worker_connection(at: &ServerDb, fallback: &RedisAsyncConn) -> RedisAsyncConn {
    at.connection().await.unwrap_or_else(|_| fallback.clone())
}

/// Pre-seed all `effective_keyspace` throwaway keys so `GET` benchmarks
/// transfer `payload_size`-byte replies rather than nil responses.
async fn seed_get_keys(
    conn: &mut RedisAsyncConn,
    prefix: &str,
    pipeline: usize,
    effective_keyspace: usize,
    payload: &[u8],
    touched: &[AtomicBool],
    cancel: &AtomicBool,
) -> Result<()> {
    if pipeline > 1 {
        let shards = effective_keyspace.clamp(1, 16);
        for shard in 0..shards {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let indices: Vec<usize> = (0..effective_keyspace).filter(|idx| idx % shards == shard).collect();
            for chunk in indices.chunks(64) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let mut p = pipe();
                for &idx in chunk {
                    let key = benchmark_key_name(prefix, pipeline, effective_keyspace, idx);
                    touched[idx].store(true, Ordering::Relaxed);
                    p.add_command(build_single_cmd(BenchmarkPreset::Set, &key, payload, idx));
                }
                let _: Vec<Value> = p.query_async(conn).await?;
            }
        }
    } else {
        for (idx, flag) in touched.iter().enumerate().take(effective_keyspace) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let key = benchmark_key_name(prefix, 1, effective_keyspace, idx);
            flag.store(true, Ordering::Relaxed);
            let _: Value = cmd("SET").arg(&key).arg(payload).query_async(conn).await?;
        }
    }
    Ok(())
}

/// Delete all throwaway keys marked in `touched` in chunks of
/// [`CLEANUP_BATCH_SIZE`].
async fn cleanup_touched_keys(
    at: &ServerDb,
    prefix: &str,
    pipeline: usize,
    keyspace_size: usize,
    touched: &[AtomicBool],
) -> u64 {
    let keys: Vec<String> = touched
        .iter()
        .enumerate()
        .filter(|&(_, flag)| flag.load(Ordering::Relaxed))
        .map(|(idx, _)| benchmark_key_name(prefix, pipeline, keyspace_size, idx))
        .collect();
    if keys.is_empty() {
        return 0;
    }
    let mut deleted_total: u64 = 0;
    for chunk in keys.chunks(CLEANUP_BATCH_SIZE) {
        if delete_keys(at, chunk.to_vec()).await.is_ok() {
            deleted_total = deleted_total.saturating_add(chunk.len() as u64);
        }
    }
    deleted_total
}

/// Execute a benchmark run with the given [`BenchmarkConfig`] and [`prefix`](new_benchmark_prefix),
/// reporting periodic progress via `on_progress` and cleaning up all
/// throwaway keys before returning.
pub async fn run_benchmark_with_prefix(
    at: &ServerDb,
    config: &BenchmarkConfig,
    prefix: String,
    cancel: &AtomicBool,
    on_progress: impl FnMut(BenchmarkProgress),
) -> Result<BenchmarkRunResult> {
    let mut cfg = config.sanitized();
    let mut primary_conn = at.connection().await?;
    if primary_conn.as_bridge().is_some() {
        let max_bridge_payload = (BRIDGE_MAX_VALUE_BYTES / 2 / cfg.pipeline.max(1)).max(1);
        cfg.payload_size = cfg.payload_size.min(max_bridge_payload);
    }

    let effective_keyspace = effective_benchmark_keyspace(&cfg);
    let payload: Vec<u8> = vec![b'x'; cfg.payload_size];
    let touched: Vec<AtomicBool> = (0..effective_keyspace).map(|_| AtomicBool::new(false)).collect();

    if cfg.preset == BenchmarkPreset::Get
        && let Err(e) = seed_get_keys(
            &mut primary_conn,
            &prefix,
            cfg.pipeline,
            effective_keyspace,
            &payload,
            &touched,
            cancel,
        )
        .await
    {
        let _ = cleanup_touched_keys(at, &prefix, cfg.pipeline, effective_keyspace, &touched).await;
        return Err(e);
    }

    let mut connections = Vec::with_capacity(cfg.concurrency);
    for worker_idx in 1..cfg.concurrency {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let conn = if worker_idx == 0 {
            primary_conn.clone()
        } else {
            open_worker_connection(at, &primary_conn).await
        };
        connections.push(conn);
    }
    connections.insert(0, primary_conn);

    let duration_ms = cfg.duration_secs.saturating_mul(1_000);
    let deadline = Duration::from_secs(cfg.duration_secs);
    let total_ops = AtomicU64::new(0);
    let total_errors = AtomicU64::new(0);
    let next_sample_ms = AtomicU64::new(PROGRESS_INTERVAL_MS);
    let recent_latencies: Mutex<Vec<u64>> = Mutex::new(Vec::with_capacity(512));
    // Holds `(last_sample_ms, last_sample_ops, timeline_points)` so each
    // timeline point records windowed throughput for its sampling interval.
    let timeline: Mutex<(u64, u64, Vec<ThroughputPoint>)> = Mutex::new((0, 0, Vec::new()));
    let progress_cb = Mutex::new(on_progress);

    let start = Instant::now();
    let payload_ref = payload.as_slice();
    let touched_ref = touched.as_slice();
    let prefix_ref = prefix.as_str();
    let total_ops_ref = &total_ops;
    let total_errors_ref = &total_errors;
    let next_sample_ref = &next_sample_ms;
    let recent_ref = &recent_latencies;
    let timeline_ref = &timeline;
    let progress_ref = &progress_cb;
    let cfg_ref = &cfg;
    let mark_writes = cfg.preset != BenchmarkPreset::Get;

    let workers = connections
        .into_iter()
        .enumerate()
        .map(|(worker_id, mut conn)| async move {
            let mut local_latencies: Vec<u64> = Vec::with_capacity(2_048);
            let mut batch_seq: usize = worker_id.wrapping_mul(997);

            while !cancel.load(Ordering::Relaxed) && start.elapsed() < deadline {
                let batch_start = Instant::now();
                let outcome: Result<()> = if cfg_ref.pipeline <= 1 {
                    let idx = batch_seq % effective_keyspace;
                    if mark_writes {
                        touched_ref[idx].store(true, Ordering::Relaxed);
                    }
                    let key = benchmark_key_name(prefix_ref, 1, effective_keyspace, idx);
                    let c = build_single_cmd(cfg_ref.preset, &key, payload_ref, batch_seq);
                    c.query_async::<Value>(&mut conn).await.map(|_| ()).map_err(Error::from)
                } else {
                    let shards = effective_keyspace.clamp(1, 16);
                    let shard = batch_seq % shards;
                    let count_in_shard = (effective_keyspace - 1 - shard) / shards + 1;
                    let mut p = pipe();
                    for i in 0..cfg_ref.pipeline {
                        let step = batch_seq.wrapping_mul(cfg_ref.pipeline).wrapping_add(i);
                        let offset = step % count_in_shard;
                        let idx = shard + offset * shards;
                        if mark_writes {
                            touched_ref[idx].store(true, Ordering::Relaxed);
                        }
                        let key = benchmark_key_name(prefix_ref, cfg_ref.pipeline, effective_keyspace, idx);
                        append_pipeline_cmd(&mut p, cfg_ref.preset, &key, payload_ref, step);
                    }
                    p.query_async::<Vec<Value>>(&mut conn)
                        .await
                        .map(|_| ())
                        .map_err(Error::from)
                };

                let batch_us = (batch_start.elapsed().as_micros() as u64).max(1);
                match outcome {
                    Ok(()) => {
                        local_latencies.push(batch_us);
                        let ops_now = total_ops_ref
                            .fetch_add(cfg_ref.pipeline as u64, Ordering::Relaxed)
                            .saturating_add(cfg_ref.pipeline as u64);
                        if let Ok(mut recent) = recent_ref.try_lock()
                            && recent.len() < 2_048
                        {
                            recent.push(batch_us);
                        }

                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        let target_ms = next_sample_ref.load(Ordering::Relaxed);
                        if elapsed_ms >= target_ms
                            && next_sample_ref
                                .compare_exchange(
                                    target_ms,
                                    elapsed_ms.saturating_add(PROGRESS_INTERVAL_MS),
                                    Ordering::Relaxed,
                                    Ordering::Relaxed,
                                )
                                .is_ok()
                        {
                            let current_p99_us = recent_ref
                                .lock()
                                .ok()
                                .map(|mut guard| {
                                    if guard.is_empty() {
                                        batch_us
                                    } else {
                                        guard.sort_unstable();
                                        let p99 = percentile_sorted(&guard, 0.99);
                                        guard.clear();
                                        p99
                                    }
                                })
                                .unwrap_or(batch_us);
                            let mut window_ops_per_sec = if elapsed_ms > 0 {
                                (ops_now as f64) * 1000.0 / (elapsed_ms as f64)
                            } else {
                                0.0
                            };
                            if let Ok(mut tl_guard) = timeline_ref.lock() {
                                let (prev_ms, prev_ops, ref mut tl) = *tl_guard;
                                let dt_ms = elapsed_ms.saturating_sub(prev_ms).max(1);
                                let d_ops = ops_now.saturating_sub(prev_ops);
                                window_ops_per_sec = (d_ops as f64) * 1000.0 / (dt_ms as f64);
                                tl.push(ThroughputPoint {
                                    elapsed_ms,
                                    ops_per_sec: window_ops_per_sec,
                                    p99_us: current_p99_us,
                                });
                                tl_guard.0 = elapsed_ms;
                                tl_guard.1 = ops_now;
                            }
                            if let Ok(mut cb) = progress_ref.lock() {
                                cb(BenchmarkProgress {
                                    elapsed_ms,
                                    duration_ms,
                                    total_ops: ops_now,
                                    current_ops_per_sec: window_ops_per_sec,
                                    current_p99_us,
                                });
                            }
                        }
                    }
                    Err(_) => {
                        total_errors_ref.fetch_add(1, Ordering::Relaxed);
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }

                batch_seq = batch_seq.wrapping_add(1);
            }

            local_latencies
        });

    let worker_latencies = join_all(workers).await;
    let elapsed_ms = (start.elapsed().as_millis() as u64).max(1);
    let cancelled = cancel.load(Ordering::Relaxed);

    let mut all_latencies: Vec<u64> = Vec::new();
    for mut vec in worker_latencies {
        all_latencies.append(&mut vec);
    }

    let cleaned_keys = cleanup_touched_keys(at, &prefix, cfg.pipeline, effective_keyspace, &touched).await;

    let mut timeline_points = timeline.into_inner().map(|(_, _, pts)| pts).unwrap_or_default();
    let final_ops = total_ops.load(Ordering::Relaxed);
    if timeline_points.is_empty() && final_ops > 0 {
        let mut sorted_copy = all_latencies.clone();
        sorted_copy.sort_unstable();
        timeline_points.push(ThroughputPoint {
            elapsed_ms,
            ops_per_sec: (final_ops as f64) * 1000.0 / (elapsed_ms as f64),
            p99_us: percentile_sorted(&sorted_copy, 0.99),
        });
    }

    let summary = summarize_benchmark(BenchmarkRawOutcome {
        latencies_us: all_latencies,
        total_ops: final_ops,
        errors: total_errors.load(Ordering::Relaxed),
        elapsed_ms,
        timeline: timeline_points,
        cleaned_keys,
        cancelled,
    });

    Ok(BenchmarkRunResult {
        prefix,
        config: cfg,
        summary,
    })
}

/// Convenience wrapper around [`run_benchmark_with_prefix`] that generates a
/// fresh random throwaway prefix automatically.
pub async fn run_benchmark(
    at: &ServerDb,
    config: &BenchmarkConfig,
    cancel: &AtomicBool,
    on_progress: impl FnMut(BenchmarkProgress),
) -> Result<BenchmarkRunResult> {
    run_benchmark_with_prefix(at, config, new_benchmark_prefix(), cancel, on_progress).await
}

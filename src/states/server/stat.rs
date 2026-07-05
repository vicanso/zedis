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

use crate::connection::{get_connection_manager, get_server};
use crate::db::{insert_metrics_sample, list_metrics_samples, prune_metrics_history};
use crate::helpers::{unix_ts, unix_ts_millis};
use crate::states::{ConnectionErrorKind, ConnectionHealth, ServerEvent, ServerTask, ZedisServerState};
use gpui::SharedString;
use gpui::prelude::*;
use parking_lot::RwLock;
use redis::cmd;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;
use std::time::Instant;
use tracing::{debug, error, warn};

#[derive(Debug, Default, Clone)]
pub struct RedisKeySpaceStats {
    pub keys: u64,
    pub expires: u64,
    pub avg_ttl: u64,
}

#[derive(Debug, Default, Clone)]
pub struct RedisServerMeta {
    pub redis_version: String,
    pub os: String,
    pub role: String,
    pub maxmemory: u64,
}

// Serialized as JSON into the `metrics_history` redb table; `serde(default)`
// keeps old persisted samples readable when new fields are added.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisMetrics {
    pub timestamp_ms: i64,
    pub latency_ms: u64,
    // --- Clients ---
    pub connected_clients: u64,
    pub rejected_connections: u64,
    pub blocked_clients: u64,

    // --- Memory ---
    pub used_memory: u64,
    pub used_memory_rss: u64,
    pub mem_fragmentation_ratio: f64,

    // --- Stats ---
    pub total_connections_received: u64,
    pub total_commands_processed: u64,
    pub instantaneous_ops_per_sec: u64,
    pub instantaneous_input_kbps: f64,
    pub instantaneous_output_kbps: f64,
    pub keyspace_hits: u64,
    pub keyspace_misses: u64,
    pub expired_keys: u64,
    pub evicted_keys: u64,

    // --- CPU ---
    pub used_cpu_sys: f64,
    pub used_cpu_user: f64,

    pub rdb_last_bgsave_success: bool,
    pub aof_last_write_success: bool,

    // --- Persistence (RDB) ---
    /// Number of writes accumulated since the last successful RDB save.
    pub rdb_changes_since_last_save: u64,
    /// Unix timestamp (seconds) of the last successful RDB save.
    /// `0` when the server has never persisted to disk in this session.
    pub rdb_last_save_time: i64,
    /// True while a `BGSAVE` fork is running. Used to disable the
    /// "Save snapshot" button so users can't spam parallel forks.
    pub rdb_bgsave_in_progress: bool,
    /// Duration (seconds) of the most recent BGSAVE. `-1` if it has
    /// never run in this session.
    pub rdb_last_bgsave_time_sec: i64,
    /// Elapsed seconds of the currently running BGSAVE, or `-1` when idle.
    pub rdb_current_bgsave_time_sec: i64,

    // --- Persistence (AOF) ---
    /// Whether `appendonly` is enabled. When `false` we hide the
    /// BGREWRITEAOF action entirely (see UI plan).
    pub aof_enabled: bool,
    /// True while an AOF rewrite fork is running.
    pub aof_rewrite_in_progress: bool,
    /// Current AOF file size in bytes.
    pub aof_current_size: u64,
    /// Size of the AOF file at the last rewrite — divisor of the
    /// growth ratio chip rendered in the panel.
    pub aof_base_size: u64,
    /// Duration (seconds) of the last AOF rewrite. `-1` if never.
    pub aof_last_rewrite_time_sec: i64,
    /// Elapsed seconds of the currently running rewrite, or `-1` idle.
    pub aof_current_rewrite_time_sec: i64,
    /// `ok` ⇒ true; anything else ⇒ false. Drives the failure banner.
    pub aof_last_bgrewrite_success: bool,

    /// `loading:1` — Redis is busy loading RDB/AOF from disk at startup
    /// or after a replica resync. Both persistence actions are blocked
    /// in this state because the server is not serving traffic anyway.
    pub loading: bool,
}

pub struct MetricsCache {
    max_history_size: usize,
    data: RwLock<HashMap<String, VecDeque<RedisMetrics>>>,
}

impl MetricsCache {
    pub fn new(max_history_size: usize) -> Self {
        Self {
            max_history_size,
            data: RwLock::new(HashMap::new()),
        }
    }
    pub fn add_metrics(&self, server_id: &str, metrics: RedisMetrics) {
        let mut data = self.data.write();
        if let Some(queue) = data.get_mut(server_id) {
            if queue.len() >= self.max_history_size {
                queue.pop_front();
            }
            queue.push_back(metrics);
        } else {
            let mut new_queue = VecDeque::with_capacity(self.max_history_size);
            new_queue.push_back(metrics);
            data.insert(server_id.to_string(), new_queue);
        }
    }
    pub fn remove_server(&self, server_id: &str) {
        let mut data = self.data.write();
        data.remove(server_id);
    }
    pub fn list_metrics(&self, server_id: &str) -> Vec<RedisMetrics> {
        let data = self.data.read();
        data.get(server_id)
            .map(|queue| queue.clone().into_iter().collect())
            .unwrap_or_default()
    }
}

static METRICS_CACHE: LazyLock<MetricsCache> = LazyLock::new(|| MetricsCache::new(1800));

pub fn get_metrics_cache() -> &'static MetricsCache {
    &METRICS_CACHE
}

/// Persist at most one sample per minute per server — the in-memory cache
/// keeps the 2s-resolution live window, disk only needs trend resolution.
const METRICS_PERSIST_INTERVAL_MS: i64 = 60_000;
/// Keep 7 days of samples (~10k rows per server at the 1/min cadence).
const METRICS_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Per-server timestamp of the last persisted sample (this process).
static METRICS_LAST_PERSISTED: LazyLock<RwLock<HashMap<String, i64>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Throttled write-behind of one metrics sample: skips unless a minute has
/// passed since the server's last persisted sample, serializes on the
/// caller, and hands the (blocking) redb write to the background executor.
/// The first persist of a session also prunes samples past retention.
/// Failures only warn — history is best-effort and must never break the
/// heartbeat.
fn maybe_persist_metrics(server_id: &str, metrics: RedisMetrics, cx: &mut Context<ZedisServerState>) {
    let timestamp_ms = metrics.timestamp_ms;
    let first_this_session;
    {
        let mut last = METRICS_LAST_PERSISTED.write();
        let prev = last.get(server_id).copied();
        if let Some(prev) = prev
            && timestamp_ms - prev < METRICS_PERSIST_INTERVAL_MS
        {
            return;
        }
        first_this_session = prev.is_none();
        last.insert(server_id.to_string(), timestamp_ms);
    }
    let Ok(payload) = serde_json::to_vec(&metrics) else {
        return;
    };
    let server_id = server_id.to_string();
    cx.background_executor()
        .spawn(async move {
            if first_this_session {
                match prune_metrics_history(&server_id, timestamp_ms - METRICS_RETENTION_MS) {
                    Ok(removed) if removed > 0 => debug!(server_id, removed, "pruned metrics history"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "prune metrics history failed"),
                }
            }
            if let Err(e) = insert_metrics_sample(&server_id, timestamp_ms, &payload) {
                warn!(error = %e, "persist metrics sample failed");
            }
        })
        .detach();
}

/// Load persisted history for the trailing `duration_ms`, decimated to at
/// most `max_points` samples. Blocking (redb read + JSON decode) — call
/// from a background task, not the render path.
pub fn load_persisted_metrics(server_id: &str, duration_ms: i64, max_points: usize) -> Vec<RedisMetrics> {
    let from_ms = unix_ts_millis() - duration_ms;
    let raw = match list_metrics_samples(server_id, from_ms) {
        Ok(raw) => raw,
        Err(e) => {
            warn!(error = %e, "load metrics history failed");
            return vec![];
        }
    };
    let samples: Vec<RedisMetrics> = raw
        .iter()
        .filter_map(|bytes| serde_json::from_slice(bytes).ok())
        .collect();
    decimate_samples(samples, max_points)
}

/// Evenly stride `samples` down to `max_points`, always keeping the first
/// and the newest sample so the charted time range stays truthful.
fn decimate_samples(samples: Vec<RedisMetrics>, max_points: usize) -> Vec<RedisMetrics> {
    if max_points == 0 {
        return vec![];
    }
    let len = samples.len();
    if len <= max_points {
        return samples;
    }
    if max_points == 1 {
        return samples.last().copied().into_iter().collect();
    }
    let mut picked = Vec::with_capacity(max_points);
    let mut last_ts = i64::MIN;
    for i in 0..max_points {
        let idx = i * (len - 1) / (max_points - 1);
        let sample = samples[idx];
        // The integer stride can land two i on one idx; skip duplicates.
        if sample.timestamp_ms != last_ts {
            last_ts = sample.timestamp_ms;
            picked.push(sample);
        }
    }
    picked
}

/// One replica's live state as reported by a master's `INFO replication`.
/// `lag_bytes` is computed against that master's `master_repl_offset` —
/// negative values can occur very briefly during failover and are clamped to 0
/// at render time.
#[derive(Debug, Default, Clone)]
pub struct ReplicaInfo {
    pub addr: SharedString,
    pub state: SharedString,
    pub offset: i64,
    pub lag_seconds: i64,
    pub lag_bytes: i64,
}

#[derive(Debug, Default, Clone)]
pub struct RedisInfo {
    pub meta: RedisServerMeta,
    // pub latency: Duration,
    pub metrics: RedisMetrics,
    // --- Keyspace (db0, db1...) ---
    pub keyspace: HashMap<String, RedisKeySpaceStats>,
    /// Per-replica live state from any master we polled. Populated from
    /// `INFO replication` `slave_n` lines; empty when the connection has no
    /// replicas (or when the user is connected to a replica directly).
    pub replicas: Vec<ReplicaInfo>,
}

/// Aggregates metrics from multiple Redis Cluster nodes into a single global view.
///
/// Strategies:
/// - **Sum**: Capacity (Memory, Keys) and Throughput (QPS, Network)
/// - **Max**: Health indicators where the worst node defines the cluster state (Fragmentation).
/// - **Static**: Version, OS (taken from the first node).
pub fn aggregate_redis_info(infos: Vec<RedisInfo>) -> RedisInfo {
    // Return default if no nodes are provided
    if infos.is_empty() {
        return RedisInfo::default();
    }

    let mut total = infos[0].clone();
    if infos.len() == 1 {
        return total;
    }

    // Concat replicas from later masters. The first master's replicas are
    // already in `total` (via the clone above) so we only walk infos[1..].
    for info in infos.iter().skip(1) {
        total.replicas.extend(info.replicas.iter().cloned());
    }

    // Temporary map to calculate weighted average for avg_ttl: DbName -> (TotalTTLProduct, TotalExpires)
    let mut ttl_accumulator: HashMap<String, (u64, u64)> = HashMap::new();

    for info in &infos {
        // --- Clients (Sum) ---
        total.metrics.connected_clients += info.metrics.connected_clients;
        total.metrics.blocked_clients += info.metrics.blocked_clients;

        // --- Memory (Sum) ---
        total.metrics.used_memory += info.metrics.used_memory;
        total.metrics.used_memory_rss += info.metrics.used_memory_rss;
        total.meta.maxmemory += info.meta.maxmemory;

        // --- Memory Health (Max) ---
        // We take the maximum fragmentation ratio because the "worst" node
        // determines the fragmentation risk of the cluster.
        if info.metrics.mem_fragmentation_ratio > total.metrics.mem_fragmentation_ratio {
            total.metrics.mem_fragmentation_ratio = info.metrics.mem_fragmentation_ratio;
        }

        // --- Stats (Sum) ---
        total.metrics.total_connections_received += info.metrics.total_connections_received;
        total.metrics.total_commands_processed += info.metrics.total_commands_processed;
        total.metrics.instantaneous_ops_per_sec += info.metrics.instantaneous_ops_per_sec;
        total.metrics.instantaneous_input_kbps += info.metrics.instantaneous_input_kbps;
        total.metrics.instantaneous_output_kbps += info.metrics.instantaneous_output_kbps;
        total.metrics.keyspace_hits += info.metrics.keyspace_hits;
        total.metrics.keyspace_misses += info.metrics.keyspace_misses;
        total.metrics.evicted_keys += info.metrics.evicted_keys;

        // --- CPU (Sum) ---
        // Accumulate total CPU time consumed by the entire cluster
        total.metrics.used_cpu_sys += info.metrics.used_cpu_sys;
        total.metrics.used_cpu_user += info.metrics.used_cpu_user;

        // --- Persistence ---
        // Sizes / changes: sum across masters (matches the SUM pattern
        // used by used_memory above). AOF enabled flag is treated as
        // homogeneous across the cluster — keep infos[0]'s value (the
        // clone already set it), do not touch in the loop.
        total.metrics.rdb_changes_since_last_save += info.metrics.rdb_changes_since_last_save;
        total.metrics.aof_current_size += info.metrics.aof_current_size;
        total.metrics.aof_base_size += info.metrics.aof_base_size;

        // In-progress flags: OR — if ANY node is forking, treat the
        // cluster as busy so the action button stays disabled.
        total.metrics.rdb_bgsave_in_progress |= info.metrics.rdb_bgsave_in_progress;
        total.metrics.aof_rewrite_in_progress |= info.metrics.aof_rewrite_in_progress;
        total.metrics.loading |= info.metrics.loading;

        // Success flags: AND — surface a failure banner if ANY node had
        // its last save fail. Idempotent under repeated infos[0].
        total.metrics.rdb_last_bgsave_success &= info.metrics.rdb_last_bgsave_success;
        total.metrics.aof_last_write_success &= info.metrics.aof_last_write_success;
        total.metrics.aof_last_bgrewrite_success &= info.metrics.aof_last_bgrewrite_success;

        // Last save time: MIN (oldest snapshot wins — "0 = never" naturally
        // dominates because nothing is smaller). Elapsed/duration counters
        // are reported per-fork-event so MAX (the node still running, or
        // the longest recent fork) is the most informative aggregate.
        if info.metrics.rdb_last_save_time < total.metrics.rdb_last_save_time {
            total.metrics.rdb_last_save_time = info.metrics.rdb_last_save_time;
        }
        if info.metrics.rdb_current_bgsave_time_sec > total.metrics.rdb_current_bgsave_time_sec {
            total.metrics.rdb_current_bgsave_time_sec = info.metrics.rdb_current_bgsave_time_sec;
        }
        if info.metrics.aof_current_rewrite_time_sec > total.metrics.aof_current_rewrite_time_sec {
            total.metrics.aof_current_rewrite_time_sec = info.metrics.aof_current_rewrite_time_sec;
        }
        if info.metrics.rdb_last_bgsave_time_sec > total.metrics.rdb_last_bgsave_time_sec {
            total.metrics.rdb_last_bgsave_time_sec = info.metrics.rdb_last_bgsave_time_sec;
        }
        if info.metrics.aof_last_rewrite_time_sec > total.metrics.aof_last_rewrite_time_sec {
            total.metrics.aof_last_rewrite_time_sec = info.metrics.aof_last_rewrite_time_sec;
        }

        // --- Keyspace (Sum & Weighted Avg) ---
        for (db, stats) in &info.keyspace {
            let entry = total.keyspace.entry(db.clone()).or_default();

            // Sum keys and expires
            entry.keys += stats.keys;
            entry.expires += stats.expires;

            // Prepare data for weighted average calculation of avg_ttl
            if stats.expires > 0 {
                let acc = ttl_accumulator.entry(db.clone()).or_insert((0, 0));
                acc.0 += stats.avg_ttl * stats.expires; // Weighted product
                acc.1 += stats.expires; // Total weight
            }
        }
    }

    // 2. Post-processing

    // Re-calculate human-readable memory string based on the summed byte count

    // Finalize avg_ttl calculation for each DB
    for (db, stats) in total.keyspace.iter_mut() {
        if let Some((weighted_sum, total_expires)) = ttl_accumulator.get(db)
            && *total_expires > 0
        {
            stats.avg_ttl = weighted_sum / total_expires;
        }
    }

    total
}
impl RedisInfo {
    pub fn parse(info_str: &str) -> Self {
        let mut info = RedisInfo::default();
        // `slave_n` lines and `master_repl_offset` may appear in either order
        // within `INFO replication`, so collect them first then compute byte
        // lag once both are known.
        let mut master_repl_offset: i64 = 0;
        let mut pending_replicas: Vec<ReplicaInfo> = Vec::new();

        for line in info_str.lines() {
            let line = line.trim();
            // ignore comment line
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once(':') {
                if key.starts_with("db") && value.contains("keys=") {
                    if let Ok(stats) = parse_keyspace_value(value) {
                        info.keyspace.insert(key.to_string(), stats);
                    }
                    continue;
                }
                if key.starts_with("slave") && key[5..].chars().all(|c| c.is_ascii_digit()) {
                    if let Some(replica) = parse_replica_value(value) {
                        pending_replicas.push(replica);
                    }
                    continue;
                }

                match key {
                    "redis_version" => info.meta.redis_version = value.to_string(),
                    "os" => info.meta.os = value.to_string(),
                    "role" => info.meta.role = value.to_string(),

                    "connected_clients" => info.metrics.connected_clients = parse_u64(value),
                    "rejected_connections" => info.metrics.rejected_connections = parse_u64(value),
                    "blocked_clients" => info.metrics.blocked_clients = parse_u64(value),

                    "used_memory" => info.metrics.used_memory = parse_u64(value),
                    "used_memory_rss" => info.metrics.used_memory_rss = parse_u64(value),
                    "maxmemory" => info.meta.maxmemory = parse_u64(value),
                    "mem_fragmentation_ratio" => info.metrics.mem_fragmentation_ratio = parse_f64(value),

                    "total_connections_received" => info.metrics.total_connections_received = parse_u64(value),
                    "total_commands_processed" => info.metrics.total_commands_processed = parse_u64(value),
                    "instantaneous_ops_per_sec" => info.metrics.instantaneous_ops_per_sec = parse_u64(value),
                    "instantaneous_input_kbps" => info.metrics.instantaneous_input_kbps = parse_f64(value),
                    "instantaneous_output_kbps" => info.metrics.instantaneous_output_kbps = parse_f64(value),
                    "keyspace_hits" => info.metrics.keyspace_hits = parse_u64(value),
                    "keyspace_misses" => info.metrics.keyspace_misses = parse_u64(value),
                    "evicted_keys" => info.metrics.evicted_keys = parse_u64(value),
                    "expired_keys" => info.metrics.expired_keys = parse_u64(value),

                    "rdb_last_bgsave_status" => info.metrics.rdb_last_bgsave_success = value == "ok",
                    "aof_last_write_status" => info.metrics.aof_last_write_success = value == "ok",

                    // INFO persistence — RDB
                    "rdb_changes_since_last_save" => info.metrics.rdb_changes_since_last_save = parse_u64(value),
                    "rdb_last_save_time" => info.metrics.rdb_last_save_time = parse_i64(value),
                    "rdb_bgsave_in_progress" => info.metrics.rdb_bgsave_in_progress = value == "1",
                    "rdb_last_bgsave_time_sec" => info.metrics.rdb_last_bgsave_time_sec = parse_i64(value),
                    "rdb_current_bgsave_time_sec" => info.metrics.rdb_current_bgsave_time_sec = parse_i64(value),

                    // INFO persistence — AOF
                    "aof_enabled" => info.metrics.aof_enabled = value == "1",
                    "aof_rewrite_in_progress" => info.metrics.aof_rewrite_in_progress = value == "1",
                    "aof_current_size" => info.metrics.aof_current_size = parse_u64(value),
                    "aof_base_size" => info.metrics.aof_base_size = parse_u64(value),
                    "aof_last_rewrite_time_sec" => info.metrics.aof_last_rewrite_time_sec = parse_i64(value),
                    "aof_current_rewrite_time_sec" => info.metrics.aof_current_rewrite_time_sec = parse_i64(value),
                    "aof_last_bgrewrite_status" => info.metrics.aof_last_bgrewrite_success = value == "ok",
                    "loading" => info.metrics.loading = value == "1",

                    "used_cpu_sys" => info.metrics.used_cpu_sys = parse_f64(value),
                    "used_cpu_user" => info.metrics.used_cpu_user = parse_f64(value),

                    "master_repl_offset" => master_repl_offset = parse_i64(value),

                    _ => {}
                }
            }
        }

        for mut replica in pending_replicas.drain(..) {
            replica.lag_bytes = (master_repl_offset - replica.offset).max(0);
            info.replicas.push(replica);
        }

        info
    }
}

// --- Helpers ---

fn parse_u64(v: &str) -> u64 {
    v.parse().unwrap_or(0)
}

fn parse_i64(v: &str) -> i64 {
    v.parse().unwrap_or(0)
}

fn parse_f64(v: &str) -> f64 {
    v.parse().unwrap_or(0.0)
}

/// Parse one `slave_n` value of the form `ip=...,port=...,state=...,offset=...,lag=...`.
/// Required: `ip`, `port`. Missing optional fields default to 0/empty.
fn parse_replica_value(v: &str) -> Option<ReplicaInfo> {
    let mut ip = String::new();
    let mut port = String::new();
    let mut state = String::new();
    let mut offset: i64 = 0;
    let mut lag_seconds: i64 = 0;
    for part in v.split(',') {
        if let Some((k, val)) = part.split_once('=') {
            match k {
                "ip" => ip = val.to_string(),
                "port" => port = val.to_string(),
                "state" => state = val.to_string(),
                "offset" => offset = parse_i64(val),
                "lag" => lag_seconds = parse_i64(val),
                _ => {}
            }
        }
    }
    if ip.is_empty() || port.is_empty() {
        return None;
    }
    Some(ReplicaInfo {
        addr: format!("{ip}:{port}").into(),
        state: state.into(),
        offset,
        lag_seconds,
        lag_bytes: 0, // computed by caller against master_repl_offset
    })
}

/// Parse the keyspace value: keys=10,expires=0,avg_ttl=0
fn parse_keyspace_value(v: &str) -> Result<RedisKeySpaceStats, ()> {
    let mut stats = RedisKeySpaceStats::default();
    for part in v.split(',') {
        if let Some((k, val)) = part.split_once('=') {
            match k {
                "keys" => stats.keys = parse_u64(val),
                "expires" => stats.expires = parse_u64(val),
                "avg_ttl" => stats.avg_ttl = parse_u64(val),
                _ => {}
            }
        }
    }
    Ok(stats)
}

impl ZedisServerState {
    /// Consecutive heartbeat PING failures before the live link is reported as
    /// Offline rather than Reconnecting (~2s heartbeat cadence -> >=6s down).
    const PING_OFFLINE_THRESHOLD: u32 = 3;

    /// Fold a heartbeat `PING` outcome into the observable [`ConnectionHealth`].
    /// Success -> `Connected` (failure counter cleared). Failure ->
    /// `Reconnecting` for the first few consecutive misses, then `Offline` past
    /// the threshold (the heartbeat can't distinguish "retrying" from "down",
    /// so this elapsed-failures heuristic stands in). Emits
    /// `ConnectionHealthChanged` only on an actual transition, so a steady
    /// state costs no extra re-render.
    fn note_ping_result(&mut self, ok: bool, cx: &mut Context<Self>) {
        let next = if ok {
            self.ping_failures = 0;
            self.last_connection_error = ConnectionErrorKind::Unknown;
            ConnectionHealth::Connected
        } else {
            self.ping_failures = self.ping_failures.saturating_add(1);
            if self.ping_failures >= Self::PING_OFFLINE_THRESHOLD {
                ConnectionHealth::Offline
            } else {
                ConnectionHealth::Reconnecting
            }
        };
        if self.connection_health != next {
            self.connection_health = next;
            cx.emit(ServerEvent::ConnectionHealthChanged);
        }
    }

    pub fn refresh_redis_info(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }

        let slow_logs_check_interval = 60;
        let mut last_slow_logs_checked_at = self.last_slow_logs_checked_at;
        if last_slow_logs_checked_at == 0 {
            last_slow_logs_checked_at = unix_ts() - slow_logs_check_interval;
        }

        let server_id = self.server_id.clone();
        let db = self.db;
        let server_id_clone = server_id.clone();

        self.spawn(
            ServerTask::RefreshRedisInfo,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let start = Instant::now();
                client.ping().await?;
                let latency = start.elapsed();
                let now = unix_ts();
                let slow_logs = if now - last_slow_logs_checked_at >= slow_logs_check_interval {
                    // ignore get slow error
                    let slow_logs = client.get_slow_logs().await.unwrap_or_default();
                    Some(slow_logs)
                } else {
                    None
                };

                let (_, list): (_, Vec<String>) =
                    client.query_async_masters(vec![cmd("INFO").arg("ALL").clone()]).await?;
                let infos: Vec<RedisInfo> = list.iter().map(|info| RedisInfo::parse(info)).collect();
                let mut info = aggregate_redis_info(infos);
                info.metrics.timestamp_ms = unix_ts_millis();
                info.metrics.latency_ms = latency.as_millis() as u64;
                Ok((info, slow_logs))
            },
            move |this, result, cx| match result {
                Ok((info, slow_logs)) => {
                    METRICS_CACHE.add_metrics(&server_id_clone, info.metrics);
                    maybe_persist_metrics(&server_id_clone, info.metrics, cx);
                    this.redis_info = Some(info);
                    if let Some(slow_logs) = slow_logs {
                        this.last_slow_log_count = slow_logs
                            .iter()
                            .filter(|item| item.timestamp >= last_slow_logs_checked_at)
                            .count();
                        this.slow_logs = slow_logs;
                        this.last_slow_logs_checked_at = unix_ts();
                    }
                    cx.emit(ServerEvent::ServerRedisInfoUpdated);
                    this.note_ping_result(true, cx);
                }
                Err(e) => {
                    // Connection is invalid, remove cached client
                    get_connection_manager().remove_client(&server_id_clone, db);
                    error!(error = %e, "Ping failed, client connection removed");
                    // Remember *why* so the offline tooltip can name it. Set
                    // before note_ping_result, which emits the health
                    // transition the status bar reads. TLS-aware so a dropped
                    // plaintext link points the user at the TLS toggle.
                    let tls_enabled = get_server(&server_id_clone)
                        .map(|s| s.tls.unwrap_or(false))
                        .unwrap_or(false);
                    this.last_connection_error = e.connection_kind_tls_aware(tls_enabled);
                    this.note_ping_result(false, cx);
                }
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replicas_and_computes_lag_bytes() {
        let raw = "# Replication\n\
                   role:master\n\
                   connected_slaves:2\n\
                   master_repl_offset:1000\n\
                   slave0:ip=10.0.0.4,port=6379,state=online,offset=900,lag=0\n\
                   slave1:ip=10.0.0.5,port=6379,state=wait_bgsave,offset=600,lag=2\n";
        let info = RedisInfo::parse(raw);
        assert_eq!(info.replicas.len(), 2);

        let r0 = &info.replicas[0];
        assert_eq!(r0.addr.as_ref(), "10.0.0.4:6379");
        assert_eq!(r0.state.as_ref(), "online");
        assert_eq!(r0.offset, 900);
        assert_eq!(r0.lag_bytes, 100);
        assert_eq!(r0.lag_seconds, 0);

        let r1 = &info.replicas[1];
        assert_eq!(r1.state.as_ref(), "wait_bgsave");
        assert_eq!(r1.lag_bytes, 400);
        assert_eq!(r1.lag_seconds, 2);
    }

    #[test]
    fn replica_lag_works_when_master_offset_appears_after_slaves() {
        // Some Redis versions/sections emit `master_repl_offset` after the
        // slave_n entries. We have to defer lag_bytes computation until both
        // are seen — the test fails if we compute eagerly inline.
        let raw = "slave0:ip=1.1.1.1,port=6379,state=online,offset=400,lag=1\n\
                   master_repl_offset:500\n";
        let info = RedisInfo::parse(raw);
        assert_eq!(info.replicas.len(), 1);
        assert_eq!(info.replicas[0].lag_bytes, 100);
    }

    #[test]
    fn parses_persistence_fields() {
        // Synthetic `INFO persistence` excerpt — covers both the "idle"
        // and "currently saving" shapes so we know `-1` is preserved as
        // the sentinel instead of being clobbered to 0.
        let raw = "# Persistence\n\
                   loading:0\n\
                   rdb_changes_since_last_save:42\n\
                   rdb_bgsave_in_progress:1\n\
                   rdb_last_save_time:1748000000\n\
                   rdb_last_bgsave_status:ok\n\
                   rdb_last_bgsave_time_sec:3\n\
                   rdb_current_bgsave_time_sec:1\n\
                   aof_enabled:1\n\
                   aof_rewrite_in_progress:0\n\
                   aof_current_size:131072\n\
                   aof_base_size:65536\n\
                   aof_last_rewrite_time_sec:-1\n\
                   aof_current_rewrite_time_sec:-1\n\
                   aof_last_bgrewrite_status:ok\n\
                   aof_last_write_status:ok\n";
        let info = RedisInfo::parse(raw);
        assert!(!info.metrics.loading);
        assert_eq!(info.metrics.rdb_changes_since_last_save, 42);
        assert!(info.metrics.rdb_bgsave_in_progress);
        assert_eq!(info.metrics.rdb_last_save_time, 1_748_000_000);
        assert!(info.metrics.rdb_last_bgsave_success);
        assert_eq!(info.metrics.rdb_last_bgsave_time_sec, 3);
        assert_eq!(info.metrics.rdb_current_bgsave_time_sec, 1);
        assert!(info.metrics.aof_enabled);
        assert!(!info.metrics.aof_rewrite_in_progress);
        assert_eq!(info.metrics.aof_current_size, 131_072);
        assert_eq!(info.metrics.aof_base_size, 65_536);
        // Sentinel `-1` must survive the parse — UI uses it to render "never".
        assert_eq!(info.metrics.aof_last_rewrite_time_sec, -1);
        assert_eq!(info.metrics.aof_current_rewrite_time_sec, -1);
        assert!(info.metrics.aof_last_bgrewrite_success);
        assert!(info.metrics.aof_last_write_success);
    }

    #[test]
    fn negative_lag_clamped_to_zero() {
        // During failover the slave offset can briefly exceed master_repl_offset.
        let raw = "master_repl_offset:100\n\
                   slave0:ip=1.1.1.1,port=6379,state=online,offset=200,lag=0\n";
        let info = RedisInfo::parse(raw);
        assert_eq!(info.replicas[0].lag_bytes, 0);
    }

    fn sample(ts: i64) -> RedisMetrics {
        RedisMetrics {
            timestamp_ms: ts,
            ..Default::default()
        }
    }

    #[test]
    fn decimate_keeps_endpoints_and_bounds_length() {
        let samples: Vec<RedisMetrics> = (0..1000).map(|i| sample(i as i64)).collect();
        let picked = decimate_samples(samples, 150);
        assert!(picked.len() <= 150);
        assert_eq!(picked.first().map(|m| m.timestamp_ms), Some(0));
        assert_eq!(picked.last().map(|m| m.timestamp_ms), Some(999));

        // Short inputs pass through untouched.
        let short: Vec<RedisMetrics> = (0..10).map(|i| sample(i as i64)).collect();
        assert_eq!(decimate_samples(short, 150).len(), 10);
        assert!(decimate_samples(vec![], 150).is_empty());
        assert_eq!(
            decimate_samples((0..10).map(|i| sample(i as i64)).collect(), 1).len(),
            1
        );
    }

    #[test]
    fn metrics_serde_roundtrip_and_forward_compat() {
        let metrics = RedisMetrics {
            timestamp_ms: 1_751_700_000_000,
            latency_ms: 3,
            used_memory: 1_048_576,
            instantaneous_ops_per_sec: 42,
            mem_fragmentation_ratio: 1.25,
            aof_enabled: true,
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&metrics).expect("serialize metrics");
        let parsed: RedisMetrics = serde_json::from_slice(&bytes).expect("deserialize metrics");
        assert_eq!(parsed.timestamp_ms, metrics.timestamp_ms);
        assert_eq!(parsed.used_memory, metrics.used_memory);
        assert_eq!(parsed.instantaneous_ops_per_sec, 42);
        assert!(parsed.aof_enabled);

        // Old on-disk samples with missing fields must still parse
        // (serde(default)) so schema growth never wipes history.
        let legacy = r#"{"timestamp_ms":1751700000000,"used_memory":123}"#;
        let parsed: RedisMetrics = serde_json::from_str(legacy).expect("parse legacy sample");
        assert_eq!(parsed.used_memory, 123);
        assert_eq!(parsed.latency_ms, 0);
    }
}

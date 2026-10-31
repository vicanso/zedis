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

use crate::connection::{
    ReplicationInfo, ServerDb, dbsize, forget_client, get_server, heartbeat_probe, master_infos, slow_logs,
};
use crate::db::get_alert_rules_manager;
use crate::helpers::{pacing, unix_ts, unix_ts_millis};
use crate::states::{
    ConnectionHealth, ServerEvent, ServerTask, ZedisGlobalStore, ZedisServerState, i18n_metrics, i18n_status_bar,
};
use gpui::prelude::*;
use gpui::{App, SharedString};
use parking_lot::RwLock;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use web_time::Instant;
use zedis_core::alerts::{AlertMetric, AlertSample, TriggeredAlert};

// The Metrics history, written and read back: the local database on the
// desktop, the bridge in the browser, whose page keeps nothing past a reload.
#[cfg(not(target_family = "wasm"))]
mod persist;
#[cfg(target_family = "wasm")]
mod persist_web;
#[cfg(not(target_family = "wasm"))]
pub use persist::load_persisted_metrics;
#[cfg(not(target_family = "wasm"))]
use persist::maybe_persist_metrics;
#[cfg(target_family = "wasm")]
pub use persist_web::load_persisted_metrics;
#[cfg(target_family = "wasm")]
use persist_web::maybe_persist_metrics;

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
    /// `availability_zone` (Valkey 8.1's `availability-zone` config, in
    /// `INFO server`); empty where unset or on Redis.
    pub availability_zone: String,
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
    // --- Client pause: Valkey 8.1+ says so in `INFO clients`, Redis says nothing ---
    pub paused_actions: PausedActions,
    pub paused_timeout_ms: u64,
    pub paused_reason: PauseReason,

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
            .map(|queue| queue.iter().copied().collect())
            .unwrap_or_default()
    }
    /// The newest sample alone — for a reader that shows one number, where
    /// [`Self::list_metrics`] copies the whole history.
    pub fn latest_metrics(&self, server_id: &str) -> Option<RedisMetrics> {
        self.data.read().get(server_id).and_then(|queue| queue.back().copied())
    }
}

static METRICS_CACHE: LazyLock<MetricsCache> = LazyLock::new(|| MetricsCache::new(1800));

pub fn get_metrics_cache() -> &'static MetricsCache {
    &METRICS_CACHE
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

/// One master's RDB/AOF snapshot for the Persistence panel's per-node
/// table (cluster only — standalone leaves this empty and uses
/// aggregated `metrics` alone).
#[derive(Debug, Default, Clone)]
pub struct PersistenceNodeSnapshot {
    /// `host:port` label for the master.
    pub label: SharedString,
    pub rdb_last_save_time: i64,
    pub rdb_changes_since_last_save: u64,
    pub rdb_bgsave_in_progress: bool,
    pub rdb_last_bgsave_success: bool,
    pub aof_enabled: bool,
    pub aof_rewrite_in_progress: bool,
    pub aof_current_size: u64,
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
    /// Per-master persistence rows. Empty on standalone (the top-level
    /// cards already cover the single node).
    pub persistence_nodes: Vec<PersistenceNodeSnapshot>,
    /// Each master's availability zone, `host:port` → zone, for the nodes
    /// that report one (Valkey 8.1+ with `availability-zone` set). The
    /// topology page labels its master rows with it.
    pub node_zones: Vec<(String, String)>,
    /// The whole `INFO replication` section of the polled node — both
    /// sides of a primary / replica link, for the Topology page's
    /// standalone view. On a cluster / Sentinel entry the aggregate keeps
    /// the first master's, which nothing reads.
    pub replication: ReplicationInfo,
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

    // Weighted average of avg_ttl per db: DbName -> (Σ avg_ttl·expires,
    // Σ expires), read from every node — the first included, whose keyspace
    // the loop below leaves in `total` as it is.
    let mut ttl_accumulator: HashMap<String, (u64, u64)> = HashMap::new();
    for info in &infos {
        for (db, stats) in &info.keyspace {
            if stats.expires > 0 {
                let acc = ttl_accumulator.entry(db.clone()).or_insert((0, 0));
                acc.0 += stats.avg_ttl * stats.expires;
                acc.1 += stats.expires;
            }
        }
    }

    // `total` *is* the first node, so the sums, maxes and flags fold the
    // other nodes onto it. Walking every node here counted the first master
    // twice: 7 keys in the database switcher for a db holding 6, and the
    // same again for clients, memory and every throughput counter.
    for info in infos.iter().skip(1) {
        // --- Clients (Sum) ---
        total.metrics.connected_clients += info.metrics.connected_clients;
        total.metrics.blocked_clients += info.metrics.blocked_clients;

        // --- Client pause (strongest wins: a cluster is paused where any master is) ---
        if info.metrics.paused_actions.rank() > total.metrics.paused_actions.rank() {
            total.metrics.paused_actions = info.metrics.paused_actions;
            total.metrics.paused_reason = info.metrics.paused_reason;
        }
        total.metrics.paused_timeout_ms = total.metrics.paused_timeout_ms.max(info.metrics.paused_timeout_ms);

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
        // Left out, these two were the first master's alone in the Metrics
        // CSV and history while every counter beside them was the cluster's.
        total.metrics.expired_keys += info.metrics.expired_keys;
        total.metrics.rejected_connections += info.metrics.rejected_connections;

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
        // its last save fail.
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

        // --- Keyspace (Sum) ---
        for (db, stats) in &info.keyspace {
            let entry = total.keyspace.entry(db.clone()).or_default();
            entry.keys += stats.keys;
            entry.expires += stats.expires;
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
/// `paused_actions` from `INFO clients` (Valkey 8.1+). `Unreported` is the
/// field's absence — a Redis — and is what tells the status bar to show
/// this app's own record instead. `Copy`, like the rest of the metrics,
/// which the history keeps by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PausedActions {
    #[default]
    Unreported,
    None,
    Write,
    All,
}

impl PausedActions {
    fn parse(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "write" => Self::Write,
            "all" => Self::All,
            _ => Self::Unreported,
        }
    }

    /// `none` < `write` < `all`.
    fn rank(self) -> u8 {
        match self {
            Self::Unreported | Self::None => 0,
            Self::Write => 1,
            Self::All => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreported => "",
            Self::None => "none",
            Self::Write => "write",
            Self::All => "all",
        }
    }
}

/// `paused_reason` from `INFO clients`: who asked for the pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PauseReason {
    #[default]
    None,
    ClientPause,
    Failover,
    Shutdown,
    Other,
}

impl PauseReason {
    fn parse(value: &str) -> Self {
        match value {
            "none" | "" => Self::None,
            "client_pause" => Self::ClientPause,
            "failover" => Self::Failover,
            "shutdown" => Self::Shutdown,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ClientPause => "client_pause",
            Self::Failover => "failover",
            Self::Shutdown => "shutdown",
            Self::Other => "other",
        }
    }
}

/// Clients paused on the server, as the status bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPause {
    /// `write` or `all`.
    pub actions: String,
    pub remaining: Duration,
    /// The server's reason (`client_pause`, `failover`, …) where it gave one.
    pub reason: Option<String>,
    /// Read from `INFO clients` (Valkey 8.1+) rather than remembered from a
    /// pause this app sent.
    pub from_server: bool,
}

/// What is paused now: the server's word where it has one — Valkey 8.1+
/// reports `paused_actions`, `none` included, so an absent field is the
/// tell — else the pause this app sent (`local`: actions, until) that has
/// not run out. A server that reports pauses is believed over the local
/// record even when they disagree: it is the one holding the clients.
pub fn client_pause_now(
    metrics: Option<&RedisMetrics>,
    local: Option<(&str, Instant)>,
    now: Instant,
) -> Option<ClientPause> {
    if let Some(metrics) = metrics
        && metrics.paused_actions != PausedActions::Unreported
    {
        return (metrics.paused_actions.rank() > 0).then(|| ClientPause {
            actions: metrics.paused_actions.as_str().to_string(),
            remaining: Duration::from_millis(metrics.paused_timeout_ms),
            reason: Some(metrics.paused_reason.as_str().to_string()),
            from_server: true,
        });
    }
    let (actions, until) = local?;
    (until > now).then(|| ClientPause {
        actions: actions.to_string(),
        remaining: until - now,
        reason: None,
        from_server: false,
    })
}

impl RedisInfo {
    pub fn parse(info_str: &str) -> Self {
        let mut info = RedisInfo {
            replication: ReplicationInfo::parse(info_str),
            ..Default::default()
        };
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
                    "availability_zone" => info.meta.availability_zone = value.to_string(),
                    "os" => info.meta.os = value.to_string(),
                    "role" => info.meta.role = value.to_string(),

                    "connected_clients" => info.metrics.connected_clients = parse_u64(value),
                    "rejected_connections" => info.metrics.rejected_connections = parse_u64(value),
                    "blocked_clients" => info.metrics.blocked_clients = parse_u64(value),
                    "paused_actions" => info.metrics.paused_actions = PausedActions::parse(value),
                    "paused_timeout_milliseconds" => info.metrics.paused_timeout_ms = parse_u64(value),
                    "paused_reason" => info.metrics.paused_reason = PauseReason::parse(value),

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

pub fn alert_metric_label(cx: &App, metric: AlertMetric) -> SharedString {
    let key = match metric {
        AlertMetric::MemoryPercent => "alert_metric_memory_percent",
        AlertMetric::BlockedClients => "alert_metric_blocked_clients",
        AlertMetric::ConnectedClients => "alert_metric_connected_clients",
        AlertMetric::LatencyMs => "alert_metric_latency_ms",
        AlertMetric::EvictedKeysPerSec => "alert_metric_evicted_keys_per_sec",
        AlertMetric::ReplicationLagMb => "alert_metric_replication_lag_mb",
        AlertMetric::InstantaneousOps => "alert_metric_instantaneous_ops",
        AlertMetric::LinkOffline => "alert_metric_link_offline",
    };
    i18n_metrics(cx, key)
}

impl ZedisServerState {
    /// Whether threshold alerts are enabled for the currently selected server.
    /// Off by default unless opted in via the connection settings or the
    /// Metrics Alerts dialog.
    pub fn alerts_enabled(&self) -> bool {
        let server_flag = get_server(&self.server_id).ok().and_then(|s| s.alerts_enabled);
        get_alert_rules_manager().is_enabled(self.server_id.as_str(), server_flag)
    }

    /// Currently active threshold breaches on the selected server.
    pub fn active_alerts(&self) -> &[TriggeredAlert] {
        self.readings.active_alerts()
    }

    /// Alerts triggered in this session on the selected server (newest first).
    pub fn recent_alerts(&self) -> &VecDeque<TriggeredAlert> {
        self.readings.recent_alerts()
    }

    /// Clear the in-memory list of recently triggered alerts.
    pub fn clear_recent_alerts(&mut self, cx: &mut Context<Self>) {
        self.readings.clear_recent_alerts();
        cx.emit(ServerEvent::ServerRedisInfoUpdated);
        cx.notify();
    }

    /// Notify state after the user saves alert settings in the dialog.
    pub fn notify_alert_config_changed(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !enabled {
            self.readings.reset_alert_evaluator();
        }
        cx.emit(ServerEvent::ServerRedisInfoUpdated);
        cx.notify();
    }

    fn evaluate_server_alerts(&mut self, has_info: bool, link_offline: bool, cx: &mut Context<Self>) {
        if !self.alerts_enabled() {
            self.readings.reset_alert_evaluator();
            return;
        }
        let sample = if link_offline {
            AlertSample {
                timestamp_ms: unix_ts_millis(),
                link_offline: true,
                ..Default::default()
            }
        } else if has_info && let Some(info) = self.readings.redis_info() {
            let max_replica_lag_bytes = info
                .replicas
                .iter()
                .map(|r| r.lag_bytes.max(0) as u64)
                .max()
                .unwrap_or(0);
            AlertSample {
                timestamp_ms: info.metrics.timestamp_ms,
                used_memory: info.metrics.used_memory,
                maxmemory: info.meta.maxmemory,
                connected_clients: info.metrics.connected_clients,
                blocked_clients: info.metrics.blocked_clients,
                max_replica_lag_bytes,
                evicted_keys: info.metrics.evicted_keys,
                instantaneous_ops_per_sec: info.metrics.instantaneous_ops_per_sec,
                latency_ms: info.metrics.latency_ms,
                link_offline: false,
            }
        } else {
            return;
        };

        let rules = get_alert_rules_manager().rules_for(self.server_id.as_str());
        let fired = self.readings.evaluate_alerts(&rules, sample);
        if fired.is_empty() {
            return;
        }
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let server_name = get_server(&self.server_id)
            .ok()
            .map(|s| s.name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.server_id.to_string());
        let title: SharedString = t!("metrics.alert_triggered_title", server = server_name, locale = &locale)
            .to_string()
            .into();
        for alert in fired {
            let metric_name = alert_metric_label(cx, alert.metric);
            let body = if alert.metric == AlertMetric::LinkOffline {
                metric_name.to_string()
            } else {
                t!(
                    "metrics.alert_triggered_body",
                    metric = metric_name.as_ref(),
                    observed = alert.metric.format_value(alert.observed),
                    comparator = alert.comparator.symbol(),
                    threshold = alert.metric.format_value(alert.threshold),
                    locale = &locale
                )
                .to_string()
            };
            self.emit_warning_notification_with_title(body.into(), title.clone(), cx);
        }
    }

    /// Fold a heartbeat `PING` outcome into the health ([`Link::note_ping`]),
    /// announcing a transition and only a transition.
    fn note_ping_result(&mut self, ok: bool, cx: &mut Context<Self>) {
        let outcome = self.link.note_ping(ok, self.server_id.as_str());
        if outcome.changed {
            cx.emit(ServerEvent::ConnectionHealthChanged);
            if outcome.restored {
                self.on_link_restored(cx);
            }
        }
        if !ok && self.connection_health() == ConnectionHealth::Offline {
            self.evaluate_server_alerts(false, true, cx);
        }
    }

    /// The heartbeat came back after a Reconnecting / Offline stretch: say
    /// so, and refresh what the user is looking at — the value on screen
    /// may be from before the outage (a failover can even have rolled it
    /// back).
    fn on_link_restored(&mut self, cx: &mut Context<Self>) {
        info!(server_id = self.server_id.as_str(), "connection restored");
        // Title + server name, so stacked toasts from several open connections
        // are tellable apart. A name can be missing (server since removed) —
        // then the label rides alone as the message, because a toast with an
        // empty half renders a blank row where that half would be.
        let server_name = get_server(&self.server_id).map(|s| s.name).unwrap_or_default();
        let label = i18n_status_bar(cx, "conn_restored");
        if server_name.is_empty() {
            self.emit_success_notification(label, SharedString::default(), cx);
        } else {
            self.emit_success_notification(server_name.into(), label, cx);
        }
        if let Some(key) = self.open.key.clone()
            && !key.is_empty()
        {
            self.reload_value(key, cx);
        }
    }

    /// Manually drop the live connection and stay `Offline` until the user hits
    /// reconnect — the status-bar health dot's disconnect toggle. Removes the
    /// pooled client and pauses the heartbeat (`manually_offline`), so the link
    /// doesn't silently come back on the next tick.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() || self.link.manually_offline {
            return;
        }
        let changed = self.link.go_offline();
        forget_client(&self.at());
        if changed {
            cx.emit(ServerEvent::ConnectionHealthChanged);
        }
    }

    /// The app is being looked at again (the window came back to the front,
    /// the page became visible): beat now instead of waiting out what is
    /// left of the relaxed interval.
    pub fn resume_heartbeat(&mut self, cx: &mut Context<Self>) {
        self.link.beat_now();
        self.refresh_redis_info(cx);
    }

    /// After a user-driven add/delete, ask `DBSIZE` now rather than wait out
    /// the heartbeat's minute. A second request while one is in flight is
    /// remembered and sent when the first lands, so add-then-delete does not
    /// apply a stale total.
    pub fn refresh_dbsize_now(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() || self.link.manually_offline {
            return;
        }
        if !self.readings.request_dbsize_refresh() {
            return;
        }
        let at = self.at();
        self.spawn(
            ServerTask::RefreshDbsize,
            move || async move { Ok(dbsize(&at).await?) },
            move |this, result, cx| {
                match result {
                    Ok(n) => {
                        this.readings.note_dbsize(n);
                        cx.emit(ServerEvent::ServerRedisInfoUpdated);
                    }
                    Err(e) => {
                        debug!(error = %e, "DBSIZE unavailable after a keyspace change; keeping the last key total");
                    }
                }
                if this.readings.finish_dbsize_refresh() {
                    this.refresh_dbsize_now(cx);
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn refresh_redis_info(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        if !self.link.beat_due(self.facts.nodes().0) {
            return;
        }

        let slow_logs_check_interval = pacing::SLOW_LOG_CHECK_SECS;
        let mut last_slow_logs_checked_at = self.readings.slow_logs_checked_at();
        // The status bar's key total: one `DBSIZE` at most every minute —
        // the number needn't track every tick, and a steady denominator
        // reads calmer than a live one. The same call the initial load
        // made, so the total never changes its meaning between the connect
        // and a minute later.
        let refresh_dbsize = unix_ts() - self.readings.dbsize_refreshed_at() >= pacing::DBSIZE_REFRESH_SECS;
        // Captured so a user-driven `DBSIZE` that lands while this beat is
        // in flight is not overwritten by the beat's older total.
        let dbsize_stamp = self.readings.dbsize_refreshed_at();
        if last_slow_logs_checked_at == 0 {
            last_slow_logs_checked_at = unix_ts() - slow_logs_check_interval;
        }

        let server_id = self.server_id.clone();
        let db = self.db;
        let server_id_clone = server_id.clone();

        self.link.beat_started();
        self.spawn(
            ServerTask::RefreshRedisInfo,
            move || async move {
                let at = ServerDb::new(server_id.as_str(), db);
                // One command when it can be: with a single master the probe
                // is the `INFO` itself (`RedisClient::heartbeat_probe`).
                let start = Instant::now();
                let probed_info = heartbeat_probe(&at).await?;
                let latency = start.elapsed();
                let now = unix_ts();
                let slow_logs = if now - last_slow_logs_checked_at >= slow_logs_check_interval {
                    // A denied / missing SLOWLOG is normal on restricted
                    // servers — the feature matrix greys the panel — so this
                    // only leaves a trace, never a notice.
                    let slow_logs = slow_logs(&at).await.unwrap_or_else(|e| {
                        debug!(error = %e, "slowlog sample skipped");
                        Vec::new()
                    });
                    Some(slow_logs)
                } else {
                    None
                };

                // Default `INFO` (not `ALL`): it already carries every section
                // `RedisInfo::parse` reads, while `ALL` additionally streams
                // `commandstats`/`latencystats` — payloads that scale with the
                // number of distinct commands and were parsed into nothing —
                // on every heartbeat tick, per master.
                let dbsize = if refresh_dbsize {
                    match dbsize(&at).await {
                        Ok(dbsize) => Some(dbsize),
                        // Denied on a restricted proxy: keep the last known
                        // total rather than guessing zero, and try again in a
                        // minute.
                        Err(e) => {
                            debug!(error = %e, "DBSIZE unavailable; keeping the last key total");
                            None
                        }
                    }
                } else {
                    None
                };
                let (servers, list): (Vec<_>, Vec<String>) = master_infos(&at, probed_info).await?.into_iter().unzip();
                // No texts at all: a user who may not run `INFO`. The probe's
                // PING is then the whole beat — the link is up, and there are
                // no metrics to record (a row of zeros is not a sample).
                let has_info = !list.is_empty();
                let infos: Vec<RedisInfo> = list.iter().map(|info| RedisInfo::parse(info)).collect();
                // Cluster only: keep a per-master persistence row so the
                // Persistence panel can show which node is still forking.
                let persistence_nodes = if servers.len() > 1 {
                    servers
                        .iter()
                        .zip(infos.iter())
                        .map(|(srv, node)| PersistenceNodeSnapshot {
                            label: format!("{}:{}", srv.host, srv.port).into(),
                            rdb_last_save_time: node.metrics.rdb_last_save_time,
                            rdb_changes_since_last_save: node.metrics.rdb_changes_since_last_save,
                            rdb_bgsave_in_progress: node.metrics.rdb_bgsave_in_progress,
                            rdb_last_bgsave_success: node.metrics.rdb_last_bgsave_success,
                            aof_enabled: node.metrics.aof_enabled,
                            aof_rewrite_in_progress: node.metrics.aof_rewrite_in_progress,
                            aof_current_size: node.metrics.aof_current_size,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let node_zones: Vec<(String, String)> = servers
                    .iter()
                    .zip(infos.iter())
                    .filter(|(_, node)| !node.meta.availability_zone.is_empty())
                    .map(|(srv, node)| {
                        (
                            format!("{}:{}", srv.host, srv.port),
                            node.meta.availability_zone.clone(),
                        )
                    })
                    .collect();
                let mut info = aggregate_redis_info(infos);
                info.persistence_nodes = persistence_nodes;
                info.node_zones = node_zones;
                info.metrics.timestamp_ms = unix_ts_millis();
                info.metrics.latency_ms = latency.as_millis() as u64;
                Ok((info, has_info, slow_logs, dbsize))
            },
            move |this, result, cx| {
                this.link.heartbeat_in_flight = false;
                // Disconnected by hand while this beat was in flight: its
                // answer is from before the disconnect. Let through, a success
                // put the health back to Connected and the next beats never
                // ran (they are paused while offline), so the dot showed a
                // live link that could be neither dropped nor re-dialled.
                if this.link.manually_offline {
                    return;
                }
                match result {
                    Ok((info, has_info, slow_logs, dbsize)) => {
                        let loading = info.metrics.loading;
                        // Sentinel: the node we resolved as master now reports
                        // itself a replica — a failover happened underneath us.
                        // Drop the client so the next call re-asks the sentinel
                        // (and tell the user once; the cached value is reloaded
                        // when the link settles).
                        if info.meta.role == "slave" && this.nodes_description().server_type == "Sentinel" {
                            warn!(
                                server_id = server_id_clone.as_str(),
                                "sentinel master demoted; re-resolving"
                            );
                            forget_client(&ServerDb::new(server_id_clone.as_str(), db));
                            if this.link.link_notice_due() {
                                this.emit_warning_notification(i18n_status_bar(cx, "conn_master_changed"), cx);
                            }
                        }
                        if has_info {
                            METRICS_CACHE.add_metrics(&server_id_clone, info.metrics);
                            maybe_persist_metrics(&server_id_clone, info.metrics, cx);
                        }
                        if refresh_dbsize {
                            this.readings.note_beat_dbsize(dbsize_stamp, dbsize);
                        }
                        this.readings.set_redis_info(info);
                        if let Some(slow_logs) = slow_logs {
                            this.readings.note_slow_logs(slow_logs, last_slow_logs_checked_at);
                        }
                        this.evaluate_server_alerts(has_info, false, cx);
                        cx.emit(ServerEvent::ServerRedisInfoUpdated);
                        this.note_ping_result(true, cx);
                        // Last, because it resets the state it runs on: the
                        // load a refused or still-loading server failed. `INFO`
                        // answers while the dataset loads and little else does,
                        // so a beat that says so is not yet the time.
                        if !loading {
                            this.retry_failed_load(cx);
                        }
                    }
                    Err(e) => {
                        // Connection is invalid, remove cached client
                        forget_client(&ServerDb::new(server_id_clone.as_str(), db));
                        error!(error = %e, "Ping failed, client connection removed");
                        // Remember *why* so the offline tooltip and the
                        // editor's link panel can name it. Set before
                        // note_ping_result, which emits the health transition
                        // they read.
                        this.note_link_failure(&e);
                        this.note_ping_result(false, cx);
                    }
                }
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{RedisClientDescription, ReplicationRole};
    use crate::states::server::Link;
    use crate::states::{ConnectionHealth, LinkProblem};
    use crate::test_support::{EventLog, init_ui, record_events, take_events};
    use gpui::TestAppContext;

    /// The aggregate starts from the first master's INFO, so the fold has
    /// to add the *other* masters to it: a loop over every node counted the
    /// first one twice (7 keys in the switcher for a db holding 6). The
    /// weighted avg_ttl still reads every node, the first included.
    /// Valkey 8.1+ writes the pause into `INFO clients`; a cluster is
    /// paused where any master is, the stronger action winning; Redis
    /// writes nothing, and then the pause this app sent is what shows.
    #[test]
    fn a_client_pause_is_the_server_s_word_where_it_has_one_else_this_app_s() {
        let info = RedisInfo::parse(
            "# Clients\r\nconnected_clients:3\r\npaused_reason:client_pause\r\npaused_actions:write\r\npaused_timeout_milliseconds:2991\r\n",
        );
        assert_eq!(info.metrics.paused_actions, PausedActions::Write);
        assert_eq!(info.metrics.paused_timeout_ms, 2991);
        assert_eq!(info.metrics.paused_reason, PauseReason::ClientPause);

        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        let server = client_pause_now(Some(&info.metrics), Some(("all", later)), now).expect("paused");
        assert_eq!(server.actions, "write", "the server's word over the local record");
        assert!(server.from_server);
        assert_eq!(server.remaining, Duration::from_millis(2991));

        let none = RedisInfo::parse("# Clients\r\npaused_actions:none\r\npaused_timeout_milliseconds:0\r\n");
        assert_eq!(
            client_pause_now(Some(&none.metrics), Some(("write", later)), now),
            None,
            "a server that reports no pause is believed over a stale local record"
        );

        let redis = RedisInfo::parse("# Clients\r\nconnected_clients:3\r\n");
        let local = client_pause_now(Some(&redis.metrics), Some(("write", later)), now).expect("local pause");
        assert!(!local.from_server);
        assert_eq!(local.remaining, Duration::from_secs(5));
        assert_eq!(
            client_pause_now(Some(&redis.metrics), Some(("write", now)), now),
            None,
            "run out"
        );
        assert_eq!(client_pause_now(None, None, now), None);

        let mut a = RedisInfo::default();
        a.metrics.paused_actions = PausedActions::None;
        let mut b = RedisInfo::default();
        b.metrics.paused_actions = PausedActions::All;
        b.metrics.paused_timeout_ms = 900;
        let total = aggregate_redis_info(vec![a, b]);
        assert_eq!(total.metrics.paused_actions, PausedActions::All);
        assert_eq!(total.metrics.paused_timeout_ms, 900);
    }

    #[test]
    fn a_cluster_aggregate_counts_the_first_master_once() {
        let node = |keys: u64, clients: u64, expires: u64, avg_ttl: u64| {
            let mut info = RedisInfo::default();
            info.metrics.connected_clients = clients;
            info.metrics.used_memory = keys * 100;
            info.metrics.expired_keys = keys;
            info.metrics.rejected_connections = clients;
            info.keyspace
                .insert("db0".to_string(), RedisKeySpaceStats { keys, expires, avg_ttl });
            info
        };
        let total = aggregate_redis_info(vec![node(1, 1, 1, 1000), node(2, 2, 1, 3000), node(3, 3, 0, 0)]);
        let db0 = total.keyspace.get("db0").expect("db0 present");
        assert_eq!(db0.keys, 6);
        assert_eq!(db0.expires, 2);
        assert_eq!(db0.avg_ttl, 2000, "weighted over both nodes with volatile keys");
        assert_eq!(total.metrics.connected_clients, 6);
        assert_eq!(total.metrics.used_memory, 600);
        assert_eq!(total.metrics.expired_keys, 6, "every master's, not the first one's");
        assert_eq!(total.metrics.rejected_connections, 6);

        let single = aggregate_redis_info(vec![node(4, 1, 0, 0)]);
        assert_eq!(single.keyspace.get("db0").expect("db0").keys, 4);
    }

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
        // The same section, kept whole for the Topology page.
        assert_eq!(info.replication.role, ReplicationRole::Primary);
        assert_eq!(info.replication.connected_replicas, 2);
        assert_eq!(info.replication.replicas[1].lag_bytes, 400);

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

    #[test]
    fn heartbeat_backoff_doubles_per_miss_and_caps_at_a_minute() {
        let secs: Vec<u64> = (0..8)
            .map(|failures| Link::heartbeat_backoff(failures).as_secs())
            .collect();
        // 0 misses is never asked for, but must not underflow below the interval.
        assert_eq!(secs, vec![2, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(Link::heartbeat_backoff(u32::MAX).as_secs(), 60);
    }

    #[test]
    fn a_second_keyspace_change_asks_dbsize_again_after_the_first_lands() {
        let mut state = ZedisServerState::new();
        assert!(state.readings.request_dbsize_refresh());
        assert!(!state.readings.request_dbsize_refresh());
        assert!(state.readings.finish_dbsize_refresh());
        assert!(state.readings.request_dbsize_refresh());
        assert!(!state.readings.finish_dbsize_refresh());
    }

    /// The heartbeat's outcome moves the health a step at a time and
    /// announces only a change: misses are Reconnecting, then Offline at the
    /// threshold, each backing the next attempt off; a reply after them is
    /// Connected again, and said so. A disconnect by hand is Offline at once,
    /// announced once, and the heartbeat sends nothing after it.
    #[gpui::test]
    fn the_heartbeat_moves_the_health_and_announces_only_a_change(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);
        state.update(cx, |state, cx| {
            state.server_id = "linktest".into();
            state.note_ping_result(true, cx);
            state.note_ping_result(true, cx);
            assert_eq!(state.connection_health(), ConnectionHealth::Connected);
            for miss in 1..=Link::PING_OFFLINE_THRESHOLD {
                state.note_ping_result(false, cx);
                assert!(state.link.heartbeat_retry_at.is_some(), "miss {miss} backs off");
            }
            assert_eq!(state.connection_health(), ConnectionHealth::Offline);
            assert!(state.link_problem().is_some());
            state.note_ping_result(true, cx);
            assert_eq!(state.connection_health(), ConnectionHealth::Connected);
            assert!(state.link.heartbeat_retry_at.is_none());
            assert!(state.link_problem().is_none());
        });
        cx.run_until_parked();
        let restored = cx.update(|cx| i18n_status_bar(cx, "conn_restored"));
        assert_eq!(
            take_events(&log),
            [
                "ConnectionHealthChanged".to_string(),
                "ConnectionHealthChanged".to_string(),
                "ConnectionHealthChanged".to_string(),
                "ConnectionHealthChanged".to_string(),
                format!("notice Success: {restored}"),
            ]
        );

        state.update(cx, |state, cx| {
            state.disconnect(cx);
            state.disconnect(cx);
            assert_eq!(state.connection_health(), ConnectionHealth::Offline);
            assert_eq!(state.link_problem(), Some(LinkProblem::Disconnected));
            state.refresh_redis_info(cx);
            assert!(!state.link.heartbeat_in_flight, "nothing is sent while offline by hand");
        });
        cx.run_until_parked();
        assert_eq!(take_events(&log), ["ConnectionHealthChanged"]);
    }

    /// A beat goes out only when it is due: one at a time, not while the
    /// last miss is still backing off, not more often than a large
    /// cluster's stretched interval, and in a background tab not more than
    /// once a relaxed interval — until the tab comes back, and sent back to
    /// the background it starts a new window.
    #[gpui::test]
    fn a_beat_goes_out_only_when_it_is_due(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);
        let beats = |log: &EventLog| {
            take_events(log)
                .iter()
                .filter(|event| event.as_str() == "TaskStarted(RefreshRedisInfo)")
                .count()
        };

        state.update(cx, |state, cx| state.refresh_redis_info(cx));
        assert_eq!(beats(&log), 0, "no server, no beat");
        state.update(cx, |state, cx| {
            state.server_id = "beattest".into();
            state.refresh_redis_info(cx);
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 1, "one at a time");
        // It fails — there is no such server — and the miss backs off.
        cx.run_until_parked();
        take_events(&log);
        state.update(cx, |state, cx| {
            assert!(state.link.heartbeat_retry_at.is_some());
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 0, "not while backing off");

        state.update(cx, |state, cx| {
            state.link.heartbeat_retry_at = None;
            state.facts.set_topology((8, 0), RedisClientDescription::default());
            state.link.last_heartbeat_at = Some(Instant::now());
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 0, "inside a large cluster's stretched interval");
        state.update(cx, |state, cx| {
            state.facts.set_topology((1, 0), RedisClientDescription::default());
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 1, "a small one's is not stretched");
        cx.run_until_parked();
        take_events(&log);

        state.update(cx, |state, cx| {
            state.link.heartbeat_retry_at = None;
            state.set_background(true);
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 1, "the first beat of a background tab");
        cx.run_until_parked();
        take_events(&log);
        state.update(cx, |state, cx| {
            state.link.heartbeat_retry_at = None;
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 0, "within the relaxed interval");
        state.update(cx, |state, cx| {
            state.set_background(false);
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 1, "a tab that comes back beats at once");
        cx.run_until_parked();
        take_events(&log);
        state.update(cx, |state, cx| {
            state.link.heartbeat_retry_at = None;
            state.set_background(true);
            state.refresh_redis_info(cx);
        });
        assert_eq!(beats(&log), 1, "back in the background, a new window");
        cx.run_until_parked();
    }
}

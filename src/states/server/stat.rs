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

use crate::connection::get_connection_manager;
use crate::helpers::unix_ts;
use crate::states::{ServerEvent, ServerTask, ZedisServerState};
use gpui::prelude::*;
use parking_lot::RwLock;
use redis::cmd;
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;
use std::time::Instant;
use tracing::error;

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

#[derive(Debug, Default, Clone, Copy)]
pub struct RedisMetrics {
    pub timestamp: i64,
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
}

static METRICS_CACHE: LazyLock<MetricsCache> = LazyLock::new(|| MetricsCache::new(1800));

pub fn get_metrics_cache() -> &'static MetricsCache {
    &METRICS_CACHE
}

#[derive(Debug, Default, Clone)]
pub struct RedisInfo {
    pub meta: RedisServerMeta,
    // pub latency: Duration,
    pub metrics: RedisMetrics,
    // --- Keyspace (db0, db1...) ---
    pub keyspace: HashMap<String, RedisKeySpaceStats>,
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

                    "used_cpu_sys" => info.metrics.used_cpu_sys = parse_f64(value),
                    "used_cpu_user" => info.metrics.used_cpu_user = parse_f64(value),

                    _ => {}
                }
            }
        }

        info
    }
}

// --- Helpers ---

fn parse_u64(v: &str) -> u64 {
    v.parse().unwrap_or(0)
}

fn parse_f64(v: &str) -> f64 {
    v.parse().unwrap_or(0.0)
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
    pub fn refresh_redis_info(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }

        let last_checked_at = self.last_slow_logs_checked_at;
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
                let slow_logs = if now - last_checked_at > 60 {
                    // ignore get slow error
                    let slow_logs = client.get_slow_logs(Some(last_checked_at)).await.unwrap_or_default();
                    Some(slow_logs)
                } else {
                    None
                };

                let list: Vec<String> = client.query_async_masters(vec![cmd("INFO").arg("ALL").clone()]).await?;
                let infos: Vec<RedisInfo> = list.iter().map(|info| RedisInfo::parse(info)).collect();
                let mut info = aggregate_redis_info(infos);
                info.metrics.timestamp = now;
                info.metrics.latency_ms = latency.as_millis() as u64;
                Ok((info, slow_logs))
            },
            move |this, result, cx| match result {
                Ok((info, slow_logs)) => {
                    METRICS_CACHE.add_metrics(&server_id_clone, info.metrics);
                    this.redis_info = Some(info);
                    if let Some(slow_logs) = slow_logs {
                        this.slow_logs = slow_logs;
                        this.last_slow_logs_checked_at = unix_ts();
                    }
                    cx.emit(ServerEvent::ServerRedisInfoUpdated);
                }
                Err(e) => {
                    // Connection is invalid, remove cached client
                    get_connection_manager().remove_client(&server_id_clone);
                    error!(error = %e, "Ping failed, client connection removed");
                }
            },
            cx,
        );
    }
}

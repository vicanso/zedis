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

use crate::connection::{SlowLogEntry, get_connection_manager};
use crate::helpers::unix_ts;
use crate::states::{ServerEvent, ServerTask, ZedisServerState};
use gpui::prelude::*;
use redis::cmd;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::error;

#[derive(Debug, Default, Clone)]
pub struct RedisKeySpaceStats {
    pub keys: u64,
    pub expires: u64,
    pub avg_ttl: u64,
}

#[derive(Debug, Default, Clone)]
pub struct RedisInfo {
    pub last_checked_at: i64,
    pub latency: Duration,
    // --- Server ---
    pub redis_version: String,
    pub os: String,
    pub uptime_in_seconds: u64,
    pub role: String, // master / slave

    // --- Clients ---
    pub connected_clients: u64,
    pub blocked_clients: u64,

    // --- Memory ---
    pub used_memory: u64,
    pub used_memory_human: String,
    pub used_memory_rss: u64,
    pub maxmemory: u64,
    pub mem_fragmentation_ratio: f64,

    // --- Stats ---
    pub total_connections_received: u64,
    pub total_commands_processed: u64,
    pub instantaneous_ops_per_sec: u64,
    pub instantaneous_input_kbps: f64,
    pub instantaneous_output_kbps: f64,
    pub keyspace_hits: u64,
    pub keyspace_misses: u64,
    pub evicted_keys: u64,

    // --- CPU ---
    pub used_cpu_sys: f64,
    pub used_cpu_user: f64,

    // --- Keyspace (db0, db1...) ---
    pub keyspace: HashMap<String, RedisKeySpaceStats>,

    /// The slow logs of the Redis server
    pub slow_logs: Vec<SlowLogEntry>,
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
        total.connected_clients += info.connected_clients;
        total.blocked_clients += info.blocked_clients;

        // --- Memory (Sum) ---
        total.used_memory += info.used_memory;
        total.used_memory_rss += info.used_memory_rss;
        total.maxmemory += info.maxmemory;

        // --- Memory Health (Max) ---
        // We take the maximum fragmentation ratio because the "worst" node
        // determines the fragmentation risk of the cluster.
        if info.mem_fragmentation_ratio > total.mem_fragmentation_ratio {
            total.mem_fragmentation_ratio = info.mem_fragmentation_ratio;
        }

        // --- Stats (Sum) ---
        total.total_connections_received += info.total_connections_received;
        total.total_commands_processed += info.total_commands_processed;
        total.instantaneous_ops_per_sec += info.instantaneous_ops_per_sec;
        total.instantaneous_input_kbps += info.instantaneous_input_kbps;
        total.instantaneous_output_kbps += info.instantaneous_output_kbps;
        total.keyspace_hits += info.keyspace_hits;
        total.keyspace_misses += info.keyspace_misses;
        total.evicted_keys += info.evicted_keys;

        // --- CPU (Sum) ---
        // Accumulate total CPU time consumed by the entire cluster
        total.used_cpu_sys += info.used_cpu_sys;
        total.used_cpu_user += info.used_cpu_user;

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
    total.used_memory_human = humansize::format_size(total.used_memory, humansize::DECIMAL);

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
                    "redis_version" => info.redis_version = value.to_string(),
                    "os" => info.os = value.to_string(),
                    "uptime_in_seconds" => info.uptime_in_seconds = parse_u64(value),
                    "role" => info.role = value.to_string(),

                    "connected_clients" => info.connected_clients = parse_u64(value),
                    "blocked_clients" => info.blocked_clients = parse_u64(value),

                    "used_memory" => info.used_memory = parse_u64(value),
                    "used_memory_human" => info.used_memory_human = value.to_string(),
                    "used_memory_rss" => info.used_memory_rss = parse_u64(value),
                    "maxmemory" => info.maxmemory = parse_u64(value),
                    "mem_fragmentation_ratio" => info.mem_fragmentation_ratio = parse_f64(value),

                    "total_connections_received" => info.total_connections_received = parse_u64(value),
                    "total_commands_processed" => info.total_commands_processed = parse_u64(value),
                    "instantaneous_ops_per_sec" => info.instantaneous_ops_per_sec = parse_u64(value),
                    "instantaneous_input_kbps" => info.instantaneous_input_kbps = parse_f64(value),
                    "instantaneous_output_kbps" => info.instantaneous_output_kbps = parse_f64(value),
                    "keyspace_hits" => info.keyspace_hits = parse_u64(value),
                    "keyspace_misses" => info.keyspace_misses = parse_u64(value),
                    "evicted_keys" => info.evicted_keys = parse_u64(value),

                    "used_cpu_sys" => info.used_cpu_sys = parse_f64(value),
                    "used_cpu_user" => info.used_cpu_user = parse_f64(value),

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

        let last_checked_at = self
            .redis_info
            .as_ref()
            .map(|item| item.last_checked_at)
            .unwrap_or_else(|| unix_ts() - 300);
        let server_id = self.server_id.clone();
        let db = self.db;
        let server_id_clone = server_id.clone();

        self.spawn(
            ServerTask::RefreshRedisInfo,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let start = Instant::now();
                client.ping().await?;
                let slow_logs = client.get_slow_logs(Some(last_checked_at)).await?;
                let latency = start.elapsed();

                let list: Vec<String> = client.query_async_masters(vec![cmd("INFO").arg("ALL").clone()]).await?;
                let infos: Vec<RedisInfo> = list.iter().map(|info| RedisInfo::parse(info)).collect();
                let mut info = aggregate_redis_info(infos);
                info.last_checked_at = unix_ts();
                info.latency = latency;
                info.slow_logs = slow_logs;
                Ok(info)
            },
            move |this, result, cx| match result {
                Ok(info) => {
                    this.redis_info = Some(info);
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

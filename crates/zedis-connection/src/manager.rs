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

use super::{
    async_connection::{
        RedisAsyncConn, open_seed_connection, open_single_connection, query_async_masters,
        query_async_masters_pipeline, remove_connection_from_pool, resolve_connection_timeout,
        resolve_response_timeout,
    },
    config::{RedisServer, SERVER_TYPE_AUTO, SERVER_TYPE_CLUSTER, SERVER_TYPE_SENTINEL, get_server},
    ssh_cluster_connection::SshMultiplexedConnection,
};
use crate::{async_connection::configure_client_connection, error::Error};
use futures::future::try_join_all;
use rand::RngExt;
use redis::{Cmd, FromRedisValue, InfoDict, ParsingError, Role, Value, aio::MultiplexedConnection, cluster, cmd};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Duration,
};
use tracing::{debug, info};
use zedis_core::string::format_host_port;
use zedis_core::ttl_cache::TtlCache;

type HashScanValue = (u64, Vec<(Vec<u8>, Vec<u8>)>);

type Result<T, E = Error> = std::result::Result<T, E>;

/// Matches Redis errors that should fallback to standalone: NOPERM, unknown command, or command not available.
/// Case-insensitive: redis-rs 1.x renders the error category as `NoPerm:` rather than echoing the raw
/// `NOPERM` code, which used to let a read-only ACL user (no `+role`) fail the whole connect.
static IGNORABLE_SERVER_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)noperm|unknown|not available").expect("failed to compile regex"));

fn is_ignorable_server_error(msg: &str) -> bool {
    IGNORABLE_SERVER_ERROR.is_match(msg)
}

#[cfg(test)]
mod ignorable_error_tests {
    use super::is_ignorable_server_error;

    #[test]
    fn matches_the_driver_rendering_of_noperm_and_unknown_commands() {
        // redis-rs 1.x: category word, then the server's detail.
        assert!(is_ignorable_server_error(
            "NoPerm: User ro has no permissions to run the 'role' command"
        ));
        // Older rendering / raw reply text.
        assert!(is_ignorable_server_error(
            "NOPERM this user has no permissions to run the 'role' command"
        ));
        assert!(is_ignorable_server_error(
            "An error was signalled by the server - ResponseError: unknown command 'ROLE'"
        ));
        assert!(is_ignorable_server_error("ERR command not available"));
        assert!(!is_ignorable_server_error("Connection refused (os error 61)"));
    }
}

// Global singleton for ConnectionManager
static CONNECTION_MANAGER: LazyLock<ConnectionManager> = LazyLock::new(ConnectionManager::new);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessMode {
    #[default]
    ReadWrite,
    // readonly mode(config)
    SafeMode,
    // acl limit
    StrictReadOnly,
}

// Enum representing the type of Redis server
#[derive(Debug, Clone, PartialEq)]
enum ServerType {
    Standalone,
    Sentinel,
    Cluster,
}

impl From<usize> for ServerType {
    fn from(value: usize) -> Self {
        match value {
            SERVER_TYPE_SENTINEL => ServerType::Sentinel,
            SERVER_TYPE_CLUSTER => ServerType::Cluster,
            _ => ServerType::Standalone,
        }
    }
}

/// Per-key heat metric. Only one of FREQ / IDLETIME is meaningful for a
/// given Redis instance because the two are mutually exclusive: LFU policies
/// (`*-lfu`) populate FREQ; everything else populates IDLETIME via the LRU
/// clock. `None` means the policy is unknown or the OBJECT command failed
/// (e.g. NOPERM, key gone).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeatMetric {
    #[default]
    None,
    /// LFU access frequency counter. Higher = hotter.
    Freq(u64),
    /// LRU idle time in seconds. Higher = colder.
    IdleTime(u64),
}

/// What heat command to issue per key during a memory scan, decided once
/// per scan from `maxmemory-policy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeatProbe {
    #[default]
    None,
    Freq,
    IdleTime,
}

impl HeatProbe {
    /// Map `maxmemory-policy` config string to the right OBJECT subcommand.
    /// Returns `None` only when the policy can't be detected — Redis defaults
    /// to LRU clock tracking even under `noeviction`, so OBJECT IDLETIME is
    /// the safe fallback for non-LFU servers.
    pub fn from_policy(policy: &str) -> Self {
        if policy.contains("lfu") {
            HeatProbe::Freq
        } else if policy.is_empty() {
            HeatProbe::None
        } else {
            HeatProbe::IdleTime
        }
    }
    pub fn redis_subcommand(self) -> Option<&'static str> {
        match self {
            HeatProbe::None => None,
            HeatProbe::Freq => Some("FREQ"),
            HeatProbe::IdleTime => Some("IDLETIME"),
        }
    }
}

pub struct KeyMemoryUsage {
    // key name
    pub key: String,
    // memory usage in bytes
    pub memory_usage: u64,
    // key type
    pub key_type: String,
    // ttl in seconds
    pub ttl: i64,
    // heat metric (FREQ or IDLETIME or unknown)
    pub heat: HeatMetric,
}

/// One aggregated `INFO commandstats` row.
#[derive(Debug, Clone)]
pub struct CommandStat {
    pub name: String,
    pub calls: u64,
    pub usec: u64,
}

/// Where in a key's value the search needle was found.
#[derive(Debug, Clone)]
pub enum MatchLocation {
    /// The whole string value.
    Value,
    /// A hash field — carries the field name.
    Field(String),
    /// A list element — carries its index.
    Index(usize),
    /// A set / sorted-set member — carries the member (truncated).
    Member(String),
}

/// One value-search hit: the key plus where the needle matched.
#[derive(Debug, Clone)]
pub struct ValueMatch {
    pub key: String,
    /// The key's Redis type (`string` / `hash` / `list` / `set` / `zset`).
    pub key_type: String,
    pub location: MatchLocation,
}

/// Result of one [`RedisClient::scan_values_round`] page.
#[derive(Debug, Clone)]
pub struct ValueSearchRound {
    /// Per-master SCAN cursors to resume from (all zero = keyspace exhausted).
    pub cursors: Vec<u64>,
    /// Keys whose value matched this round, with the match location.
    pub matches: Vec<ValueMatch>,
    /// Keys examined this round.
    pub scanned: usize,
    /// Values skipped for exceeding the size / element-count gate.
    pub skipped_oversized: usize,
    /// True once every cursor returned to 0 (whole keyspace covered).
    pub done: bool,
}

/// Case-insensitive substring match on a lossy-UTF8 view of raw bytes.
fn contains_needle(bytes: &[u8], needle_lower: &str) -> bool {
    String::from_utf8_lossy(bytes).to_lowercase().contains(needle_lower)
}

/// Lossy-UTF8 render of a matched member/field, truncated for the location chip.
fn truncate_member(bytes: &[u8]) -> String {
    const MAX_CHARS: usize = 80;
    let s = String::from_utf8_lossy(bytes);
    if s.chars().count() > MAX_CHARS {
        let mut t: String = s.chars().take(MAX_CHARS).collect();
        t.push('…');
        t
    } else {
        s.into_owned()
    }
}

/// Parse `cmdstat_<name>:calls=N,usec=N,…` lines from an `INFO commandstats`
/// blob, summing into `agg` so several cluster nodes accumulate into one
/// total per command. Unrecognised lines are skipped.
fn aggregate_command_stats(text: &str, agg: &mut HashMap<String, (u64, u64)>) {
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("cmdstat_") else {
            continue;
        };
        let Some((name, fields)) = rest.split_once(':') else {
            continue;
        };
        let (mut calls, mut usec) = (0u64, 0u64);
        for kv in fields.split(',') {
            if let Some(v) = kv.strip_prefix("calls=") {
                calls = v.parse().unwrap_or(0);
            } else if let Some(v) = kv.strip_prefix("usec=") {
                usec = v.parse().unwrap_or(0);
            }
        }
        let entry = agg.entry(name.to_string()).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(calls);
        entry.1 = entry.1.saturating_add(usec);
    }
}

#[cfg(test)]
mod command_stats_tests {
    use super::aggregate_command_stats;
    use std::collections::HashMap;

    #[test]
    fn parses_and_sums_across_nodes() {
        let mut agg = HashMap::new();
        aggregate_command_stats(
            "cmdstat_get:calls=10,usec=20,usec_per_call=2.00\ncmdstat_set:calls=5,usec=15",
            &mut agg,
        );
        // A second node's blob accumulates into the same totals.
        aggregate_command_stats("cmdstat_get:calls=3,usec=6\n# comment\ngarbage", &mut agg);
        assert_eq!(agg.get("get"), Some(&(13, 26)));
        assert_eq!(agg.get("set"), Some(&(5, 15)));
        assert_eq!(agg.len(), 2);
    }
}

// Wrapper for the underlying Redis client
#[derive(Clone)]
enum RClient {
    // Boxed: RedisServer has grown (per-server key-tree prefs) and would
    // otherwise trip clippy::large_enum_variant against the cluster arms.
    Single(Box<RedisServer>),
    Cluster(cluster::ClusterClient),
    SshCluster(cluster::ClusterClient),
}

// Node roles in a Redis setup
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NodeRole {
    #[default]
    Master,
    Slave,
    Fail,
    Unknown, // e.g. "handshake", "noaddr"
}

// Represents a single Redis node
#[derive(Debug, Clone, Default)]
struct RedisNode {
    server: RedisServer,
    // connection_url: String,
    role: NodeRole,
    master_name: Option<String>,
    /// Cluster node id (column 0 of `CLUSTER NODES`). `None` outside cluster mode.
    cluster_id: Option<String>,
    /// For replicas in cluster mode: the cluster id of the master they replicate.
    master_cluster_id: Option<String>,
    /// Slot ranges owned by this master node (cluster mode only).
    slots: Vec<(u16, u16)>,
    /// In-flight slot migrations reported on this node (`CLUSTER NODES`).
    migrations: Vec<SlotMigration>,
}

impl RedisNode {
    /// `host:port` as a label — an IPv6 literal bracketed.
    pub fn host_port(&self) -> String {
        format_host_port(&self.server.host, self.server.port)
    }
}

/// Direction of an in-flight slot migration reported by `CLUSTER NODES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotMigrationKind {
    /// This node owns the slot and is migrating it *to* `peer_id`.
    Migrating,
    /// This node is importing the slot *from* `peer_id`.
    Importing,
}

/// One migrating/importing marker from a `CLUSTER NODES` line
/// (e.g. `[12345->-target_id]` / `[12345-<-source_id]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotMigration {
    pub slot: u16,
    pub kind: SlotMigrationKind,
    pub peer_id: String,
}

// Information parsed from `CLUSTER NODES` command
#[derive(Debug, Clone)]
pub struct ClusterNodeInfo {
    pub id: String,
    pub ip: String,
    pub port: u16,
    pub role: NodeRole,
    /// For slaves: id of the master they replicate. `None` for masters.
    pub master_id: Option<String>,
    /// Slot ranges owned by this node (only set on masters).
    pub slots: Vec<(u16, u16)>,
    /// In-flight migration markers on this node.
    pub migrations: Vec<SlotMigration>,
}

/// One contiguous owned slot range with its master, used by the Topology
/// slot map (0–16383 bar).
#[derive(Debug, Clone)]
pub struct ClusterSlotRange {
    pub start: u16,
    pub end: u16,
    pub node_id: String,
    pub addr: String,
    /// Stable palette index for the owning master (UI colour).
    pub color_index: usize,
}

/// A slot currently in MIGRATING/IMPORTING state, paired across source
/// and target when both sides of the gossip are visible.
#[derive(Debug, Clone)]
pub struct ClusterMigrationEntry {
    pub slot: u16,
    pub source_id: String,
    pub source_addr: String,
    pub target_id: String,
    pub target_addr: String,
}

/// Per-master summary for the slot legend / load heatmap join key.
#[derive(Debug, Clone)]
pub struct ClusterMasterSlotSummary {
    pub node_id: String,
    pub addr: String,
    pub slot_count: u32,
    pub color_index: usize,
}

/// Structured cluster slot view built from `CLUSTER NODES` — ownership
/// ranges, active migrations, and per-master slot counts. Empty outside
/// cluster mode.
#[derive(Debug, Clone, Default)]
pub struct ClusterSlotMap {
    pub owners: Vec<ClusterSlotRange>,
    pub migrations: Vec<ClusterMigrationEntry>,
    pub masters: Vec<ClusterMasterSlotSummary>,
    /// How many of the 16384 hash slots are assigned to some master.
    pub assigned_slots: u32,
}

/// Total hash slots in a Redis Cluster.
pub const CLUSTER_HASH_SLOTS: u32 = 16384;

/// Parses a Redis address string like "ip:port@cport" or just "ip:port".
/// Establishes an asynchronous connection based on the client type.
async fn get_async_connection(client: &RClient, db: usize, use_cache: bool) -> Result<RedisAsyncConn> {
    match client {
        RClient::Single(config) => {
            let conn = open_single_connection(config, db, use_cache).await?;
            Ok(RedisAsyncConn::Single(conn))
        }
        RClient::Cluster(client) => {
            // Per-server timeouts are baked into the cluster builder at
            // build time, so the no-config getter uses them.
            let mut conn = client.get_async_connection().await?;
            configure_client_connection(&mut conn).await;
            Ok(RedisAsyncConn::Cluster(conn))
        }
        RClient::SshCluster(client) => {
            let mut conn: redis::cluster_async::ClusterConnection<SshMultiplexedConnection> =
                client.get_async_generic_connection().await?;
            configure_client_connection(&mut conn).await;
            Ok(RedisAsyncConn::SshCluster(conn))
        }
    }
}

/// Condition word for a batch `EXPIRE` (Redis 7.0+, and every Valkey
/// release — the fork started at 7.2). The server answers 0 for a key the
/// condition rejects, so a batch can report how many keys it really touched.
/// A key without a TTL counts as an infinite one for GT / LT: GT never
/// touches it, LT always does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpireCondition {
    /// Only keys that have no TTL yet.
    Nx,
    /// Only keys that already have a TTL.
    Xx,
    /// Only when the new expiry is later than the current one.
    Gt,
    /// Only when the new expiry is sooner than the current one.
    Lt,
}

impl ExpireCondition {
    /// The option word as `EXPIRE` spells it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Nx => "NX",
            Self::Xx => "XX",
            Self::Gt => "GT",
            Self::Lt => "LT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowLogEntry {
    pub id: i64,
    pub timestamp: i64,
    /// The slow log's measure — `amount` as microseconds. Meaningless for
    /// a `COMMANDLOG` size log, whose third field is bytes.
    pub duration: Duration,
    /// The entry's third field, raw: microseconds in the slow log, bytes in
    /// `COMMANDLOG`'s `LARGE-REQUEST` / `LARGE-REPLY` logs.
    pub amount: u64,
    pub args: Vec<String>,
    pub client_addr: Option<String>,
    pub client_name: Option<String>,
}

impl FromRedisValue for SlowLogEntry {
    fn from_redis_value(v: Value) -> Result<Self, ParsingError> {
        let items = match v {
            Value::Array(items) => items,
            _ => return Err(ParsingError::from("Expected Array for SlowLogEntry")),
        };

        if items.len() < 4 {
            return Err(ParsingError::from("SlowLogEntry expects at least 4 fields"));
        }

        // get int from value
        let get_int = |val: &Value| -> Result<i64, ParsingError> {
            match val {
                Value::Int(i) => Ok(*i),
                Value::BulkString(bytes) => String::from_utf8_lossy(bytes)
                    .parse::<i64>()
                    .map_err(|_| ParsingError::from("Invalid integer string")),
                Value::SimpleString(s) => s
                    .parse::<i64>()
                    .map_err(|_| ParsingError::from("Invalid integer string")),
                _ => Err(ParsingError::from("Expected Integer field")),
            }
        };

        // get string from value
        let get_string = |val: &Value| -> String {
            match val {
                Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                Value::SimpleString(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                Value::Okay => "OK".to_string(),
                Value::Nil => "".to_string(),
                _ => "".to_string(),
            }
        };

        let id = get_int(&items[0])?;
        let timestamp = get_int(&items[1])?;
        let amount = get_int(&items[2])?.max(0) as u64;

        let args = match &items[3] {
            Value::Array(arg_items) => arg_items.iter().map(get_string).collect(),
            _ => vec![],
        };

        let client_addr = if items.len() > 4 {
            let s = get_string(&items[4]);
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        };

        let client_name = if items.len() > 5 {
            let s = get_string(&items[5]);
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        };

        Ok(SlowLogEntry {
            id,
            timestamp,
            duration: Duration::from_micros(amount),
            amount,
            args,
            client_addr,
            client_name,
        })
    }
}

// `connection` lives on the (Clone) `RedisClient` on purpose: `RedisAsyncConn`
// wraps redis multiplexed / cluster connections, which are cheap Arc-shared
// handles — cloning a `RedisClient` shares the same underlying connection, which
// is exactly the intended multiplexing behaviour (no per-clone socket).
/// What node discovery found for a server entry (private to this module
/// and the pool / pub-sub submodules that run discovery).
struct NodeDiscovery {
    nodes: Vec<RedisNode>,
    server_type: ServerType,
    /// Sentinel only: every master the sentinel monitors, sorted by name —
    /// the connected one and the alternatives a panel can offer.
    sentinel_master_names: Vec<String>,
}

impl NodeDiscovery {
    fn new(nodes: Vec<RedisNode>, server_type: ServerType) -> Self {
        Self {
            nodes,
            server_type,
            sentinel_master_names: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct RedisClient {
    access_mode: AccessMode,
    db: usize,
    databases: usize,
    modules: Vec<(String, Version)>,
    server_type: ServerType,
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    /// See [`NodeDiscovery::sentinel_master_names`].
    sentinel_master_names: Vec<String>,
    version: Version,
    is_valkey: bool,
    connection: RedisAsyncConn,
    /// What built `connection` — kept so a caller can open a *second*,
    /// uncached connection to the same server (`open_dedicated_connection`)
    /// without re-running topology discovery.
    client: RClient,
}
/// One node in the structured topology: address, role marker glyph, and an
/// optional annotation (e.g. `slots 0-5460`, `(mymaster)`).
#[derive(Debug, Clone, Default)]
pub struct TopologyEntry {
    pub addr: String,
    pub role_marker: String,
    pub annotation: String,
    /// `CLUSTER NODES` node id (only populated for cluster mode; empty
    /// for sentinel/standalone where the concept doesn't apply). Used
    /// by `CLUSTER FORGET node_id` and `CLUSTER REPLICATE node_id`,
    /// both of which target by id rather than address.
    pub node_id: String,
    /// Sentinel master name (only populated for sentinel master rows;
    /// empty everywhere else). Used by `SENTINEL FAILOVER name`,
    /// `SENTINEL RESET pattern`, `SENTINEL REMOVE name` — Sentinel
    /// ops target by master name, not by addr or node_id.
    pub master_name: String,
}

/// One master plus the replicas it owns. Replicas already filtered to those
/// that actually replicate this master.
#[derive(Debug, Clone, Default)]
pub struct TopologyMaster {
    pub master: TopologyEntry,
    pub replicas: Vec<TopologyEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct RedisClientDescription {
    pub is_valkey: bool,
    pub server_type: String,
    pub master_nodes: String,
    pub slave_nodes: String,
    pub modules: String,
    /// Structured topology — each master with its replicas. Empty for
    /// standalone connections without grouped data; the consumer should fall
    /// back to `master_nodes` / `slave_nodes` flat strings then.
    pub topology: Vec<TopologyMaster>,
    /// Cluster slot ownership + migration markers. Empty outside cluster mode.
    pub slot_map: ClusterSlotMap,
    /// Sentinel only: every master the sentinel monitors, sorted by name.
    /// More than one means the entry did not name a master and the first
    /// was taken — the Topology panel offers the rest.
    pub sentinel_master_names: Vec<String>,
}

pub struct ConnectionManager {
    clients: TtlCache<u64, RedisClient>,
}

/// Global accessor for the connection manager.
pub fn get_connection_manager() -> &'static ConnectionManager {
    &CONNECTION_MANAGER
}

/// Clears expired clients from the connection manager.
pub fn clear_expired_clients() -> (usize, usize) {
    CONNECTION_MANAGER.clients.clear_expired()
}

mod client;
mod commandlog;
mod pool;
mod pubsub_channels;
mod replication;
mod sharded_pubsub;
mod slots;

pub use commandlog::CommandLogKind;
pub use pubsub_channels::{MAX_PUBSUB_CHANNELS, PubsubChannel, PubsubChannelsSnapshot};
pub use replication::FAILOVER_TIMEOUT_MS;
pub use sharded_pubsub::ShardedPubSub;
pub use slots::plan_reshard_slots;
#[allow(unused_imports)]
use slots::*;

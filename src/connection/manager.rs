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
        RedisAsyncConn, open_single_connection, query_async_masters, query_async_masters_pipeline,
        remove_connection_from_pool, resolve_connection_timeout, resolve_response_timeout,
    },
    config::{RedisServer, get_server},
    ssh_cluster_connection::SshMultiplexedConnection,
};
use crate::helpers::TtlCache;
use crate::{connection::async_connection::set_client_name, error::Error};
use futures::future::try_join_all;
use gpui::SharedString;
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
use tracing::{debug, error, info};

type HashScanValue = (u64, Vec<(Vec<u8>, Vec<u8>)>);

type Result<T, E = Error> = std::result::Result<T, E>;

/// Matches Redis errors that should fallback to standalone: NOPERM, unknown command, or command not available.
static IGNORABLE_SERVER_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"NOPERM|unknown|not available").expect("failed to compile regex"));

fn is_ignorable_server_error(msg: &str) -> bool {
    IGNORABLE_SERVER_ERROR.is_match(msg)
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
            2 => ServerType::Sentinel,
            3 => ServerType::Cluster,
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
    pub key: SharedString,
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
    Field(SharedString),
    /// A list element — carries its index.
    Index(usize),
    /// A set / sorted-set member — carries the member (truncated).
    Member(SharedString),
}

/// One value-search hit: the key plus where the needle matched.
#[derive(Debug, Clone)]
pub struct ValueMatch {
    pub key: SharedString,
    /// The key's Redis type (`string` / `hash` / `list` / `set` / `zset`).
    pub key_type: SharedString,
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
fn truncate_member(bytes: &[u8]) -> SharedString {
    const MAX_CHARS: usize = 80;
    let s = String::from_utf8_lossy(bytes);
    if s.chars().count() > MAX_CHARS {
        let mut t: String = s.chars().take(MAX_CHARS).collect();
        t.push('…');
        t.into()
    } else {
        s.into_owned().into()
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
    Single(RedisServer),
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
}

impl RedisNode {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
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
}

/// Parses a Redis address string like "ip:port@cport" or just "ip:port".
fn parse_address(address_str: &str) -> Result<(String, u16, Option<u16>)> {
    // Split into address part and optional cluster bus port part
    let (addr_part, cport_part) = address_str
        .split_once('@')
        .map(|(a, c)| (a, Some(c)))
        .unwrap_or((address_str, None));

    // Parse IP and Port
    let (ip, port_str) = addr_part.split_once(':').ok_or_else(|| Error::Invalid {
        message: format!("Invalid address format: {}", addr_part),
    })?;

    let port = port_str.parse::<u16>().map_err(|e| Error::Invalid {
        message: format!("Invalid port '{}': {}", port_str, e),
    })?;

    // Parse cluster bus port if present
    let cport = cport_part
        .map(|s| {
            s.parse::<u16>().map_err(|e| Error::Invalid {
                message: format!("Invalid cluster bus port '{}': {}", s, e),
            })
        })
        .transpose()?;

    Ok((ip.to_string(), port, cport))
}

/// Parses the output of the `CLUSTER NODES` command.
///
/// Columns (whitespace-separated):
///  0: node id
///  1: addr (`ip:port@cport[,hostname]`)
///  2: flags (comma-list, e.g. `master,myself`)
///  3: master id (`-` for masters)
///  4..7: ping-sent / pong-recv / config-epoch / link-state
///  8..: slot ranges, each either `N` (single) or `N-M`. Migration markers
///        like `[N->-id]` / `[N-<-id]` are skipped.
fn parse_cluster_nodes(raw_data: &str) -> Result<Vec<ClusterNodeInfo>> {
    let mut nodes = Vec::new();

    for line in raw_data.trim().lines() {
        debug!(line, "cluster nodes");
        let parts: Vec<&str> = line.split_whitespace().collect();

        // Basic validation: ensure enough columns exist
        if parts.len() < 8 {
            continue;
        }

        let id = parts[0].to_string();
        let (ip, port, _) = parse_address(parts[1])?;

        // Parse flags to determine role
        let flags: HashSet<String> = parts[2].split(',').map(String::from).collect();
        let role = if flags.contains("master") {
            NodeRole::Master
        } else if flags.contains("slave") {
            NodeRole::Slave
        } else if flags.contains("fail") {
            NodeRole::Fail
        } else {
            NodeRole::Unknown
        };

        let master_id = if parts[3] != "-" {
            Some(parts[3].to_string())
        } else {
            None
        };

        let mut slots = Vec::new();
        for raw in parts.iter().skip(8) {
            // Skip migration markers like `[5461->-target_id]` / `[5461-<-source_id]`.
            if raw.starts_with('[') {
                continue;
            }
            if let Some((lo, hi)) = raw.split_once('-')
                && let (Ok(lo), Ok(hi)) = (lo.parse::<u16>(), hi.parse::<u16>())
            {
                slots.push((lo, hi));
                continue;
            }
            if let Ok(single) = raw.parse::<u16>() {
                slots.push((single, single));
            }
        }

        nodes.push(ClusterNodeInfo {
            id,
            ip,
            port,
            role,
            master_id,
            slots,
        });
    }

    Ok(nodes)
}

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
            set_client_name(&mut conn).await;
            Ok(RedisAsyncConn::Cluster(conn))
        }
        RClient::SshCluster(client) => {
            let mut conn: redis::cluster_async::ClusterConnection<SshMultiplexedConnection> =
                client.get_async_generic_connection().await?;
            set_client_name(&mut conn).await;
            Ok(RedisAsyncConn::SshCluster(conn))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowLogEntry {
    pub id: i64,
    pub timestamp: i64,
    pub duration: Duration,
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
        let duration = get_int(&items[2])?;

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
            duration: Duration::from_micros(duration as u64),
            args,
            client_addr,
            client_name,
        })
    }
}

// TODO 是否在client中保存connection
#[derive(Clone)]
pub struct RedisClient {
    access_mode: AccessMode,
    db: usize,
    databases: usize,
    modules: Vec<(String, Version)>,
    server_type: ServerType,
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    version: Version,
    is_valkey: bool,
    connection: RedisAsyncConn,
}
/// One node in the structured topology: address, role marker glyph, and an
/// optional annotation (e.g. `slots 0-5460`, `(mymaster)`).
#[derive(Debug, Clone, Default)]
pub struct TopologyEntry {
    pub addr: SharedString,
    pub role_marker: SharedString,
    pub annotation: SharedString,
    /// `CLUSTER NODES` node id (only populated for cluster mode; empty
    /// for sentinel/standalone where the concept doesn't apply). Used
    /// by `CLUSTER FORGET node_id` and `CLUSTER REPLICATE node_id`,
    /// both of which target by id rather than address.
    pub node_id: SharedString,
    /// Sentinel master name (only populated for sentinel master rows;
    /// empty everywhere else). Used by `SENTINEL FAILOVER name`,
    /// `SENTINEL RESET pattern`, `SENTINEL REMOVE name` — Sentinel
    /// ops target by master name, not by addr or node_id.
    pub master_name: SharedString,
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
    pub server_type: SharedString,
    pub master_nodes: SharedString,
    pub slave_nodes: SharedString,
    pub modules: SharedString,
    /// Structured topology — each master with its replicas. Empty for
    /// standalone connections without grouped data; the consumer should fall
    /// back to `master_nodes` / `slave_nodes` flat strings then.
    pub topology: Vec<TopologyMaster>,
}
impl RedisClient {
    pub fn nodes(&self) -> (usize, usize) {
        (self.master_nodes.len(), self.nodes.len())
    }
    pub fn version(&self) -> String {
        self.version.to_string()
    }
    pub fn databases(&self) -> usize {
        self.databases
    }
    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    /// Returns the list of master node server configurations.
    pub fn master_servers(&self) -> Vec<RedisServer> {
        self.master_nodes.iter().map(|node| node.server.clone()).collect()
    }

    pub fn nodes_description(&self) -> RedisClientDescription {
        let master_nodes: Vec<String> = self.master_nodes.iter().map(|node| node.host_port()).collect();
        let slave_nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| !master_nodes.contains(&node.host_port()))
            .map(|node| node.host_port().clone())
            .collect();
        let modules = self
            .modules
            .iter()
            .map(|(name, ver)| format!("{name}@{ver}"))
            .collect::<Vec<_>>()
            .join(", ");
        let topology = self.build_topology();
        RedisClientDescription {
            is_valkey: self.is_valkey,
            server_type: format!("{:?}", self.server_type).into(),
            master_nodes: master_nodes.join(",").into(),
            slave_nodes: slave_nodes.join(",").into(),
            modules: modules.into(),
            topology,
        }
    }

    /// Build the structured topology consumed by the status-bar tooltip.
    ///
    /// Cluster mode groups replicas under their master via `master_cluster_id`
    /// and annotates the master with its slot ranges. Sentinel groups by
    /// `master_name`. Standalone returns an empty list so the caller falls
    /// back to its existing flat summary.
    fn build_topology(&self) -> Vec<TopologyMaster> {
        let role_marker = |role: &NodeRole| -> SharedString {
            match role {
                NodeRole::Master => "●",
                NodeRole::Slave => "↳",
                NodeRole::Fail => "✗",
                NodeRole::Unknown => "?",
            }
            .into()
        };
        let format_slots = |slots: &[(u16, u16)]| -> String {
            if slots.is_empty() {
                return String::new();
            }
            let parts: Vec<String> = slots
                .iter()
                .map(|(lo, hi)| if lo == hi { lo.to_string() } else { format!("{lo}-{hi}") })
                .collect();
            format!("slots {}", parts.join(","))
        };

        let mut out: Vec<TopologyMaster> = Vec::new();

        match self.server_type {
            ServerType::Cluster => {
                for master in self.master_nodes.iter() {
                    let entry = TopologyEntry {
                        addr: master.host_port().into(),
                        role_marker: role_marker(&master.role),
                        annotation: format_slots(&master.slots).into(),
                        node_id: master.cluster_id.clone().unwrap_or_default().into(),
                        master_name: SharedString::default(),
                    };
                    let replicas: Vec<TopologyEntry> = if let Some(master_id) = master.cluster_id.as_ref() {
                        self.nodes
                            .iter()
                            .filter(|n| {
                                n.master_cluster_id.as_ref() == Some(master_id)
                                    && (n.role == NodeRole::Slave || n.role == NodeRole::Fail)
                            })
                            .map(|replica| TopologyEntry {
                                addr: replica.host_port().into(),
                                role_marker: role_marker(&replica.role),
                                annotation: SharedString::default(),
                                node_id: replica.cluster_id.clone().unwrap_or_default().into(),
                                master_name: SharedString::default(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Sentinel => {
                for master in self.master_nodes.iter() {
                    let label = master.master_name.as_deref().unwrap_or("");
                    // node_id is left empty in sentinel mode — CLUSTER FORGET /
                    // REPLICATE don't apply. master_name is populated only on
                    // the master row (replicas don't directly receive
                    // SENTINEL ops, which all target by master name).
                    let entry = TopologyEntry {
                        addr: master.host_port().into(),
                        role_marker: role_marker(&master.role),
                        annotation: if label.is_empty() {
                            SharedString::default()
                        } else {
                            format!("({label})").into()
                        },
                        node_id: SharedString::default(),
                        master_name: SharedString::from(label.to_string()),
                    };
                    let replicas: Vec<TopologyEntry> = self
                        .nodes
                        .iter()
                        .filter(|n| n.role == NodeRole::Slave && n.master_name.as_deref() == Some(label))
                        .map(|replica| TopologyEntry {
                            addr: replica.host_port().into(),
                            role_marker: role_marker(&replica.role),
                            annotation: SharedString::default(),
                            node_id: SharedString::default(),
                            master_name: SharedString::default(),
                        })
                        .collect();
                    out.push(TopologyMaster {
                        master: entry,
                        replicas,
                    });
                }
            }
            ServerType::Standalone => {
                // No replicas surfaced from a single Standalone connection — let
                // the caller fall back to its existing summary.
            }
        }

        out
    }
    /// Returns the connection to the Redis server.
    /// # Returns
    /// * `RedisAsyncConn` - The connection to the Redis server.
    pub fn connection(&self) -> RedisAsyncConn {
        self.connection.clone()
    }

    /// Checks if the client is a cluster client.
    /// # Returns
    /// * `bool` - True if the client is a cluster client, false otherwise.
    pub fn is_cluster(&self) -> bool {
        self.server_type == ServerType::Cluster
    }
    /// Checks if the client version is at least the given version.
    /// # Arguments
    /// * `version` - The version to check.
    /// # Returns
    /// * `bool` - True if the client version is at least the given version, false otherwise.
    pub fn is_at_least_version(&self, version: &str) -> bool {
        self.version >= Version::parse(version).unwrap_or(Version::new(0, 0, 0))
    }
    pub fn supports_rejson(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "ReJSON")
    }

    /// Whether the RediSearch module is loaded on this server. The
    /// module name reported by `MODULE LIST` is the lowercase string
    /// `"search"` (true across all RediSearch versions 1.x → 2.x).
    /// Drives the visibility of the Search entry in the Tools menu.
    pub fn supports_search(&self) -> bool {
        self.modules.iter().any(|(name, _)| name == "search")
    }

    /// Unlinks keys on all master nodes concurrently.
    /// # Arguments
    /// * `keys_per_node` - A vector of vectors of keys, one for each master node.
    /// # Returns
    /// * `Result<(), Error>` - The result of the operation.
    pub async fn unlike_keys(&self, keys_per_node: Vec<Vec<SharedString>>) -> Result<(), Error> {
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (index, keys) in keys_per_node.iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("UNLINK").arg(key.as_str());
            }
            pipes[index] = Some(pipe);
        }
        query_async_masters_pipeline(master_addrs, self.db, pipes).await?;
        Ok(())
    }

    /// Unlinks keys that may be distributed across different nodes.
    pub async fn unlike_keys_scattered(&self, keys: Vec<SharedString>) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.is_cluster() {
            let mut conn = self.connection();
            let mut pipe = redis::pipe();
            for key in &keys {
                pipe.cmd("UNLINK").arg(key.as_str());
            }
            let _: () = pipe.query_async(&mut conn).await?;
            return Ok(());
        }
        let conn = self.connection();
        for chunk in keys.chunks(1000) {
            let futures = chunk.iter().map(|key| {
                let mut conn_clone = conn.clone();
                async move {
                    let _: () = cmd("UNLINK").arg(key.as_str()).query_async(&mut conn_clone).await?;
                    Ok::<(), Error>(())
                }
            });
            let _: Vec<()> = try_join_all(futures).await?;
        }
        Ok(())
    }

    /// Apply `EXPIRE` (`ttl_secs = Some`) or `PERSIST` (`None`) to many keys,
    /// cluster-safe. Mirrors [`Self::unlike_keys_scattered`]: one pipeline on a
    /// standalone, per-key concurrent commands (chunked) on a cluster so no
    /// cross-slot pipeline is ever built.
    pub async fn set_ttl_keys_scattered(&self, keys: Vec<SharedString>, ttl_secs: Option<u64>) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.is_cluster() {
            let mut conn = self.connection();
            let mut pipe = redis::pipe();
            for key in &keys {
                match ttl_secs {
                    Some(secs) => pipe.cmd("EXPIRE").arg(key.as_str()).arg(secs),
                    None => pipe.cmd("PERSIST").arg(key.as_str()),
                };
            }
            let _: Vec<i64> = pipe.query_async(&mut conn).await?;
            return Ok(());
        }
        let conn = self.connection();
        for chunk in keys.chunks(1000) {
            let futures = chunk.iter().map(|key| {
                let mut conn_clone = conn.clone();
                let key = key.clone();
                async move {
                    match ttl_secs {
                        Some(secs) => {
                            let _: i64 = cmd("EXPIRE")
                                .arg(key.as_str())
                                .arg(secs)
                                .query_async(&mut conn_clone)
                                .await?;
                        }
                        None => {
                            let _: i64 = cmd("PERSIST").arg(key.as_str()).query_async(&mut conn_clone).await?;
                        }
                    }
                    Ok::<(), Error>(())
                }
            });
            let _: Vec<()> = try_join_all(futures).await?;
        }
        Ok(())
    }

    /// Returns the memory usage of a key.
    /// # Arguments
    /// * `key` - The key to get the memory usage of.
    /// * `key_type` - The type of the key.
    /// # Returns
    /// * `Result<u64>` - The memory usage of the key.
    pub async fn memory_usage(&self, key: &str, key_type: &str) -> Result<u64> {
        let mut conn = self.connection.clone();
        let key_type = key_type.to_lowercase();

        if self.is_at_least_version("4.0.0") {
            let memory_usage: u64 = cmd("MEMORY").arg("USAGE").arg(key).query_async(&mut conn).await?;
            return Ok(memory_usage);
        }

        if key_type == "str" {
            let len: u64 = cmd("STRLEN").arg(key).query_async(&mut conn).await?;
            return Ok(len + 56);
        }

        let total_count: u64 = match key_type.as_str() {
            "list" => cmd("LLEN").arg(key).query_async(&mut conn).await?,
            "hash" => cmd("HLEN").arg(key).query_async(&mut conn).await?,
            "set" => cmd("SCARD").arg(key).query_async(&mut conn).await?,
            "zset" => cmd("ZCARD").arg(key).query_async(&mut conn).await?,
            _ => 0,
        };

        if total_count == 0 {
            return Ok(0);
        }

        if total_count < 1000 {
            let data: Vec<u8> = cmd("DUMP").arg(key).query_async(&mut conn).await?;
            return Ok(data.len() as u64);
        }

        let sample_count = 100;

        let (sample_bytes, actual_count) = match key_type.as_str() {
            "list" => {
                let items: Vec<Vec<u8>> = cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(sample_count - 1)
                    .query_async(&mut conn)
                    .await?;
                let len: usize = items.iter().map(|x| x.len()).sum();
                (len, items.len())
            }
            "set" => {
                let items: Vec<Vec<u8>> = cmd("SRANDMEMBER")
                    .arg(key)
                    .arg(sample_count)
                    .query_async(&mut conn)
                    .await?;
                let len: usize = items.iter().map(|x| x.len()).sum();
                (len, items.len())
            }
            "zset" => {
                let items: Vec<Vec<u8>> = cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(sample_count - 1)
                    .query_async(&mut conn)
                    .await?;

                let members_len: usize = items.iter().map(|x| x.len()).sum();
                // ZSET score (double, 8 bytes)
                let scores_len = items.len() * 8;
                (members_len + scores_len, items.len())
            }
            "hash" => {
                let (_, items): HashScanValue = cmd("HSCAN")
                    .arg(key)
                    .arg(0)
                    .arg("COUNT")
                    .arg(sample_count)
                    .query_async(&mut conn)
                    .await?;

                let len: usize = items.iter().map(|(k, v)| k.len() + v.len()).sum();
                (len, items.len())
            }
            _ => (0, 0),
        };

        if actual_count == 0 {
            return Ok(0);
        }

        let avg_len = sample_bytes as u64 / actual_count as u64;

        let overhead = match key_type.as_str() {
            "zset" => 64,
            "hash" | "set" => 32,
            _ => 16,
        };

        Ok(total_count * (avg_len + overhead))
    }

    /// Returns the slow logs of the Redis server, optionally filtered by timestamp.
    ///
    /// # Returns
    /// * `Vec<SlowLogEntry>` - A vector of slow log entries.
    pub async fn get_slow_logs(&self) -> Result<Vec<SlowLogEntry>> {
        let (_, logs_arr): (_, Vec<Vec<SlowLogEntry>>) = self
            .query_async_masters(vec![cmd("SLOWLOG").arg("GET").clone()])
            .await?;

        let mut logs: Vec<SlowLogEntry> = logs_arr.into_iter().flatten().collect();
        logs.sort_unstable_by_key(|entry| Reverse(entry.timestamp));

        Ok(logs)
    }
    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<T>` - A vector of results from the commands.
    pub async fn query_async_masters<T: FromRedisValue>(&self, cmds: Vec<Cmd>) -> Result<(Vec<RedisServer>, Vec<T>)> {
        let Some(first) = cmds.first() else {
            return Err(Error::Invalid {
                message: "Commands are empty".to_string(),
            });
        };
        let addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut new_cmds = vec![Some(first.clone()); addrs.len()];
        for (index, cmd) in cmds.iter().enumerate() {
            new_cmds[index] = Some(cmd.clone());
        }
        let values = query_async_masters(&addrs, self.db, new_cmds).await?;
        let values: Vec<T> = values.into_iter().flatten().collect();
        Ok((addrs, values))
    }
    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<Option<T>>` - A vector of results from the commands.
    pub async fn query_async_masters_with_option<T: FromRedisValue>(
        &self,
        cmds: Vec<Option<Cmd>>,
    ) -> Result<Vec<Option<T>>> {
        let addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let values = query_async_masters(&addrs, self.db, cmds).await?;
        Ok(values)
    }
    /// Calculates the total DB size across all masters.
    /// # Returns
    /// * `u64` - The total DB size.
    pub async fn dbsize(&self) -> Result<u64> {
        let (_, list): (_, Vec<u64>) = self.query_async_masters(vec![cmd("DBSIZE")]).await?;
        Ok(list.iter().sum())
    }
    /// Pings the server to check connectivity.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.connection.clone();
        let _: () = cmd("PING").query_async(&mut conn).await?;
        Ok(())
    }
    /// Returns the number of master nodes.
    /// # Returns
    /// * `usize` - The number of master nodes.
    pub fn count_masters(&self) -> Result<usize> {
        Ok(self.master_nodes.len())
    }
    /// Samples a subset of keys from the Redis server.
    /// # Arguments
    /// * `ratio` - The ratio of keys to sample.
    /// * `count` - The count of keys to sample.
    /// * `cursors` - The cursors to continue the scan from.
    /// # Returns
    /// * `(Vec<u64>, Vec<KeyMemoryUsage>)` - A tuple containing the new cursors and the key memory usage.
    pub async fn sample_scan_memory_usage(
        &self,
        ratio: f32,
        count: u64,
        cursors: Option<Vec<u64>>,
        heat: HeatProbe,
    ) -> Result<(u64, Vec<u64>, Vec<KeyMemoryUsage>)> {
        let pattern = "*";
        let (cursors, mut keys_per_node) = self.scan_nodes(cursors, pattern, count, None).await?;

        let total_count: usize = keys_per_node.iter().map(|keys| keys.len()).sum();

        if ratio < 1.0 {
            let mut rng = rand::rng();
            for keys in keys_per_node.iter_mut() {
                keys.retain(|_| rng.random::<f32>() < ratio);
            }
        }
        let capacity = keys_per_node.iter().map(|keys| keys.len()).sum();
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        let heat_subcommand = heat.redis_subcommand();
        let cmds_per_key: usize = if heat_subcommand.is_some() { 4 } else { 3 };
        for (index, keys) in keys_per_node.iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("TYPE")
                    .arg(key.as_str())
                    .cmd("MEMORY")
                    .arg("USAGE")
                    .arg(key.as_str())
                    .arg("SAMPLES")
                    .arg("5")
                    .cmd("TTL")
                    .arg(key.as_str());
                if let Some(sub) = heat_subcommand {
                    // OBJECT FREQ / OBJECT IDLETIME — both O(1).
                    pipe.cmd("OBJECT").arg(sub).arg(key.as_str());
                }
            }
            pipes[index] = Some(pipe);
        }

        let results_per_node = query_async_masters_pipeline(master_addrs, self.db, pipes).await?;

        let mut keys_memory_usage = Vec::with_capacity(capacity);
        for (index, results) in results_per_node.into_iter().enumerate() {
            let Some(results) = results else {
                continue;
            };
            let keys = &keys_per_node[index];
            for (i, chunk) in results.chunks_exact(cmds_per_key).enumerate() {
                if i >= keys.len() {
                    break;
                }
                let key = &keys[i];

                let key_type = match &chunk[0] {
                    Value::SimpleString(s) => s.clone(),
                    Value::BulkString(d) => String::from_utf8_lossy(d).to_string(),
                    _ => "unknown".to_string(),
                };

                let memory: u64 = match &chunk[1] {
                    Value::Int(m) => *m as u64,
                    _ => 0,
                };

                let ttl: i64 = match &chunk[2] {
                    Value::Int(t) => *t,
                    _ => -2,
                };
                if ttl == -2 {
                    continue;
                }

                // OBJECT FREQ/IDLETIME may legitimately error per-key
                // (key vanished between SCAN and pipeline execution, or
                // policy mismatch on an older Redis). Treat any non-Int
                // result as "unknown heat" rather than failing the batch.
                let heat = match (heat, chunk.get(3)) {
                    (HeatProbe::Freq, Some(Value::Int(v))) => HeatMetric::Freq((*v).max(0) as u64),
                    (HeatProbe::IdleTime, Some(Value::Int(v))) => HeatMetric::IdleTime((*v).max(0) as u64),
                    _ => HeatMetric::None,
                };

                keys_memory_usage.push(KeyMemoryUsage {
                    key: key.clone(),
                    memory_usage: memory,
                    key_type,
                    ttl,
                    heat,
                });
            }
        }

        Ok((total_count as u64, cursors, keys_memory_usage))
    }

    /// Returns the active `maxmemory-policy` setting, or an empty string if
    /// the command fails (NOPERM, restricted environment). The caller should
    /// treat empty as "unknown" and skip heat probing.
    pub async fn maxmemory_policy(&self) -> Result<String> {
        let mut conn = self.connection.clone();
        let res: redis::RedisResult<HashMap<String, String>> = cmd("CONFIG")
            .arg("GET")
            .arg("maxmemory-policy")
            .query_async(&mut conn)
            .await;
        match res {
            Ok(map) => Ok(map.get("maxmemory-policy").cloned().unwrap_or_default()),
            Err(_) => Ok(String::new()),
        }
    }

    /// Fetch `INFO commandstats`, aggregated across all master nodes on a
    /// cluster (where the reply is a per-node map) or the single node
    /// otherwise. Returns one [`CommandStat`] per command.
    pub async fn command_stats(&self) -> Result<Vec<CommandStat>> {
        let mut conn = self.connection.clone();
        let info: Value = cmd("INFO").arg("commandstats").query_async(&mut conn).await?;
        let mut agg: HashMap<String, (u64, u64)> = HashMap::new();
        match info {
            Value::Map(items) => {
                for (_, node_val) in items {
                    if let Ok(text) = String::from_redis_value(node_val) {
                        aggregate_command_stats(&text, &mut agg);
                    }
                }
            }
            other => {
                if let Ok(text) = String::from_redis_value(other) {
                    aggregate_command_stats(&text, &mut agg);
                }
            }
        }
        Ok(agg
            .into_iter()
            .map(|(name, (calls, usec))| CommandStat { name, calls, usec })
            .collect())
    }

    /// One bounded round of **search-by-value**: SCAN a single page across all
    /// masters, then for each *string* key read its value (size-gated) and
    /// case-insensitively substring-match it against `needle_lower` (which must
    /// already be lowercased). Non-string keys and values larger than
    /// `max_value_bytes` are skipped (the latter counted in `skipped_oversized`).
    ///
    /// The caller drives this in a cancellable loop, accumulating across pages
    /// until the keyspace is exhausted (`done`) or a scan/time budget trips —
    /// results are an explicit **sample**, never guaranteed exhaustive. Reads
    /// route per-key through the shared cluster-aware connection (they pipeline
    /// and don't block), guarded by the caller's caps rather than a dedicated
    /// connection.
    pub async fn scan_values_round(
        &self,
        pattern: &str,
        needle_lower: &str,
        max_value_bytes: u64,
        max_container_elems: u64,
        cursors: Option<Vec<u64>>,
        page_count: u64,
    ) -> Result<ValueSearchRound> {
        let (cursors, keys_per_node) = self.scan_nodes(cursors, pattern, page_count, None).await?;
        let mut conn = self.connection.clone();
        let mut matches = Vec::new();
        let mut scanned = 0usize;
        let mut skipped_oversized = 0usize;
        for keys in keys_per_node {
            for key in keys {
                scanned += 1;
                let key_str = key.as_ref();
                // A key may vanish mid-scan; treat read errors as "no match".
                let key_type: String = cmd("TYPE")
                    .arg(key_str)
                    .query_async(&mut conn)
                    .await
                    .unwrap_or_default();
                // Only the first match per key is recorded (one row per key);
                // the inline preview shows the full value. Containers are gated
                // on element count so a giant collection isn't pulled whole.
                let location = match key_type.as_str() {
                    "string" => {
                        let len: u64 = cmd("STRLEN").arg(key_str).query_async(&mut conn).await.unwrap_or(0);
                        if len > max_value_bytes {
                            skipped_oversized += 1;
                            continue;
                        }
                        let value: Vec<u8> = cmd("GET").arg(key_str).query_async(&mut conn).await.unwrap_or_default();
                        contains_needle(&value, needle_lower).then_some(MatchLocation::Value)
                    }
                    "hash" => {
                        let n: u64 = cmd("HLEN").arg(key_str).query_async(&mut conn).await.unwrap_or(0);
                        if n > max_container_elems {
                            skipped_oversized += 1;
                            continue;
                        }
                        let fields: Vec<(Vec<u8>, Vec<u8>)> = cmd("HGETALL")
                            .arg(key_str)
                            .query_async(&mut conn)
                            .await
                            .unwrap_or_default();
                        fields
                            .into_iter()
                            .find(|(f, v)| contains_needle(f, needle_lower) || contains_needle(v, needle_lower))
                            .map(|(f, _)| MatchLocation::Field(truncate_member(&f)))
                    }
                    "list" => {
                        let n: u64 = cmd("LLEN").arg(key_str).query_async(&mut conn).await.unwrap_or(0);
                        if n > max_container_elems {
                            skipped_oversized += 1;
                            continue;
                        }
                        let items: Vec<Vec<u8>> = cmd("LRANGE")
                            .arg(key_str)
                            .arg(0)
                            .arg(-1)
                            .query_async(&mut conn)
                            .await
                            .unwrap_or_default();
                        items
                            .into_iter()
                            .position(|e| contains_needle(&e, needle_lower))
                            .map(MatchLocation::Index)
                    }
                    "set" => {
                        let n: u64 = cmd("SCARD").arg(key_str).query_async(&mut conn).await.unwrap_or(0);
                        if n > max_container_elems {
                            skipped_oversized += 1;
                            continue;
                        }
                        let members: Vec<Vec<u8>> = cmd("SMEMBERS")
                            .arg(key_str)
                            .query_async(&mut conn)
                            .await
                            .unwrap_or_default();
                        members
                            .into_iter()
                            .find(|m| contains_needle(m, needle_lower))
                            .map(|m| MatchLocation::Member(truncate_member(&m)))
                    }
                    "zset" => {
                        let n: u64 = cmd("ZCARD").arg(key_str).query_async(&mut conn).await.unwrap_or(0);
                        if n > max_container_elems {
                            skipped_oversized += 1;
                            continue;
                        }
                        let members: Vec<Vec<u8>> = cmd("ZRANGE")
                            .arg(key_str)
                            .arg(0)
                            .arg(-1)
                            .query_async(&mut conn)
                            .await
                            .unwrap_or_default();
                        members
                            .into_iter()
                            .find(|m| contains_needle(m, needle_lower))
                            .map(|m| MatchLocation::Member(truncate_member(&m)))
                    }
                    // Streams and module types aren't searched.
                    _ => None,
                };
                if let Some(location) = location {
                    matches.push(ValueMatch {
                        key: key.clone(),
                        key_type: key_type.into(),
                        location,
                    });
                }
            }
        }
        let done = cursors.iter().all(|&c| c == 0);
        Ok(ValueSearchRound {
            cursors,
            matches,
            scanned,
            skipped_oversized,
            done,
        })
    }

    /// Build a bounded, type-aware text preview of a key's value (for the
    /// value-search preview pane). Containers are sampled to ~200 elements.
    pub async fn get_value_preview(&self, key: &str) -> Result<String> {
        let mut conn = self.connection.clone();
        let key_type: String = cmd("TYPE").arg(key).query_async(&mut conn).await?;
        const N: isize = 200;
        let text = match key_type.as_str() {
            "string" => {
                let v: Vec<u8> = cmd("GET").arg(key).query_async(&mut conn).await?;
                String::from_utf8_lossy(&v).into_owned()
            }
            "hash" => {
                let fields: Vec<(Vec<u8>, Vec<u8>)> = cmd("HGETALL").arg(key).query_async(&mut conn).await?;
                fields
                    .iter()
                    .take(N as usize)
                    .map(|(f, v)| format!("{}: {}", String::from_utf8_lossy(f), String::from_utf8_lossy(v)))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "list" => {
                let items: Vec<Vec<u8>> = cmd("LRANGE").arg(key).arg(0).arg(N - 1).query_async(&mut conn).await?;
                items
                    .iter()
                    .map(|e| String::from_utf8_lossy(e).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "set" => {
                let members: Vec<Vec<u8>> = cmd("SRANDMEMBER").arg(key).arg(N).query_async(&mut conn).await?;
                members
                    .iter()
                    .map(|m| String::from_utf8_lossy(m).into_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "zset" => {
                let members: Vec<(Vec<u8>, f64)> = cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(N - 1)
                    .arg("WITHSCORES")
                    .query_async(&mut conn)
                    .await?;
                members
                    .iter()
                    .map(|(m, s)| format!("{} ({s})", String::from_utf8_lossy(m)))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            other => format!("(type: {other} — open in editor to view)"),
        };
        Ok(text)
    }

    /// Fetch a key's raw value bytes via `GET` for the cross-server diff
    /// (empty when the key is absent). A wrong-type key surfaces an error.
    pub async fn get_key_bytes(&self, key: &str) -> Result<Vec<u8>> {
        let mut conn = self.connection.clone();
        let value: Option<Vec<u8>> = cmd("GET").arg(key).query_async(&mut conn).await?;
        Ok(value.unwrap_or_default())
    }

    /// Initiates a SCAN operation across all masters.
    /// # Arguments
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<SharedString>)` - A tuple containing the new cursors and the keys.
    pub async fn first_scan(
        &self,
        pattern: &str,
        count: u64,
        with_ttl: bool,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<(SharedString, SharedString, i64)>)> {
        let (cursors, keys) = self.scan(None, pattern, count, with_ttl, type_filter).await?;
        Ok((cursors, keys))
    }
    pub async fn scan_nodes(
        &self,
        cursors: Option<Vec<u64>>,
        pattern: &str,
        count: u64,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<Vec<SharedString>>)> {
        debug!("scan, cursors: {cursors:?}, pattern: {pattern}, count: {count}");
        let mut first_scan = false;
        let cur = if let Some(cursors) = cursors {
            cursors
        } else {
            first_scan = true;
            let master_count = self.count_masters()?;
            vec![0; master_count]
        };

        if !first_scan && cur.iter().all(|&c| c == 0) {
            return Ok((cur.clone(), vec![vec![]; cur.len()]));
        }

        // Build one Option<Cmd> per master: Some for active nodes, None for completed ones.
        let cmds: Vec<Option<Cmd>> = cur
            .iter()
            .map(|&cursor| {
                if first_scan || cursor != 0 {
                    let mut c = cmd("SCAN");
                    c.cursor_arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(count);
                    // Redis 6.0+ server-side type filter (gated by the caller).
                    if let Some(t) = type_filter {
                        c.arg("TYPE").arg(t);
                    }
                    Some(c)
                } else {
                    None
                }
            })
            .collect();

        let values: Vec<Option<(u64, Vec<Vec<u8>>)>> = self.query_async_masters_with_option(cmds).await?;

        let mut next_cursors = cur;
        let mut keys_per_node: Vec<Vec<SharedString>> = vec![vec![]; next_cursors.len()];

        for (idx, result) in values.into_iter().enumerate() {
            if let Some((new_cursor, keys_in_node)) = result {
                next_cursors[idx] = new_cursor;
                let mut node_keys = Vec::with_capacity(keys_in_node.len());
                for k in keys_in_node {
                    node_keys.push(SharedString::new(String::from_utf8_lossy(&k).into_owned()));
                }
                keys_per_node[idx] = node_keys;
            }
        }

        Ok((next_cursors, keys_per_node))
    }
    /// Continues a SCAN operation.
    /// # Arguments
    /// * `cursors` - A vector of cursors for each master.
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<SharedString>)` - A tuple containing the new cursors and the keys.
    pub async fn scan(
        &self,
        cursors: Option<Vec<u64>>,
        pattern: &str,
        count: u64,
        with_ttl: bool,
        type_filter: Option<&str>,
    ) -> Result<(Vec<u64>, Vec<(SharedString, SharedString, i64)>)> {
        // Server-side TYPE filter on Redis 6.0+; the client-side `retain` below
        // covers older servers (the per-key TYPE is fetched regardless). TYPE
        // filters within each COUNT batch, so a sparse type just needs more
        // rounds — the caller's paging loop handles that.
        let server_type = if type_filter.is_some() && self.is_at_least_version("6.0.0") {
            type_filter
        } else {
            None
        };
        let (new_cursors, keys_per_node) = self.scan_nodes(cursors, pattern, count, server_type).await?;

        // Pipeline TYPE (+ optional TTL) per key in one RTT per master.
        // TTL is skipped entirely when the caller doesn't need it — saves
        // one command per key on large dbs where the chip is disabled.
        let cmds_per_key = if with_ttl { 2 } else { 1 };
        let master_addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let mut pipes: Vec<Option<redis::Pipeline>> = vec![None; master_addrs.len()];
        for (idx, keys) in keys_per_node.iter().enumerate() {
            if !keys.is_empty() {
                let mut pipe = redis::pipe();
                for key in keys {
                    pipe.cmd("TYPE").arg(key.as_str());
                    if with_ttl {
                        pipe.cmd("TTL").arg(key.as_str());
                    }
                }
                pipes[idx] = Some(pipe);
            }
        }

        let pipe_results = query_async_masters_pipeline(master_addrs, self.db, pipes).await?;

        let capacity: usize = keys_per_node.iter().map(|ks| ks.len()).sum();
        let mut all_keys = Vec::with_capacity(capacity);
        for (idx, keys) in keys_per_node.into_iter().enumerate() {
            let results = pipe_results.get(idx).and_then(|r| r.as_ref());
            for (i, key) in keys.into_iter().enumerate() {
                let type_val = results.and_then(|vals| vals.get(i * cmds_per_key));
                let key_type = type_val
                    .map(|val| match val {
                        Value::SimpleString(s) => SharedString::from(s.clone()),
                        Value::BulkString(d) => SharedString::from(String::from_utf8_lossy(d).into_owned()),
                        _ => SharedString::default(),
                    })
                    .unwrap_or_default();
                let ttl_secs: i64 = if with_ttl {
                    match results.and_then(|vals| vals.get(i * cmds_per_key + 1)) {
                        Some(Value::Int(t)) => *t,
                        _ => -2,
                    }
                } else {
                    // Sentinel: TTL was not fetched. Renderer treats -2 as
                    // "no chip" so it's safe to use it here too.
                    -2
                };
                all_keys.push((key, key_type, ttl_secs));
            }
        }
        // Always filter client-side too: covers Redis < 6.0 (no server TYPE)
        // and is a no-op when the server already filtered.
        if let Some(t) = type_filter {
            all_keys.retain(|(_, key_type, _)| key_type.as_ref() == t);
        }
        Ok((new_cursors, all_keys))
    }
}

pub struct ConnectionManager {
    clients: TtlCache<u64, RedisClient>,
}

/// Detects the type of Redis server (Sentinel, Cluster, or Standalone).
/// This function checks the role of the Redis server and returns the server type.
/// # Arguments
/// * `client` - The Redis client to check the server type.
/// # Returns
/// * `ServerType` - The type of the Redis server.
async fn detect_server_type(mut conn: MultiplexedConnection) -> Result<ServerType> {
    // Check if it's a Sentinel
    // Note: `ROLE` command might not exist on old Redis versions, consider fallback if needed.
    // Assuming modern Redis here.
    let role: Role = cmd("ROLE").query_async(&mut conn).await?;

    if let Role::Sentinel { .. } = role {
        return Ok(ServerType::Sentinel);
    }

    // Check if Cluster mode is enabled via INFO command
    let info: InfoDict = cmd("INFO").arg("cluster").query_async(&mut conn).await?;
    let cluster_enabled = info.get("cluster_enabled").unwrap_or(0i64);

    if cluster_enabled == 1 {
        Ok(ServerType::Cluster)
    } else {
        Ok(ServerType::Standalone)
    }
}

async fn check_permission_by_probing(mut conn: RedisAsyncConn) -> bool {
    let probe: redis::RedisResult<String> = cmd("SET")
        .arg("_zedis_auth_test_")
        .arg("1")
        .arg("EX")
        .arg("1")
        .query_async(&mut conn)
        .await;

    match probe {
        Ok(_) => false,
        Err(e) => e.code() == Some("NOPERM"),
    }
}

async fn safe_check_user_readonly(mut conn: RedisAsyncConn) -> bool {
    let user: String = cmd("ACL")
        .arg("WHOAMI")
        .query_async(&mut conn)
        .await
        .unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    let result: redis::RedisResult<String> = cmd("ACL")
        .arg("DRYRUN")
        .arg(user)
        .arg("SET")
        .arg("zedis")
        .arg("treexie")
        .query_async(&mut conn)
        .await;
    match result {
        Ok(res) => res != "OK",

        Err(e) => {
            if let Some(code) = e.code()
                && code == "NOPERM"
            {
                if e.to_string().contains("acl|dryrun") {
                    return check_permission_by_probing(conn).await;
                }
                return true;
            }
            false
        }
    }
}

async fn get_modules(mut conn: RedisAsyncConn) -> Result<Vec<(String, Version)>> {
    let module_list: Vec<redis::Value> = cmd("MODULE").arg("LIST").query_async(&mut conn).await?;
    let mut modules = Vec::with_capacity(module_list.len());
    for module_info in module_list {
        if let Value::Array(info_kv) = module_info {
            // 遍历内部的 Key-Value 数组 (例如 ["name", "ReJSON", "ver", 20407])
            let mut name: Option<String> = None;
            let mut version = Version::new(0, 0, 0);
            let mut iter = info_kv.chunks(2);
            while let Some([key, val]) = iter.next() {
                if let Value::BulkString(k) = key {
                    match k.as_slice() {
                        b"name" => {
                            if let Value::BulkString(v) = val {
                                name = Some(String::from_utf8_lossy(v).into_owned());
                            }
                        }
                        b"ver" => {
                            if let Value::Int(v) = val {
                                let v = *v as u64;
                                version = Version::new(v / 10000, (v % 10000) / 100, v % 100);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if let Some(name) = name {
                modules.push((name, version));
            }
        }
    }
    Ok(modules)
}
async fn get_databases(mut conn: RedisAsyncConn) -> Result<usize> {
    // Step 1 — CONFIG GET databases: the exact count on self-hosted /
    // unrestricted servers.
    let config_reply: redis::RedisResult<Vec<String>> =
        cmd("CONFIG").arg("GET").arg("databases").query_async(&mut conn).await;
    if let Ok(reply) = config_reply
        && let Some(count) = reply.get(1).and_then(|s| s.parse::<usize>().ok()).filter(|&n| n > 0)
    {
        return Ok(count);
    }

    // Step 2 — CONFIG blocked (e.g. AWS ElastiCache) → degrade to INFO
    // keyspace, which ACLs and managed clouds almost always allow. It lists
    // only *non-empty* DBs, so the highest `dbN:` line proves at least N+1
    // selectable DBs exist; clamp up to the conventional 16 so the switcher
    // offers the usual range.
    let info_reply: redis::RedisResult<String> = cmd("INFO").arg("keyspace").query_async(&mut conn).await;
    if let Ok(info) = info_reply {
        let mut max_db: Option<usize> = None;
        for line in info.lines() {
            if let Some(rest) = line.strip_prefix("db")
                && let Some(n) = rest.split(':').next().and_then(|s| s.parse::<usize>().ok())
            {
                max_db = Some(max_db.map_or(n, |m| m.max(n)));
            }
        }
        if let Some(n) = max_db {
            return Ok((n + 1).max(16));
        }
    }

    // Both probes inconclusive (CONFIG blocked + every DB empty). Fall back to
    // 1 — the user can still set an explicit count in the server config.
    Ok(1)
}
impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            clients: TtlCache::new(Duration::from_secs(5 * 60)),
        }
    }
    /// Discovers Redis nodes and server type based on initial configuration.
    async fn get_redis_nodes(&self, name: &str) -> Result<(Vec<RedisNode>, ServerType)> {
        let config = get_server(name)?;
        let (mut conn, server_type) = {
            let conn = match open_single_connection(&config, 0, false).await {
                Ok(conn) => conn,
                Err(e) => {
                    if !e.to_string().contains("AuthenticationFailed") {
                        error!("detect server type failed: {e:?}, use standalone mode");
                        return Ok((
                            vec![RedisNode {
                                server: config.clone(),
                                role: NodeRole::Master,
                                ..Default::default()
                            }],
                            ServerType::Standalone,
                        ));
                    }
                    // sentinel without password
                    // detect server type again
                    let mut tmp_config = config.clone();
                    tmp_config.password = None;
                    open_single_connection(&tmp_config, 0, false).await?
                }
            };
            if let Some(server_type) = config.server_type
                && server_type > 0
            {
                (conn, server_type.into())
            } else {
                match detect_server_type(conn.clone()).await {
                    Ok(server_type) => (conn, server_type),
                    Err(e) => {
                        if !is_ignorable_server_error(&e.to_string()) {
                            return Err(e);
                        }
                        error!("detect server type failed: {e:?}, use standalone mode");
                        (conn, ServerType::Standalone)
                    }
                }
            }
        };
        match server_type {
            ServerType::Cluster => {
                // Fetch cluster topology
                let nodes: String = cmd("CLUSTER").arg("NODES").query_async(&mut conn).await?;
                // Parse nodes and convert to RedisNode
                let nodes = parse_cluster_nodes(&nodes)?
                    .iter()
                    .map(|item| {
                        let mut tmp_config = config.clone();
                        tmp_config.port = item.port;
                        if !item.ip.is_empty() {
                            tmp_config.host = item.ip.clone();
                        }

                        RedisNode {
                            server: tmp_config,
                            role: item.role.clone(),
                            cluster_id: Some(item.id.clone()),
                            master_cluster_id: item.master_id.clone(),
                            slots: item.slots.clone(),
                            ..Default::default()
                        }
                    })
                    .collect();
                Ok((nodes, server_type))
            }
            ServerType::Sentinel => {
                // let mut conn = client.get_multiplexed_async_connection().await?;
                // Fetch masters from Sentinel
                let masters_response: Vec<HashMap<String, String>> =
                    cmd("SENTINEL").arg("MASTERS").query_async(&mut conn).await?;
                let mut nodes = vec![];

                for item in masters_response {
                    let ip = item.get("ip").ok_or_else(|| Error::Invalid {
                        message: "ip is not found".to_string(),
                    })?;
                    let port: u16 = item
                        .get("port")
                        .ok_or_else(|| Error::Invalid {
                            message: "port is not found".to_string(),
                        })?
                        .parse()
                        .map_err(|e| Error::Invalid {
                            message: format!("Invalid port {e:?}"),
                        })?;
                    let name = item.get("name").ok_or_else(|| Error::Invalid {
                        message: "master_name is not found".to_string(),
                    })?;
                    // Filter by master name if configured
                    if let Some(master_name) = &config.master_name
                        && name != master_name
                    {
                        continue;
                    }
                    let mut tmp_config = config.clone();
                    tmp_config.host = ip.clone();
                    tmp_config.port = port;

                    nodes.push(RedisNode {
                        server: tmp_config,
                        role: NodeRole::Master,
                        master_name: Some(name.clone()),
                        ..Default::default()
                    });
                }
                // Check for ambiguous master configuration
                let unique_masters: HashSet<_> = nodes.iter().filter_map(|n| n.master_name.as_ref()).collect();
                if unique_masters.len() > 1 {
                    return Err(Error::Invalid {
                        message: format!(
                            "Multiple masters found in Sentinel, please specify master_name, master_names: {unique_masters:?}"
                        ),
                    });
                }

                Ok((nodes, server_type))
            }
            _ => Ok((
                vec![RedisNode {
                    server: config.clone(),
                    role: NodeRole::Master,
                    ..Default::default()
                }],
                server_type,
            )),
        }
    }
    pub fn remove_client(&self, server_id: &str, db: usize) {
        let Ok(config) = get_server(server_id) else {
            return;
        };
        let key = config.get_hash(db);
        self.clients.remove(&key);
        remove_connection_from_pool(&config, db);
    }
    pub async fn get_pubsub_connection(&self, server_id: &str) -> Result<redis::aio::PubSub> {
        let config = get_server(server_id)?;
        let url = config.get_connection_url();
        let client = if let Some(certificates) = config.tls_certificates() {
            redis::Client::build_with_tls(url, certificates)
        } else {
            redis::Client::open(url)
        }?;
        let pubsub = client.get_async_pubsub().await?;
        Ok(pubsub)
    }
    /// Retrieves or creates a RedisClient for the given configuration name without caching.
    pub async fn get_client_without_cache(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        let (nodes, server_type) = self.get_redis_nodes(server_id).await?;
        debug!(server_id, server_type = ?server_type, nodes = ?nodes, "get redis nodes");
        let Some(first_node) = nodes.first() else {
            return Err(Error::Invalid {
                message: "no nodes found".to_string(),
            });
        };
        let client = match server_type {
            ServerType::Cluster => {
                let addrs: Vec<String> = nodes.iter().map(|n| n.server.get_connection_url()).collect();
                // Bake the (per-server, else global) timeouts into the client
                // here — the cluster connection getters use the client's
                // configured timeouts rather than a per-call override.
                let mut builder = cluster::ClusterClientBuilder::new(addrs)
                    .connection_timeout(resolve_connection_timeout(&first_node.server))
                    .response_timeout(resolve_response_timeout(&first_node.server));
                if let Some(certificates) = first_node.server.tls_certificates() {
                    builder = builder.certs(certificates);
                }
                if first_node.server.insecure.unwrap_or(false) {
                    builder = builder.danger_accept_invalid_hostnames(true);
                }
                if first_node.server.is_ssh_tunnel() {
                    builder = builder.username(server_id);

                    RClient::SshCluster(builder.build()?)
                } else {
                    RClient::Cluster(builder.build()?)
                }
            }
            _ => RClient::Single(first_node.server.clone()),
        };
        let master_nodes: Vec<RedisNode> = nodes
            .iter()
            .filter(|node| node.role == NodeRole::Master)
            .cloned()
            .collect();
        let master_nodes_description: Vec<String> = master_nodes.iter().map(|node| node.host_port()).collect();
        info!(master_nodes = ?master_nodes_description, "server master nodes");
        let connection = get_async_connection(&client, db, false).await?;
        let access_mode = if safe_check_user_readonly(connection.clone()).await {
            AccessMode::StrictReadOnly
        } else if config.readonly.unwrap_or(false) {
            AccessMode::SafeMode
        } else {
            AccessMode::ReadWrite
        };
        let modules = get_modules(connection.clone()).await.unwrap_or_default();
        // Prefer the user-configured count — it works on managed clouds that
        // block `CONFIG` (ElastiCache) and on Valkey cluster (multi-db). Only
        // probe `CONFIG GET databases` when the server config leaves it unset.
        let databases = match config.databases {
            Some(n) => n,
            None => get_databases(connection.clone()).await.unwrap_or(1),
        };
        let mut client = RedisClient {
            db,
            databases,
            modules,
            access_mode,
            server_type: server_type.clone(),
            nodes,
            master_nodes,
            version: Version::new(0, 0, 0),
            is_valkey: false,
            connection,
        };
        let mut conn = client.connection.clone();
        let get_version = |info: InfoDict| -> (bool, Option<Version>) {
            if let Some(v) = info.get::<String>("valkey_version") {
                return (true, Version::parse(&v).ok());
            }
            if let Some(v) = info.get::<String>("redis_version") {
                return (false, Version::parse(&v).ok());
            }
            (false, None)
        };

        (client.is_valkey, client.version) = match server_type {
            ServerType::Cluster => {
                let info: redis::Value = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let mut version = None;
                let mut is_valkey = false;
                if let redis::Value::Map(items) = info {
                    for (_, node_info_val) in items {
                        if let Ok(info) = InfoDict::from_redis_value(node_info_val)
                            && let (valkey, Some(v)) = get_version(info)
                        {
                            version = Some(v);
                            is_valkey = valkey;
                            break;
                        }
                    }
                }
                (is_valkey, version.unwrap_or(Version::new(0, 0, 0)))
            }
            _ => {
                let info: InfoDict = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let (is_valkey, version) = get_version(info);
                (is_valkey, version.unwrap_or(Version::new(0, 0, 0)))
            }
        };

        debug!(server_id, version = client.version(), modules = ?client.modules, db, access_mode = ?client.access_mode(), "create redis client success");
        Ok(client)
    }
    /// Retrieves or creates a RedisClient for the given configuration name.
    pub async fn get_client(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        let key = config.get_hash(db);
        if let Some(client) = self.clients.get(&key) {
            debug!(server_id, db, "get client from cache");
            return Ok(client.clone());
        }
        let client = self.get_client_without_cache(server_id, db).await?;
        // Cache the client
        self.clients.insert(key, client.clone());
        Ok(client)
    }
    /// Shorthand to get an async connection directly.
    pub async fn get_connection(&self, server_id: &str, db: usize) -> Result<RedisAsyncConn> {
        let client = self.get_client(server_id, db).await?;
        Ok(client.connection.clone())
    }
}

/// Global accessor for the connection manager.
pub fn get_connection_manager() -> &'static ConnectionManager {
    &CONNECTION_MANAGER
}

/// Clears expired clients from the connection manager.
pub fn clear_expired_clients() -> (usize, usize) {
    CONNECTION_MANAGER.clients.clear_expired()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cluster_nodes_extracts_id_master_and_slots() {
        let raw = "07c37dfeb235213a872192d90877d0cd55635b91 127.0.0.1:30004@31004 slave e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 0 0 4 connected\n\
                   67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 127.0.0.1:30002@31002 master - 0 0 2 connected 5461-10922\n\
                   e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 127.0.0.1:30001@31001 myself,master - 0 0 1 connected 0-5460 [12345->-67ed]";
        let parsed = parse_cluster_nodes(raw).expect("parse must succeed");
        assert_eq!(parsed.len(), 3);

        let slave = &parsed[0];
        assert_eq!(slave.role, NodeRole::Slave);
        assert_eq!(
            slave.master_id.as_deref(),
            Some("e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca")
        );
        assert!(slave.slots.is_empty());

        let m1 = &parsed[1];
        assert_eq!(m1.role, NodeRole::Master);
        assert!(m1.master_id.is_none());
        assert_eq!(m1.slots, vec![(5461, 10922)]);

        // Migration markers in `[12345->-...]` form must be ignored — only
        // 0-5460 should be picked up here.
        let m2 = &parsed[2];
        assert_eq!(m2.slots, vec![(0, 5460)]);
        assert_eq!(m2.id, "e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca");
    }
}

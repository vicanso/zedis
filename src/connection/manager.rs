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
        RedisAsyncConn, get_redis_connection_timeout, get_redis_response_timeout, open_single_connection,
        query_async_masters,
    },
    config::{RedisServer, get_server},
    ssh_cluster_connection::SshMultiplexedConnection,
};
use crate::error::Error;
use crate::helpers::TtlCache;
use futures::future::try_join_all;
use gpui::SharedString;
use redis::{Cmd, FromRedisValue, InfoDict, ParsingError, Role, Value, aio::MultiplexedConnection, cluster, cmd};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Duration,
};
use tracing::{debug, error, info};

type HashScanValue = (u64, Vec<(Vec<u8>, Vec<u8>)>);

type Result<T, E = Error> = std::result::Result<T, E>;

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
}

impl RedisNode {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

// Information parsed from `CLUSTER NODES` command
#[derive(Debug, Clone)]
pub struct ClusterNodeInfo {
    pub ip: String,
    pub port: u16,
    pub role: NodeRole,
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
fn parse_cluster_nodes(raw_data: &str) -> Result<Vec<ClusterNodeInfo>> {
    let mut nodes = Vec::new();

    for line in raw_data.trim().lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();

        // Basic validation: ensure enough columns exist
        if parts.len() < 8 {
            continue;
        }

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

        nodes.push(ClusterNodeInfo { ip, port, role });
    }

    Ok(nodes)
}

/// Establishes an asynchronous connection based on the client type.
async fn get_async_connection(client: &RClient, db: usize) -> Result<RedisAsyncConn> {
    match client {
        RClient::Single(config) => {
            let conn = open_single_connection(config, db).await?;
            Ok(RedisAsyncConn::Single(conn))
        }
        RClient::Cluster(client) => {
            let cfg = cluster::ClusterConfig::default()
                .set_connection_timeout(get_redis_connection_timeout())
                .set_response_timeout(get_redis_response_timeout());
            let conn = client.get_async_connection_with_config(cfg).await?;
            Ok(RedisAsyncConn::Cluster(conn))
        }
        RClient::SshCluster(client) => {
            let conn: redis::cluster_async::ClusterConnection<SshMultiplexedConnection> =
                client.get_async_generic_connection().await?;
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
    server_type: ServerType,
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    version: Version,
    connection: RedisAsyncConn,
}
#[derive(Debug, Clone, Default)]
pub struct RedisClientDescription {
    pub server_type: SharedString,
    pub master_nodes: SharedString,
    pub slave_nodes: SharedString,
}
impl RedisClient {
    pub fn nodes(&self) -> (usize, usize) {
        (self.master_nodes.len(), self.nodes.len())
    }
    pub fn version(&self) -> String {
        self.version.to_string()
    }
    pub fn supports_db_selection(&self) -> bool {
        self.server_type != ServerType::Cluster
    }
    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    pub fn nodes_description(&self) -> RedisClientDescription {
        let master_nodes: Vec<String> = self.master_nodes.iter().map(|node| node.host_port()).collect();
        let slave_nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| !master_nodes.contains(&node.host_port()))
            .map(|node| node.host_port().clone())
            .collect();
        RedisClientDescription {
            server_type: format!("{:?}", self.server_type).into(),
            master_nodes: master_nodes.join(",").into(),
            slave_nodes: slave_nodes.join(",").into(),
        }
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

    pub async fn unlike_keys(&self, keys: Vec<SharedString>) -> Result<(), Error> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.is_cluster() {
            let mut conn = self.connection();
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("UNLINK").arg(key.as_str());
            }
            let _: () = pipe.query_async(&mut conn).await?;
            return Ok(());
        }

        // cluster mode
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
    /// # Arguments
    /// * `after_timestamp` - Optional Unix timestamp (seconds). Only returns logs with timestamp > this value.
    ///
    /// # Returns
    /// * `Vec<SlowLogEntry>` - A vector of slow log entries.
    pub async fn get_slow_logs(&self, after_timestamp: Option<i64>) -> Result<Vec<SlowLogEntry>> {
        let logs_arr: Vec<Vec<SlowLogEntry>> = self
            .query_async_masters(vec![cmd("SLOWLOG").arg("GET").arg("200").clone()])
            .await?;

        let mut logs: Vec<SlowLogEntry> = logs_arr.into_iter().flatten().collect();

        // Filter by timestamp if provided
        if let Some(ts) = after_timestamp {
            logs.retain(|entry| entry.timestamp > ts);
        }

        Ok(logs)
    }
    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<T>` - A vector of results from the commands.
    pub async fn query_async_masters<T: FromRedisValue>(&self, cmds: Vec<Cmd>) -> Result<Vec<T>> {
        let addrs: Vec<_> = self.master_nodes.iter().map(|item| item.server.clone()).collect();
        let values = query_async_masters(addrs, self.db, cmds).await?;
        Ok(values)
    }
    /// Calculates the total DB size across all masters.
    /// # Returns
    /// * `u64` - The total DB size.
    pub async fn dbsize(&self) -> Result<u64> {
        let list = self.query_async_masters(vec![cmd("DBSIZE")]).await?;
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
    /// Initiates a SCAN operation across all masters.
    /// # Arguments
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<SharedString>)` - A tuple containing the new cursors and the keys.
    pub async fn first_scan(&self, pattern: &str, count: u64) -> Result<(Vec<u64>, Vec<SharedString>)> {
        let master_count = self.count_masters()?;
        let cursors = vec![0; master_count];

        let (cursors, keys) = self.scan(cursors, pattern, count).await?;
        Ok((cursors, keys))
    }
    /// Continues a SCAN operation.
    /// # Arguments
    /// * `cursors` - A vector of cursors for each master.
    /// * `pattern` - The pattern to match keys.
    /// * `count` - The count of keys to return.
    /// # Returns
    /// * `(Vec<u64>, Vec<SharedString>)` - A tuple containing the new cursors and the keys.
    pub async fn scan(&self, cursors: Vec<u64>, pattern: &str, count: u64) -> Result<(Vec<u64>, Vec<SharedString>)> {
        debug!("scan, cursors: {cursors:?}, pattern: {pattern}, count: {count}");
        let cmds: Vec<Cmd> = cursors
            .iter()
            .map(|cursor| {
                cmd("SCAN")
                    .cursor_arg(*cursor)
                    .arg("MATCH")
                    .arg(pattern)
                    .arg("COUNT")
                    .arg(count)
                    .clone()
            })
            .collect();
        let values: Vec<(u64, Vec<Vec<u8>>)> = self.query_async_masters(cmds).await?;
        let mut cursors = Vec::with_capacity(values.len());
        let mut keys = Vec::with_capacity(values[0].1.len() * values.len());
        for (cursor, keys_in_node) in values {
            cursors.push(cursor);
            keys.extend(
                keys_in_node
                    .iter()
                    .map(|k| String::from_utf8_lossy(k).to_string().into()),
            );
        }
        keys.sort_unstable();
        Ok((cursors, keys))
    }
}

pub struct ConnectionManager {
    clients: TtlCache<String, RedisClient>,
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
            let conn = match open_single_connection(&config, 0).await {
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
                    open_single_connection(&tmp_config, 0).await?
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
                        if !e.to_string().contains("Command is not available") {
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
                        tmp_config.host = item.ip.clone();

                        RedisNode {
                            server: tmp_config,
                            role: item.role.clone(),
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
    pub fn remove_client(&self, name: &str) {
        self.clients.remove(&name.to_string());
    }
    /// Retrieves or creates a RedisClient for the given configuration name.
    pub async fn get_client(&self, server_id: &str, db: usize) -> Result<RedisClient> {
        let config = get_server(server_id)?;
        let key = format!("{:x}:{}", config.get_hash(), db);
        if let Some(client) = self.clients.get(&key) {
            debug!(server_id, db, "get client from cache");
            return Ok(client.clone());
        }
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
                let mut builder = cluster::ClusterClientBuilder::new(addrs);
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
        let connection = get_async_connection(&client, db).await?;
        let access_mode = if safe_check_user_readonly(connection.clone()).await {
            AccessMode::StrictReadOnly
        } else if config.readonly.unwrap_or(false) {
            AccessMode::SafeMode
        } else {
            AccessMode::ReadWrite
        };
        let mut client = RedisClient {
            db,
            access_mode,
            server_type: server_type.clone(),
            nodes,
            master_nodes,
            version: Version::new(0, 0, 0),
            connection,
        };
        let mut conn = client.connection.clone();
        client.version = match server_type {
            ServerType::Cluster => {
                let info: redis::Value = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let mut version = "unknown".to_string();
                if let redis::Value::Map(items) = info {
                    for (_, node_info_val) in items {
                        if let Ok(info) = InfoDict::from_redis_value(node_info_val)
                            && let Some(v) = info.get::<String>("redis_version")
                        {
                            version = v;
                            break;
                        }
                    }
                }
                Version::parse(&version).unwrap_or(Version::new(0, 0, 0))
            }
            _ => {
                let info: InfoDict = cmd("INFO").arg("server").query_async(&mut conn).await?;
                let version = info.get::<String>("redis_version").unwrap_or_default();
                Version::parse(&version).unwrap_or(Version::new(0, 0, 0))
            }
        };

        debug!(server_id, version = client.version(), db, access_mode = ?client.access_mode(), "create redis client success");

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

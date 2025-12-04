// Copyright 2025 Tree xie.
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

use super::async_connection::{RedisAsyncConn, query_async_masters};
use super::config::get_config;
use crate::error::Error;
use dashmap::DashMap;
use gpui::SharedString;
use redis::AsyncConnectionConfig;
use redis::FromRedisValue;
use redis::cmd;
use redis::{Client, Cmd, cluster};
use redis::{InfoDict, Role};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

type Result<T, E = Error> = std::result::Result<T, E>;

// Global singleton for ConnectionManager
static CONNECTION_MANAGER: LazyLock<ConnectionManager> = LazyLock::new(ConnectionManager::new);

// Enum representing the type of Redis server
#[derive(Debug, Clone, PartialEq)]
enum ServerType {
    Standalone,
    Cluster,
    Sentinel,
}

// Wrapper for the underlying Redis client
#[derive(Clone)]
enum RClient {
    Single(Client),
    Cluster(cluster::ClusterClient),
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
    addr: String,
    role: NodeRole,
    master_name: Option<String>,
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

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Establishes an asynchronous connection based on the client type.
async fn get_async_connection(client: &RClient) -> Result<RedisAsyncConn> {
    match client {
        RClient::Single(client) => {
            let cfg = AsyncConnectionConfig::default()
                .set_connection_timeout(Some(CONNECTION_TIMEOUT))
                .set_response_timeout(Some(RESPONSE_TIMEOUT));
            let conn = client
                .get_multiplexed_async_connection_with_config(&cfg)
                .await?;
            Ok(RedisAsyncConn::Single(conn))
        }
        RClient::Cluster(client) => {
            let cfg = cluster::ClusterConfig::default()
                .set_connection_timeout(CONNECTION_TIMEOUT)
                .set_response_timeout(RESPONSE_TIMEOUT);
            let conn = client.get_async_connection_with_config(cfg).await?;
            Ok(RedisAsyncConn::Cluster(conn))
        }
    }
}

// TODO 是否在client中保存connection
#[derive(Clone)]
pub struct RedisClient {
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    version: String,
    connection: RedisAsyncConn,
}
impl RedisClient {
    pub fn nodes(&self) -> (usize, usize) {
        (self.master_nodes.len(), self.nodes.len())
    }
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Executes commands on all master nodes concurrently.
    /// # Arguments
    /// * `cmds` - A vector of commands to execute.
    /// # Returns
    /// * `Vec<T>` - A vector of results from the commands.
    pub async fn query_async_masters<T: FromRedisValue>(&self, cmds: Vec<Cmd>) -> Result<Vec<T>> {
        let addrs: Vec<_> = self
            .master_nodes
            .iter()
            .map(|item| item.addr.as_str())
            .collect();
        let values = query_async_masters(addrs, cmds).await?;
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
    pub async fn first_scan(
        &self,
        pattern: &str,
        count: u64,
    ) -> Result<(Vec<u64>, Vec<SharedString>)> {
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
    pub async fn scan(
        &self,
        cursors: Vec<u64>,
        pattern: &str,
        count: u64,
    ) -> Result<(Vec<u64>, Vec<SharedString>)> {
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
    clients: DashMap<String, RedisClient>,
}

/// Detects the type of Redis server (Sentinel, Cluster, or Standalone).
/// This function checks the role of the Redis server and returns the server type.
/// # Arguments
/// * `client` - The Redis client to check the server type.
/// # Returns
/// * `ServerType` - The type of the Redis server.
async fn detect_server_type(client: &Client) -> Result<ServerType> {
    let mut conn = client.get_multiplexed_async_connection().await?;
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

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
        }
    }
    /// Discovers Redis nodes and server type based on initial configuration.
    async fn get_redis_nodes(&self, name: &str) -> Result<(Vec<RedisNode>, ServerType)> {
        let config = get_config(name)?;
        let url = config.get_connection_url();
        let mut client = Client::open(url.clone())?;
        // Attempt to connect and detect server type
        // Handles logic to retry without password if authentication fails
        let server_type = match detect_server_type(&client).await {
            Ok(server_type) => server_type,
            Err(e) => {
                // Retry without password if auth failed and config might allow empty password
                // or simply to handle sentinel cases which often have no auth
                if config.password.is_none() || !e.to_string().contains("AuthenticationFailed") {
                    return Err(e);
                }
                let mut tmp_config = config.clone();
                tmp_config.password = None;
                client = Client::open(tmp_config.get_connection_url())?;
                detect_server_type(&client).await?
            }
        };
        match server_type {
            ServerType::Cluster => {
                let mut conn = client.get_multiplexed_async_connection().await?;
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
                            addr: tmp_config.get_connection_url(),
                            role: item.role.clone(),
                            ..Default::default()
                        }
                    })
                    .collect();
                Ok((nodes, server_type))
            }
            ServerType::Sentinel => {
                let mut conn = client.get_multiplexed_async_connection().await?;
                // Fetch masters from Sentinel
                let masters_response: Vec<HashMap<String, String>> = cmd("SENTINEL")
                    .arg("MASTERS")
                    .query_async(&mut conn)
                    .await?;
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
                        addr: tmp_config.get_connection_url(),
                        role: NodeRole::Master,
                        master_name: Some(name.clone()),
                    });
                }
                // Check for ambiguous master configuration
                let unique_masters: HashSet<_> = nodes
                    .iter()
                    .filter_map(|n| n.master_name.as_ref())
                    .collect();
                if unique_masters.len() > 1 {
                    return Err(Error::Invalid {
                        message: "Multiple masters found in Sentinel, please specify master_name"
                            .into(),
                    });
                }

                Ok((nodes, server_type))
            }
            _ => Ok((
                vec![RedisNode {
                    addr: url,
                    role: NodeRole::Master,
                    ..Default::default()
                }],
                server_type,
            )),
        }
    }
    pub fn remove_client(&self, name: &str) {
        self.clients.remove(name);
    }
    /// Retrieves or creates a RedisClient for the given configuration name.
    pub async fn get_client(&self, server_id: &str) -> Result<RedisClient> {
        if let Some(client) = self.clients.get(server_id) {
            return Ok(client.clone());
        }
        let (nodes, server_type) = self.get_redis_nodes(server_id).await?;
        let client = match server_type {
            ServerType::Cluster => {
                let addrs: Vec<String> = nodes.iter().map(|n| n.addr.clone()).collect();
                let client = cluster::ClusterClient::new(addrs)?;
                RClient::Cluster(client)
            }
            _ => {
                let client = Client::open(nodes[0].addr.clone())?;
                RClient::Single(client)
            }
        };
        let master_nodes = nodes
            .iter()
            .filter(|node| node.role == NodeRole::Master)
            .cloned()
            .collect();
        let connection = get_async_connection(&client).await?;
        let mut client = RedisClient {
            nodes,
            master_nodes,
            version: "".to_string(),
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
                version
            }
            _ => {
                let info: InfoDict = cmd("INFO").arg("server").query_async(&mut conn).await?;
                info.get::<String>("redis_version").unwrap_or_default()
            }
        };
        // Cache the client
        self.clients.insert(server_id.to_string(), client.clone());
        Ok(client)
    }
    /// Shorthand to get an async connection directly.
    pub async fn get_connection(&self, server_id: &str) -> Result<RedisAsyncConn> {
        let client = self.get_client(server_id).await?;
        Ok(client.connection.clone())
    }
}

/// Global accessor for the connection manager.
pub fn get_connection_manager() -> &'static ConnectionManager {
    &CONNECTION_MANAGER
}

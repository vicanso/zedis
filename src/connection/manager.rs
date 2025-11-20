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

use super::config::get_config;
use crate::error::Error;
use dashmap::DashMap;
use redis::{Client, Cmd, Connection, ConnectionLike, RedisResult, cluster};
use redis::{Commands, cmd};
use redis::{FromRedisValue, Value};
use redis::{InfoDict, Role};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

type Result<T, E = Error> = std::result::Result<T, E>;

static CONNECTION_MANAGER: LazyLock<ConnectionManager> = LazyLock::new(ConnectionManager::new);

pub enum RedisConn {
    Single(Connection),
    Cluster(Box<cluster::ClusterConnection>),
}

impl ConnectionLike for RedisConn {
    fn req_packed_command(&mut self, cmd: &[u8]) -> RedisResult<Value> {
        match self {
            RedisConn::Single(conn) => conn.req_packed_command(cmd),
            RedisConn::Cluster(conn) => conn.req_packed_command(cmd),
        }
    }
    fn req_packed_commands(
        &mut self,
        cmd: &[u8],
        offset: usize,
        count: usize,
    ) -> RedisResult<Vec<Value>> {
        match self {
            RedisConn::Single(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisConn::Cluster(conn) => conn.req_packed_commands(cmd, offset, count),
        }
    }
    fn get_db(&self) -> i64 {
        match self {
            RedisConn::Single(conn) => conn.get_db(),
            RedisConn::Cluster(conn) => conn.get_db(),
        }
    }
    fn check_connection(&mut self) -> bool {
        match self {
            RedisConn::Single(conn) => conn.check_connection(),
            RedisConn::Cluster(conn) => conn.check_connection(),
        }
    }
    fn is_open(&self) -> bool {
        match self {
            RedisConn::Single(conn) => conn.is_open(),
            RedisConn::Cluster(conn) => conn.is_open(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ServerType {
    Standalone,
    Cluster,
    Sentinel,
}

#[derive(Clone)]
enum RClient {
    Single(Client),
    Cluster(cluster::ClusterClient),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NodeRole {
    #[default]
    Master,
    Slave,
    Fail,
    Unknown, // e.g. "handshake", "noaddr"
}

#[derive(Debug, Clone, Default)]
struct RedisNode {
    addr: String,
    role: NodeRole,
    master_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClusterNodeInfo {
    pub node_id: String,
    pub ip: String,
    pub port: u16,
    pub role: NodeRole,
    pub cluster_bus_port: Option<u16>,
    pub flags: HashSet<String>,
    pub master_id: Option<String>,
    pub ping_sent_timestamp_ms: u64,
    pub pong_recv_timestamp_ms: u64,
    pub config_epoch: u64,
    pub link_state: String,
    pub slot_ranges: Vec<String>,
}

fn parse_address(address_str: &str) -> Result<(String, u16, Option<u16>)> {
    // 1. 将 "ip:port" 和 "cport" (可选) 分开
    let (ip_port_part, cport_part) = match address_str.split_once('@') {
        Some((addr, cport)) => (addr, Some(cport)),
        None => (address_str, None), // 没有 '@'，意味着没有 cport
    };

    // 2. 将 "ip" 和 "port" 分开
    let (ip_str, port_str) = ip_port_part.split_once(':').ok_or_else(|| Error::Invalid {
        message: format!("Invalid address (no :): {}", ip_port_part),
    })?;

    // 3. 解析 port
    let port = port_str.parse::<u16>().map_err(|e| Error::Invalid {
        message: format!("Invalid port '{}': {}", port_str, e),
    })?;

    // 4. 解析 cport (如果存在)
    let cluster_bus_port = match cport_part {
        Some(s) => Some(s.parse::<u16>().map_err(|e| Error::Invalid {
            message: format!("Invalid cport '{}': {}", s, e),
        })?),
        None => None,
    };

    Ok((ip_str.to_string(), port, cluster_bus_port))
}

fn parse_cluster_nodes(raw_data: &str) -> Result<Vec<ClusterNodeInfo>> {
    let mut nodes = Vec::new();

    // 1. 遍历原始文本的每一行
    for line in raw_data.trim().lines() {
        // 2. 按空格分割每一列
        let parts: Vec<&str> = line.split_whitespace().collect();

        // 3. 'CLUSTER NODES' 至少有 8 列。如果少于 8 列，
        //    这可能是一个空行或格式错误的行，安全地跳过它。
        if parts.len() < 8 {
            continue;
        }

        // --- 4. 开始安全地解析每一列 ---

        // [0] Node ID
        let node_id = parts[0].to_string();

        // [1] Address (格式: "ip:port@cport")
        let (ip, port, cluster_bus_port) = parse_address(parts[1])?;

        // [2] Flags (格式: "myself,master,fail?")
        let flags_str = parts[2];
        let flags: HashSet<String> = flags_str.split(',').map(String::from).collect();
        let role = if flags.contains("master") {
            NodeRole::Master
        } else if flags.contains("slave") {
            NodeRole::Slave
        } else if flags.contains("fail") {
            NodeRole::Fail
        } else {
            NodeRole::Unknown
        };

        // [3] Master ID (如果是 master，则为 "-")
        let master_id = if parts[3] == "-" {
            None
        } else {
            Some(parts[3].to_string())
        };

        // [4] Ping Sent (u64)
        let ping_sent_timestamp_ms = parts[4].parse::<u64>().map_err(|e| Error::Invalid {
            message: format!("Invalid ping_sent '{}': {}", parts[4], e),
        })?;

        // [5] Pong Recv (u64)
        let pong_recv_timestamp_ms = parts[5].parse::<u64>().map_err(|e| Error::Invalid {
            message: format!("Invalid pong_recv '{}': {}", parts[5], e),
        })?;

        // [6] Config Epoch (u64)
        let config_epoch = parts[6].parse::<u64>().map_err(|e| Error::Invalid {
            message: format!("Invalid config_epoch '{}': {}", parts[6], e),
        })?;

        // [7] Link State
        let link_state = parts[7].to_string();

        // [8+] Slot Ranges (所有剩余的列)
        let slot_ranges = parts
            .get(8..)
            .unwrap_or(&[])
            .iter()
            .map(|s| s.to_string())
            .collect();

        // 5. 将解析后的结构体添加到我们的 Vec 中
        nodes.push(ClusterNodeInfo {
            node_id,
            ip,
            port,
            cluster_bus_port,
            role,
            flags,
            master_id,
            ping_sent_timestamp_ms,
            pong_recv_timestamp_ms,
            config_epoch,
            link_state,
            slot_ranges,
        });
    }

    Ok(nodes)
}

// TODO 是否在client中保存connection
#[derive(Clone)]
pub struct RedisClient {
    client: RClient,
    nodes: Vec<RedisNode>,
    master_nodes: Vec<RedisNode>,
    server_type: ServerType,
}
impl RedisClient {
    pub fn get_connection(&self) -> Result<RedisConn> {
        match &self.client {
            RClient::Single(client) => {
                let conn = client.get_connection()?;
                Ok(RedisConn::Single(conn))
            }
            RClient::Cluster(client) => {
                let conn = client.get_connection()?;
                Ok(RedisConn::Cluster(Box::new(conn)))
            }
        }
    }
    fn is_cluster(&self) -> bool {
        self.server_type == ServerType::Cluster
    }
    fn query_masters<T: FromRedisValue>(&self, cmds: Vec<Cmd>) -> Result<Vec<T>> {
        if cmds.is_empty() {
            return Err(Error::Invalid {
                message: "Commands are empty".to_string(),
            });
        }
        let first_cmd = cmds[0].clone();
        let mut values = Vec::with_capacity(self.nodes.len() / 2 + 1);
        for (index, node) in self.master_nodes.iter().enumerate() {
            let client = Client::open(node.addr.clone())?;
            let mut conn = client.get_connection()?;
            let value: T = cmds
                .get(index)
                .cloned()
                .unwrap_or_else(|| first_cmd.clone())
                .query(&mut conn)?;
            values.push(value);
        }
        Ok(values)
    }
    pub fn dbsize(&self) -> Result<u64> {
        let list = self.query_masters(vec![cmd("DBSIZE")])?;
        Ok(list.iter().sum())
    }
    pub fn ping(&self) -> Result<()> {
        let mut conn = self.get_connection()?;
        let _: () = cmd("PING").query(&mut conn)?;
        Ok(())
    }
    pub fn count_masters(&self) -> Result<usize> {
        Ok(self.master_nodes.len())
    }
    pub fn first_scan(&self, pattern: &str, count: u64) -> Result<(Vec<u64>, Vec<String>)> {
        let master_count = self.count_masters()?;
        let cursors = vec![0; master_count];

        let (cursors, keys) = self.scan(cursors, pattern, count)?;
        Ok((cursors, keys))
    }
    pub fn get<T: FromRedisValue>(&self, key: &str) -> Result<Option<T>> {
        let value = self.get_connection()?.get(key)?;
        Ok(value)
    }
    pub fn scan(
        &self,
        cursors: Vec<u64>,
        pattern: &str,
        count: u64,
    ) -> Result<(Vec<u64>, Vec<String>)> {
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
        let values: Vec<(u64, Vec<String>)> = self.query_masters(cmds)?;
        let mut cursors = Vec::with_capacity(values.len());
        let mut keys = Vec::with_capacity(values[0].1.len() * values.len());
        for (cursor, keys_in_node) in values {
            cursors.push(cursor);
            keys.extend(keys_in_node);
        }
        keys.sort_unstable();
        Ok((cursors, keys))
    }
}

pub struct ConnectionManager {
    clients: DashMap<String, RedisClient>,
}

fn detect_server_type(client: &Client) -> Result<ServerType> {
    let mut conn = client.get_connection()?;
    let role: Role = cmd("ROLE").query(&mut conn)?;
    match role {
        Role::Sentinel { .. } => Ok(ServerType::Sentinel),
        _ => {
            let info: InfoDict = cmd("INFO").arg("cluster").query(&mut conn)?;
            let is_cluster = info.get("cluster_enabled").unwrap_or(0i64) == 1i64;
            if is_cluster {
                Ok(ServerType::Cluster)
            } else {
                Ok(ServerType::Standalone)
            }
        }
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
        }
    }
    fn get_redis_nodes(&self, name: &str) -> Result<(Vec<RedisNode>, ServerType)> {
        let config = get_config(name)?;
        let url = config.get_connection_url();
        let mut client = Client::open(url.clone())?;
        // 如果密码错，再试无密码的
        // sentinel大概率无密码
        let server_type = match detect_server_type(&client) {
            Ok(server_type) => server_type,
            Err(e) => {
                if config.password.is_none() || !e.to_string().contains("AuthenticationFailed") {
                    return Err(e);
                }
                let mut tmp_config = config.clone();
                tmp_config.password = None;
                client = Client::open(tmp_config.get_connection_url())?;
                detect_server_type(&client)?
            }
        };
        match server_type {
            ServerType::Cluster => {
                let mut conn = client.get_connection()?;
                let nodes: String = cmd("CLUSTER").arg("NODES").query(&mut conn)?;
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
                let mut conn = client.get_connection()?;
                let masters_response: Vec<HashMap<String, String>> =
                    cmd("SENTINEL").arg("MASTERS").query(&mut conn)?;
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
                let mut master_names: Vec<_> = nodes
                    .iter()
                    .map(|item| item.master_name.clone().unwrap_or_default())
                    .collect();
                master_names.dedup();
                if master_names.len() > 1 {
                    return Err(Error::Invalid {
                        message: "Sentinel should set master name".to_string(),
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
    pub fn get_client(&self, name: &str) -> Result<RedisClient> {
        if let Some(client) = self.clients.get(name) {
            return Ok(client.clone());
        }
        let (nodes, server_type) = self.get_redis_nodes(name)?;
        let client = match server_type {
            ServerType::Cluster => {
                let client = cluster::ClusterClient::new(
                    nodes
                        .iter()
                        .map(|node| node.addr.clone())
                        .collect::<Vec<String>>(),
                )?;
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
        let client = RedisClient {
            client,
            nodes,
            master_nodes,
            server_type,
        };
        self.clients.insert(name.to_string(), client.clone());
        Ok(client)
    }
}

pub fn get_connection_manager() -> &'static ConnectionManager {
    &CONNECTION_MANAGER
}

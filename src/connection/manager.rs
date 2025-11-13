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
use redis::{Client, Cmd, Connection, ConnectionLike, RedisResult, cluster};
use redis::{Commands, cmd};
use redis::{FromRedisValue, Value};
use redis::{InfoDict, Role};
use std::sync::LazyLock;

type Result<T, E = Error> = std::result::Result<T, E>;

static CONNECTION_MANAGER: LazyLock<ConnectionManager> = LazyLock::new(|| ConnectionManager::new());

pub enum RedisConn {
    Single(Connection),
    Cluster(cluster::ClusterConnection),
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

enum RClient {
    Single(Client),
    Cluster(cluster::ClusterClient),
}

#[derive(Debug, Clone)]
struct RedisNode {
    addr: String,
    master: bool,
}

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
                Ok(RedisConn::Cluster(conn))
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
    pub fn count_masters(&self) -> Result<usize> {
        Ok(self.master_nodes.len())
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

pub struct ConnectionManager {}

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
        Self {}
    }
    fn get_redis_nodes(&self, name: &str) -> Result<(Vec<RedisNode>, ServerType)> {
        let config = get_config(name)?;
        let url = config.get_connection_url();
        let client = Client::open(url.clone())?;
        let server_type = detect_server_type(&client)?;
        // 后续再处理其它场景
        // https://redis.io/docs/latest/commands/cluster-nodes/
        Ok((
            vec![RedisNode {
                addr: url,
                master: true,
            }],
            server_type,
        ))
    }
    pub fn get_client(&self, name: &str) -> Result<RedisClient> {
        let (nodes, server_type) = self.get_redis_nodes(name)?;
        let client = match server_type {
            ServerType::Standalone => {
                let client = Client::open(nodes[0].addr.clone())?;
                RClient::Single(client)
            }
            ServerType::Cluster => {
                let client = cluster::ClusterClient::new(
                    nodes
                        .iter()
                        .map(|node| node.addr.clone())
                        .collect::<Vec<String>>(),
                )?;
                RClient::Cluster(client)
            }
            ServerType::Sentinel => {
                return Err(Error::Invalid {
                    message: "Sentinel is not supported".to_string(),
                });
            }
        };
        let master_nodes = nodes.iter().filter(|node| node.master).cloned().collect();
        Ok(RedisClient {
            client,
            nodes,
            master_nodes,
            server_type,
        })
    }
}

pub fn get_connection_manager() -> &'static ConnectionManager {
    &CONNECTION_MANAGER
}

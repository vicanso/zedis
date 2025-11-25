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

use crate::error::Error;
use async_trait::async_trait;
use redis::Client;
use redis::Cmd;
use redis::Pipeline;
use redis::RedisFuture;
use redis::aio::ConnectionLike;
use redis::aio::MultiplexedConnection;
use redis::cluster_async::ClusterConnection;
use redis::{FromRedisValue, Value};

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone)]
pub enum RedisAsyncConn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

#[async_trait]
impl ConnectionLike for RedisAsyncConn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_command(cmd),
        }
    }
    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_commands(cmd, offset, count),
        }
    }
    fn get_db(&self) -> i64 {
        match self {
            RedisAsyncConn::Single(conn) => conn.get_db(),
            RedisAsyncConn::Cluster(_) => 0,
        }
    }
}

pub(crate) async fn query_async_masters<T: FromRedisValue>(
    addrs: Vec<&str>,
    cmds: Vec<Cmd>,
) -> Result<Vec<T>> {
    if cmds.is_empty() {
        return Err(Error::Invalid {
            message: "Commands are empty".to_string(),
        });
    }
    let first_cmd = cmds[0].clone();
    let mut values = Vec::with_capacity(addrs.len());
    for (index, addr) in addrs.iter().enumerate() {
        let client = Client::open(*addr)?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        let value: T = cmds
            .get(index)
            .cloned()
            .unwrap_or_else(|| first_cmd.clone())
            .query_async(&mut conn)
            .await?;
        values.push(value);
    }
    Ok(values)
}

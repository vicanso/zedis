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
use futures::future::try_join_all;
use redis::Client;
use redis::Cmd;
use redis::Pipeline;
use redis::RedisFuture;
use redis::aio::ConnectionLike;
use redis::aio::MultiplexedConnection;
use redis::cluster_async::ClusterConnection;
use redis::{FromRedisValue, Value};
use std::sync::LazyLock;
use std::time::Duration;

type Result<T, E = Error> = std::result::Result<T, E>;

static DELAY: LazyLock<Option<Duration>> = LazyLock::new(|| {
    let value = std::env::var("REDIS_DELAY").unwrap_or_default();
    humantime::parse_duration(&value).ok()
});

/// A wrapper enum for Redis asynchronous connections.
///
/// This unifies `MultiplexedConnection` (for single nodes) and
/// `ClusterConnection` (for clusters) under a single type,
/// allowing generic usage across the application.
#[derive(Clone)]
pub enum RedisAsyncConn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl ConnectionLike for RedisAsyncConn {
    #[inline]
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        let cmd_future = match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_command(cmd),
        };
        if let Some(delay) = *DELAY {
            return Box::pin(async move {
                smol::Timer::after(delay).await;
                cmd_future.await
            });
        }
        cmd_future
    }
    #[inline]
    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        let cmd_future = match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_commands(cmd, offset, count),
        };
        if let Some(delay) = *DELAY {
            return Box::pin(async move {
                smol::Timer::after(delay).await;
                cmd_future.await
            });
        }
        cmd_future
    }
    #[inline]
    fn get_db(&self) -> i64 {
        match self {
            RedisAsyncConn::Single(conn) => conn.get_db(),
            RedisAsyncConn::Cluster(_) => 0,
        }
    }
}

/// Queries multiple Redis master nodes concurrently.
///
/// This function establishes connections to all provided addresses in parallel
/// and executes the corresponding commands.
///
/// # Arguments
///
/// * `addrs` - A vector of Redis connection strings (e.g., "redis://127.0.0.1").
/// * `cmds` - A vector of commands to execute. If there are fewer commands than addresses,
///   the first command is reused for the remaining addresses.
pub(crate) async fn query_async_masters<T: FromRedisValue>(addrs: Vec<&str>, cmds: Vec<Cmd>) -> Result<Vec<T>> {
    let first_cmd = cmds.first().ok_or_else(|| Error::Invalid {
        message: "Commands are empty".to_string(),
    })?;
    let tasks = addrs.into_iter().enumerate().map(|(index, addr)| {
        // Clone data to move ownership into the async block.
        let addr = addr.to_string();
        // Use the specific command for this index, or fallback to the first command.
        let cmd = cmds.get(index).unwrap_or(first_cmd).clone();

        async move {
            // TODO get client connect from cache
            // Establish a multiplexed async connection to the specific node.
            let client = Client::open(addr)?;
            let mut conn = client.get_multiplexed_async_connection().await?;

            // Execute the command asynchronously.
            let value: T = cmd.query_async(&mut conn).await?;

            Ok::<T, Error>(value)
        }
    });

    let values = try_join_all(tasks).await?;

    Ok(values)
}

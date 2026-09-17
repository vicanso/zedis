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

//! The one connection type every Redis call in this workspace speaks to.
//!
//! It lives in its own module rather than beside the dialling code because
//! the browser needs the *type* and none of the dialling: there its only
//! variant is the HTTP bridge, and `async_connection` is not compiled at all
//! (ADR 9).
//!
//! On the host it implements redis-rs's `ConnectionLike`, which is what lets
//! every existing `cmd(...).query_async(&mut conn)` keep working. That trait
//! is behind redis's `aio` feature, which cannot build for
//! `wasm32-unknown-unknown` — it demands a socket runtime. The browser gets
//! the same call sites from `bridge::{BridgeQuery, BridgeExec}` instead:
//! inherent methods win over trait methods, so where `aio` provides
//! `Cmd::query_async` the trait is never consulted, and where it does not the
//! trait supplies it under the same name.

use crate::bridge::BridgeConn;
#[cfg(not(target_family = "wasm"))]
use crate::ssh_cluster_connection::SshMultiplexedConnection;
#[cfg(not(target_family = "wasm"))]
use redis::{
    Cmd, Pipeline, RedisFuture, Value,
    aio::{ConnectionLike, MultiplexedConnection},
    cluster_async::ClusterConnection,
};

/// A wrapper enum for Redis asynchronous connections.
///
/// This unifies `MultiplexedConnection` (for single nodes) and
/// `ClusterConnection` (for clusters) under a single type,
/// allowing generic usage across the application.
#[derive(Clone)]
pub enum RedisAsyncConn {
    #[cfg(not(target_family = "wasm"))]
    Single(MultiplexedConnection),
    #[cfg(not(target_family = "wasm"))]
    Cluster(ClusterConnection),
    #[cfg(not(target_family = "wasm"))]
    SshCluster(ClusterConnection<SshMultiplexedConnection>),
    /// Commands travel to `zedis-bridge` over HTTP instead of a socket —
    /// the web build's only transport, because a browser has no socket to
    /// open (ADR 9). A variant rather than a separate type, so that every
    /// caller of this enum keeps working unchanged.
    Bridge(BridgeConn),
}

#[cfg(not(target_family = "wasm"))]
impl ConnectionLike for RedisAsyncConn {
    #[inline]
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        let cmd_future = match self {
            RedisAsyncConn::Single(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::Cluster(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::SshCluster(conn) => conn.req_packed_command(cmd),
            RedisAsyncConn::Bridge(conn) => conn.req_packed_command(cmd),
        };
        if let Some(delay) = *crate::async_connection::DELAY {
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
            RedisAsyncConn::SshCluster(conn) => conn.req_packed_commands(cmd, offset, count),
            RedisAsyncConn::Bridge(conn) => conn.req_packed_commands(cmd, offset, count),
        };
        if let Some(delay) = *crate::async_connection::DELAY {
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
            RedisAsyncConn::SshCluster(conn) => conn.get_db(),
            RedisAsyncConn::Bridge(conn) => conn.get_db(),
        }
    }
}

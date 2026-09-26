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

//! What the app asks of a server as a whole: what it is when it connects, how
//! it is doing on every heartbeat, and the few commands that act on all of it
//! (`BGSAVE`, `FLUSHDB`, `REPLICAOF`, `FAILOVER`).
//!
//! Most of these are `RedisClient` methods that already existed; the app used
//! to reach them by taking the pooled client out of the manager, and a caller
//! holding a client is a caller that can build commands (ADR 10). So each gets
//! a door that takes a [`ServerDb`] instead.

use crate::config::RedisServer;
use crate::error::Error;
use crate::floors::Floor;
use crate::manager::{AccessMode, RedisClientDescription, SlowLogEntry, get_connection_manager};
use crate::server_db::ServerDb;
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

/// What connecting to a server found out about it.
#[derive(Debug, Clone)]
pub struct ServerSummary {
    /// Keys in the selected database, summed over the masters.
    pub dbsize: u64,
    /// `(masters, replicas)`.
    pub nodes: (usize, usize),
    pub description: RedisClientDescription,
    pub version: String,
    /// How many databases `SELECT` accepts.
    pub databases: usize,
    pub access_mode: AccessMode,
    pub supports_rejson: bool,
    pub supports_search: bool,
}

/// Connect (or reuse the pooled client) and describe the server.
pub async fn server_summary(at: &ServerDb) -> Result<ServerSummary> {
    let client = at.client().await?;
    Ok(ServerSummary {
        dbsize: client.dbsize().await?,
        nodes: client.nodes(),
        description: client.nodes_description(),
        version: client.version(),
        databases: client.databases(),
        access_mode: client.access_mode(),
        supports_rejson: client.supports_rejson(),
        supports_search: client.supports_search(),
    })
}

/// Whether the server is at or past `floor` — the one version primitive
/// (`floors.rs`), asked of the connected client.
pub async fn server_supports(at: &ServerDb, floor: Floor) -> Result<bool> {
    Ok(at.client().await?.supports(floor))
}

/// The heartbeat's probe, on the client's own connection (ADR 5): the `INFO`
/// itself where there is one master — the caller times the call, and that is
/// the status bar's latency — or a `PING` on a cluster, answered `None`.
pub async fn heartbeat_probe(at: &ServerDb) -> Result<Option<String>> {
    at.client().await?.heartbeat_probe().await
}

/// `INFO` from every master, each with the node it came from. `probed` is the
/// text [`heartbeat_probe`] already fetched: with one master it *is* the
/// answer and nothing more is sent.
pub async fn master_infos(at: &ServerDb, probed: Option<String>) -> Result<Vec<(RedisServer, String)>> {
    let client = at.client().await?;
    let (servers, infos): (_, Vec<String>) = match probed {
        Some(info) => (client.master_servers(), vec![info]),
        None => client.query_async_masters(vec![cmd("INFO")]).await?,
    };
    Ok(servers.into_iter().zip(infos).collect())
}

/// The slow log, merged over the masters.
pub async fn slow_logs(at: &ServerDb) -> Result<Vec<SlowLogEntry>> {
    at.client().await?.get_slow_logs().await
}

/// `DBSIZE`, summed over the masters.
pub async fn dbsize(at: &ServerDb) -> Result<u64> {
    at.client().await?.dbsize().await
}

/// `BGSAVE` on every master. The reply is a status line that differs between
/// forks and is not read: what the user sees comes from the next `INFO`.
pub async fn bgsave(at: &ServerDb) -> Result<()> {
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![cmd("BGSAVE")]).await?;
    Ok(())
}

/// `BGSAVE CANCEL` on every master (Valkey 8.1+, `floors::BGSAVE_CANCEL`):
/// the snapshot in progress is stopped, a scheduled one dropped. A master
/// with nothing to cancel answers an error, which the caller shows.
pub async fn bgsave_cancel(at: &ServerDb) -> Result<()> {
    let mut cancel = cmd("BGSAVE");
    cancel.arg("CANCEL");
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![cancel]).await?;
    Ok(())
}

/// `BGREWRITEAOF` on every master.
pub async fn bgrewriteaof(at: &ServerDb) -> Result<()> {
    let (_, _replies): (_, Vec<String>) = at
        .client()
        .await?
        .query_async_masters(vec![cmd("BGREWRITEAOF")])
        .await?;
    Ok(())
}

/// `FLUSHDB` on every master.
pub async fn flush_db(at: &ServerDb) -> Result<()> {
    at.client().await?.flush_db().await
}

/// `FLUSHALL` on every master.
pub async fn flush_all(at: &ServerDb) -> Result<()> {
    at.client().await?.flush_all().await
}

/// `REPLICAOF host port`.
pub async fn replicaof(at: &ServerDb, host: &str, port: u16) -> Result<()> {
    at.client().await?.replicaof(host, port).await
}

/// `REPLICAOF NO ONE`.
pub async fn replicaof_no_one(at: &ServerDb) -> Result<()> {
    at.client().await?.replicaof_no_one().await
}

/// `FAILOVER [TO host port [FORCE]] TIMEOUT ms`.
pub async fn failover(at: &ServerDb, target: Option<(&str, u16)>, force: bool, timeout_ms: u64) -> Result<()> {
    at.client().await?.failover(target, force, timeout_ms).await
}

/// `FAILOVER ABORT`.
pub async fn failover_abort(at: &ServerDb) -> Result<()> {
    at.client().await?.failover_abort().await
}

/// Drop the pooled client, so the next operation — or the next heartbeat —
/// rebuilds it: a Sentinel or Cluster entry re-runs its discovery, which is
/// how a failover is followed. Sends nothing.
pub fn forget_client(at: &ServerDb) {
    get_connection_manager().remove_client(at.server_id(), at.db());
}

/// Unlock the entry's writes for `WRITE_UNLOCK_SECS`, wherever something
/// other than this process enforces the lock. On the desktop nothing does —
/// the lock is the app's own `AccessMode::SafeMode`, re-engaged by its
/// timer — so there is nothing to tell.
#[cfg(not(target_family = "wasm"))]
pub async fn unlock_writes(_at: &ServerDb) -> Result<()> {
    Ok(())
}

/// In the browser the bridge enforces the lock and is told: it keeps the
/// window per account and entry, and refuses writes outside it whatever
/// the page believes (ADR 14). The confirmation it wants is the entry's
/// name — production asks for the name, and the page's dialog has just had
/// it answered.
#[cfg(target_family = "wasm")]
pub async fn unlock_writes(at: &ServerDb) -> Result<()> {
    let transport = crate::bridge::bridge_transport().ok_or_else(|| Error::Invalid {
        message: "no bridge transport".to_string(),
    })?;
    let name = crate::config::get_server(at.server_id())?.name;
    transport
        .unlock_writes(at.server_id().to_string(), name)
        .await
        .map_err(|e| Error::Invalid { message: e.to_string() })
}

/// Lock the entry's writes again before the window ends. See
/// [`unlock_writes`] for why the desktop has nothing to say.
#[cfg(not(target_family = "wasm"))]
pub async fn lock_writes(_at: &ServerDb) -> Result<()> {
    Ok(())
}

#[cfg(target_family = "wasm")]
pub async fn lock_writes(at: &ServerDb) -> Result<()> {
    let transport = crate::bridge::bridge_transport().ok_or_else(|| Error::Invalid {
        message: "no bridge transport".to_string(),
    })?;
    transport
        .lock_writes(at.server_id().to_string())
        .await
        .map_err(|e| Error::Invalid { message: e.to_string() })
}

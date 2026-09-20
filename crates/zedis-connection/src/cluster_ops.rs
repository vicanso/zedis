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

//! Cluster administration: the commands that address *one node*, and the two
//! that are told to every master.
//!
//! A cluster command is not a [`ServerDb`] operation. `CLUSTER REPLICATE`,
//! `FAILOVER`, `ADDSLOTS` and `SETSLOT` are not gossiped: they do what they
//! say on the node they reach, and reaching the wrong one is a different
//! outcome, not a slower one. So they take a [`ClusterNode`] — which node of
//! which configured cluster — and the view names the node it means.
//!
//! `MEET` and `FORGET` are the exceptions and take a `ServerDb`: they are
//! told to every master, `FORGET` because gossip re-adds a node that only
//! some masters dropped.

use crate::async_connection::{open_node_connection, open_node_connection_cached};
use crate::config::get_server;
use crate::error::Error;
use crate::manager::cluster_migrate_slots;
use crate::manager::{AtomicSlotMigration, cluster_cancel_slot_migrations, cluster_get_slot_migrations};
use crate::server_db::ServerDb;
use redis::aio::MultiplexedConnection;
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Slots per `CLUSTER ADDSLOTS` call: it takes one argument per slot (the
/// range form is Redis 7.0+) and a full-shard gap is over five thousand.
const ADDSLOTS_CHUNK: usize = 1024;
/// Keys drained per `MIGRATE` while moving one slot by hand.
const MIGRATE_BATCH: usize = 100;
/// How long the source waits for the target to take a `MIGRATE` batch.
const MIGRATE_TIMEOUT_MS: i64 = 10_000;

/// One node of one configured cluster, by address (`host:port`, as `CLUSTER
/// NODES` spells it). The counterpart of [`ServerDb`] for the commands that
/// are answered by whichever node hears them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClusterNode {
    server_id: String,
    addr: String,
}

impl ClusterNode {
    pub fn new(server_id: impl Into<String>, addr: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            addr: addr.into(),
        }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// A connection of this call's own. Crate private like
    /// [`ServerDb::connection`], and for the same reason.
    async fn connection(&self) -> Result<MultiplexedConnection> {
        // Boxed for the same reason as `ServerDb::connection`: dialing is a
        // large future, and a caller that opens several nodes in a loop
        // would carry all of them on one stack frame.
        Box::pin(open_node_connection(&self.server_id, &self.addr)).await
    }

    /// The pooled per-node connection, for traffic that *recurs* — a poll
    /// that samples every master on an interval, where a fresh handshake per
    /// node per tick is pure churn. Never for connection-scoped state.
    async fn cached_connection(&self) -> Result<MultiplexedConnection> {
        Box::pin(open_node_connection_cached(&self.server_id, &self.addr)).await
    }
}

/// `CLUSTER FAILOVER [FORCE]` on this node — it must be a replica, and it is
/// the one that gets promoted.
pub async fn node_failover(node: &ClusterNode, force: bool) -> Result<()> {
    let mut c = cmd("CLUSTER");
    c.arg("FAILOVER");
    if force {
        c.arg("FORCE");
    }
    let _: String = c.query_async(&mut node.connection().await?).await?;
    Ok(())
}

/// `CLUSTER REPLICATE master_node_id` — make this node a replica of that
/// master. The node must already be an empty, known cluster member; Redis
/// refuses otherwise.
pub async fn node_replicate(node: &ClusterNode, master_node_id: &str) -> Result<()> {
    let _: String = cmd("CLUSTER")
        .arg("REPLICATE")
        .arg(master_node_id)
        .query_async(&mut node.connection().await?)
        .await?;
    Ok(())
}

/// `CLUSTER SETSLOT slot STABLE` — clear a half-finished migration's
/// importing/migrating marks. Told to each node that still carries one.
pub async fn node_stabilize_slot(node: &ClusterNode, slot: u16) -> Result<()> {
    let _: String = cmd("CLUSTER")
        .arg("SETSLOT")
        .arg(slot)
        .arg("STABLE")
        .query_async(&mut node.connection().await?)
        .await
        .map_err(|e| Error::Invalid {
            message: format!("SETSLOT {slot} STABLE on {}: {e}", node.addr),
        })?;
    Ok(())
}

/// `CLUSTER ADDSLOTS` — hand slots nobody owns to this master. Rejected for
/// a slot that already has an owner, so the caller passes only the gaps.
pub async fn node_add_slots(node: &ClusterNode, slots: &[u16]) -> Result<()> {
    if slots.is_empty() {
        return Ok(());
    }
    let conn = &mut node.connection().await?;
    for chunk in slots.chunks(ADDSLOTS_CHUNK) {
        let mut c = cmd("CLUSTER");
        c.arg("ADDSLOTS");
        for slot in chunk {
            c.arg(*slot);
        }
        let _: String = c.query_async(conn).await.map_err(|e| Error::Invalid {
            message: format!("CLUSTER ADDSLOTS on {}: {e}", node.addr),
        })?;
    }
    Ok(())
}

/// `CLUSTER MIGRATESLOTS` (Valkey 9) — hand these ranges to `target_id`, the
/// server doing the moving. Only the source node can start one.
pub async fn node_migrate_slots(node: &ClusterNode, ranges: &[(u16, u16)], target_id: &str) -> Result<()> {
    cluster_migrate_slots(&mut node.connection().await?, ranges, target_id)
        .await
        .map_err(|e| Error::Invalid {
            message: format!("CLUSTER MIGRATESLOTS on {}: {e}", node.addr),
        })
}

/// `CLUSTER CANCELSLOTMIGRATIONS` — only the node that started a migration
/// can abort it.
pub async fn node_cancel_slot_migrations(node: &ClusterNode) -> Result<()> {
    cluster_cancel_slot_migrations(&mut node.connection().await?)
        .await
        .map_err(|e| Error::Invalid {
            message: format!("CLUSTER CANCELSLOTMIGRATIONS on {}: {e}", node.addr),
        })
}

/// `CLUSTER GETSLOTMIGRATIONS` on this node, on the pooled connection — the
/// Reshard tab polls it.
pub async fn node_slot_migrations(node: &ClusterNode) -> Result<Vec<AtomicSlotMigration>> {
    cluster_get_slot_migrations(&mut node.cached_connection().await?).await
}

/// What the load heatmap samples from each master.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeLoad {
    pub used_memory: u64,
    pub ops_per_sec: u64,
    pub connected_clients: u64,
}

/// Default `INFO` on this node, read down to the three numbers the heatmap
/// draws. On the pooled connection, and default `INFO` rather than `INFO
/// all`: this runs on a poll interval, and `all` additionally streams the
/// command-scaled stats just to be thrown away.
pub async fn node_load(node: &ClusterNode) -> Result<NodeLoad> {
    let info: String = cmd("INFO").query_async(&mut node.cached_connection().await?).await?;
    let mut load = NodeLoad::default();
    for line in info.lines() {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        match field {
            "used_memory" => load.used_memory = value.parse().unwrap_or(0),
            "instantaneous_ops_per_sec" => load.ops_per_sec = value.parse().unwrap_or(0),
            "connected_clients" => load.connected_clients = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    Ok(load)
}

/// `CLUSTER MEET host port` on every master, so the new node is introduced
/// to all of them rather than learned by gossip from one.
pub async fn cluster_meet(at: &ServerDb, host: &str, port: u16) -> Result<()> {
    let mut c = cmd("CLUSTER");
    c.arg("MEET").arg(host).arg(port);
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![c]).await?;
    Ok(())
}

/// `CLUSTER FORGET node_id` on every master. It has to reach all of them
/// within 60s or the next gossip round re-adds the node, which is why this
/// one fans out rather than naming a node.
pub async fn cluster_forget(at: &ServerDb, node_id: &str) -> Result<()> {
    let mut c = cmd("CLUSTER");
    c.arg("FORGET").arg(node_id);
    let (_, _replies): (_, Vec<String>) = at.client().await?.query_async_masters(vec![c]).await?;
    Ok(())
}

/// Every master of the cluster, as `host:port`.
pub async fn master_addrs(at: &ServerDb) -> Result<Vec<String>> {
    Ok(at
        .client()
        .await?
        .master_servers()
        .iter()
        .map(|server| format!("{}:{}", server.host, server.port))
        .collect())
}

/// Which slot is moving where, for [`migrate_slot`].
pub struct SlotMove<'a> {
    pub slot: u16,
    pub source: &'a ClusterNode,
    pub source_id: &'a str,
    pub target: &'a ClusterNode,
    pub target_id: &'a str,
    /// Every master, including source and target: where ownership is
    /// committed once the keys are across.
    pub master_addrs: &'a [String],
}

/// Move one slot by hand — the pre-Valkey-9 reshard, which is four steps and
/// no server-side supervision:
///
/// 1. `SETSLOT … IMPORTING` on the target and `MIGRATING` on the source, so
///    both redirect clients while the keys are in flight;
/// 2. `GETKEYSINSLOT` + `MIGRATE` in batches until the slot is empty —
///    `NOKEY` counts as done for its batch, a key having vanished mid-flight;
/// 3. `SETSLOT … NODE target` on **every** master, which is what makes the
///    new owner authoritative rather than a redirect;
///
/// and nothing rolls back on its own: a failure between 1 and 3 leaves the
/// slot marked, which `node_stabilize_slot` is for. Prefer
/// [`node_migrate_slots`] where the server has it.
pub async fn migrate_slot(mv: SlotMove<'_>) -> Result<()> {
    let SlotMove {
        slot,
        source,
        source_id,
        target,
        target_id,
        master_addrs,
    } = mv;
    let (target_host, target_port) = split_addr(target.addr())?;
    // `MIGRATE … AUTH` speaks to the target as a client would.
    let password = get_server(&source.server_id).ok().and_then(|server| server.password);
    let source_conn = &mut source.connection().await?;
    let target_conn = &mut target.connection().await?;

    let _: String = cmd("CLUSTER")
        .arg("SETSLOT")
        .arg(slot)
        .arg("IMPORTING")
        .arg(source_id)
        .query_async(target_conn)
        .await?;
    let _: String = cmd("CLUSTER")
        .arg("SETSLOT")
        .arg(slot)
        .arg("MIGRATING")
        .arg(target_id)
        .query_async(source_conn)
        .await?;

    loop {
        let keys: Vec<String> = cmd("CLUSTER")
            .arg("GETKEYSINSLOT")
            .arg(slot)
            .arg(MIGRATE_BATCH)
            .query_async(source_conn)
            .await?;
        if keys.is_empty() {
            break;
        }
        let mut migrate = cmd("MIGRATE");
        migrate
            .arg(target_host)
            .arg(target_port)
            .arg("")
            .arg(0)
            .arg(MIGRATE_TIMEOUT_MS);
        if let Some(password) = password.as_deref() {
            migrate.arg("AUTH").arg(password);
        }
        migrate.arg("KEYS");
        for key in &keys {
            migrate.arg(key);
        }
        if let Err(e) = migrate.query_async::<String>(source_conn).await {
            let message = e.to_string();
            // A key that vanished mid-flight: the batch is still done.
            if !message.contains("NOKEY") {
                return Err(Error::Invalid { message });
            }
        }
    }

    for addr in master_addrs {
        let node = ClusterNode::new(&source.server_id, addr);
        let _: String = cmd("CLUSTER")
            .arg("SETSLOT")
            .arg(slot)
            .arg("NODE")
            .arg(target_id)
            .query_async(&mut node.connection().await?)
            .await
            .map_err(|e| Error::Invalid {
                message: format!("SETSLOT NODE on {addr}: {e}"),
            })?;
    }
    Ok(())
}

/// `host:port` apart. The last colon is the separator, so an IPv6 literal
/// parses whether it arrives bracketed or bare.
fn split_addr(addr: &str) -> Result<(&str, u16)> {
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| Error::Invalid {
        message: format!("invalid node addr {addr}"),
    })?;
    let port = port.parse().map_err(|e| Error::Invalid {
        message: format!("invalid node port in {addr}: {e}"),
    })?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::split_addr;

    #[test]
    fn a_node_addr_splits_at_its_last_colon() {
        assert_eq!(split_addr("127.0.0.1:7000").expect("ipv4"), ("127.0.0.1", 7000));
        assert_eq!(split_addr("[::1]:7000").expect("bracketed"), ("[::1]", 7000));
        assert_eq!(split_addr("::1:7000").expect("bare"), ("::1", 7000));
        assert!(split_addr("127.0.0.1").is_err(), "no port");
        assert!(split_addr("127.0.0.1:http").is_err(), "not a port");
    }
}

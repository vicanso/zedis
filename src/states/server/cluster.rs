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

//! Cluster topology operations (FAILOVER / MEET / FORGET / REPLICATE /
//! RESHARD).
//!
//! Destructive ops route through a confirm dialog at the view layer
//! (`ZedisDialog::new_alert` + production-tag escalation). This module
//! trusts the gate and dispatches the command.
//!
//! Targeting model per command:
//!   * `CLUSTER FAILOVER [FORCE]` — must run **on** the replica that
//!     should take over (Redis routes the command locally; it isn't
//!     gossiped). One-shot connection to that node.
//!   * `CLUSTER MEET host port` — sent to one master; cluster gossip
//!     propagates membership. Fan-out via the pooled masters client
//!     is also correct (every master tries to meet — redundant but
//!     harmless).
//!   * `CLUSTER FORGET node_id` — must hit **every** master within
//!     60s, otherwise the missed master re-introduces the dropped
//!     node via gossip. Fan-out is the safe shape.
//!   * `CLUSTER REPLICATE node_id` — must run on the node that should
//!     become a replica. One-shot connection to that node.
//!   * Reshard — for each slot: SETSLOT MIGRATING/IMPORTING on source
//!     and target, MIGRATE keys, then SETSLOT NODE on every master.

use crate::connection::{Capability, get_connection_manager, get_server, open_node_connection, plan_reshard_slots};
use crate::error::Error;
use crate::states::{ServerTask, ZedisServerState};
use gpui::{SharedString, prelude::*};
use redis::cmd;
use tracing::warn;

/// Per-master load sample for the Topology heatmap (memory + OPS).
#[derive(Debug, Clone)]
pub struct ClusterNodeLoad {
    pub node_id: SharedString,
    pub addr: SharedString,
    pub used_memory: u64,
    pub ops_per_sec: u64,
    pub connected_clients: u64,
    pub slot_count: u32,
    pub color_index: usize,
}

/// Outcome of a reshard batch (slots moved + any per-slot errors).
#[derive(Debug, Clone)]
pub struct ClusterReshardResult {
    pub moved: u32,
    pub total: u32,
    pub errors: Vec<String>,
}

impl ZedisServerState {
    /// `CLUSTER FAILOVER [FORCE]` on a specific replica.
    ///
    /// `target_addr` is the replica's `host:port` (must match the
    /// `address` field reported by `CLUSTER NODES`). `force=true`
    /// skips the master-handshake step — used when the master is
    /// unreachable and an immediate takeover is preferred over a
    /// graceful one.
    pub fn cluster_failover(&mut self, target_addr: SharedString, force: bool, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let target_for_msg = target_addr.clone();
        self.spawn(
            ServerTask::ClusterFailover,
            move || async move {
                let mut conn = open_node_connection(server_id.as_ref(), target_addr.as_ref()).await?;
                let mut c = cmd("CLUSTER");
                c.arg("FAILOVER");
                if force {
                    c.arg("FORCE");
                }
                let _: String = c.query_async(&mut conn).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER FAILOVER sent → {target_for_msg}").into(),
                        "FAILOVER".into(),
                        cx,
                    );
                    // Eager refresh so the new master/replica roles
                    // appear in `nodes_description` without waiting
                    // for the next 2s heartbeat.
                    this.refresh_redis_info(cx);
                }
                // Error path: `spawn` already records via add_error_message.
            },
            cx,
        );
    }

    /// `CLUSTER MEET host port` — introduce a new node by address.
    /// Fanned out to every master via the pooled client; redundant
    /// for a single new node but harmless and avoids picking which
    /// master to send through. The new node's `node_id` is allocated
    /// by the cluster and learned via the next gossip round.
    pub fn cluster_meet(&mut self, host: SharedString, port: u16, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let host_for_op = host.clone();
        self.spawn(
            ServerTask::ClusterMeet,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut c = cmd("CLUSTER");
                c.arg("MEET").arg(host_for_op.as_ref()).arg(port);
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER MEET {host}:{port} sent").into(),
                        "MEET".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `CLUSTER FORGET node_id` — fan-out to every master so gossip
    /// can't reintroduce the dropped node. Redis requires the forget
    /// to land on all masters within 60s or the next gossip round
    /// re-adds the node.
    pub fn cluster_forget(&mut self, node_id: SharedString, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let id_for_op = node_id.clone();
        self.spawn(
            ServerTask::ClusterForget,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut c = cmd("CLUSTER");
                c.arg("FORGET").arg(id_for_op.as_ref());
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER FORGET {node_id} sent to all masters").into(),
                        "FORGET".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `CLUSTER REPLICATE master_node_id` — turn the node at
    /// `target_addr` into a replica of the given master. The
    /// command must run **on** the target node (Redis doesn't
    /// gossip it), so this opens a one-shot connection to it.
    /// The target must already be empty and a known cluster
    /// member — Redis rejects with an error otherwise.
    pub fn cluster_replicate(
        &mut self,
        target_addr: SharedString,
        master_node_id: SharedString,
        cx: &mut Context<Self>,
    ) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let target_for_msg = target_addr.clone();
        let master_for_msg = master_node_id.clone();
        self.spawn(
            ServerTask::ClusterReplicate,
            move || async move {
                let mut conn = open_node_connection(server_id.as_ref(), target_addr.as_ref()).await?;
                let _: String = cmd("CLUSTER")
                    .arg("REPLICATE")
                    .arg(master_node_id.as_ref())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER REPLICATE {master_for_msg} → {target_for_msg}").into(),
                        "REPLICATE".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// Move `slots` onto `target` (node_id + host:port). Source ownership
    /// is discovered per slot via `CLUSTER NODES` on the target connection
    /// when not supplied — callers that already planned a source should
    /// pass `source_hint` (addr, id) so we skip the lookup.
    ///
    /// For each slot the sequence mirrors `redis-cli --cluster reshard`:
    /// 1. `SETSLOT IMPORTING` on target, `SETSLOT MIGRATING` on source
    /// 2. loop `GETKEYSINSLOT` + `MIGRATE … KEYS`
    /// 3. `SETSLOT NODE target` on source, target, and every other master
    pub fn cluster_reshard(
        &mut self,
        target_addr: SharedString,
        target_id: SharedString,
        slots: Vec<u16>,
        source_by_slot: Vec<(u16, String, String)>, // slot, source_addr, source_id
        cx: &mut Context<Self>,
    ) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        if slots.is_empty() {
            self.emit_warning_notification("No slots to move".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let total = slots.len() as u32;
        let target_for_msg = target_addr.clone();
        self.spawn(
            ServerTask::ClusterReshard,
            move || async move {
                reshard_slots(
                    server_id.as_ref(),
                    target_addr.as_ref(),
                    target_id.as_ref(),
                    &slots,
                    &source_by_slot,
                )
                .await
            },
            move |this, result, cx| match result {
                Ok(r) => {
                    if r.errors.is_empty() {
                        this.emit_success_notification(
                            format!(
                                "Reshard complete: moved {}/{} slots → {target_for_msg}",
                                r.moved, r.total
                            )
                            .into(),
                            "RESHARD".into(),
                            cx,
                        );
                    } else {
                        let detail = r.errors.join("; ");
                        this.emit_warning_notification(
                            format!("Reshard partial: moved {}/{} slots. Errors: {detail}", r.moved, r.total).into(),
                            cx,
                        );
                    }
                    this.refresh_redis_info(cx);
                }
                Err(_) => {
                    // spawn already records the error message.
                }
            },
            cx,
        );
        let _ = total; // used in async path via slots.len()
    }
}

/// Fetch `INFO` memory/stats from each cluster master for the load heatmap.
/// Runs outside `ZedisServerState::spawn` so the Topology view can poll
/// without competing with the server-task busy gate.
pub async fn fetch_cluster_node_loads(
    server_id: &str,
    masters: &[(String, String, u32, usize)], // id, addr, slot_count, color_index
) -> Result<Vec<ClusterNodeLoad>, Error> {
    let mut out = Vec::with_capacity(masters.len());
    for (node_id, addr, slot_count, color_index) in masters {
        let mut conn = open_node_connection(server_id, addr).await?;
        let info: String = cmd("INFO").arg("all").query_async(&mut conn).await?;
        let mut used_memory = 0u64;
        let mut ops_per_sec = 0u64;
        let mut connected_clients = 0u64;
        for line in info.lines() {
            if let Some((k, v)) = line.split_once(':') {
                match k {
                    "used_memory" => used_memory = v.parse().unwrap_or(0),
                    "instantaneous_ops_per_sec" => ops_per_sec = v.parse().unwrap_or(0),
                    "connected_clients" => connected_clients = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        out.push(ClusterNodeLoad {
            node_id: node_id.clone().into(),
            addr: addr.clone().into(),
            used_memory,
            ops_per_sec,
            connected_clients,
            slot_count: *slot_count,
            color_index: *color_index,
        });
    }
    Ok(out)
}

/// Master ownership row used by the reshard planner / source mapper.
#[derive(Debug, Clone)]
pub struct ClusterMasterRanges {
    pub node_id: String,
    pub addr: String,
    pub ranges: Vec<(u16, u16)>,
}

/// Build `(slot, source_addr, source_id)` rows from a slot map for the
/// planner output. Used by the Topology reshard form.
pub fn source_owners_for_slots(
    masters: &[ClusterMasterRanges],
    slots: &[u16],
) -> Result<Vec<(u16, String, String)>, String> {
    let mut out = Vec::with_capacity(slots.len());
    for &slot in slots {
        let owner = masters
            .iter()
            .find(|m| m.ranges.iter().any(|&(lo, hi)| slot >= lo && slot <= hi));
        let Some(m) = owner else {
            return Err(format!("slot {slot} has no owner in the current map"));
        };
        out.push((slot, m.addr.clone(), m.node_id.clone()));
    }
    Ok(out)
}

/// Plan slots using the shared pure helper; re-exported convenience for UI.
pub fn plan_cluster_reshard(
    masters: &[(String, Vec<(u16, u16)>)],
    source_id: Option<&str>,
    target_id: &str,
    count: u32,
) -> Result<Vec<u16>, String> {
    plan_reshard_slots(masters, source_id, target_id, count)
}

async fn reshard_slots(
    server_id: &str,
    target_addr: &str,
    target_id: &str,
    slots: &[u16],
    source_by_slot: &[(u16, String, String)],
) -> Result<ClusterReshardResult, Error> {
    let password = get_server(server_id).ok().and_then(|s| s.password);
    let (target_host, target_port) = target_addr.rsplit_once(':').ok_or_else(|| Error::Invalid {
        message: format!("invalid target addr {target_addr}"),
    })?;
    let target_port: u16 = target_port.parse().map_err(|e| Error::Invalid {
        message: format!("invalid target port: {e}"),
    })?;

    // All master addrs for the final SETSLOT NODE fan-out.
    let client = get_connection_manager().get_client(server_id, 0).await?;
    let master_servers = client.master_servers();
    let master_addrs: Vec<String> = master_servers
        .iter()
        .map(|s| format!("{}:{}", s.host, s.port))
        .collect();

    let mut source_lookup: std::collections::HashMap<u16, (String, String)> = source_by_slot
        .iter()
        .map(|(s, a, i)| (*s, (a.clone(), i.clone())))
        .collect();

    let mut moved = 0u32;
    let mut errors = Vec::new();

    for &slot in slots {
        let (source_addr, source_id) = match source_lookup.remove(&slot) {
            Some(v) => v,
            None => {
                errors.push(format!("slot {slot}: missing source mapping"));
                continue;
            }
        };
        if source_id == target_id {
            // Already on target — skip.
            moved += 1;
            continue;
        }

        if let Err(e) = migrate_one_slot(MigrateSlotArgs {
            server_id,
            slot,
            source_addr: &source_addr,
            source_id: &source_id,
            target_addr,
            target_id,
            target_host,
            target_port,
            password: password.as_deref(),
            master_addrs: &master_addrs,
        })
        .await
        {
            warn!(slot, error = %e, "reshard slot failed");
            errors.push(format!("slot {slot}: {e}"));
            continue;
        }
        moved += 1;
    }

    Ok(ClusterReshardResult {
        moved,
        total: slots.len() as u32,
        errors,
    })
}

struct MigrateSlotArgs<'a> {
    server_id: &'a str,
    slot: u16,
    source_addr: &'a str,
    source_id: &'a str,
    target_addr: &'a str,
    target_id: &'a str,
    target_host: &'a str,
    target_port: u16,
    password: Option<&'a str>,
    master_addrs: &'a [String],
}

async fn migrate_one_slot(args: MigrateSlotArgs<'_>) -> Result<(), Error> {
    let MigrateSlotArgs {
        server_id,
        slot,
        source_addr,
        source_id,
        target_addr,
        target_id,
        target_host,
        target_port,
        password,
        master_addrs,
    } = args;

    let mut source_conn = open_node_connection(server_id, source_addr).await?;
    let mut target_conn = open_node_connection(server_id, target_addr).await?;

    // Mark migration intent.
    let _: String = cmd("CLUSTER")
        .arg("SETSLOT")
        .arg(slot)
        .arg("IMPORTING")
        .arg(source_id)
        .query_async(&mut target_conn)
        .await?;
    let _: String = cmd("CLUSTER")
        .arg("SETSLOT")
        .arg(slot)
        .arg("MIGRATING")
        .arg(target_id)
        .query_async(&mut source_conn)
        .await?;

    // Drain keys in batches.
    const BATCH: usize = 100;
    const MIGRATE_TIMEOUT_MS: i64 = 10_000;
    loop {
        let keys: Vec<String> = cmd("CLUSTER")
            .arg("GETKEYSINSLOT")
            .arg(slot)
            .arg(BATCH)
            .query_async(&mut source_conn)
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
        if let Some(pw) = password {
            migrate.arg("AUTH").arg(pw);
        }
        migrate.arg("KEYS");
        for k in &keys {
            migrate.arg(k);
        }
        // MIGRATE may return "NOKEY" when a key vanished mid-flight — treat
        // as success for that batch.
        match migrate.query_async::<String>(&mut source_conn).await {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("NOKEY") {
                    return Err(Error::Invalid { message: msg });
                }
            }
        }
    }

    // Commit ownership on every known master (source/target included).
    for addr in master_addrs {
        let mut conn = open_node_connection(server_id, addr).await?;
        let _: String = cmd("CLUSTER")
            .arg("SETSLOT")
            .arg(slot)
            .arg("NODE")
            .arg(target_id)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Invalid {
                message: format!("SETSLOT NODE on {addr}: {e}"),
            })?;
    }

    Ok(())
}

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
//!   * `SETSLOT … STABLE` — the repair for a reshard that was interrupted
//!     and left a slot marked on both ends.
//!   * `ADDSLOTS` — the repair for a cluster that lost slot coverage.

use crate::connection::{
    AtomicSlotMigration, Capability, cluster_cancel_slot_migrations, cluster_get_slot_migrations,
    cluster_migrate_slots, get_connection_manager, get_server, group_slot_ranges, open_node_connection,
    open_node_connection_cached, plan_cluster_rebalance as plan_rebalance_slots, plan_reshard_slots,
};
use crate::error::Error;
use crate::states::{ServerTask, ZedisServerState, i18n_common};
use futures::future::try_join_all;
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

    /// `CLUSTER SETSLOT <slot> STABLE` on both ends of a migration that
    /// never finished — the state an interrupted reshard leaves behind
    /// (`[slot->-target]` on the source, `[slot-<-source]` on the target).
    /// Clearing the markers settles the slot with its current owner; the
    /// keys that already moved stay moved, which the confirm dialog says.
    ///
    /// Both ends are addressed directly: `SETSLOT` is not gossiped. An
    /// address the map could not pair is simply not sent to — the other
    /// end still gets cleared, which is what a half-visible migration
    /// needs.
    pub fn cluster_stabilize_slot(&mut self, slot: u16, addrs: Vec<SharedString>, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let addrs: Vec<SharedString> = addrs.into_iter().filter(|addr| !addr.is_empty()).collect();
        if addrs.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        self.spawn_with_arg(
            ServerTask::ClusterStabilizeSlot,
            slot.to_string(),
            move || async move {
                for addr in &addrs {
                    let mut conn = open_node_connection(server_id.as_ref(), addr.as_ref()).await?;
                    let _: String = cmd("CLUSTER")
                        .arg("SETSLOT")
                        .arg(slot)
                        .arg("STABLE")
                        .query_async(&mut conn)
                        .await
                        .map_err(|e| Error::Invalid {
                            message: format!("SETSLOT {slot} STABLE on {addr}: {e}"),
                        })?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER SETSLOT {slot} STABLE").into(),
                        "SETSLOT".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `CLUSTER ADDSLOTS` on `target_addr` — hand slots nobody owns to a
    /// master. The command runs **on** the node that will own them and is
    /// rejected for a slot that already has an owner, so callers pass only
    /// the coverage gaps.
    ///
    /// Sent in chunks: `ADDSLOTS` takes one argument per slot (the range
    /// form is Redis 7.0+), and a full-shard gap is over five thousand of
    /// them.
    pub fn cluster_add_slots(&mut self, target_addr: SharedString, slots: Vec<u16>, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        if slots.is_empty() {
            return;
        }
        /// Slots per `CLUSTER ADDSLOTS` call.
        const CHUNK: usize = 1024;
        let server_id = self.server_id.clone();
        let count = slots.len();
        let target_for_msg = target_addr.clone();
        self.spawn_with_arg(
            ServerTask::ClusterAddSlots,
            target_addr.clone(),
            move || async move {
                let mut conn = open_node_connection(server_id.as_ref(), target_addr.as_ref()).await?;
                for chunk in slots.chunks(CHUNK) {
                    let mut c = cmd("CLUSTER");
                    c.arg("ADDSLOTS");
                    for slot in chunk {
                        c.arg(*slot);
                    }
                    let _: String = c.query_async(&mut conn).await.map_err(|e| Error::Invalid {
                        message: format!("CLUSTER ADDSLOTS on {target_addr}: {e}"),
                    })?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER ADDSLOTS — {count} slots → {target_for_msg}").into(),
                        "ADDSLOTS".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// Valkey 9's atomic slot migration: one `CLUSTER MIGRATESLOTS` per
    /// source node, each handing its own ranges to `target_id`. The
    /// command returns as soon as the server accepted the job, so this
    /// finishes immediately and the Reshard tab watches the result through
    /// `CLUSTER GETSLOTMIGRATIONS` — unlike the legacy path, closing the
    /// app no longer strands a slot half-migrated.
    pub fn cluster_migrate_slots_atomic(
        &mut self,
        jobs: Vec<(SharedString, Vec<(u16, u16)>)>,
        target_id: SharedString,
        cx: &mut Context<Self>,
    ) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let jobs: Vec<(SharedString, Vec<(u16, u16)>)> = jobs
            .into_iter()
            .filter(|(addr, ranges)| !addr.is_empty() && !ranges.is_empty())
            .collect();
        if jobs.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let slot_count: usize = jobs
            .iter()
            .flat_map(|(_, ranges)| ranges.iter())
            .map(|(lo, hi)| usize::from(hi - lo) + 1)
            .sum();
        let target_for_msg = target_id.clone();
        self.spawn_with_arg(
            ServerTask::ClusterMigrateSlots,
            target_id.clone(),
            move || async move {
                for (source_addr, ranges) in &jobs {
                    let mut conn = open_node_connection(server_id.as_ref(), source_addr.as_ref()).await?;
                    cluster_migrate_slots(&mut conn, ranges, target_id.as_ref())
                        .await
                        .map_err(|e| Error::Invalid {
                            message: format!("CLUSTER MIGRATESLOTS on {source_addr}: {e}"),
                        })?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER MIGRATESLOTS — {slot_count} slots → {target_for_msg}").into(),
                        "MIGRATESLOTS".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `CLUSTER CANCELSLOTMIGRATIONS` on each source node — only the node
    /// that started a migration can abort it.
    pub fn cluster_cancel_slot_migrations(&mut self, source_addrs: Vec<SharedString>, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let source_addrs: Vec<SharedString> = source_addrs.into_iter().filter(|a| !a.is_empty()).collect();
        if source_addrs.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let count = source_addrs.len();
        self.spawn(
            ServerTask::ClusterCancelSlotMigrations,
            move || async move {
                for addr in &source_addrs {
                    let mut conn = open_node_connection(server_id.as_ref(), addr.as_ref()).await?;
                    cluster_cancel_slot_migrations(&mut conn)
                        .await
                        .map_err(|e| Error::Invalid {
                            message: format!("CLUSTER CANCELSLOTMIGRATIONS on {addr}: {e}"),
                        })?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("CLUSTER CANCELSLOTMIGRATIONS sent to {count} node(s)").into(),
                        "CANCELSLOTMIGRATIONS".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// Run a rebalance leg by leg. `atomic` picks the Valkey 9 path (the
    /// servers own the move and the app can be closed) over the legacy
    /// `SETSLOT` + `MIGRATE` loop.
    pub fn cluster_rebalance(&mut self, legs: Vec<RebalanceLeg>, atomic: bool, cx: &mut Context<Self>) {
        if !self.can(Capability::ClusterWrite) {
            self.emit_warning_notification("Read-only mode — cluster ops blocked".into(), cx);
            return;
        }
        let legs: Vec<RebalanceLeg> = legs.into_iter().filter(|leg| !leg.slots.is_empty()).collect();
        if legs.is_empty() {
            return;
        }
        if self.manually_offline {
            self.emit_warning_notification(i18n_common(cx, "reconnect_first"), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let total: u32 = legs.iter().map(|leg| leg.slots.len() as u32).sum();
        let leg_count = legs.len();

        // The legacy path reports per slot; the atomic one finishes as soon
        // as the servers accepted the jobs, so it needs no bar.
        let (progress_tx, progress_rx) = smol::channel::unbounded::<(u32, u32)>();
        if !atomic {
            self.set_reshard_progress(Some((0, total)), cx);
            cx.spawn(async move |handle, cx| {
                while let Ok(progress) = progress_rx.recv().await {
                    let updated = handle.update(cx, |this: &mut Self, cx| {
                        if this.reshard_progress.is_some() {
                            this.set_reshard_progress(Some(progress), cx);
                        }
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        self.spawn(
            ServerTask::ClusterReshard,
            move || async move {
                let mut moved = 0u32;
                let mut errors: Vec<String> = Vec::new();
                for leg in &legs {
                    if atomic {
                        let mut conn = open_node_connection(server_id.as_ref(), &leg.source_addr).await?;
                        let ranges = group_slot_ranges(&leg.slots);
                        match cluster_migrate_slots(&mut conn, &ranges, &leg.target_id).await {
                            Ok(()) => moved += leg.slots.len() as u32,
                            Err(e) => errors.push(format!("{} → {}: {e}", leg.source_addr, leg.target_id)),
                        }
                        continue;
                    }
                    let source_by_slot: Vec<(u16, String, String)> = leg
                        .slots
                        .iter()
                        .map(|slot| (*slot, leg.source_addr.clone(), leg.source_id.clone()))
                        .collect();
                    // Each leg counts from zero, so offset its ticks by
                    // what earlier legs already moved.
                    let (leg_tx, leg_rx) = smol::channel::unbounded::<(u32, u32)>();
                    let outer = progress_tx.clone();
                    let done_before = moved;
                    smol::spawn(async move {
                        while let Ok((processed, _)) = leg_rx.recv().await {
                            if outer.send((done_before + processed, total)).await.is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                    let result = reshard_slots(
                        server_id.as_ref(),
                        &leg.target_addr,
                        &leg.target_id,
                        &leg.slots,
                        &source_by_slot,
                        leg_tx,
                    )
                    .await?;
                    moved += result.moved;
                    errors.extend(result.errors);
                }
                Ok(ClusterReshardResult { moved, total, errors })
            },
            move |this, result, cx| {
                this.set_reshard_progress(None, cx);
                if let Ok(outcome) = result {
                    if outcome.errors.is_empty() {
                        this.emit_success_notification(
                            format!(
                                "Rebalance: {}/{} slots over {leg_count} moves",
                                outcome.moved, outcome.total
                            )
                            .into(),
                            "REBALANCE".into(),
                            cx,
                        );
                    } else {
                        let detail = outcome.errors.join("; ");
                        this.emit_warning_notification(
                            format!(
                                "Rebalance partial: {}/{} slots. Errors: {detail}",
                                outcome.moved, outcome.total
                            )
                            .into(),
                            cx,
                        );
                    }
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
        // `spawn` refuses to run while manually disconnected (and warns) —
        // bail before arming the progress state so it can't get stuck.
        if self.manually_offline {
            self.emit_warning_notification(i18n_common(cx, "reconnect_first"), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let total = slots.len() as u32;
        let target_for_msg = target_addr.clone();

        // Progress plumbing: the background loop reports (processed, total)
        // after each slot; a foreground drainer mirrors it into
        // `reshard_progress` so the Topology panel can render a live bar
        // instead of a static "running…" line.
        let (progress_tx, progress_rx) = smol::channel::unbounded::<(u32, u32)>();
        self.set_reshard_progress(Some((0, total)), cx);
        cx.spawn(async move |handle, cx| {
            while let Ok(progress) = progress_rx.recv().await {
                let updated = handle.update(cx, |this: &mut Self, cx| {
                    // Only track while a reshard is actually in flight —
                    // the completion handler below owns the final None.
                    if this.reshard_progress.is_some() {
                        this.set_reshard_progress(Some(progress), cx);
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        self.spawn(
            ServerTask::ClusterReshard,
            move || async move {
                reshard_slots(
                    server_id.as_ref(),
                    target_addr.as_ref(),
                    target_id.as_ref(),
                    &slots,
                    &source_by_slot,
                    progress_tx,
                )
                .await
            },
            move |this, result, cx| {
                this.set_reshard_progress(None, cx);
                match result {
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
                                format!("Reshard partial: moved {}/{} slots. Errors: {detail}", r.moved, r.total)
                                    .into(),
                                cx,
                            );
                        }
                        this.refresh_redis_info(cx);
                    }
                    Err(_) => {
                        // spawn already records the error message.
                    }
                }
            },
            cx,
        );
    }
}

/// `CLUSTER GETSLOTMIGRATIONS` on every master, tagged with the node it
/// came from. Runs outside `ZedisServerState::spawn` so the Reshard tab can
/// poll without competing with the server-task busy gate — the same shape
/// as the load heatmap below.
///
/// A master that will not answer is skipped rather than failing the poll:
/// the point is to show what *is* running, and one unreachable node must
/// not blank the list.
pub async fn fetch_slot_migrations(server_id: &str, masters: &[String]) -> Vec<(String, AtomicSlotMigration)> {
    let tasks = masters.iter().map(|addr| async move {
        let mut conn = open_node_connection_cached(server_id, addr).await.ok()?;
        let migrations = cluster_get_slot_migrations(&mut conn).await.ok()?;
        Some(
            migrations
                .into_iter()
                .map(|migration| (addr.clone(), migration))
                .collect::<Vec<_>>(),
        )
    });
    futures::future::join_all(tasks)
        .await
        .into_iter()
        .flatten()
        .flatten()
        .collect()
}

/// Fetch `INFO` memory/stats from each cluster master for the load heatmap.
/// Runs outside `ZedisServerState::spawn` so the Topology view can poll
/// without competing with the server-task busy gate.
///
/// This runs on a poll interval, so it is shaped for repetition: masters are
/// sampled **concurrently** on **pooled** connections (a fresh TCP+auth
/// handshake per master per tick was pure churn), and it fetches default
/// `INFO` — the three fields read below live in the default sections, while
/// `INFO all` additionally streamed the command-scaled stats just to be
/// thrown away.
pub async fn fetch_cluster_node_loads(
    server_id: &str,
    masters: &[(String, String, u32, usize)], // id, addr, slot_count, color_index
) -> Result<Vec<ClusterNodeLoad>, Error> {
    let tasks = masters
        .iter()
        .map(|(node_id, addr, slot_count, color_index)| async move {
            let mut conn = open_node_connection_cached(server_id, addr).await?;
            let info: String = cmd("INFO").query_async(&mut conn).await?;
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
            Ok::<ClusterNodeLoad, Error>(ClusterNodeLoad {
                node_id: node_id.clone().into(),
                addr: addr.clone().into(),
                used_memory,
                ops_per_sec,
                connected_clients,
                slot_count: *slot_count,
                color_index: *color_index,
            })
        });
    try_join_all(tasks).await
}

/// Master ownership row used by the reshard planner / source mapper.
#[derive(Debug, Clone)]
pub struct ClusterMasterRanges {
    pub node_id: String,
    pub addr: String,
    pub ranges: Vec<(u16, u16)>,
}

/// One leg of a rebalance, resolved from node ids to the addresses the
/// commands actually need.
#[derive(Debug, Clone)]
pub struct RebalanceLeg {
    pub source_addr: String,
    pub source_id: String,
    pub target_addr: String,
    pub target_id: String,
    pub slots: Vec<u16>,
}

/// Even out the slots across the masters, one leg at a time.
///
/// The legs run sequentially on purpose. Atomically they are cheap — one
/// `MIGRATESLOTS` per leg, and the servers do the rest — but on the legacy
/// path each leg walks its slots key by key, and firing several at once
/// would have every master both exporting and importing at the same time.
pub fn plan_cluster_rebalance_moves(
    masters: &[(String, Vec<(u16, u16)>)],
) -> Result<Vec<zedis_connection::RebalanceMove>, String> {
    plan_rebalance_slots(masters)
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
    progress: smol::channel::Sender<(u32, u32)>,
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
    let total = slots.len() as u32;

    for (index, &slot) in slots.iter().enumerate() {
        // Best-effort progress tick — the channel is unbounded and the
        // receiver may already be gone (view torn down), so ignore failures.
        let report = |processed: u32| {
            let _ = progress.try_send((processed, total));
        };
        let (source_addr, source_id) = match source_lookup.remove(&slot) {
            Some(v) => v,
            None => {
                errors.push(format!("slot {slot}: missing source mapping"));
                report(index as u32 + 1);
                continue;
            }
        };
        if source_id == target_id {
            // Already on target — skip.
            moved += 1;
            report(index as u32 + 1);
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
            report(index as u32 + 1);
            continue;
        }
        moved += 1;
        report(index as u32 + 1);
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

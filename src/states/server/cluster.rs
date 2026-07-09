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

//! Cluster topology operations (FAILOVER / MEET / FORGET / REPLICATE).
//!
//! All four are destructive on the cluster shape, so the view layer
//! routes user clicks through a confirm dialog
//! (`ZedisDialog::new_alert` and `dialog_button_props`, with
//! production-tag escalation). This module trusts the gate and just
//! dispatches the command. P2.1 ships these methods only — UI
//! buttons land in P2.3.
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
//!
//! Notifications are English literals on this commit; i18n keys land
//! in P2.3 when the user-facing buttons appear.

use crate::connection::{Capability, get_connection_manager, open_node_connection};
use crate::states::{ServerTask, ZedisServerState};
use gpui::{SharedString, prelude::*};
use redis::cmd;

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
}

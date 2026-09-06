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

//! Standalone replication from the server state: `REPLICAOF host port`,
//! `REPLICAOF NO ONE`, `FAILOVER TO host port [FORCE] TIMEOUT ms` and
//! `FAILOVER ABORT`, all sent to the node the pooled client talks to.
//! The link's state is not fetched here: the heartbeat's `INFO` already
//! carries the replication section, parsed into `RedisInfo::replication`,
//! which the Topology page reads live. Every change ends with an eager
//! `refresh_redis_info` so the page shows the new roles before the next
//! tick. The view routes each one through a confirm dialog first.

use crate::connection::{Capability, FAILOVER_TIMEOUT_MS, get_connection_manager};
use crate::states::{ServerTask, ZedisGlobalStore, ZedisServerState, i18n_topology};
use gpui::{SharedString, prelude::*};
use rust_i18n::t;

impl ZedisServerState {
    /// Capability gate shared by every change: a read-only connection gets
    /// one notice and nothing is sent.
    fn replication_write_allowed(&mut self, cx: &mut Context<Self>) -> bool {
        if self.can(Capability::ReplicationWrite) {
            return true;
        }
        self.emit_warning_notification(i18n_topology(cx, "repl_readonly_banner"), cx);
        false
    }

    fn replication_locale(&self, cx: &Context<Self>) -> String {
        cx.global::<ZedisGlobalStore>().read(cx).locale().to_string()
    }

    /// `REPLICAOF host port` — this node drops its dataset and follows
    /// `host:port`.
    pub fn replicaof(&mut self, host: SharedString, port: u16, cx: &mut Context<Self>) {
        if !self.replication_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let addr: SharedString = format!("{host}:{port}").into();
        let addr_for_msg = addr.clone();
        self.spawn_with_arg(
            ServerTask::Replicaof,
            addr,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                Ok(client.replicaof(&host, port).await?)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let locale = this.replication_locale(cx);
                    let message = t!("topology.repl_replicaof_sent", addr = addr_for_msg, locale = &locale);
                    this.emit_success_notification(message.to_string().into(), "REPLICAOF".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `REPLICAOF NO ONE` — this node stops following its primary, keeps
    /// what it replicated so far and accepts writes again.
    pub fn replicaof_no_one(&mut self, cx: &mut Context<Self>) {
        if !self.replication_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        self.spawn_with_arg(
            ServerTask::Replicaof,
            "NO ONE",
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                Ok(client.replicaof_no_one().await?)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let message = i18n_topology(cx, "repl_promoted");
                    this.emit_success_notification(message, "REPLICAOF NO ONE".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `FAILOVER TO host port [FORCE] TIMEOUT ms` on this primary — the
    /// replica at `target` takes over once it has caught up (or, forced,
    /// when the timeout runs out).
    pub fn failover(&mut self, host: SharedString, port: u16, force: bool, cx: &mut Context<Self>) {
        if !self.replication_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let addr: SharedString = format!("{host}:{port}").into();
        let addr_for_msg = addr.clone();
        self.spawn_with_arg(
            ServerTask::Failover,
            addr,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                Ok(client.failover(Some((&host, port)), force, FAILOVER_TIMEOUT_MS).await?)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let locale = this.replication_locale(cx);
                    let message = t!("topology.repl_failover_sent", addr = addr_for_msg, locale = &locale);
                    this.emit_success_notification(message.to_string().into(), "FAILOVER".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `FAILOVER ABORT` — cancels a failover still waiting for its replica.
    pub fn failover_abort(&mut self, cx: &mut Context<Self>) {
        if !self.replication_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        self.spawn_with_arg(
            ServerTask::Failover,
            "ABORT",
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                Ok(client.failover_abort().await?)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let message = i18n_topology(cx, "repl_failover_aborted");
                    this.emit_success_notification(message, "FAILOVER ABORT".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }
}

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

//! Sentinel topology operations (FAILOVER / RESET / REMOVE).
//!
//! All three are sent via the pooled client to **every connected
//! Sentinel instance** (`query_async_masters` in Sentinel mode
//! fans out to the sentinels themselves, since the GUI's connection
//! is *to* the sentinel quorum, not to the data masters they
//! monitor). The view layer routes user clicks through a confirm
//! dialog (`ZedisDialog::new_alert` + `dialog_button_props`) so a
//! stray click never demotes a running master.
//!
//! All commands target by **master name** — the Sentinel-monitored
//! label set in `sentinel.conf`. `TopologyEntry::master_name` carries
//! this from the parse layer; the UI reads it directly.

use crate::connection::get_connection_manager;
use crate::states::{ServerTask, ZedisServerState};
use gpui::{SharedString, prelude::*};
use redis::cmd;

impl ZedisServerState {
    /// `SENTINEL FAILOVER master_name` — force a manual failover on
    /// the named master. Used when the operator wants to swap the
    /// master role to a healthy replica without waiting for the
    /// sentinel quorum's automatic detection. Fans out to all
    /// sentinels so the new role is acknowledged across the quorum.
    pub fn sentinel_failover(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        if self.readonly() {
            self.emit_warning_notification("Read-only mode — sentinel ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let name_for_op = master_name.clone();
        self.spawn(
            ServerTask::SentinelFailover,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut c = cmd("SENTINEL");
                c.arg("FAILOVER").arg(name_for_op.as_ref());
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("SENTINEL FAILOVER {master_name} sent").into(),
                        "FAILOVER".into(),
                        cx,
                    );
                    // Eager refresh so the new master/replica roles
                    // appear in `nodes_description` without waiting
                    // for the next 2s heartbeat.
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL RESET pattern` — reset sentinel state for every
    /// master whose name matches the glob pattern. Most common use
    /// is `SENTINEL RESET *` to force a full topology re-discovery
    /// after a network split healed. The pattern is a glob, not a
    /// regex.
    pub fn sentinel_reset(&mut self, pattern: SharedString, cx: &mut Context<Self>) {
        if self.readonly() {
            self.emit_warning_notification("Read-only mode — sentinel ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let pattern_for_op = pattern.clone();
        self.spawn(
            ServerTask::SentinelReset,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut c = cmd("SENTINEL");
                c.arg("RESET").arg(pattern_for_op.as_ref());
                // RESET returns the integer count of masters reset on
                // each sentinel; we only care that the command
                // succeeded, so type as i64.
                let (_, _replies): (_, Vec<i64>) = client.query_async_masters(vec![c]).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(format!("SENTINEL RESET {pattern} sent").into(), "RESET".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL REMOVE master_name` — stop monitoring the named
    /// master across every sentinel in the quorum. Destructive —
    /// once removed, future reads of `SENTINEL MASTERS` won't
    /// include it. To re-add, the operator must `SENTINEL MONITOR`
    /// the master from scratch (config + quorum).
    pub fn sentinel_remove(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        if self.readonly() {
            self.emit_warning_notification("Read-only mode — sentinel ops blocked".into(), cx);
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let name_for_op = master_name.clone();
        self.spawn(
            ServerTask::SentinelRemove,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut c = cmd("SENTINEL");
                c.arg("REMOVE").arg(name_for_op.as_ref());
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![c]).await?;
                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        format!("SENTINEL REMOVE {master_name} sent").into(),
                        "REMOVE".into(),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }
}

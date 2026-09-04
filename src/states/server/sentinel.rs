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

//! Sentinel administration from the server state: what the sentinels
//! monitor (`SENTINEL MASTERS`, kept in `sentinel_masters`) and the
//! commands that change it — `MONITOR`, `SET`, `REMOVE`, `RESET`,
//! `FLUSHCONFIG`, `FAILOVER`, `CKQUORUM`.
//!
//! None of it touches the pooled client: that is a connection to the data
//! master the sentinels announced, where `SENTINEL` is an unknown command.
//! Every operation goes through `crate::connection::sentinel`, which dials
//! the sentinels themselves — the entry's seeds plus the peers they report
//! — and answers per sentinel, so a change that only part of the quorum
//! accepted says so. The view routes the destructive ones through a
//! confirm dialog first.
//!
//! All commands target by **master name**, the label from `sentinel.conf`;
//! `TopologyEntry::master_name` carries it from the parse layer.

use crate::connection::{
    Capability, SentinelMaster, SentinelReply, get_server, get_servers, save_servers, sentinel_ckquorum,
    sentinel_failover, sentinel_flushconfig, sentinel_masters, sentinel_monitor, sentinel_remove, sentinel_reset,
    sentinel_set, summarize_replies,
};
use crate::states::{GlobalEvent, ServerEvent, ServerTask, ZedisGlobalStore, ZedisServerState, i18n_topology};
use gpui::{SharedString, prelude::*};
use rust_i18n::t;
use tracing::error;

impl ZedisServerState {
    /// The monitored masters as the sentinels describe them (quorum, timing,
    /// down flags); empty until [`Self::load_sentinel_masters`] answers.
    pub fn sentinel_masters(&self) -> &[SentinelMaster] {
        &self.sentinel_masters
    }

    /// `SENTINEL MASTERS` from the first reachable sentinel; emits
    /// `SentinelInfoUpdated` when it lands.
    pub fn load_sentinel_masters(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        self.spawn(
            ServerTask::SentinelInfo,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_masters(&server).await?)
            },
            move |this, result, cx| {
                if let Ok(masters) = result {
                    this.sentinel_masters = masters;
                    cx.emit(ServerEvent::SentinelInfoUpdated);
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// Capability gate shared by every write: a read-only connection gets one
    /// notice and nothing is sent.
    fn sentinel_write_allowed(&mut self, cx: &mut Context<Self>) -> bool {
        if self.can(Capability::SentinelWrite) {
            return true;
        }
        self.emit_warning_notification(i18n_topology(cx, "sentinel_ops_readonly"), cx);
        false
    }

    fn locale(&self, cx: &Context<Self>) -> String {
        cx.global::<ZedisGlobalStore>().read(cx).locale().to_string()
    }

    /// One toast for a broadcast: every sentinel acknowledged, or which ones
    /// refused and why. Either way the monitored-master list and the
    /// heartbeat are refreshed, so the panel shows the new state at once.
    fn report_sentinel_replies(&mut self, command: &str, replies: &[SentinelReply], cx: &mut Context<Self>) {
        let locale = self.locale(cx);
        let (total, failed) = summarize_replies(replies);
        if failed.is_empty() {
            let message = t!(
                "topology.sentinel_op_ok",
                command = command,
                total = total,
                locale = &locale
            );
            self.emit_success_notification(message.to_string().into(), format!("SENTINEL {command}").into(), cx);
        } else {
            let message = t!(
                "topology.sentinel_op_partial",
                command = command,
                failed = failed.len(),
                total = total,
                errors = failed.join("; "),
                locale = &locale
            );
            self.emit_warning_notification(message.to_string().into(), cx);
        }
        self.load_sentinel_masters(cx);
        self.refresh_redis_info(cx);
    }

    /// `SENTINEL FAILOVER name` — force a failover on one sentinel; the
    /// quorum carries it out. For when a replica must take over now, without
    /// waiting for automatic detection.
    pub fn sentinel_failover(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        if !self.sentinel_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        self.spawn(
            ServerTask::SentinelFailover,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_failover(&server, &name).await?)
            },
            move |this, result, cx| {
                if result.is_ok() {
                    let locale = this.locale(cx);
                    let message = t!("topology.sentinel_failover_sent", name = master_name, locale = &locale);
                    this.emit_success_notification(message.to_string().into(), "SENTINEL FAILOVER".into(), cx);
                    this.load_sentinel_masters(cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL RESET pattern` on every sentinel — forget and re-discover
    /// the replicas and peer sentinels of the masters matching the glob;
    /// the usual repair after a network split healed.
    pub fn sentinel_reset(&mut self, pattern: SharedString, cx: &mut Context<Self>) {
        if !self.sentinel_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let pattern = pattern.to_string();
        self.spawn(
            ServerTask::SentinelReset,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_reset(&server, &pattern).await?)
            },
            move |this, result, cx| {
                if let Ok(replies) = result {
                    this.report_sentinel_replies("RESET", &replies, cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL REMOVE name` on every sentinel — stop monitoring the master.
    /// The master and its replicas keep running unwatched; bringing it back
    /// is a `MONITOR`.
    pub fn sentinel_remove(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        if !self.sentinel_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        self.spawn(
            ServerTask::SentinelRemove,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_remove(&server, &name).await?)
            },
            move |this, result, cx| {
                if let Ok(replies) = result {
                    this.report_sentinel_replies("REMOVE", &replies, cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL MONITOR name ip port quorum` on every sentinel — start
    /// watching a master.
    pub fn sentinel_monitor(
        &mut self,
        master_name: SharedString,
        ip: SharedString,
        port: u16,
        quorum: u32,
        cx: &mut Context<Self>,
    ) {
        if !self.sentinel_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        let ip = ip.to_string();
        self.spawn(
            ServerTask::SentinelMonitor,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_monitor(&server, &name, &ip, port, quorum).await?)
            },
            move |this, result, cx| {
                if let Ok(replies) = result {
                    this.report_sentinel_replies("MONITOR", &replies, cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL SET name option value` for each option, on every sentinel.
    pub fn sentinel_set(&mut self, master_name: SharedString, options: Vec<(String, String)>, cx: &mut Context<Self>) {
        if !self.sentinel_write_allowed(cx) || options.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        self.spawn(
            ServerTask::SentinelSet,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_set(&server, &name, &options).await?)
            },
            move |this, result, cx| {
                if let Ok(replies) = result {
                    this.report_sentinel_replies("SET", &replies, cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL CKQUORUM name` on every sentinel: each says whether, from
    /// where it stands, enough sentinels are reachable to authorize a
    /// failover. Read-only, so no capability gate.
    pub fn sentinel_ckquorum(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        self.spawn(
            ServerTask::SentinelCkquorum,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_ckquorum(&server, &name).await?)
            },
            move |this, result, cx| {
                let Ok(replies) = result else { return };
                let lines: Vec<String> = replies
                    .iter()
                    .map(|r| match &r.result {
                        Ok(reply) => format!("{}: {reply}", r.sentinel),
                        Err(e) => format!("{}: {e}", r.sentinel),
                    })
                    .collect();
                let message: SharedString = lines.join("\n").into();
                if replies.iter().all(|r| r.result.is_ok()) {
                    this.emit_success_notification(message, format!("SENTINEL CKQUORUM {master_name}").into(), cx);
                } else {
                    this.emit_warning_notification(message, cx);
                }
            },
            cx,
        );
    }

    /// `SENTINEL FLUSHCONFIG` on every sentinel — each rewrites its
    /// configuration file from its in-memory state.
    pub fn sentinel_flushconfig(&mut self, cx: &mut Context<Self>) {
        if !self.sentinel_write_allowed(cx) {
            return;
        }
        let server_id = self.server_id.clone();
        self.spawn(
            ServerTask::SentinelFlushConfig,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(sentinel_flushconfig(&server).await?)
            },
            move |this, result, cx| {
                if let Ok(replies) = result {
                    this.report_sentinel_replies("FLUSHCONFIG", &replies, cx);
                }
            },
            cx,
        );
    }

    /// Point the entry at another of the sentinel's masters: the master name
    /// is written into the saved connection, the pooled client dropped, and
    /// the server reloaded on the new master.
    pub fn switch_sentinel_master(&mut self, master_name: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let name = master_name.to_string();
        cx.spawn(async move |handle, cx| {
            let saved: Result<(), crate::error::Error> = cx
                .background_spawn({
                    let server_id = server_id.clone();
                    async move {
                        let mut servers = get_servers()?;
                        let Some(entry) = servers.iter_mut().find(|s| s.id == server_id) else {
                            return Ok(());
                        };
                        entry.master_name = Some(name);
                        save_servers(servers).await?;
                        Ok(())
                    }
                })
                .await;
            let _ = handle.update(cx, |this, cx| match saved {
                Ok(()) => {
                    cx.global::<ZedisGlobalStore>().clone().update(cx, |_state, cx| {
                        cx.emit(GlobalEvent::ServerListUpdated);
                    });
                    this.reconnect(cx);
                }
                Err(e) => {
                    error!(error = %e, "switching the sentinel master failed");
                    this.emit_error_notification(e.to_string().into(), cx);
                }
            });
        })
        .detach();
    }

    /// After a connect: the sentinel monitors several masters and the entry
    /// named none, so the first was taken — say which, once per connect.
    pub(crate) fn note_sentinel_master_choice(&mut self, cx: &mut Context<Self>) {
        let names = &self.nodes_description.sentinel_master_names;
        if names.len() < 2 {
            return;
        }
        let configured = get_server(&self.server_id)
            .ok()
            .and_then(|s| s.master_name)
            .is_some_and(|n| !n.is_empty());
        if configured {
            return;
        }
        let Some(current) = self
            .nodes_description
            .topology
            .first()
            .map(|m| m.master.master_name.clone())
        else {
            return;
        };
        let locale = self.locale(cx);
        let message = t!(
            "topology.sentinel_multi_master_notice",
            count = names.len(),
            name = current,
            locale = &locale
        );
        self.emit_warning_notification(message.to_string().into(), cx);
    }
}

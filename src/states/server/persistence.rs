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

//! Persistence admin ops (BGSAVE / BGREWRITEAOF).
//!
//! Both commands trigger a fork inside Redis — they are not destructive
//! on their own, but on a large dataset the fork can spike memory
//! (copy-on-write) and cause a latency hiccup. The view layer routes
//! user clicks through a confirm dialog; this module dispatches the
//! command to all masters and kicks an immediate `INFO` refresh so the
//! in-progress flag flips into the UI without waiting for the heartbeat.

use crate::connection::get_connection_manager;
use crate::states::{ServerTask, ZedisServerState, i18n_persistence};
use gpui::prelude::*;
use redis::cmd;

impl ZedisServerState {
    /// Fan-out `BGSAVE` to every master in the cluster (single-node
    /// counts as one master). The fork itself returns control to the
    /// caller immediately — the heavy work happens in the Redis child
    /// process — so we kick the next `refresh_redis_info` cycle to pick
    /// up `rdb_bgsave_in_progress:1` and flip the button state.
    pub fn bgsave(&mut self, cx: &mut Context<Self>) {
        // Read-only guard: never let the GUI issue admin commands when
        // the user explicitly locked the connection. The button is
        // already disabled in this state — this is the second layer of
        // defence for the keyboard/command-palette path.
        if self.readonly() {
            self.emit_warning_notification(i18n_persistence(cx, "readonly_blocked"), cx);
            return;
        }

        let server_id = self.server_id.clone();
        let db = self.db;
        self.spawn(
            ServerTask::Bgsave,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                // Type the response loosely — Redis returns "Background
                // saving started" but some forks/replicas can return
                // slightly different status strings. We only need it to
                // succeed; the user-visible state comes from the next
                // `INFO` poll.
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![cmd("BGSAVE")]).await?;
                Ok(())
            },
            |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        i18n_persistence(cx, "bgsave_started_message"),
                        i18n_persistence(cx, "bgsave_started_title"),
                        cx,
                    );
                    // Eager refresh so `rdb_bgsave_in_progress` flips
                    // true without waiting for the next 2s heartbeat.
                    this.refresh_redis_info(cx);
                }
                // Error path: `spawn` already records via add_error_message.
            },
            cx,
        );
    }

    /// Fan-out `BGREWRITEAOF`. Same shape as `bgsave` — UI gating
    /// (hidden when `!aof_enabled`, disabled when in progress) lives in
    /// the view; here we trust the caller and just dispatch.
    pub fn bgrewriteaof(&mut self, cx: &mut Context<Self>) {
        if self.readonly() {
            self.emit_warning_notification(i18n_persistence(cx, "readonly_blocked"), cx);
            return;
        }

        let server_id = self.server_id.clone();
        let db = self.db;
        self.spawn(
            ServerTask::Bgrewriteaof,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let (_, _replies): (_, Vec<String>) = client.query_async_masters(vec![cmd("BGREWRITEAOF")]).await?;
                Ok(())
            },
            |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        i18n_persistence(cx, "bgrewriteaof_started_message"),
                        i18n_persistence(cx, "bgrewriteaof_started_title"),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }
}

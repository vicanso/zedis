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

use crate::connection::{Capability, bgrewriteaof, bgsave, bgsave_cancel};
use crate::states::{ServerTask, ZedisServerState, i18n_persistence};
use gpui::prelude::*;

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
        if !self.can(Capability::PersistenceWrite) {
            self.emit_warning_notification(i18n_persistence(cx, "readonly_blocked"), cx);
            return;
        }

        let at = self.at();
        self.spawn(
            ServerTask::Bgsave,
            move || async move {
                // The reply is not read: the user-visible state comes from
                // the next `INFO` poll.
                Ok(bgsave(&at).await?)
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

    /// `BGSAVE CANCEL` on every master (Valkey 8.1+): the snapshot in
    /// progress is stopped and the previous RDB file stays. The panel
    /// offers it only where `floors::BGSAVE_CANCEL` holds.
    pub fn bgsave_cancel(&mut self, cx: &mut Context<Self>) {
        if !self.can(Capability::PersistenceWrite) {
            self.emit_warning_notification(i18n_persistence(cx, "readonly_blocked"), cx);
            return;
        }
        let at = self.at();
        self.spawn(
            ServerTask::BgsaveCancel,
            move || async move { Ok(bgsave_cancel(&at).await?) },
            |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification(
                        i18n_persistence(cx, "bgsave_cancelled_message"),
                        i18n_persistence(cx, "bgsave_cancelled_title"),
                        cx,
                    );
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// Fan-out `BGREWRITEAOF`. Same shape as `bgsave` — UI gating
    /// (hidden when `!aof_enabled`, disabled when in progress) lives in
    /// the view; here we trust the caller and just dispatch.
    pub fn bgrewriteaof(&mut self, cx: &mut Context<Self>) {
        if !self.can(Capability::PersistenceWrite) {
            self.emit_warning_notification(i18n_persistence(cx, "readonly_blocked"), cx);
            return;
        }

        let at = self.at();
        self.spawn(
            ServerTask::Bgrewriteaof,
            move || async move { Ok(bgrewriteaof(&at).await?) },
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

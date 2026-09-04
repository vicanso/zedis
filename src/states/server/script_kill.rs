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

//! Stopping a runaway script from the state: `SCRIPT KILL` / `FUNCTION
//! KILL` over `crate::connection::kill_running` (fresh connections that a
//! `BUSY` server still answers — the pooled client is stuck behind it),
//! then one toast that says what each node answered. Offered by the
//! status bar while the link is down with `BUSY` as the reason, and by
//! the Lua script and Functions panels.

use crate::connection::{KillOutcome, KillTarget, get_server, kill_running};
use crate::states::{ServerTask, ZedisGlobalStore, ZedisServerState};
use gpui::prelude::*;
use rust_i18n::t;

impl ZedisServerState {
    /// Send the kill to every data node of this entry and report. No
    /// capability gate: it changes no data, and a read-only ACL user gets
    /// the server's `NOPERM` in the toast.
    pub fn kill_running_script(&mut self, target: KillTarget, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        self.spawn(
            ServerTask::KillScript,
            move || async move {
                let server = get_server(&server_id)?;
                Ok(kill_running(&server, target).await?)
            },
            move |this, result, cx| {
                let Ok(replies) = result else { return };
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
                let nodes = |outcome: fn(&KillOutcome) -> bool| -> Vec<&str> {
                    replies
                        .iter()
                        .filter(|r| outcome(&r.outcome))
                        .map(|r| r.node.as_str())
                        .collect()
                };
                let killed = nodes(|o| matches!(o, KillOutcome::Killed));
                let unkillable = nodes(|o| matches!(o, KillOutcome::Unkillable));
                let idle = nodes(|o| matches!(o, KillOutcome::NothingRunning));
                let failed: Vec<String> = replies
                    .iter()
                    .filter_map(|r| match &r.outcome {
                        KillOutcome::Failed(e) => Some(format!("{}: {e}", r.node)),
                        _ => None,
                    })
                    .collect();
                let title = target.command();
                if !killed.is_empty() {
                    let message = t!("common.script_killed", node = killed.join(", "), locale = &locale);
                    this.emit_success_notification(message.to_string().into(), title.into(), cx);
                    // The heartbeat was backing off behind BUSY: let it look
                    // again now, so the link reads Connected without waiting
                    // out the retry window.
                    this.heartbeat_retry_at = None;
                    this.refresh_redis_info(cx);
                } else if !unkillable.is_empty() {
                    let message = t!(
                        "common.script_unkillable",
                        node = unkillable.join(", "),
                        locale = &locale
                    );
                    this.emit_warning_notification(message.to_string().into(), cx);
                } else if !failed.is_empty() {
                    let message = t!(
                        "common.script_kill_failed",
                        errors = failed.join("; "),
                        locale = &locale
                    );
                    this.emit_warning_notification(message.to_string().into(), cx);
                } else {
                    let message = t!("common.script_not_running", nodes = idle.join(", "), locale = &locale);
                    this.emit_info_notification(message.to_string().into(), cx);
                }
            },
            cx,
        );
    }
}

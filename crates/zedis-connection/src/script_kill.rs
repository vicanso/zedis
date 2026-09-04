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

//! `SCRIPT KILL` / `FUNCTION KILL` for a server stuck in a runaway script.
//!
//! Past `busy-reply-threshold` a server answers `BUSY` to everything but
//! `AUTH`, `SCRIPT KILL`, `FUNCTION KILL`, `FUNCTION STATS` and `SHUTDOWN`.
//! That includes the pooled client's every command, the heartbeat, and the
//! handshake of any new connection Zedis would normally open — `SELECT`,
//! topology detection — so the kill has to travel on a connection that
//! sends only what a busy server still takes: dialled on db 0 (no `SELECT`),
//! `AUTH` from the entry, the `CLIENT` niceties best-effort. Every data
//! node the entry can name gets the command (on a cluster any master may
//! be the busy one); a node with nothing running answers `NOTBUSY`, which
//! is not a failure. A script that has already written cannot be killed
//! (`UNKILLABLE`): only `SHUTDOWN NOSAVE` ends it, and that is left to the
//! operator on purpose.

use crate::async_connection::open_single_connection;
use crate::config::{RedisServer, SERVER_TYPE_SENTINEL};
use crate::error::Error;
use crate::manager::get_connection_manager;
use crate::sentinel::sentinel_masters;
use futures::future::join_all;
use redis::cmd;
use zedis_core::string::format_host_port;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Which engine's running code to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillTarget {
    /// `SCRIPT KILL` — an `EVAL` / `EVALSHA` script.
    Script,
    /// `FUNCTION KILL` — a `FCALL` function.
    Function,
}

impl KillTarget {
    pub fn command(self) -> &'static str {
        match self {
            KillTarget::Script => "SCRIPT KILL",
            KillTarget::Function => "FUNCTION KILL",
        }
    }
}

/// What one node answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// `OK`: the script was stopped.
    Killed,
    /// `NOTBUSY`: nothing was running there.
    NothingRunning,
    /// `UNKILLABLE`: the script has written already; only `SHUTDOWN NOSAVE`
    /// stops it.
    Unkillable,
    /// The node could not be reached, or refused (`NOPERM`, …).
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct KillReply {
    /// `host:port` of the data node.
    pub node: String,
    pub outcome: KillOutcome,
}

/// Send the kill to every data node of `server` and report per node.
pub async fn kill_running(server: &RedisServer, target: KillTarget) -> Result<Vec<KillReply>> {
    let nodes = kill_targets(server).await?;
    let runs = nodes.iter().map(|node| async move {
        let addr = format_host_port(&node.host, node.port);
        let outcome = match open_single_connection(node, 0, false).await {
            Err(e) => KillOutcome::Failed(e.to_string()),
            Ok(mut conn) => {
                let (command, sub) = match target {
                    KillTarget::Script => ("SCRIPT", "KILL"),
                    KillTarget::Function => ("FUNCTION", "KILL"),
                };
                classify_reply(cmd(command).arg(sub).query_async::<String>(&mut conn).await)
            }
        };
        KillReply { node: addr, outcome }
    });
    Ok(join_all(runs).await)
}

/// The nodes to dial: the pooled client's masters when one is cached (no
/// dial needed for that), else what the entry itself can name — the
/// standalone address, the masters a sentinel announces, or a cluster
/// entry's seeds.
async fn kill_targets(server: &RedisServer) -> Result<Vec<RedisServer>> {
    if let Some(masters) = get_connection_manager().cached_master_servers(&server.id, 0) {
        return Ok(masters);
    }
    if server.server_type == Some(SERVER_TYPE_SENTINEL) {
        let masters = sentinel_masters(server).await?;
        let wanted = server.master_name.as_deref().filter(|n| !n.is_empty());
        return Ok(masters
            .into_iter()
            .filter(|m| wanted.is_none_or(|w| w == m.name))
            .map(|m| {
                let mut node = server.clone();
                node.host = m.ip;
                node.port = m.port;
                node
            })
            .collect());
    }
    Ok(server
        .seed_endpoints()
        .into_iter()
        .map(|(host, port)| {
            let mut node = server.clone();
            node.host = host;
            node.port = port;
            node
        })
        .collect())
}

/// The server's reply as an outcome; the error prefixes are Redis's own.
pub fn classify_reply(reply: std::result::Result<String, redis::RedisError>) -> KillOutcome {
    match reply {
        Ok(_) => KillOutcome::Killed,
        Err(e) => {
            let text = e.to_string();
            let code = e.code().unwrap_or_default();
            if code == "NOTBUSY" || text.contains("NOTBUSY") {
                KillOutcome::NothingRunning
            } else if code == "UNKILLABLE" || text.contains("UNKILLABLE") {
                KillOutcome::Unkillable
            } else {
                KillOutcome::Failed(text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::{ErrorKind, RedisError};

    /// A server error carrying a reply code Redis itself does not name, the
    /// way `-NOTBUSY …` / `-UNKILLABLE …` come back through redis-rs.
    fn server_error(code: &'static str, detail: &str) -> RedisError {
        RedisError::from((ErrorKind::Extension, code, detail.to_string()))
    }

    #[test]
    fn replies_are_read_by_their_error_code() {
        assert_eq!(classify_reply(Ok("OK".into())), KillOutcome::Killed);
        assert_eq!(
            classify_reply(Err(server_error("NOTBUSY", "No scripts in execution right now."))),
            KillOutcome::NothingRunning
        );
        assert_eq!(
            classify_reply(Err(server_error(
                "UNKILLABLE",
                "Sorry the script already executed write commands against the dataset."
            ))),
            KillOutcome::Unkillable
        );
        assert!(matches!(
            classify_reply(Err(server_error("NOPERM", "this user has no permissions"))),
            KillOutcome::Failed(text) if text.contains("NOPERM")
        ));
    }

    #[test]
    fn targets_name_their_command() {
        assert_eq!(KillTarget::Script.command(), "SCRIPT KILL");
        assert_eq!(KillTarget::Function.command(), "FUNCTION KILL");
    }
}

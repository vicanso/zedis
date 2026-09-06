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

//! Valkey 8.1's `COMMANDLOG`: the slow log generalised into three logs of
//! the same entry shape — `SLOW` (what `SLOWLOG` still answers, on every
//! server), `LARGE-REQUEST` and `LARGE-REPLY` (commands whose request or
//! reply crossed `commandlog-request-larger-than` /
//! `commandlog-reply-larger-than` bytes). Redis has no equivalent:
//! [`floors::COMMANDLOG`](crate::floors::COMMANDLOG) is the first
//! Valkey-only floor. An entry's third field is microseconds in the slow
//! log and bytes in the size logs — `SlowLogEntry::amount` keeps it raw
//! next to the `duration` the slow log reads.

use super::{RedisClient, SlowLogEntry};
use crate::error::Error;
use redis::cmd;
use std::cmp::Reverse;

type Result<T, E = Error> = std::result::Result<T, E>;

/// One of the three `COMMANDLOG` logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandLogKind {
    /// Execution time over `commandlog-execution-slower-than` — the slow
    /// log, `SLOWLOG` on every server.
    #[default]
    Slow,
    /// Request over `commandlog-request-larger-than` bytes.
    LargeRequest,
    /// Reply over `commandlog-reply-larger-than` bytes.
    LargeReply,
}

impl CommandLogKind {
    pub const ALL: [CommandLogKind; 3] = [Self::Slow, Self::LargeRequest, Self::LargeReply];

    /// The `<type>` token of `COMMANDLOG GET / LEN / RESET`.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Slow => "SLOW",
            Self::LargeRequest => "LARGE-REQUEST",
            Self::LargeReply => "LARGE-REPLY",
        }
    }

    /// A key-safe name (`slow`, `large_request`, `large_reply`) for i18n
    /// keys and file names.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::LargeRequest => "large_request",
            Self::LargeReply => "large_reply",
        }
    }

    /// The slow log — `SLOWLOG` on every server, so no floor applies.
    pub const fn is_slow(self) -> bool {
        matches!(self, Self::Slow)
    }
}

impl RedisClient {
    /// The entries of one log on every master, newest first. The slow log
    /// goes through `SLOWLOG GET` (every server); a size log through
    /// `COMMANDLOG GET -1 <type>` — every entry, the log's own `max-len`
    /// caps it.
    pub async fn get_command_logs(&self, kind: CommandLogKind) -> Result<Vec<SlowLogEntry>> {
        if kind.is_slow() {
            return self.get_slow_logs().await;
        }
        let (_, per_node): (_, Vec<Vec<SlowLogEntry>>) = self
            .query_async_masters(vec![cmd("COMMANDLOG").arg("GET").arg(-1).arg(kind.wire()).clone()])
            .await?;
        let mut logs: Vec<SlowLogEntry> = per_node.into_iter().flatten().collect();
        logs.sort_unstable_by_key(|entry| Reverse(entry.timestamp));
        Ok(logs)
    }

    /// `COMMANDLOG RESET <type>` — or `SLOWLOG RESET` for the slow log —
    /// on every master.
    pub async fn commandlog_reset(&self, kind: CommandLogKind) -> Result<()> {
        if kind.is_slow() {
            return self.slowlog_reset().await;
        }
        let (_, _statuses): (_, Vec<String>) = self
            .query_async_masters(vec![cmd("COMMANDLOG").arg("RESET").arg(kind.wire()).clone()])
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_own_wire_token_and_name() {
        let wires: Vec<&str> = CommandLogKind::ALL.iter().map(|k| k.wire()).collect();
        let names: Vec<&str> = CommandLogKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(wires, ["SLOW", "LARGE-REQUEST", "LARGE-REPLY"]);
        assert_eq!(names, ["slow", "large_request", "large_reply"]);
        assert!(CommandLogKind::Slow.is_slow());
        assert!(!CommandLogKind::LargeReply.is_slow());
    }
}

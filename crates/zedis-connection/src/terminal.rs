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

//! The terminal's side of the connection layer: a connection of its own, and
//! the one reply in the app that is kept *raw*.
//!
//! Every other operation of this crate answers with a struct a view can draw.
//! The terminal cannot: its job is to show whatever the server said to
//! whatever the user typed, and to show it again in another format when asked.
//! So it holds a [`TerminalReply`] — the value, opaque — and asks it the few
//! questions a transcript needs (`is_ok`, `is_queued`, which db a `SELECT`
//! picked) instead of matching on a `redis::Value` itself (ADR 10).

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::conn::RedisAsyncConn;
use crate::error::{ConnectionErrorKind, Error};
use crate::reply_format::{ReplyFormat, format_exec, format_reply};
use crate::server_db::ServerDb;
use futures::lock::Mutex;
use redis::{Value, cmd, parse_redis_value};
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// The server's answer to one typed line, with the command that produced it:
/// rendering needs the command to tell a hash or `WITHSCORES` pair list from
/// a plain list (RESP2 flattens both).
#[derive(Debug, Clone)]
pub struct TerminalReply {
    cmd: String,
    args: Vec<String>,
    value: Value,
}

impl TerminalReply {
    /// A reply from its RESP encoding (`+OK\r\n`, `$-1\r\n`, …) — what a
    /// test or a fixture writes instead of naming a `redis::Value`.
    pub fn from_resp(cmd: &str, args: &[String], resp: &[u8]) -> Result<Self> {
        Ok(Self {
            cmd: cmd.to_string(),
            args: args.to_vec(),
            value: parse_redis_value(resp)?,
        })
    }

    /// The command name as it was typed.
    pub fn command(&self) -> &str {
        &self.cmd
    }

    /// The reply as text in `format`.
    pub fn render(&self, format: ReplyFormat) -> String {
        format_reply(&self.cmd, &self.args, &self.value, format)
    }

    /// `+OK`.
    pub fn is_ok(&self) -> bool {
        matches!(self.value, Value::Okay)
    }

    /// Nil — a missing key, or an `EXEC` a `WATCH` aborted.
    pub fn is_nil(&self) -> bool {
        matches!(self.value, Value::Nil)
    }

    /// `+QUEUED`: the line joined an open `MULTI`.
    pub fn is_queued(&self) -> bool {
        matches!(&self.value, Value::SimpleString(s) if s == "QUEUED")
    }

    /// The replies of an `EXEC`, one per queued command; `None` when the
    /// reply is not an array.
    pub fn exec_replies(&self) -> Option<ExecReplies> {
        match &self.value {
            Value::Array(replies) => Some(ExecReplies(replies.clone())),
            _ => None,
        }
    }

    /// The db a successful `SELECT <n>` moved the connection to, else `None`.
    /// Only the plain one-argument form counts: anything else Redis accepted
    /// was not a database switch.
    pub fn selected_db(&self) -> Option<usize> {
        if !self.cmd.eq_ignore_ascii_case("SELECT") || !self.is_ok() {
            return None;
        }
        match self.args.as_slice() {
            [db] => db.parse().ok(),
            _ => None,
        }
    }
}

/// What `EXEC` answered, to be laid out against the commands it ran.
#[derive(Debug, Clone)]
pub struct ExecReplies(Vec<Value>);

impl ExecReplies {
    /// One row per queued command, in `format`.
    pub fn render(&self, commands: &[String], format: ReplyFormat) -> String {
        format_exec(commands, &self.0, format)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The terminal's connection, opened on first use and kept.
///
/// It is a dedicated one (ADR 4): `SELECT` and `MULTI` typed in a terminal
/// change the state of whichever connection carries them, and on the pooled
/// one that state is shared with the key tree. Cloning the session shares the
/// connection, so every line — and every later batch — sees the same state:
/// the db a `SELECT` picked, a `MULTI` still open. A fresh session
/// (`TerminalSession::default()`) is how the terminal forgets all of that on
/// a server switch.
#[derive(Clone, Default)]
pub struct TerminalSession {
    conn: Arc<Mutex<Option<RedisAsyncConn>>>,
}

impl TerminalSession {
    /// Whether an error of this kind means the terminal's connection is gone
    /// and the next line must reopen it — by kind, so the app can ask about
    /// its own error type. Mirrors what the pool does with its own client:
    /// a dropped link, refused connect or broken tunnel discards the
    /// connection; a response timeout does not — the multiplexed connection
    /// stays in step after one, and a dead link surfaces as a network error
    /// on the next line anyway.
    pub fn drops_link(kind: ConnectionErrorKind) -> bool {
        use ConnectionErrorKind as K;
        matches!(kind, K::Network | K::Tls | K::Tunnel)
    }

    /// Send one command as typed. The connection is opened against `at` the
    /// first time and reused after; an error that means it is gone
    /// ([`Self::drops_link`]) forgets it, so the next line reconnects
    /// instead of failing the same way.
    pub async fn run(&self, at: &ServerDb, cmd_name: &str, args: &[String]) -> Result<TerminalReply> {
        // The held connection is shared by every line; the confirmation is
        // this line's, so it goes on this run's clone and not in the slot.
        let mut conn = self
            .connection(at)
            .await?
            .with_confirmation(at.confirmation().map(str::to_string));
        match cmd(cmd_name).arg(args).query_async::<Value>(&mut conn).await {
            Ok(value) => Ok(TerminalReply {
                cmd: cmd_name.to_string(),
                args: args.to_vec(),
                value,
            }),
            Err(e) => {
                let e = Error::from(e);
                if Self::drops_link(e.connection_kind()) {
                    self.conn.lock().await.take();
                }
                Err(e)
            }
        }
    }

    async fn connection(&self, at: &ServerDb) -> Result<RedisAsyncConn> {
        let mut slot = self.conn.lock().await;
        if let Some(conn) = slot.as_ref() {
            return Ok(conn.clone());
        }
        let conn = at.dedicated_connection().await?;
        *slot = Some(conn.clone());
        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn reply(cmd: &str, list: &[&str], resp: &[u8]) -> TerminalReply {
        TerminalReply::from_resp(cmd, &args(list), resp).expect("valid RESP")
    }

    #[test]
    fn selected_db_reads_only_a_successful_plain_select() {
        assert_eq!(reply("SELECT", &["3"], b"+OK\r\n").selected_db(), Some(3));
        assert_eq!(reply("select", &["0"], b"+OK\r\n").selected_db(), Some(0));
        // Refused by the server: the connection did not move.
        assert_eq!(reply("SELECT", &["99"], b"$-1\r\n").selected_db(), None);
        // Another command that happens to answer OK.
        assert_eq!(reply("GET", &["3"], b"+OK\r\n").selected_db(), None);
        assert_eq!(reply("SELECT", &[], b"+OK\r\n").selected_db(), None);
        assert_eq!(reply("SELECT", &["3", "x"], b"+OK\r\n").selected_db(), None);
        assert_eq!(reply("SELECT", &["three"], b"+OK\r\n").selected_db(), None);
    }

    #[test]
    fn a_reply_answers_what_a_transcript_asks_of_it() {
        let ok = reply("MULTI", &[], b"+OK\r\n");
        assert!(ok.is_ok() && !ok.is_nil() && !ok.is_queued());
        assert_eq!(ok.command(), "MULTI");
        assert!(ok.exec_replies().is_none());

        let queued = reply("SET", &["k", "v"], b"+QUEUED\r\n");
        assert!(queued.is_queued() && !queued.is_ok());

        let aborted = reply("EXEC", &[], b"*-1\r\n");
        assert!(aborted.is_nil() && aborted.exec_replies().is_none());

        let exec = reply("EXEC", &[], b"*2\r\n+OK\r\n:2\r\n");
        let replies = exec.exec_replies().expect("an array");
        assert_eq!(replies.len(), 2);
        let text = replies.render(&args(&["SET k v", "INCR n"]), ReplyFormat::Text);
        assert!(text.contains("SET k v") && text.contains("INCR n"), "{text}");

        assert!(TerminalReply::from_resp("GET", &[], b"not resp").is_err());
    }
}

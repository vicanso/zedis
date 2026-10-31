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

//! The Redis Lua debugger (LDB, `SCRIPT DEBUG`), as a session a view can
//! drive: start a script, step, continue, set breakpoints, evaluate, abort.
//!
//! LDB speaks over the connection that sent the `EVAL`: every debugger
//! command (`s`, `c`, `b 3`, `p`, `e …`, `r …`, `a`) is answered with one
//! array of status lines (`* Stopped at 3, stop reason = break point`,
//! `<value> n = 1`, `<redis> GET k`, …) while the script is paused. The step
//! that finishes it is answered with *two* replies — the log ending in
//! `<endsession>`, then the script's own return value — after which the
//! server closes the connection (in both `YES` and `SYNC` mode).
//!
//! Two things follow. The reply nobody asked for is the one that matters,
//! and redis-rs's multiplexed connection drops such a reply — so the session
//! runs on a [`RawConnection`], which reads the second reply when the first
//! says the session ended. And commands are never pipelined while a script
//! is paused: LDB reads the socket itself, so bytes that arrived with an
//! earlier command sit in the server's query buffer where it never looks,
//! and the session hangs. Each command is written alone, then `p` alone for
//! the locals at the new stop.
//!
//! The session owns its connection (like the terminal, ADR 4): LDB blocks it
//! for the whole session. Dropping the session closes the connection, which
//! makes the server abort the script — in `SYNC` mode the server stops being
//! blocked, in forked mode the child exits.
//!
//! Desktop only: the browser reaches Redis one request at a time through the
//! bridge, which has no connection to hold a session on.

use crate::error::Error;
use crate::lua_script::format_value;
use crate::raw_connection::RawConnection;
use crate::server_db::ServerDb;
use redis::{RedisError, Value, cmd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

type Result<T, E = Error> = std::result::Result<T, E>;

/// What the server marks the end of a session with.
const END_SESSION: &str = "<endsession>";

/// How changes made by the debugged script are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugMode {
    /// `SCRIPT DEBUG YES`: the server forks, the script runs in the child,
    /// and every change it makes is thrown away at the end. The server keeps
    /// serving other clients meanwhile.
    #[default]
    Forked,
    /// `SCRIPT DEBUG SYNC`: the script runs in the server itself, keeps its
    /// changes, and blocks every other client for the whole session.
    Sync,
}

impl DebugMode {
    fn word(self) -> &'static str {
        match self {
            DebugMode::Forked => "YES",
            DebugMode::Sync => "SYNC",
        }
    }
}

/// One debugger command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugCommand {
    /// Run to the next line (`step`; LDB's `next` is the same).
    Step,
    /// Run to the next breakpoint or the end.
    Continue,
    /// Stop the script with an error; in `SYNC` mode its changes so far stay.
    Abort,
    /// Add a breakpoint on a 1-based source line.
    Break(u32),
    /// Remove the breakpoint on a line.
    Unbreak(u32),
    /// Remove every breakpoint.
    ClearBreaks,
    /// Evaluate a Lua expression in the paused script's context.
    Eval(String),
    /// Run a Redis command from inside the script (`redis.call`).
    Redis(Vec<String>),
}

impl DebugCommand {
    fn words(&self) -> Vec<String> {
        match self {
            DebugCommand::Step => vec!["s".to_string()],
            DebugCommand::Continue => vec!["c".to_string()],
            DebugCommand::Abort => vec!["a".to_string()],
            DebugCommand::Break(line) => vec!["b".to_string(), line.to_string()],
            DebugCommand::Unbreak(line) => vec!["b".to_string(), format!("-{line}")],
            DebugCommand::ClearBreaks => vec!["b".to_string(), "0".to_string()],
            DebugCommand::Eval(code) => vec!["e".to_string(), code.clone()],
            DebugCommand::Redis(args) => {
                let mut words = vec!["r".to_string()];
                words.extend(args.iter().cloned());
                words
            }
        }
    }
}

/// One line of debugger output worth showing, by what produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugLine {
    /// `redis.debug(…)` called by the script.
    Debug(String),
    /// A Redis command the script (or an `r` command) sent.
    RedisCall(String),
    /// The reply to that command.
    RedisReply(String),
    /// The value of an `e` expression.
    RetVal(String),
    /// The debugger refused something.
    Error(String),
    /// Anything else the debugger said ("All breakpoints removed.").
    Info(String),
}

/// A local variable at the current stop, as LDB prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaLocal {
    pub name: String,
    pub value: String,
}

/// How the script ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugEnd {
    /// The return value formatted for display, or the error message.
    pub result: String,
    pub error: bool,
}

/// What one command led to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugStep {
    /// Output lines, in order. Source listings are left out: the view draws
    /// the source itself.
    pub log: Vec<DebugLine>,
    /// The 1-based line the script is paused before, when a command moved it.
    pub stopped_at: Option<u32>,
    /// Why it stopped there ("step over", "break point", …).
    pub stop_reason: Option<String>,
    /// Locals at the current stop; empty once the script ended.
    pub locals: Vec<LuaLocal>,
    /// `Some` once the script finished, failed or was aborted: the session
    /// is over and its connection closed.
    pub end: Option<DebugEnd>,
}

/// A running debugger session. Cloning shares it; every command waits for
/// the one before it.
#[derive(Clone)]
pub struct LuaDebugSession {
    conn: RawConnection,
    live: Arc<AtomicBool>,
}

impl LuaDebugSession {
    /// Start debugging `code` with `keys` / `args`, stopped before its first
    /// line, with `breakpoints` (1-based lines) already set.
    ///
    /// The step answered here may already carry an `end`: a script that does
    /// not compile ends before its first line.
    pub async fn start(
        at: &ServerDb,
        code: &str,
        keys: &[String],
        args: &[String],
        breakpoints: &[u32],
        mode: DebugMode,
    ) -> Result<(Self, DebugStep)> {
        let Some(conn) = at.raw_connection().await? else {
            return Err(Error::Invalid {
                message: "the Lua debugger needs a single-node connection; it is not available on a cluster"
                    .to_string(),
            });
        };
        let session = Self {
            conn,
            live: Arc::new(AtomicBool::new(true)),
        };
        let mut debug = cmd("SCRIPT");
        debug.arg("DEBUG").arg(mode.word());
        if let Value::ServerError(error) = session.one(debug).await? {
            session.finish().await;
            return Err(RedisError::from(error).into());
        }

        let mut eval = cmd("EVAL");
        eval.arg(code).arg(keys.len()).arg(keys).arg(args);
        let mut step = session.after(eval).await?;
        if step.end.is_none() {
            for line in breakpoints {
                let reply = session.one(ldb_command(&DebugCommand::Break(*line).words())).await?;
                // Only a refusal is worth keeping from a breakpoint's reply.
                step.log
                    .extend(parse_lines(&status_lines(&reply)).log.into_iter().filter(is_refusal));
            }
            step.locals = session.locals().await?;
        }
        Ok((session, step))
    }

    /// Send one command and read what it led to. After a step whose `end`
    /// is `Some`, the session is over and every later call fails.
    pub async fn send(&self, command: &DebugCommand) -> Result<DebugStep> {
        if !self.is_live() {
            return Err(Error::Invalid {
                message: "the debugging session has ended".to_string(),
            });
        }
        let mut step = self.after(ldb_command(&command.words())).await?;
        if step.end.is_none() {
            step.locals = self.locals().await?;
        }
        Ok(step)
    }

    /// Whether the session can take another command.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Send `command` and read the step it led to, the script's result
    /// included when the session ended.
    async fn after(&self, command: redis::Cmd) -> Result<DebugStep> {
        let first = self.one(command).await?;
        // Refused outside a session (the `EVAL` never started one): that
        // error is how the script ended.
        if let Value::ServerError(error) = first {
            self.finish().await;
            return Ok(DebugStep {
                end: Some(end_from(Value::ServerError(error))),
                ..DebugStep::default()
            });
        }
        let lines = status_lines(&first);
        let mut step = parse_lines(&lines);
        if lines.iter().any(|line| line == END_SESSION) {
            let result = self.conn.read_reply().await;
            self.finish().await;
            step.end = Some(end_from(result?));
            step.stopped_at = None;
            step.stop_reason = None;
        }
        Ok(step)
    }

    async fn locals(&self) -> Result<Vec<LuaLocal>> {
        let reply = self.one(ldb_command(&["p".to_string()])).await?;
        Ok(parse_lines(&status_lines(&reply)).locals)
    }

    /// One command, one reply. A failure leaves nothing to read: the
    /// session is over.
    async fn one(&self, command: redis::Cmd) -> Result<Value> {
        match self.conn.request(&[command], 1).await {
            Ok(mut replies) => replies.pop().ok_or_else(|| Error::Invalid {
                message: "the debugger sent no reply".to_string(),
            }),
            Err(e) => {
                self.finish().await;
                Err(e)
            }
        }
    }

    async fn finish(&self) {
        self.live.store(false, Ordering::Release);
        self.conn.close().await;
    }
}

/// A debugger command, built from its words. The words (`s`, `c`, `b 3`, …)
/// are what LDB reads off the connection while a script is paused, not Redis
/// commands: the server answers them only inside a session.
fn ldb_command(words: &[String]) -> redis::Cmd {
    let mut iter = words.iter();
    let mut c = cmd(iter.next().map(String::as_str).unwrap_or("p"));
    for word in iter {
        c.arg(word.as_str());
    }
    c
}

/// The script's reply as the end of the session.
fn end_from(value: Value) -> DebugEnd {
    match value {
        Value::ServerError(error) => DebugEnd {
            result: RedisError::from(error).to_string(),
            error: true,
        },
        value => DebugEnd {
            result: format_value(&value),
            error: false,
        },
    }
}

/// The status lines of one LDB reply. A lone status (not an array) counts
/// as one line.
fn status_lines(value: &Value) -> Vec<String> {
    fn line(value: &Value) -> Option<String> {
        match value {
            Value::SimpleString(s) => Some(s.clone()),
            Value::Okay => Some("OK".to_string()),
            Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            Value::VerbatimString { text, .. } => Some(text.clone()),
            _ => None,
        }
    }
    match value {
        Value::Array(items) => items.iter().filter_map(line).collect(),
        other => line(other).into_iter().collect(),
    }
}

/// Read LDB's status lines into a step (no `end`: that needs the next reply).
fn parse_lines(lines: &[String]) -> DebugStep {
    let mut step = DebugStep::default();
    for raw in lines {
        let line = raw.as_str();
        if line == END_SESSION || source_listing_line(line).is_some() {
            continue;
        }
        if let Some((at, reason)) = parse_stop(line) {
            step.stopped_at = Some(at);
            step.stop_reason = Some(reason);
        } else if let Some(rest) = line.strip_prefix("<value> ") {
            let (name, value) = rest.split_once(" = ").unwrap_or((rest, ""));
            step.locals.push(LuaLocal {
                name: name.to_string(),
                value: value.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("<debug> ") {
            step.log.push(DebugLine::Debug(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix("<redis> ") {
            step.log.push(DebugLine::RedisCall(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix("<reply> ") {
            step.log.push(DebugLine::RedisReply(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix("<retval> ") {
            step.log.push(DebugLine::RetVal(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix("<error> ") {
            step.log.push(DebugLine::Error(rest.to_string()));
        } else if line == "No local variables in the current context." {
            // An empty locals list says this already.
        } else {
            step.log.push(DebugLine::Info(line.to_string()));
        }
    }
    step
}

fn is_refusal(line: &DebugLine) -> bool {
    matches!(line, DebugLine::Error(_) | DebugLine::Info(_))
}

/// `* Stopped at 3, stop reason = break point` → `(3, "break point")`.
fn parse_stop(line: &str) -> Option<(u32, String)> {
    let rest = line.strip_prefix("* Stopped at ")?;
    let (number, reason) = rest.split_once(", stop reason = ")?;
    Some((number.trim().parse().ok()?, reason.to_string()))
}

/// A line of a source listing (`-> 3   x = 1`, `  #4   y = 2`): LDB writes
/// a two-character current-line marker, a breakpoint marker, then the line
/// number. Answers the line number.
fn source_listing_line(line: &str) -> Option<u32> {
    let marker = line.get(..2)?;
    if marker != "->" && marker != "  " {
        return None;
    }
    let rest = line.get(2..)?;
    let rest = rest.strip_prefix('#').or_else(|| rest.strip_prefix(' '))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    // The number is padded to three columns and followed by a space.
    rest.get(digits.len()..)?.starts_with(' ').then_some(())?;
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn statuses(list: &[&str]) -> Value {
        Value::Array(list.iter().map(|s| Value::SimpleString(s.to_string())).collect())
    }

    #[test]
    fn a_stop_names_its_line_and_reason() {
        let step = parse_lines(&lines(&[
            "* Stopped at 3, stop reason = break point",
            "->#3   redis.debug('hello', a)",
        ]));
        assert_eq!(step.stopped_at, Some(3));
        assert_eq!(step.stop_reason.as_deref(), Some("break point"));
        assert!(step.log.is_empty(), "the listing line is not output: {:?}", step.log);
    }

    #[test]
    fn source_listings_are_recognised_with_every_marker() {
        assert_eq!(source_listing_line("-> 1   local a = 1"), Some(1));
        assert_eq!(source_listing_line("   2   local b = 2"), Some(2));
        assert_eq!(source_listing_line("  #3   redis.debug(a)"), Some(3));
        assert_eq!(source_listing_line("->#12  return n"), Some(12));
        assert_eq!(source_listing_line("All breakpoints removed."), None);
        assert_eq!(source_listing_line("<value> n = 1"), None);
        assert_eq!(source_listing_line("  no number here"), None);
    }

    #[test]
    fn locals_keep_their_names_and_values() {
        let step = parse_lines(&lines(&[
            "<value> t = {1; 2}",
            "<value> (for index) = 1",
            "<value> s = \"a = b\"",
        ]));
        assert_eq!(
            step.locals,
            vec![
                LuaLocal {
                    name: "t".into(),
                    value: "{1; 2}".into()
                },
                LuaLocal {
                    name: "(for index)".into(),
                    value: "1".into()
                },
                LuaLocal {
                    name: "s".into(),
                    value: "\"a = b\"".into()
                },
            ]
        );
        assert!(
            parse_lines(&lines(&["No local variables in the current context."]))
                .log
                .is_empty()
        );
    }

    #[test]
    fn output_lines_are_sorted_by_what_produced_them() {
        let step = parse_lines(&lines(&[
            "<debug> line 3: \"hello\", 1",
            "<redis> GET k1",
            "<reply> NULL",
            "<retval> 2",
            "<error> Unknown Redis Lua debugger command or wrong number of arguments.",
            "All breakpoints removed.",
        ]));
        assert_eq!(
            step.log,
            vec![
                DebugLine::Debug("line 3: \"hello\", 1".into()),
                DebugLine::RedisCall("GET k1".into()),
                DebugLine::RedisReply("NULL".into()),
                DebugLine::RetVal("2".into()),
                DebugLine::Error("Unknown Redis Lua debugger command or wrong number of arguments.".into()),
                DebugLine::Info("All breakpoints removed.".into()),
            ]
        );
    }

    #[test]
    fn a_reply_reads_as_status_lines() {
        let reply = statuses(&["* Stopped at 2, stop reason = step over", "-> 2   local n = 0"]);
        let step = parse_lines(&status_lines(&reply));
        assert_eq!(step.stopped_at, Some(2));
        assert!(step.log.is_empty());
        // `+OK` comes back from redis-rs as its own variant.
        assert_eq!(status_lines(&Value::Okay), ["OK"]);
    }

    #[test]
    fn the_script_reply_is_the_end_of_the_session() {
        let end = end_from(Value::Array(vec![Value::Int(1), Value::BulkString(b"x".to_vec())]));
        assert!(!end.error);
        assert!(
            end.result.contains("(integer) 1") && end.result.contains("\"x\""),
            "{}",
            end.result
        );

        let refused = redis::parse_redis_value(b"-ERR script aborted for user request\r\n").expect("valid RESP");
        let end = end_from(refused);
        assert!(end.error);
        assert!(end.result.contains("aborted"), "{}", end.result);
    }

    #[test]
    fn the_end_marker_is_not_output() {
        let step = parse_lines(&lines(&["<debug> line 3: \"hello\", 1", "<endsession>"]));
        assert_eq!(step.log, vec![DebugLine::Debug("line 3: \"hello\", 1".into())]);
    }

    #[test]
    fn commands_are_spelled_the_way_ldb_reads_them() {
        assert_eq!(DebugCommand::Step.words(), ["s"]);
        assert_eq!(DebugCommand::Continue.words(), ["c"]);
        assert_eq!(DebugCommand::Abort.words(), ["a"]);
        assert_eq!(DebugCommand::Break(7).words(), ["b", "7"]);
        assert_eq!(DebugCommand::Unbreak(7).words(), ["b", "-7"]);
        assert_eq!(DebugCommand::ClearBreaks.words(), ["b", "0"]);
        assert_eq!(DebugCommand::Eval("1+1".into()).words(), ["e", "1+1"]);
        assert_eq!(
            DebugCommand::Redis(vec!["GET".into(), "k".into()]).words(),
            ["r", "GET", "k"]
        );
        assert_eq!(DebugMode::Forked.word(), "YES");
        assert_eq!(DebugMode::Sync.word(), "SYNC");
    }
}

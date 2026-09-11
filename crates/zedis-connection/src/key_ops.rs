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

//! Type-native operations the editors offer beyond add / edit / delete.
//!
//! These are the commands you reach for *while looking at a key* and that
//! are easy to get wrong by hand: trimming a list to a window, bumping a
//! counter, popping the head of a queue. Every one of them is available in
//! the terminal — what this module buys is a form that cannot produce a
//! malformed argument, a confirmation in front of the destructive ones, and
//! a result the panel can show.
//!
//! One enum rather than eight functions so the UI can treat "which
//! operation" as data: the dialog is built from the op's fields and the
//! outcome is rendered from its reply.

use crate::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{Value, cmd};
use zedis_core::json::JsonPathOp;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Which end of a list or sorted set an operation works from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromEnd {
    Head,
    Tail,
}

/// A single type-native operation against one key.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyOp {
    /// `LTRIM key start stop` — keep only that window, drop the rest.
    ListTrim { start: i64, stop: i64 },
    /// `LPOP` / `RPOP`, optionally with a count (6.2+ — the caller gates).
    ListPop { end: FromEnd, count: u64 },
    /// `ZINCRBY key delta member`.
    ZsetIncrBy { member: String, delta: f64 },
    /// `ZPOPMIN` / `ZPOPMAX` with a count.
    ZsetPop { end: FromEnd, count: u64 },
    /// `HINCRBY key field delta`.
    HashIncrBy { field: String, delta: i64 },
    /// `INCRBY` for a whole delta, `INCRBYFLOAT` otherwise — one field in
    /// the form either way, because "add 1" and "add 0.5" are the same
    /// intent and the split is Redis's, not the user's.
    StringIncrBy { delta: f64 },
    /// `APPEND key text`.
    StringAppend { text: String },
    /// `GETEX key EX seconds` / `GETEX key PERSIST` (6.2+ — the caller
    /// gates). Reads the value and changes its expiry in one command.
    StringGetEx { ttl: Option<u64> },
    /// One `JSON.*` write at a path of a RedisJSON document — the same
    /// operations the JSON tree applies locally to a plain string.
    Json { path: String, op: JsonPathOp },
}

/// What an operation reported back, in the shape the panel renders it.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyOpOutcome {
    /// The value a counter now holds.
    Number(String),
    /// Entries the operation took out of the key.
    Removed(Vec<String>),
    /// A length or a count the command answered with.
    Count(u64),
    /// Succeeded with nothing worth reporting.
    Done,
}

impl KeyOp {
    /// Whether this operation destroys data, and therefore goes behind a
    /// confirmation. A counter bump does not; trimming a list does.
    /// A command-like one-line description for a collection's change log,
    /// or `None` for the String operations — a string's history is its
    /// before-and-after snapshots, not a list of operations.
    pub fn describe(&self) -> Option<String> {
        let pop = |end: &FromEnd, head: &str, tail: &str| match end {
            FromEnd::Head => head.to_string(),
            FromEnd::Tail => tail.to_string(),
        };
        match self {
            KeyOp::ListTrim { start, stop } => Some(format!("LTRIM {start} {stop}")),
            KeyOp::ListPop { end, count } => Some(format!("{} {count}", pop(end, "LPOP", "RPOP"))),
            KeyOp::ZsetIncrBy { member, delta } => Some(format!("ZINCRBY {} {member}", format_number(*delta))),
            KeyOp::ZsetPop { end, count } => Some(format!("{} {count}", pop(end, "ZPOPMIN", "ZPOPMAX"))),
            KeyOp::HashIncrBy { field, delta } => Some(format!("HINCRBY {field} {delta}")),
            // A JSON document's history is its snapshots, like a string's.
            KeyOp::StringIncrBy { .. }
            | KeyOp::StringAppend { .. }
            | KeyOp::StringGetEx { .. }
            | KeyOp::Json { .. } => None,
        }
    }

    pub fn is_destructive(&self) -> bool {
        match self {
            KeyOp::ListTrim { .. } | KeyOp::ListPop { .. } | KeyOp::ZsetPop { .. } => true,
            KeyOp::Json { op, .. } => op.is_destructive(),
            _ => false,
        }
    }
}

/// Runs `op` against `key`.
///
/// The caller has already checked the version floors the two 6.2 forms need
/// (`LPOP … count`, `GETEX`); nothing here guesses at a server version.
pub async fn run_key_op(conn: &mut RedisAsyncConn, key: &str, op: KeyOp) -> Result<KeyOpOutcome> {
    match op {
        KeyOp::ListTrim { start, stop } => {
            let _: () = cmd("LTRIM").arg(key).arg(start).arg(stop).query_async(conn).await?;
            let len: u64 = cmd("LLEN").arg(key).query_async(conn).await?;
            Ok(KeyOpOutcome::Count(len))
        }
        KeyOp::ListPop { end, count } => {
            let word = match end {
                FromEnd::Head => "LPOP",
                FromEnd::Tail => "RPOP",
            };
            let mut command = cmd(word);
            command.arg(key);
            // The countless form is the one every supported server has; the
            // count argument arrived in 6.2, so it is only sent when asked
            // for more than one.
            if count > 1 {
                command.arg(count);
                let popped: Option<Vec<String>> = command.query_async(conn).await?;
                return Ok(KeyOpOutcome::Removed(popped.unwrap_or_default()));
            }
            let popped: Option<String> = command.query_async(conn).await?;
            Ok(KeyOpOutcome::Removed(popped.into_iter().collect()))
        }
        KeyOp::ZsetIncrBy { member, delta } => {
            let score: f64 = cmd("ZINCRBY")
                .arg(key)
                .arg(delta)
                .arg(&member)
                .query_async(conn)
                .await?;
            Ok(KeyOpOutcome::Number(format_number(score)))
        }
        KeyOp::ZsetPop { end, count } => {
            let word = match end {
                FromEnd::Head => "ZPOPMIN",
                FromEnd::Tail => "ZPOPMAX",
            };
            // Flat `member, score, member, score…`; only the members are
            // worth naming back to the user.
            let flat: Vec<String> = cmd(word).arg(key).arg(count).query_async(conn).await?;
            let members = flat.chunks(2).filter_map(|pair| pair.first().cloned()).collect();
            Ok(KeyOpOutcome::Removed(members))
        }
        KeyOp::HashIncrBy { field, delta } => {
            let value: i64 = cmd("HINCRBY").arg(key).arg(&field).arg(delta).query_async(conn).await?;
            Ok(KeyOpOutcome::Number(value.to_string()))
        }
        KeyOp::StringIncrBy { delta } => {
            // A whole delta keeps the integer command, so a counter stays an
            // integer: `INCRBYFLOAT 1` would rewrite "5" as "6" but make the
            // next `INCR` fail on a value Redis no longer reads as an int.
            if delta.fract() == 0.0 && delta.is_finite() {
                let value: i64 = cmd("INCRBY").arg(key).arg(delta as i64).query_async(conn).await?;
                return Ok(KeyOpOutcome::Number(value.to_string()));
            }
            let value: String = cmd("INCRBYFLOAT").arg(key).arg(delta).query_async(conn).await?;
            Ok(KeyOpOutcome::Number(value))
        }
        KeyOp::StringAppend { text } => {
            let len: u64 = cmd("APPEND").arg(key).arg(&text).query_async(conn).await?;
            Ok(KeyOpOutcome::Count(len))
        }
        KeyOp::StringGetEx { ttl } => {
            let mut command = cmd("GETEX");
            command.arg(key);
            match ttl {
                Some(seconds) => {
                    command.arg("EX").arg(seconds);
                }
                None => {
                    command.arg("PERSIST");
                }
            }
            let _: Option<String> = command.query_async(conn).await?;
            Ok(KeyOpOutcome::Done)
        }
        KeyOp::Json { path, op } => run_json_op(conn, key, &path, op).await,
    }
}

/// One `JSON.*` write. A `$`-rooted path makes RedisJSON answer per match
/// in an array; a legacy `.`-rooted one answers a scalar — the reply is
/// read either way, and only the first match is reported.
async fn run_json_op(conn: &mut RedisAsyncConn, key: &str, path: &str, op: JsonPathOp) -> Result<KeyOpOutcome> {
    let mut command = cmd(op.command());
    command.arg(key).arg(path);
    match &op {
        JsonPathOp::Set(value) | JsonPathOp::ArrAppend(value) => {
            command.arg(value.to_string());
        }
        JsonPathOp::NumIncrBy(delta) => {
            command.arg(format_number(*delta));
        }
        // The server wants a JSON string, quotes included.
        JsonPathOp::StrAppend(text) => {
            command.arg(serde_json::to_string(text)?);
        }
        JsonPathOp::Del | JsonPathOp::Toggle | JsonPathOp::Clear => {}
    }
    let reply: Value = command.query_async(conn).await?;
    Ok(match op {
        JsonPathOp::Set(_) => KeyOpOutcome::Done,
        // Answered as a JSON-encoded string: `[6]` for `$`, `6` for a
        // legacy path.
        JsonPathOp::NumIncrBy(_) => {
            let text = reply_text(&reply);
            let number = text
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            KeyOpOutcome::Number(number)
        }
        JsonPathOp::Toggle => match first_integer(&reply) {
            Some(flag) => KeyOpOutcome::Number((flag != 0).to_string()),
            None => KeyOpOutcome::Done,
        },
        JsonPathOp::Del | JsonPathOp::ArrAppend(_) | JsonPathOp::StrAppend(_) | JsonPathOp::Clear => {
            match first_integer(&reply) {
                Some(count) => KeyOpOutcome::Count(count.max(0) as u64),
                None => KeyOpOutcome::Done,
            }
        }
    })
}

fn reply_text(reply: &Value) -> String {
    match reply {
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => text.clone(),
        Value::Int(n) => n.to_string(),
        Value::Array(items) => items.first().map(reply_text).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The reply's integer, or the first non-null one of a per-match array.
fn first_integer(reply: &Value) -> Option<i64> {
    match reply {
        Value::Int(n) => Some(*n),
        Value::Array(items) => items.iter().find_map(first_integer),
        _ => None,
    }
}

/// A score without the trailing `.0` an integral f64 would print — the
/// form Redis itself answers with.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{FromEnd, JsonPathOp, KeyOp, Value, first_integer, format_number, reply_text};

    #[test]
    fn only_the_data_losing_operations_ask_for_confirmation() {
        assert!(KeyOp::ListTrim { start: 0, stop: 9 }.is_destructive());
        assert!(
            KeyOp::ListPop {
                end: FromEnd::Head,
                count: 1
            }
            .is_destructive()
        );
        assert!(
            KeyOp::ZsetPop {
                end: FromEnd::Tail,
                count: 1
            }
            .is_destructive()
        );
        // Counters and appends only ever add.
        assert!(!KeyOp::StringIncrBy { delta: 1.0 }.is_destructive());
        assert!(
            !KeyOp::HashIncrBy {
                field: "f".into(),
                delta: -1
            }
            .is_destructive()
        );
        assert!(
            !KeyOp::ZsetIncrBy {
                member: "m".into(),
                delta: 1.5
            }
            .is_destructive()
        );
        assert!(!KeyOp::StringAppend { text: "x".into() }.is_destructive());
        assert!(!KeyOp::StringGetEx { ttl: Some(60) }.is_destructive());
        // JSON writes follow their own operation's answer.
        assert!(
            KeyOp::Json {
                path: "$.a".into(),
                op: JsonPathOp::Del
            }
            .is_destructive()
        );
        assert!(
            !KeyOp::Json {
                path: "$.a".into(),
                op: JsonPathOp::Toggle
            }
            .is_destructive()
        );
    }

    #[test]
    fn json_replies_are_read_for_either_path_syntax() {
        // `$` paths answer per match; legacy paths answer a scalar.
        assert_eq!(first_integer(&Value::Array(vec![Value::Nil, Value::Int(3)])), Some(3));
        assert_eq!(first_integer(&Value::Int(1)), Some(1));
        assert_eq!(first_integer(&Value::Array(vec![Value::Nil])), None);
        assert_eq!(reply_text(&Value::BulkString(b"[6]".to_vec())), "[6]");
        assert_eq!(reply_text(&Value::Array(vec![Value::BulkString(b"7".to_vec())])), "7");
    }

    #[test]
    fn collection_operations_describe_themselves_and_string_ones_do_not() {
        assert_eq!(
            KeyOp::ListTrim { start: 0, stop: 9 }.describe().as_deref(),
            Some("LTRIM 0 9")
        );
        assert_eq!(
            KeyOp::ZsetPop {
                end: FromEnd::Tail,
                count: 2
            }
            .describe()
            .as_deref(),
            Some("ZPOPMAX 2")
        );
        assert_eq!(
            KeyOp::ZsetIncrBy {
                member: "m".into(),
                delta: 4.0
            }
            .describe()
            .as_deref(),
            Some("ZINCRBY 4 m"),
            "a whole delta prints without `.0`"
        );
        assert_eq!(KeyOp::StringAppend { text: "x".into() }.describe(), None);
    }

    #[test]
    fn scores_print_the_way_redis_answers_them() {
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(-2.0), "-2");
        assert_eq!(format_number(1.5), "1.5");
    }
}

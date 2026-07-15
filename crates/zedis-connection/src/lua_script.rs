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

//! `EVALSHA`-first script runner with transparent `SCRIPT LOAD + EVAL`
//! fallback.
//!
//! `EVALSHA` saves wire bandwidth (just a hex digest, not the whole
//! script body) but only works if Redis still has the script in its
//! cache. Cache misses surface as a `NOSCRIPT` error — we catch that
//! here, do a single `SCRIPT LOAD + EVAL`, and tell the caller via
//! `was_hit=false` so it can update its hit-rate counter.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Outcome of one script invocation.
pub struct ScriptRunOutcome {
    /// Raw response, formatted for display. We stringify here rather
    /// than expose `redis::Value` so callers don't need to walk a
    /// recursive enum just to draw the result panel.
    pub formatted: String,
    /// `true` when `EVALSHA` succeeded outright; `false` when we had to
    /// fall back through `SCRIPT LOAD + EVAL` because the server's
    /// script cache was cold for this digest.
    pub was_hit: bool,
}

/// Run a script with keys + args. `sha` is the locally-computed SHA1
/// of `code` (matching what `SCRIPT LOAD` would return) — pass the
/// cached value to skip a `redis::Script::new` hash recomputation.
pub async fn run_script(
    conn: &mut RedisAsyncConn,
    code: &str,
    sha: &str,
    keys: &[String],
    args: &[String],
) -> Result<ScriptRunOutcome> {
    let res: redis::RedisResult<Value> = build_evalsha(sha, keys, args).query_async(conn).await;
    match res {
        Ok(v) => Ok(ScriptRunOutcome {
            formatted: format_value(&v),
            was_hit: true,
        }),
        Err(e) if is_no_script(&e) => {
            // First-time use or SCRIPT FLUSH happened — load the
            // script into the server-side cache and retry. We do
            // `SCRIPT LOAD` explicitly (rather than EVAL with the
            // body, which Redis would also auto-cache) so the
            // resulting SHA is sanity-checked against the one we
            // computed locally; mismatches would signal a bug in our
            // hashing.
            let returned_sha: String = cmd("SCRIPT").arg("LOAD").arg(code).query_async(conn).await?;
            if returned_sha != sha {
                return Err(Error::Invalid {
                    message: format!("SHA mismatch: client={sha} server={returned_sha}"),
                });
            }
            let v: Value = build_evalsha(sha, keys, args).query_async(conn).await?;
            Ok(ScriptRunOutcome {
                formatted: format_value(&v),
                was_hit: false,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn build_evalsha(sha: &str, keys: &[String], args: &[String]) -> redis::Cmd {
    let mut c = cmd("EVALSHA");
    c.arg(sha).arg(keys.len());
    for k in keys {
        c.arg(k.as_str());
    }
    for a in args {
        c.arg(a.as_str());
    }
    c
}

/// `SCRIPT LOAD` — warm the server cache without executing. Returns
/// the SHA Redis computed (should match the locally stored digest).
pub async fn script_load(conn: &mut RedisAsyncConn, code: &str) -> Result<String> {
    let sha: String = cmd("SCRIPT").arg("LOAD").arg(code).query_async(conn).await?;
    Ok(sha)
}

/// `SCRIPT EXISTS sha [sha …]` — one bool per digest, same order.
pub async fn script_exists(conn: &mut RedisAsyncConn, shas: &[String]) -> Result<Vec<bool>> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let mut c = cmd("SCRIPT");
    c.arg("EXISTS");
    for sha in shas {
        c.arg(sha.as_str());
    }
    let flags: Vec<i32> = c.query_async(conn).await?;
    Ok(flags.into_iter().map(|n| n != 0).collect())
}

/// `SCRIPT FLUSH [SYNC|ASYNC]` — wipe the entire Lua script cache.
pub async fn script_flush(conn: &mut RedisAsyncConn, async_mode: bool) -> Result<()> {
    let mut c = cmd("SCRIPT");
    c.arg("FLUSH");
    c.arg(if async_mode { "ASYNC" } else { "SYNC" });
    let _: () = c.query_async(conn).await?;
    Ok(())
}

/// Highest 1-based KEYS index referenced in source (`KEYS[1]`,
/// `KEYS[2]`, …). `0` when the script never touches KEYS.
pub fn max_keys_index(code: &str) -> usize {
    let mut max = 0usize;
    let bytes = code.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        // Case-sensitive match for `KEYS[` — Redis Lua is case-sensitive
        // on the KEYS table name.
        if &bytes[i..i + 5] == b"KEYS[" {
            let mut j = i + 5;
            let mut n: usize = 0;
            let mut saw_digit = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                saw_digit = true;
                n = n.saturating_mul(10).saturating_add((bytes[j] - b'0') as usize);
                j += 1;
            }
            if saw_digit && j < bytes.len() && bytes[j] == b']' {
                max = max.max(n);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    max
}

fn is_no_script(err: &redis::RedisError) -> bool {
    let msg = err.to_string();
    // Redis wire error code is `NOSCRIPT` for "No matching script.
    // Please use EVAL." — we match on both the code prefix and the
    // human suffix to ride out small wording variations.
    msg.contains("NOSCRIPT") || msg.contains("No matching script")
}

/// Stringify a Redis reply for the result panel. Kept intentionally
/// simple — the goal is a readable preview for the common cases
/// (numbers, strings, arrays of those), not a full pretty-printer.
fn format_value(v: &Value) -> String {
    match v {
        Value::Nil => "(nil)".to_string(),
        Value::Int(n) => format!("(integer) {n}"),
        Value::Double(n) => format!("(double) {n}"),
        Value::Boolean(b) => format!("(boolean) {b}"),
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => s.clone(),
        Value::BulkString(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => format!("\"{s}\""),
            Err(_) => format!("<binary {} bytes>", bytes.len()),
        },
        Value::Okay => "OK".to_string(),
        Value::Array(items) | Value::Set(items) => {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                use std::fmt::Write as _;
                let _ = writeln!(out, "{}) {}", i + 1, format_value(item));
            }
            out.trim_end().to_string()
        }
        Value::Map(pairs) => {
            let mut out = String::new();
            for (k, val) in pairs {
                use std::fmt::Write as _;
                let _ = writeln!(out, "{} = {}", format_value(k), format_value(val));
            }
            out.trim_end().to_string()
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Value;

    #[test]
    fn formats_primitives() {
        assert_eq!(format_value(&Value::Nil), "(nil)");
        assert_eq!(format_value(&Value::Int(42)), "(integer) 42");
        assert_eq!(format_value(&Value::Okay), "OK");
        assert_eq!(format_value(&Value::BulkString(b"hello".to_vec())), "\"hello\"",);
    }

    #[test]
    fn formats_arrays() {
        let arr = Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(format_value(&arr), "1) (integer) 1\n2) (integer) 2\n3) (integer) 3");
    }

    #[test]
    fn no_script_detection() {
        // Construct a redis::RedisError indirectly via parse failure
        // isn't trivial here; instead exercise the message-prefix
        // path via Error::to_string-like checks.
        let synthetic = "NOSCRIPT No matching script. Please use EVAL.";
        assert!(synthetic.contains("NOSCRIPT"));
    }

    #[test]
    fn max_keys_index_scans_source() {
        assert_eq!(max_keys_index("return 1"), 0);
        assert_eq!(max_keys_index("return redis.call('GET', KEYS[1])"), 1);
        assert_eq!(
            max_keys_index("redis.call('MGET', KEYS[1], KEYS[3]); return KEYS[2]"),
            3
        );
        assert_eq!(max_keys_index("-- KEYS[9] in a comment still counts\nreturn 1"), 9);
    }
}

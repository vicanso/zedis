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

//! Hash field writes that carry their TTL decision.
//!
//! On Redis 8.0+ `HSETEX` sets a field and its expiry in one command; the
//! caller passes `atomic` from the feature probe (`ServerCommand::HSetEx`),
//! never from a version number. Elsewhere it is `HSET` followed by
//! `HEXPIRE` / `HPERSIST` — two commands, and a window in which the field
//! exists without its TTL. `HSET` also discards a field's existing TTL (a
//! server rule since 7.4), so *keeping* a TTL across a value edit is only
//! possible through `HSETEX KEEPTTL`.

use super::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{Cmd, Pipeline, Value, cmd, pipe};

type Result<T, E = Error> = std::result::Result<T, E>;

/// What a field write does to that field's TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTtl {
    /// Leave the TTL alone (`HSETEX KEEPTTL`). The `HSET` fallback cannot
    /// honour this — the server drops the TTL together with the old value.
    Keep,
    /// Remove any TTL.
    Persist,
    /// Set the TTL, in seconds.
    Expire(i64),
}

impl FieldTtl {
    /// The editor's convention: `None` keeps, a non-positive value removes,
    /// `Some(secs)` sets.
    pub fn from_editor(ttl: Option<i64>) -> Self {
        match ttl {
            None => FieldTtl::Keep,
            Some(secs) if secs > 0 => FieldTtl::Expire(secs),
            Some(_) => FieldTtl::Persist,
        }
    }
}

/// `HSETEX key [KEEPTTL | EX secs] FIELDS 1 field value`.
fn hsetex_cmd(key: &str, field: &str, value: &str, ttl: FieldTtl) -> Cmd {
    let mut c = cmd("HSETEX");
    c.arg(key);
    match ttl {
        FieldTtl::Keep => {
            c.arg("KEEPTTL");
        }
        FieldTtl::Expire(secs) => {
            c.arg("EX").arg(secs);
        }
        FieldTtl::Persist => {}
    }
    c.arg("FIELDS").arg(1).arg(field).arg(value);
    c
}

/// The pre-8.0 shape: `HSET`, then whichever TTL command the decision
/// needs (none for `Keep` — nothing can be kept once `HSET` ran).
fn push_fallback(p: &mut Pipeline, key: &str, field: &str, value: &str, ttl: FieldTtl) {
    p.cmd("HSET").arg(key).arg(field).arg(value);
    match ttl {
        FieldTtl::Expire(secs) => {
            p.cmd("HEXPIRE").arg(key).arg(secs).arg("FIELDS").arg(1).arg(field);
        }
        FieldTtl::Persist => {
            p.cmd("HPERSIST").arg(key).arg("FIELDS").arg(1).arg(field);
        }
        FieldTtl::Keep => {}
    }
}

/// Set `field` to `value` with the given TTL decision; returns whether the
/// field was created rather than overwritten. With `atomic` (the server
/// has `HSETEX`) that is one MULTI — `HEXISTS` for the created flag, then
/// the `HSETEX` — otherwise `HSET` plus its TTL command in one pipeline.
pub async fn write_hash_field(
    conn: &mut RedisAsyncConn,
    key: &str,
    field: &str,
    value: &str,
    ttl: FieldTtl,
    atomic: bool,
) -> Result<bool> {
    if atomic {
        let mut p = pipe();
        p.atomic().cmd("HEXISTS").arg(key).arg(field);
        p.add_command(hsetex_cmd(key, field, value, ttl));
        let (existed, _set): (i64, i64) = p.query_async(conn).await?;
        return Ok(existed == 0);
    }
    let mut p = pipe();
    push_fallback(&mut p, key, field, value, ttl);
    let replies: Vec<Value> = p.query_async(conn).await?;
    Ok(matches!(replies.first(), Some(Value::Int(1))))
}

/// Rename a field while writing its value: the new field is written with
/// the TTL decision and the old one deleted, in one MULTI. A rename cannot
/// carry the old field's TTL over — `Keep` leaves the new field without one.
pub async fn rename_hash_field(
    conn: &mut RedisAsyncConn,
    key: &str,
    old_field: &str,
    new_field: &str,
    value: &str,
    ttl: FieldTtl,
    atomic: bool,
) -> Result<()> {
    let mut p = pipe();
    p.atomic();
    if atomic {
        p.add_command(hsetex_cmd(key, new_field, value, ttl));
    } else {
        push_fallback(&mut p, key, new_field, value, ttl);
    }
    p.cmd("HDEL").arg(key).arg(old_field);
    let _: Vec<Value> = p.query_async(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Arg;

    fn words(c: &Cmd) -> Vec<String> {
        c.args_iter()
            .map(|a| match a {
                Arg::Simple(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => String::new(),
            })
            .collect()
    }

    fn pipeline_words(p: &Pipeline) -> Vec<Vec<String>> {
        p.cmd_iter().map(words).collect()
    }

    #[test]
    fn editor_ttl_convention() {
        assert_eq!(FieldTtl::from_editor(None), FieldTtl::Keep);
        assert_eq!(FieldTtl::from_editor(Some(-1)), FieldTtl::Persist);
        assert_eq!(FieldTtl::from_editor(Some(0)), FieldTtl::Persist);
        assert_eq!(FieldTtl::from_editor(Some(30)), FieldTtl::Expire(30));
    }

    #[test]
    fn hsetex_spells_each_decision() {
        assert_eq!(
            words(&hsetex_cmd("k", "f", "v", FieldTtl::Keep)),
            ["HSETEX", "k", "KEEPTTL", "FIELDS", "1", "f", "v"]
        );
        assert_eq!(
            words(&hsetex_cmd("k", "f", "v", FieldTtl::Expire(60))),
            ["HSETEX", "k", "EX", "60", "FIELDS", "1", "f", "v"]
        );
        assert_eq!(
            words(&hsetex_cmd("k", "f", "v", FieldTtl::Persist)),
            ["HSETEX", "k", "FIELDS", "1", "f", "v"]
        );
    }

    #[test]
    fn fallback_is_hset_then_the_ttl_command() {
        let mut p = pipe();
        push_fallback(&mut p, "k", "f", "v", FieldTtl::Expire(60));
        assert_eq!(
            pipeline_words(&p),
            [
                vec!["HSET", "k", "f", "v"],
                vec!["HEXPIRE", "k", "60", "FIELDS", "1", "f"]
            ]
        );
        let mut p = pipe();
        push_fallback(&mut p, "k", "f", "v", FieldTtl::Persist);
        assert_eq!(pipeline_words(&p)[1], ["HPERSIST", "k", "FIELDS", "1", "f"]);
        let mut p = pipe();
        push_fallback(&mut p, "k", "f", "v", FieldTtl::Keep);
        assert_eq!(pipeline_words(&p).len(), 1, "nothing can keep a TTL after HSET");
    }
}

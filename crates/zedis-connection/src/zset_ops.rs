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

//! Sorted-set operations: the three ways the editor pages one — by rank, by
//! a score window, by a member pattern — and its writes.
//!
//! Members are bytes, kept as answered; scores are `f64`. `descending` is
//! the editor's sort order: by rank it picks `ZREVRANGE`, by score
//! `ZREVRANGEBYSCORE` — whose `max` comes *before* its `min`, which is why
//! the caller passes the window the same way round for both directions.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::floors;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A member with its score.
pub type ScoredMember = (Vec<u8>, f64);

/// `ZCARD`.
pub async fn zset_card(at: &ServerDb, key: &str) -> Result<usize> {
    Ok(cmd("ZCARD").arg(key).query_async(&mut at.connection().await?).await?)
}

/// Ranks `start..=stop` in the given direction, with scores.
pub async fn zset_range(
    at: &ServerDb,
    key: &str,
    descending: bool,
    start: usize,
    stop: usize,
) -> Result<Vec<ScoredMember>> {
    Ok(cmd(if descending { "ZREVRANGE" } else { "ZRANGE" })
        .arg(key)
        .arg(start)
        .arg(stop)
        .arg("WITHSCORES")
        .query_async(&mut at.connection().await?)
        .await?)
}

/// One `ZSCAN` round over the members matching `pattern` (a glob, as
/// given): the next cursor and the members with their scores.
pub async fn zset_scan(
    at: &ServerDb,
    key: &str,
    cursor: u64,
    pattern: &str,
    count: u64,
) -> Result<(u64, Vec<ScoredMember>)> {
    let (next_cursor, flat): (u64, Vec<Vec<u8>>) = cmd("ZSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok((next_cursor, scored(flat)))
}

/// The members whose score is within `min..=max` (Redis's own syntax: `-inf`,
/// `(5`, `+inf`), `count` of them from `offset`, in the given direction.
pub async fn zset_range_by_score(
    at: &ServerDb,
    key: &str,
    descending: bool,
    (min, max): (&str, &str),
    offset: usize,
    count: usize,
) -> Result<Vec<ScoredMember>> {
    let mut c = cmd(if descending {
        "ZREVRANGEBYSCORE"
    } else {
        "ZRANGEBYSCORE"
    });
    c.arg(key);
    if descending {
        c.arg(max).arg(min);
    } else {
        c.arg(min).arg(max);
    }
    let flat: Vec<Vec<u8>> = c
        .arg("WITHSCORES")
        .arg("LIMIT")
        .arg(offset)
        .arg(count)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(scored(flat))
}

/// `ZCOUNT key min max`.
pub async fn zset_count_by_score(at: &ServerDb, key: &str, min: &str, max: &str) -> Result<usize> {
    Ok(cmd("ZCOUNT")
        .arg(key)
        .arg(min)
        .arg(max)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `ZADD key score member`, then — for an edit that renamed the member —
/// `ZREM` of the old one. Answers whether the member is new to the set.
pub async fn zset_put(at: &ServerDb, key: &str, member: &[u8], score: f64, replaces: Option<&[u8]>) -> Result<bool> {
    let conn = &mut at.connection().await?;
    let added: usize = cmd("ZADD").arg(key).arg(score).arg(member).query_async(conn).await?;
    if let Some(old) = replaces.filter(|old| *old != member) {
        let _: () = cmd("ZREM").arg(key).arg(old).query_async(conn).await?;
    }
    Ok(added > 0)
}

/// `ZADD key score member…`; answers how many members were newly added.
pub async fn zset_add_many(at: &ServerDb, key: &str, items: &[(f64, &[u8])]) -> Result<usize> {
    if items.is_empty() {
        return Ok(0);
    }
    let mut c = cmd("ZADD");
    c.arg(key);
    for (score, member) in items {
        c.arg(*score).arg(*member);
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// `ZREM key member…`; answers how many were members.
pub async fn zset_remove(at: &ServerDb, key: &str, members: &[&[u8]]) -> Result<usize> {
    if members.is_empty() {
        return Ok(0);
    }
    let mut c = cmd("ZREM");
    c.arg(key);
    for member in members {
        c.arg(*member);
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// `ZMPOP numkeys key… MIN|MAX COUNT count` (Redis 7.0+).
/// Returns `(popped_key, scored_members)`, or `None` when every sorted set was empty.
pub async fn zset_mpop(
    at: &ServerDb,
    keys: &[&str],
    max: bool,
    count: usize,
) -> Result<Option<(String, Vec<ScoredMember>)>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let client = at.client().await?;
    if !client.supports(floors::ZMPOP) {
        return Err(Error::Invalid {
            message: "ZMPOP requires Redis 7.0 or Valkey 7.2+".to_string(),
        });
    }
    let mut conn = client.connection();
    let mut c = cmd("ZMPOP");
    c.arg(keys.len());
    for key in keys {
        c.arg(*key);
    }
    c.arg(if max { "MAX" } else { "MIN" }).arg("COUNT").arg(count.max(1));
    let raw: Value = c.query_async(&mut conn).await?;
    Ok(parse_zset_mpop(&raw))
}

fn parse_zset_mpop(value: &Value) -> Option<(String, Vec<ScoredMember>)> {
    let Value::Array(pair) = value else {
        return None;
    };
    if pair.len() != 2 {
        return None;
    }
    let key = reply::text_lossy(&pair[0])?;
    let Value::Array(items) = &pair[1] else {
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    // RESP2 / RESP3: `[[member, score], ...]`, or flat `[member, score, ...]`.
    if items.first().is_some_and(|first| matches!(first, Value::Array(_))) {
        for item in items {
            let Value::Array(ms) = item else { continue };
            let Some(member_val) = ms.first() else { continue };
            let member = reply::bytes(member_val).or_else(|| reply::text_lossy(member_val).map(String::into_bytes))?;
            let score = ms.get(1).and_then(reply::float).unwrap_or(0.0);
            out.push((member, score));
        }
    } else {
        let flat: Vec<Vec<u8>> = items
            .iter()
            .filter_map(|v| reply::bytes(v).or_else(|| reply::text_lossy(v).map(String::into_bytes)))
            .collect();
        out = scored(flat);
    }
    Some((key, out))
}

/// A flat `[member, score, member, score, …]` reply as pairs. A score that
/// does not parse reads as 0, a member left without one is dropped.
fn scored(flat: Vec<Vec<u8>>) -> Vec<ScoredMember> {
    let mut out = Vec::with_capacity(flat.len() / 2);
    let mut items = flat.into_iter();
    while let (Some(member), Some(score)) = (items.next(), items.next()) {
        out.push((member, String::from_utf8_lossy(&score).parse().unwrap_or_default()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{parse_zset_mpop, scored};
    use redis::Value;

    #[test]
    fn a_flat_reply_pairs_up_and_an_odd_tail_is_dropped() {
        let flat = |items: &[&str]| items.iter().map(|s| s.as_bytes().to_vec()).collect::<Vec<_>>();
        assert_eq!(
            scored(flat(&["a", "1.5", "b", "-inf", "c", "oops"])),
            vec![
                (b"a".to_vec(), 1.5),
                (b"b".to_vec(), f64::NEG_INFINITY),
                (b"c".to_vec(), 0.0)
            ]
        );
        assert_eq!(scored(flat(&["a", "1", "b"])), vec![(b"a".to_vec(), 1.0)]);
        assert!(scored(Vec::new()).is_empty());
    }

    #[test]
    fn zset_mpop_parses_nested_and_nil_replies() {
        assert_eq!(parse_zset_mpop(&Value::Nil), None);
        let nested = Value::Array(vec![
            Value::BulkString(b"board".to_vec()),
            Value::Array(vec![
                Value::Array(vec![Value::BulkString(b"alice".to_vec()), Value::Double(42.5)]),
                Value::Array(vec![
                    Value::BulkString(b"bob".to_vec()),
                    Value::BulkString(b"99".to_vec()),
                ]),
            ]),
        ]);
        assert_eq!(
            parse_zset_mpop(&nested),
            Some((
                "board".to_string(),
                vec![(b"alice".to_vec(), 42.5), (b"bob".to_vec(), 99.0)]
            ))
        );
    }
}

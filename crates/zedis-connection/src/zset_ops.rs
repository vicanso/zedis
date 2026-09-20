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
use crate::server_db::ServerDb;
use redis::cmd;

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
    use super::scored;

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
}

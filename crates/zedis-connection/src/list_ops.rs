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

//! List operations — and, below, why deleting by position is one.
//!
//! Redis has no "remove the element at index N": `LREM` matches on *value*
//! and `LSET` only overwrites. The standard workaround is to overwrite the
//! position with a value nothing else can hold and then remove that value —
//! which is what the single-element path does, and what generalises here.
//!
//! Deleting several positions is not that workaround repeated: each removal
//! renumbers everything after it, so a second index taken from the same
//! snapshot would address the wrong element (or, past the new end, fail).
//! Stamping *every* selected position with the **same** marker first and
//! then removing that marker once sidesteps renumbering entirely — no index
//! is ever used after the list has shifted under it.

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::error::Error;
use crate::floors;
use crate::key_ops::FromEnd;
use crate::reply;
use crate::server_db::ServerDb;
use redis::{Value, cmd, pipe};
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Removes the elements at `indexes` from the list at `key`.
///
/// Runs as one `MULTI`/`EXEC`, so no other client can observe the marker or
/// interleave a write between the stamping and the removal. Duplicate
/// indexes are harmless (the second `LSET` writes the same marker).
///
/// Returns how many elements `LREM` actually removed.
pub async fn remove_list_indexes(at: &ServerDb, key: &str, indexes: &[usize]) -> Result<u64> {
    if indexes.is_empty() {
        return Ok(0);
    }
    // Unique per call: a value already in the list would make `LREM` delete
    // elements nobody selected. v7 rather than v4 only because that is the
    // feature this workspace enables; uniqueness is what the marker needs.
    let marker = format!("__zedis:del:{}", Uuid::now_v7());
    let mut pipeline = pipe();
    pipeline.atomic();
    for index in indexes {
        pipeline.cmd("LSET").arg(key).arg(*index).arg(&marker).ignore();
    }
    // Count 0 — every stamped position at once, wherever they ended up.
    pipeline.cmd("LREM").arg(key).arg(0).arg(&marker);
    let mut removed: Vec<u64> = pipeline.query_async(&mut at.connection().await?).await?;
    Ok(removed.pop().unwrap_or(0))
}

/// `LLEN`.
pub async fn list_len(at: &ServerDb, key: &str) -> Result<usize> {
    Ok(cmd("LLEN").arg(key).query_async(&mut at.connection().await?).await?)
}

/// `LRANGE key start stop` (both inclusive), bytes kept as answered.
pub async fn list_range(at: &ServerDb, key: &str, start: usize, stop: usize) -> Result<Vec<Vec<u8>>> {
    Ok(cmd("LRANGE")
        .arg(key)
        .arg(start)
        .arg(stop)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `LPUSH` (`front`) or `RPUSH`; answers the list's new length.
pub async fn list_push(at: &ServerDb, key: &str, value: &[u8], front: bool) -> Result<usize> {
    list_push_many(at, key, &[value], front).await
}

/// One `LPUSH` or `RPUSH` of every value, in the order given. `LPUSH`
/// inserts right to left, so a caller that wants the block to read in
/// this order at the head passes the values reversed.
pub async fn list_push_many(at: &ServerDb, key: &str, values: &[&[u8]], front: bool) -> Result<usize> {
    if values.is_empty() {
        return Ok(0);
    }
    let mut command = cmd(if front { "LPUSH" } else { "RPUSH" });
    command.arg(key);
    for value in values {
        command.arg(*value);
    }
    Ok(command.query_async(&mut at.connection().await?).await?)
}

/// Overwrite the element at `index` — but only while it still holds
/// `expected`, the bytes the editor loaded. `false` means somebody changed
/// it meanwhile and nothing was written. A check, not a lock: the `LINDEX`
/// and the `LSET` are two commands, which is as much as editing one row of
/// a list by hand warrants.
pub async fn list_set_if_unchanged(
    at: &ServerDb,
    key: &str,
    index: usize,
    expected: &[u8],
    value: &[u8],
) -> Result<bool> {
    let conn = &mut at.connection().await?;
    let current: Vec<u8> = cmd("LINDEX").arg(key).arg(index).query_async(conn).await?;
    if current != expected {
        return Ok(false);
    }
    let _: () = cmd("LSET").arg(key).arg(index).arg(value).query_async(conn).await?;
    Ok(true)
}

fn end_side(end: FromEnd) -> &'static str {
    match end {
        FromEnd::Head => "LEFT",
        FromEnd::Tail => "RIGHT",
    }
}

/// `LMOVE source destination LEFT|RIGHT LEFT|RIGHT` (Redis 6.2+).
/// Returns the moved element, or `None` when `source` was empty.
pub async fn list_move(
    at: &ServerDb,
    source: &str,
    destination: &str,
    from: FromEnd,
    to: FromEnd,
) -> Result<Option<Vec<u8>>> {
    let reply: Option<Vec<u8>> = cmd("LMOVE")
        .arg(source)
        .arg(destination)
        .arg(end_side(from))
        .arg(end_side(to))
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(reply)
}

/// `LMPOP numkeys key… LEFT|RIGHT COUNT count` (Redis 7.0+).
/// Returns `(popped_key, elements)`, or `None` when every list was empty.
pub async fn list_mpop(
    at: &ServerDb,
    keys: &[&str],
    side: FromEnd,
    count: usize,
) -> Result<Option<(String, Vec<Vec<u8>>)>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let client = at.client().await?;
    if !client.supports(floors::LMPOP) {
        return Err(Error::Invalid {
            message: "LMPOP requires Redis 7.0 or Valkey 7.2+".to_string(),
        });
    }
    let mut conn = client.connection();
    let mut c = cmd("LMPOP");
    c.arg(keys.len());
    for key in keys {
        c.arg(*key);
    }
    c.arg(end_side(side)).arg("COUNT").arg(count.max(1));
    let raw: Value = c.query_async(&mut conn).await?;
    Ok(parse_list_mpop(&raw))
}

fn parse_list_mpop(value: &Value) -> Option<(String, Vec<Vec<u8>>)> {
    match value {
        Value::Nil => None,
        Value::Array(pair) if pair.len() == 2 => {
            let key = reply::text_lossy(&pair[0])?;
            let Value::Array(items) = &pair[1] else {
                return None;
            };
            let elems = items
                .iter()
                .filter_map(|v| reply::bytes(v).or_else(|| reply::text_lossy(v).map(String::into_bytes)))
                .collect();
            Some((key, elems))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_list_mpop;
    use redis::Value;

    #[test]
    fn list_mpop_parses_nil_and_popped_elements() {
        assert_eq!(parse_list_mpop(&Value::Nil), None);
        let reply = Value::Array(vec![
            Value::BulkString(b"queue:a".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"job-1".to_vec()),
                Value::BulkString(vec![0xff, 0x00]),
            ]),
        ]);
        assert_eq!(
            parse_list_mpop(&reply),
            Some(("queue:a".to_string(), vec![b"job-1".to_vec(), vec![0xff, 0x00]]))
        );
    }
}

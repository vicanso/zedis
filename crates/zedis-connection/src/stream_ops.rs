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

//! Streams: reading a page of entries, describing the stream and its consumer
//! groups, and every write the stream editor makes.
//!
//! The live tail is [`crate::StreamTail`] — it holds a connection of its own,
//! which is what a blocking `XREAD` needs and a page read does not.
//!
//! `XINFO` is the loosest reply in Redis: nested, different between RESP2 and
//! RESP3, and different again between versions and forks. So it is walked as
//! a `Value` with [`crate::reply`] and answered as the structs below —
//! *leniently*, because a field a fork does not report is a blank cell, not a
//! failed panel. What the view cannot be shown, it simply does not show.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::reply::{int, text_lossy, uint};
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// `XPENDING` page size — both the initial per-group load and every "load
/// more" click fetch this many entries.
pub const PENDING_PAGE: usize = 100;

/// One entry: its id and its field → value pairs, in order.
pub type StreamEntry = (String, Vec<(String, String)>);

/// One consumer of a group (`XINFO CONSUMERS`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamConsumer {
    pub name: String,
    /// Messages pending delivery to this consumer.
    pub pending: usize,
    /// Milliseconds since the consumer last interacted with the server.
    pub idle_ms: i64,
}

/// A pending message (`XPENDING key group start end count`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamPending {
    pub id: String,
    pub consumer: String,
    /// Milliseconds since the message was delivered.
    pub idle_ms: i64,
    /// How many times it has been delivered.
    pub delivery_count: i64,
}

/// One consumer group, with its consumers and the first page of its PEL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamGroup {
    pub name: String,
    pub consumers_count: usize,
    pub pending_count: usize,
    pub last_delivered_id: String,
    /// Entries not yet delivered to any consumer (0 = no lag).
    pub lag: i64,
    pub consumers: Vec<StreamConsumer>,
    pub pending_entries: Vec<StreamPending>,
    /// Whether `pending_entries` is the whole PEL — false when the page came
    /// back full, so more can be loaded.
    pub pending_done: bool,
}

/// Idempotent-producer counters (`XINFO STREAM`, Redis 8.6+ with IDMP in use
/// or configured). Absent from older servers' replies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamIdmp {
    /// Producers currently tracked (`pids-tracked`).
    pub pids_tracked: usize,
    /// Idempotency ids currently tracked (`iids-tracked`).
    pub iids_tracked: usize,
    /// Entries added through IDMP (`iids-added`).
    pub iids_added: usize,
    /// Duplicate publishes suppressed (`iids-duplicates`) — the number that
    /// proves the at-most-once guarantee is doing work.
    pub iids_duplicates: usize,
}

/// The stream itself (`XINFO STREAM`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamSummary {
    pub groups_count: usize,
    /// Id of the oldest entry.
    pub first_entry_id: String,
    /// Id of the newest entry.
    pub last_entry_id: String,
    /// `last-generated-id` — the highest id the stream has ever minted, which
    /// stays put when the newest entry is deleted. This, not `last_entry_id`,
    /// is what `XADD` compares against and what `XSETID` changes, so the two
    /// are kept apart.
    pub last_generated_id: String,
    /// Internal radix-tree keys (structural).
    pub radix_tree_keys: usize,
    /// Radix-tree nodes — a proxy for the memory footprint.
    pub radix_tree_nodes: usize,
    /// `None` on a server whose `XINFO` does not report them (pre-8.6).
    pub idmp: Option<StreamIdmp>,
}

/// What `XINFO` says about a stream, fetched on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamInfo {
    /// `None` when `XINFO STREAM` was refused or unreadable — the groups are
    /// still worth showing.
    pub summary: Option<StreamSummary>,
    pub groups: Vec<StreamGroup>,
}

/// How `XTRIM` decides what to drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTrim {
    /// Keep only the newest `n` entries (`MAXLEN n`).
    MaxLen(u64),
    /// Drop every entry with an id below this one (`MINID id`).
    MinId(String),
}

/// What happens to groups' PEL references of removed entries (`XTRIM` /
/// `XDELEX` / `XACKDEL`, Redis 8.2+).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StreamRefPolicy {
    /// The server default — references stay behind (classic `XDEL` / `XTRIM`).
    #[default]
    KeepRef,
    /// Also remove the references from every group's PEL.
    DelRef,
    /// Only remove entries every group has acknowledged.
    Acked,
}

impl StreamRefPolicy {
    /// The option word as sent on the wire.
    pub fn word(self) -> &'static str {
        match self {
            StreamRefPolicy::KeepRef => "KEEPREF",
            StreamRefPolicy::DelRef => "DELREF",
            StreamRefPolicy::Acked => "ACKED",
        }
    }
}

/// `XLEN`.
pub async fn stream_len(at: &ServerDb, key: &str) -> Result<usize> {
    Ok(cmd("XLEN").arg(key).query_async(&mut at.connection().await?).await?)
}

/// A page of entries, oldest-first (`XRANGE`) or newest-first (`XREVRANGE`).
/// `cursor` is the last id of the previous page and is excluded; `None`
/// starts at that direction's end. Answers `(next_cursor, entries)`, the
/// cursor empty once the stream has been walked to its end.
pub async fn stream_page(
    at: &ServerDb,
    key: &str,
    cursor: Option<&str>,
    count: usize,
    reverse: bool,
) -> Result<(String, Vec<StreamEntry>)> {
    // XRANGE    key start end COUNT n — oldest → newest, cursor = highest id
    // XREVRANGE key end start COUNT n — newest → oldest, cursor = lowest id
    let bound = cursor.map_or_else(
        || if reverse { "+".to_string() } else { "-".to_string() },
        |cursor| format!("({cursor}"),
    );
    let (from, to) = if reverse {
        (bound.as_str(), "-")
    } else {
        (bound.as_str(), "+")
    };
    let raw: Vec<(String, Vec<String>)> = cmd(if reverse { "XREVRANGE" } else { "XRANGE" })
        .arg(key)
        .arg(from)
        .arg(to)
        .arg("COUNT")
        .arg(count)
        .query_async(&mut at.connection().await?)
        .await?;

    let done = raw.len() < count;
    let entries: Vec<StreamEntry> = raw.into_iter().map(|(id, flat)| (id, pairs(flat))).collect();
    let next = if done {
        String::new()
    } else {
        entries.last().map(|(id, _)| id.clone()).unwrap_or_default()
    };
    Ok((next, entries))
}

/// `XINFO STREAM` + `XINFO GROUPS` +, per group, `XINFO CONSUMERS` and the
/// first `XPENDING` page.
///
/// Only `XINFO GROUPS` can fail the call: a refused `XINFO STREAM` leaves the
/// summary empty, and a refused `XPENDING` an empty pending list, because a
/// `NOPERM` on one of them must not blank the whole panel — the per-entry
/// actions surface a real error when actually used.
pub async fn stream_info(at: &ServerDb, key: &str) -> Result<StreamInfo> {
    let conn = &mut at.connection().await?;
    let stream_raw: Value = cmd("XINFO")
        .arg("STREAM")
        .arg(key)
        .query_async(conn)
        .await
        .unwrap_or(Value::Nil);
    let summary = fields(&stream_raw).map(|map| StreamSummary {
        groups_count: field_usize(&map, "groups"),
        first_entry_id: field(&map, "first-entry").map(entry_id).unwrap_or_default(),
        last_entry_id: field(&map, "last-entry").map(entry_id).unwrap_or_default(),
        last_generated_id: field_text(&map, "last-generated-id"),
        radix_tree_keys: field_usize(&map, "radix-tree-keys"),
        radix_tree_nodes: field_usize(&map, "radix-tree-nodes"),
        // Presence-gated rather than version-gated: an older server simply
        // does not report the fields.
        idmp: map.iter().any(|(name, _)| name == "pids-tracked").then(|| StreamIdmp {
            pids_tracked: field_usize(&map, "pids-tracked"),
            iids_tracked: field_usize(&map, "iids-tracked"),
            iids_added: field_usize(&map, "iids-added"),
            iids_duplicates: field_usize(&map, "iids-duplicates"),
        }),
    });

    let groups_raw: Value = cmd("XINFO").arg("GROUPS").arg(key).query_async(conn).await?;
    let mut groups = Vec::new();
    for group in rows(&groups_raw) {
        let Some(map) = fields(group) else { continue };
        let name = field_text(&map, "name");
        let consumers_raw: Value = cmd("XINFO")
            .arg("CONSUMERS")
            .arg(key)
            .arg(&name)
            .query_async(conn)
            .await
            .unwrap_or(Value::Nil);
        let consumers = rows(&consumers_raw)
            .filter_map(fields)
            .map(|map| StreamConsumer {
                name: field_text(&map, "name"),
                pending: field_usize(&map, "pending"),
                idle_ms: field_int(&map, "idle"),
            })
            .collect();
        let pending_entries = pending_page(at, key, &name, "-").await.unwrap_or_default();
        groups.push(StreamGroup {
            consumers_count: field_usize(&map, "consumers"),
            pending_count: field_usize(&map, "pending"),
            last_delivered_id: field_text(&map, "last-delivered-id"),
            lag: field_int(&map, "lag"),
            pending_done: pending_entries.len() < PENDING_PAGE,
            name,
            consumers,
            pending_entries,
        });
    }
    Ok(StreamInfo { summary, groups })
}

/// One `XPENDING key group start + PENDING_PAGE` page, oldest first.
pub async fn pending_page(at: &ServerDb, key: &str, group: &str, start: &str) -> Result<Vec<StreamPending>> {
    let raw: Value = cmd("XPENDING")
        .arg(key)
        .arg(group)
        .arg(start)
        .arg("+")
        .arg(PENDING_PAGE)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(rows(&raw)
        .filter_map(|entry| {
            let [id, consumer, idle, delivered] = <[&Value; 4]>::try_from(&rows(entry).collect::<Vec<_>>()[..]).ok()?;
            Some(StreamPending {
                id: text_lossy(id).unwrap_or_default(),
                consumer: text_lossy(consumer).unwrap_or_default(),
                idle_ms: int(idle).unwrap_or_default(),
                delivery_count: int(delivered).unwrap_or_default(),
            })
        })
        .collect())
}

/// `XADD key id field value …`; `id` is `*` for a server-minted one. Answers
/// the id the entry got.
pub async fn stream_add(at: &ServerDb, key: &str, id: &str, fields: &[(String, String)]) -> Result<String> {
    let mut c = cmd("XADD");
    c.arg(key).arg(id);
    for (field, value) in fields {
        c.arg(field).arg(value);
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// `XDEL key id…`; answers how many entries were there to delete.
pub async fn stream_delete(at: &ServerDb, key: &str, ids: &[&str]) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut c = cmd("XDEL");
    c.arg(key);
    for id in ids {
        c.arg(*id);
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// `XTRIM`, with the reference policy where the server takes one.
pub async fn stream_trim(at: &ServerDb, key: &str, trim: &StreamTrim, policy: Option<StreamRefPolicy>) -> Result<i64> {
    let mut c = cmd("XTRIM");
    c.arg(key);
    match trim {
        StreamTrim::MaxLen(n) => c.arg("MAXLEN").arg(*n),
        StreamTrim::MinId(id) => c.arg("MINID").arg(id),
    };
    if let Some(policy) = policy {
        c.arg(policy.word());
    }
    Ok(c.query_async(&mut at.connection().await?).await?)
}

/// `XSETID key id` — move the stream's `last-generated-id`.
pub async fn stream_set_id(at: &ServerDb, key: &str, id: &str) -> Result<()> {
    Ok(cmd("XSETID")
        .arg(key)
        .arg(id)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `XGROUP CREATE key group start`.
pub async fn group_create(at: &ServerDb, key: &str, group: &str, start_id: &str) -> Result<()> {
    Ok(cmd("XGROUP")
        .arg("CREATE")
        .arg(key)
        .arg(group)
        .arg(start_id)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `XGROUP SETID key group id` — where the group reads from next.
pub async fn group_set_id(at: &ServerDb, key: &str, group: &str, id: &str) -> Result<()> {
    Ok(cmd("XGROUP")
        .arg("SETID")
        .arg(key)
        .arg(group)
        .arg(id)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `XGROUP DESTROY key group` — the group and its whole pending list.
pub async fn group_destroy(at: &ServerDb, key: &str, group: &str) -> Result<()> {
    Ok(cmd("XGROUP")
        .arg("DESTROY")
        .arg(key)
        .arg(group)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `XGROUP CREATECONSUMER key group consumer`; `false` when it already
/// existed.
pub async fn consumer_create(at: &ServerDb, key: &str, group: &str, consumer: &str) -> Result<bool> {
    let created: i64 = cmd("XGROUP")
        .arg("CREATECONSUMER")
        .arg(key)
        .arg(group)
        .arg(consumer)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(created == 1)
}

/// `XGROUP DELCONSUMER key group consumer`; answers how many pending
/// messages went with it.
pub async fn consumer_delete(at: &ServerDb, key: &str, group: &str, consumer: &str) -> Result<usize> {
    let pending: i64 = cmd("XGROUP")
        .arg("DELCONSUMER")
        .arg(key)
        .arg(group)
        .arg(consumer)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(pending.max(0) as usize)
}

/// `XACK key group id` — acknowledge one pending entry.
pub async fn stream_ack(at: &ServerDb, key: &str, group: &str, id: &str) -> Result<()> {
    let _: i64 = cmd("XACK")
        .arg(key)
        .arg(group)
        .arg(id)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(())
}

/// `XACKDEL key group KEEPREF IDS 1 id` (Redis 8.2+) — acknowledge one
/// pending entry *and* delete it from the stream in one atomic step.
/// `KEEPREF` matches classic `XDEL` semantics: other groups' references
/// stay. The per-id status codes (1 deleted, -1 not found, 2 refused under
/// `ACKED`) are not read — `KEEPREF` never yields 2.
pub async fn stream_ack_delete(at: &ServerDb, key: &str, group: &str, id: &str) -> Result<()> {
    let _: Value = cmd("XACKDEL")
        .arg(key)
        .arg(group)
        .arg(StreamRefPolicy::KeepRef.word())
        .arg("IDS")
        .arg(1)
        .arg(id)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(())
}

/// `XNACK key group FAIL IDS 1 id` (Redis 8.8+) — release one pending entry
/// back to the group PEL without acking: its consumer is cleared and it
/// moves to the head of the idle order, claimable at once. `FAIL` keeps the
/// delivery counter, so a retry policy still sees the failed attempt
/// (`SILENT` would undo it, `FATAL` would poison the entry).
pub async fn stream_nack(at: &ServerDb, key: &str, group: &str, id: &str) -> Result<()> {
    // The reply is how many ids were released (0 = not pending).
    let _: i64 = cmd("XNACK")
        .arg(key)
        .arg(group)
        .arg("FAIL")
        .arg("IDS")
        .arg(1)
        .arg(id)
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(())
}

/// `XCLAIM key group consumer 0 id JUSTID` — force-reassign one pending
/// entry (min-idle-time 0, so it always claims; `JUSTID` leaves the delivery
/// counter untouched).
pub async fn stream_claim(at: &ServerDb, key: &str, group: &str, consumer: &str, id: &str) -> Result<()> {
    let _: Value = cmd("XCLAIM")
        .arg(key)
        .arg(group)
        .arg(consumer)
        .arg(0)
        .arg(id)
        .arg("JUSTID")
        .query_async(&mut at.connection().await?)
        .await?;
    Ok(())
}

/// `XAUTOCLAIM key group consumer min-idle 0-0 COUNT n JUSTID` — claim a
/// batch of entries idle for long enough. Answers how many were claimed.
pub async fn stream_autoclaim(
    at: &ServerDb,
    key: &str,
    group: &str,
    consumer: &str,
    min_idle_ms: u64,
    count: usize,
) -> Result<usize> {
    let raw: Value = cmd("XAUTOCLAIM")
        .arg(key)
        .arg(group)
        .arg(consumer)
        .arg(min_idle_ms)
        .arg("0-0")
        .arg("COUNT")
        .arg(count)
        .arg("JUSTID")
        .query_async(&mut at.connection().await?)
        .await?;
    // Reply: [next-cursor, [claimed ids…], [deleted ids…]].
    Ok(rows(&raw).nth(1).map(|ids| rows(ids).count()).unwrap_or(0))
}

/// A flat `[field, value, field, value, …]` list as pairs; a field left
/// without a value is dropped.
fn pairs(flat: Vec<String>) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(flat.len() / 2);
    let mut items = flat.into_iter();
    while let (Some(field), Some(value)) = (items.next(), items.next()) {
        out.push((field, value));
    }
    out
}

/// The elements of an array (or a RESP3 set), and nothing for anything else.
fn rows(value: &Value) -> impl Iterator<Item = &Value> {
    match value {
        Value::Array(items) | Value::Set(items) => items.iter(),
        _ => [].iter(),
    }
}

/// An `XINFO` reply's fields. Lenient where [`crate::reply::pairs`] is
/// strict: an unreadable field name skips that field instead of failing the
/// reply, because a fork that reports one odd field still has a panel's worth
/// of readable ones. `None` only when the reply is not a listing at all.
fn fields(value: &Value) -> Option<Vec<(String, &Value)>> {
    match value {
        Value::Map(items) => Some(
            items
                .iter()
                .filter_map(|(name, value)| Some((text_lossy(name)?, value)))
                .collect(),
        ),
        Value::Array(items) => Some(
            items
                .chunks(2)
                .filter_map(|chunk| Some((text_lossy(chunk.first()?)?, chunk.get(1)?)))
                .collect(),
        ),
        _ => None,
    }
}

fn field<'a>(map: &[(String, &'a Value)], name: &str) -> Option<&'a Value> {
    map.iter().find(|(field, _)| field == name).map(|(_, value)| *value)
}

fn field_text(map: &[(String, &Value)], name: &str) -> String {
    field(map, name).and_then(text_lossy).unwrap_or_default()
}

fn field_usize(map: &[(String, &Value)], name: &str) -> usize {
    field(map, name).and_then(uint).unwrap_or_default() as usize
}

fn field_int(map: &[(String, &Value)], name: &str) -> i64 {
    field(map, name).and_then(int).unwrap_or_default()
}

/// The id out of an `XINFO STREAM` first-/last-entry pair (`[id, [f, v, …]]`).
fn entry_id(value: &Value) -> String {
    rows(value).next().and_then(text_lossy).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn xinfo_fields_are_read_from_both_protocols_and_a_bad_one_is_skipped() {
        let flat = Value::Array(vec![
            bulk("groups"),
            Value::Int(2),
            Value::Nil,
            Value::Int(9),
            bulk("lag"),
        ]);
        let map = fields(&flat).expect("a listing");
        assert_eq!(field_usize(&map, "groups"), 2);
        // A field without a readable name, and a name without a value, are
        // both skipped rather than failing the reply.
        assert_eq!(field_int(&map, "lag"), 0);
        assert_eq!(field_text(&map, "missing"), "");

        let resp3 = Value::Map(vec![(bulk("name"), bulk("g1")), (bulk("lag"), Value::Int(-1))]);
        let map = fields(&resp3).expect("a map");
        assert_eq!(field_text(&map, "name"), "g1");
        assert_eq!(field_int(&map, "lag"), -1, "a lag is signed; a count is not");
        assert_eq!(field_usize(&map, "lag"), 0);

        assert!(fields(&Value::Int(1)).is_none(), "not a listing at all");
    }

    #[test]
    fn an_entry_id_is_the_head_of_its_pair_and_pairs_drop_an_odd_tail() {
        let entry = Value::Array(vec![bulk("1-1"), Value::Array(vec![bulk("f"), bulk("v")])]);
        assert_eq!(entry_id(&entry), "1-1");
        assert_eq!(entry_id(&Value::Nil), "");

        let flat = ["f", "1", "g", "2", "odd"].map(str::to_string).to_vec();
        assert_eq!(pairs(flat), vec![("f".into(), "1".into()), ("g".into(), "2".into())]);
    }
}

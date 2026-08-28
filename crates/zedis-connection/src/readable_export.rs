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

//! Human-readable bulk export: fetch keys with their full values and
//! render them as JSON entries or CSV rows. The binary DUMP/RESTORE
//! export (`dump_restore.rs`) is for machine migration; this one is for
//! eyes and downstream tools. Values are decoded as lossy UTF-8 of the
//! raw bytes — no decompression or format detection, so the file shows
//! exactly what is stored.

use crate::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::{cmd, pipe};
use zedis_core::csv::build_csv_record;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Per-key fetch limits for [`read_readable_chunk`].
///
/// `Default` is what the app exports with; tests shrink the numbers to
/// exercise paging and truncation without huge fixtures.
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    /// Elements per round trip when a collection is paged. A collection
    /// at or under this size is fetched with the exact single command
    /// (`SMEMBERS` / `HGETALL`); a larger one switches to cursor / index
    /// / id paging so no single reply can balloon the server's output
    /// buffer or hold its event loop for the whole collection.
    pub page: usize,
    /// Ceiling on elements fetched per key: past it the value is cut and
    /// the entry marked [`ReadableEntry::truncated`]. The binary DUMP
    /// export has no cap and stays the full-fidelity path.
    pub max_elems: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            page: 5_000,
            max_elems: 100_000,
        }
    }
}

/// A fully-fetched value in display form.
#[derive(Debug)]
pub enum ReadableValue {
    Text(String),
    List(Vec<String>),
    Set(Vec<String>),
    /// Field/value pairs in `HGETALL` order.
    Hash(Vec<(String, String)>),
    /// Members with scores, ascending score order (`ZRANGE`).
    Zset(Vec<(String, f64)>),
    /// Entries as `(id, field/value pairs)` in `XRANGE` order.
    Stream(Vec<(String, Vec<(String, String)>)>),
}

/// One exported key. `value` is `None` for types this exporter cannot
/// render (module types) — the entry still records key/type/TTL.
#[derive(Debug)]
pub struct ReadableEntry {
    pub key: String,
    pub key_type: String,
    /// Remaining TTL in milliseconds, `-1` for none.
    pub pttl_ms: i64,
    pub value: Option<ReadableValue>,
    /// The collection hit [`ReadLimits::max_elems`] and was cut short.
    /// Surfaces as `"truncated": true` in JSON and a `truncated` CSV
    /// column, so a partial export can never pass for a complete one.
    pub truncated: bool,
}

impl ReadableEntry {
    /// Rough value payload size — for progress/log display, not accounting.
    pub fn approx_bytes(&self) -> u64 {
        let value_bytes = match &self.value {
            None => 0,
            Some(ReadableValue::Text(s)) => s.len(),
            Some(ReadableValue::List(items)) | Some(ReadableValue::Set(items)) => items.iter().map(String::len).sum(),
            Some(ReadableValue::Hash(pairs)) => pairs.iter().map(|(f, v)| f.len() + v.len()).sum(),
            Some(ReadableValue::Zset(pairs)) => pairs.iter().map(|(m, _)| m.len() + 8).sum(),
            Some(ReadableValue::Stream(entries)) => entries
                .iter()
                .map(|(id, fields)| id.len() + fields.iter().map(|(f, v)| f.len() + v.len()).sum::<usize>())
                .sum(),
        };
        value_bytes as u64
    }
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

fn lossy_pairs(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<(String, String)> {
    pairs.into_iter().map(|(a, b)| (lossy(a), lossy(b))).collect()
}

/// One raw `HSCAN` reply: `(cursor, field/value pairs)`.
type HashScanPage = (u64, Vec<(Vec<u8>, Vec<u8>)>);

/// One raw `XRANGE` page: `(id, field/value pairs)` per entry.
type RawStreamPage = Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)>;

/// List elements in index order, paged by `LRANGE start stop`. `LLEN` is
/// O(1), so the truncation verdict costs nothing extra.
async fn read_list(conn: &mut RedisAsyncConn, key: &str, limits: ReadLimits) -> Result<(Vec<String>, bool)> {
    let len: usize = cmd("LLEN").arg(key).query_async(conn).await?;
    let take = len.min(limits.max_elems);
    let mut items: Vec<String> = Vec::with_capacity(take.min(limits.page));
    while items.len() < take {
        let start = items.len();
        let stop = (start + limits.page).min(take) - 1;
        let page: Vec<Vec<u8>> = cmd("LRANGE").arg(key).arg(start).arg(stop).query_async(conn).await?;
        if page.is_empty() {
            // The list shrank between pages — export what we have.
            break;
        }
        items.extend(page.into_iter().map(lossy));
    }
    Ok((items, len > limits.max_elems))
}

/// Set members via `SSCAN` once the set outgrows one page. A SCAN
/// iteration may repeat an element across a rehash — for an oversized
/// set the export favors bounded replies over that edge of exactness
/// (small sets keep the exact `SMEMBERS`).
async fn read_set(conn: &mut RedisAsyncConn, key: &str, limits: ReadLimits) -> Result<(Vec<String>, bool)> {
    let card: usize = cmd("SCARD").arg(key).query_async(conn).await?;
    if card <= limits.page {
        let members: Vec<Vec<u8>> = cmd("SMEMBERS").arg(key).query_async(conn).await?;
        return Ok((members.into_iter().map(lossy).collect(), false));
    }
    let mut items: Vec<String> = Vec::with_capacity(limits.page);
    let mut cursor: u64 = 0;
    loop {
        let (next, page): (u64, Vec<Vec<u8>>) = cmd("SSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limits.page)
            .query_async(conn)
            .await?;
        items.extend(page.into_iter().map(lossy));
        cursor = next;
        if cursor == 0 {
            return Ok((items, false));
        }
        if items.len() >= limits.max_elems {
            items.truncate(limits.max_elems);
            return Ok((items, true));
        }
    }
}

/// Hash fields via `HSCAN` once the hash outgrows one page (same SCAN
/// caveat as [`read_set`]).
async fn read_hash(conn: &mut RedisAsyncConn, key: &str, limits: ReadLimits) -> Result<(Vec<(String, String)>, bool)> {
    let len: usize = cmd("HLEN").arg(key).query_async(conn).await?;
    if len <= limits.page {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = cmd("HGETALL").arg(key).query_async(conn).await?;
        return Ok((lossy_pairs(pairs), false));
    }
    let mut items: Vec<(String, String)> = Vec::with_capacity(limits.page);
    let mut cursor: u64 = 0;
    loop {
        let (next, page): HashScanPage = cmd("HSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limits.page)
            .query_async(conn)
            .await?;
        items.extend(page.into_iter().map(|(f, v)| (lossy(f), lossy(v))));
        cursor = next;
        if cursor == 0 {
            return Ok((items, false));
        }
        if items.len() >= limits.max_elems {
            items.truncate(limits.max_elems);
            return Ok((items, true));
        }
    }
}

/// Members with scores in ascending score order, paged by `ZRANGE start
/// stop` — index paging keeps the documented order exactly (`ZSCAN`
/// would not).
async fn read_zset(conn: &mut RedisAsyncConn, key: &str, limits: ReadLimits) -> Result<(Vec<(String, f64)>, bool)> {
    let card: usize = cmd("ZCARD").arg(key).query_async(conn).await?;
    let take = card.min(limits.max_elems);
    let mut items: Vec<(String, f64)> = Vec::with_capacity(take.min(limits.page));
    while items.len() < take {
        let start = items.len();
        let stop = (start + limits.page).min(take) - 1;
        let page: Vec<(Vec<u8>, f64)> = cmd("ZRANGE")
            .arg(key)
            .arg(start)
            .arg(stop)
            .arg("WITHSCORES")
            .query_async(conn)
            .await?;
        if page.is_empty() {
            break;
        }
        items.extend(page.into_iter().map(|(member, score)| (lossy(member), score)));
    }
    Ok((items, card > limits.max_elems))
}

/// The id right after `id` in `XRANGE` order (same millisecond, next
/// sequence). Paging with it works on every server — exclusive-start
/// ranges (`(id`) need Redis ≥ 6.2.
fn next_stream_id(id: &str) -> Option<String> {
    let (ms, seq) = id.split_once('-')?;
    let seq: u64 = seq.parse().ok()?;
    Some(format!("{ms}-{}", seq.checked_add(1)?))
}

/// Entries of one stream in display form: `(id, field/value pairs)`.
type StreamEntries = Vec<(String, Vec<(String, String)>)>;

/// Stream entries in id order, paged by `XRANGE start + COUNT page`.
async fn read_stream(conn: &mut RedisAsyncConn, key: &str, limits: ReadLimits) -> Result<(StreamEntries, bool)> {
    let mut entries: StreamEntries = Vec::new();
    let mut start = "-".to_string();
    loop {
        let page: RawStreamPage = cmd("XRANGE")
            .arg(key)
            .arg(&start)
            .arg("+")
            .arg("COUNT")
            .arg(limits.page)
            .query_async(conn)
            .await?;
        let page_len = page.len();
        let last_id = page.last().map(|(id, _)| id.clone());
        entries.extend(page.into_iter().map(|(id, fields)| (id, lossy_pairs(fields))));
        if page_len < limits.page {
            return Ok((entries, false));
        }
        if entries.len() >= limits.max_elems {
            entries.truncate(limits.max_elems);
            return Ok((entries, true));
        }
        match last_id.as_deref().and_then(next_stream_id) {
            Some(next) => start = next,
            // An id that isn't `ms-seq` (or seq at u64::MAX): stop rather
            // than loop forever, and mark the cut instead of hiding it.
            None => return Ok((entries, true)),
        }
    }
}

/// Fetch one chunk of keys with full values. Keys that vanished between
/// SCAN and the fetch (`TYPE` = none) are silently dropped, matching the
/// binary exporter. Cluster-safe: every command is keyed. Collections are
/// paged and capped per `limits` — see [`ReadLimits`].
pub async fn read_readable_chunk(
    conn: &mut RedisAsyncConn,
    keys: &[String],
    limits: ReadLimits,
) -> Result<Vec<ReadableEntry>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    // TYPE + PTTL for the whole chunk in one pipeline round-trip.
    let mut meta_pipe = pipe();
    for key in keys {
        meta_pipe.cmd("TYPE").arg(key).cmd("PTTL").arg(key);
    }
    let meta: Vec<(String, i64)> = meta_pipe.query_async(conn).await?;

    let mut entries = Vec::with_capacity(keys.len());
    for (key, (key_type, pttl_ms)) in keys.iter().zip(meta) {
        let (value, truncated) = match key_type.as_str() {
            "none" => continue,
            "string" => (
                Some(ReadableValue::Text(lossy(
                    cmd("GET").arg(key).query_async::<Vec<u8>>(conn).await?,
                ))),
                false,
            ),
            "list" => {
                let (items, truncated) = read_list(conn, key, limits).await?;
                (Some(ReadableValue::List(items)), truncated)
            }
            "set" => {
                let (items, truncated) = read_set(conn, key, limits).await?;
                (Some(ReadableValue::Set(items)), truncated)
            }
            "hash" => {
                let (items, truncated) = read_hash(conn, key, limits).await?;
                (Some(ReadableValue::Hash(items)), truncated)
            }
            "zset" => {
                let (items, truncated) = read_zset(conn, key, limits).await?;
                (Some(ReadableValue::Zset(items)), truncated)
            }
            "stream" => {
                let (items, truncated) = read_stream(conn, key, limits).await?;
                (Some(ReadableValue::Stream(items)), truncated)
            }
            // Module types (ReJSON, Bloom, TimeSeries, ...) have no
            // generic readable form — keep the key visible, value null.
            _ => (None, false),
        };
        entries.push(ReadableEntry {
            key: key.clone(),
            key_type,
            pttl_ms,
            value,
            truncated,
        });
    }
    Ok(entries)
}

fn value_to_json(value: &ReadableValue) -> serde_json::Value {
    use serde_json::{Value, json};
    match value {
        ReadableValue::Text(s) => json!(s),
        ReadableValue::List(items) | ReadableValue::Set(items) => json!(items),
        ReadableValue::Hash(pairs) => Value::Object(pairs.iter().map(|(f, v)| (f.clone(), json!(v))).collect()),
        ReadableValue::Zset(pairs) => json!(
            pairs
                .iter()
                .map(|(member, score)| json!({ "member": member, "score": score }))
                .collect::<Vec<_>>()
        ),
        ReadableValue::Stream(entries) => json!(
            entries
                .iter()
                .map(|(id, fields)| {
                    json!({
                        "id": id,
                        "fields": Value::Object(fields.iter().map(|(f, v)| (f.clone(), json!(v))).collect()),
                    })
                })
                .collect::<Vec<_>>()
        ),
    }
}

/// One JSON object per key: `{"key", "type", "ttl_ms", "value"}` —
/// `value` is `null` for module types, `ttl_ms` is `null` for no expiry.
/// A capped collection additionally carries `"truncated": true` (the
/// field is absent on complete entries, keeping their output unchanged).
pub fn entry_to_json(entry: &ReadableEntry) -> serde_json::Value {
    let mut json = serde_json::json!({
        "key": entry.key,
        "type": entry.key_type,
        "ttl_ms": if entry.pttl_ms >= 0 { Some(entry.pttl_ms) } else { None },
        "value": entry.value.as_ref().map(value_to_json),
    });
    if entry.truncated {
        json["truncated"] = serde_json::json!(true);
    }
    json
}

/// Column header for the CSV export (CRLF-terminated).
pub fn csv_header() -> String {
    build_csv_record(&["key", "type", "ttl_ms", "value", "truncated"])
}

/// One CSV row per key; non-string values are JSON-encoded into the
/// value cell (the pragmatic standard for nested data in flat CSV).
/// The `truncated` cell is `true` for a capped collection, else empty.
pub fn entry_to_csv(entry: &ReadableEntry) -> String {
    let ttl = if entry.pttl_ms >= 0 {
        entry.pttl_ms.to_string()
    } else {
        String::new()
    };
    let value = match &entry.value {
        Some(ReadableValue::Text(s)) => s.clone(),
        Some(other) => value_to_json(other).to_string(),
        None => String::new(),
    };
    let truncated = if entry.truncated { "true" } else { "" };
    build_csv_record(&[&entry.key, &entry.key_type, &ttl, &value, truncated])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: Option<ReadableValue>) -> ReadableEntry {
        ReadableEntry {
            key: "user:1".to_string(),
            key_type: "hash".to_string(),
            pttl_ms: 60_000,
            value,
            truncated: false,
        }
    }

    #[test]
    fn json_entry_shapes_per_type() {
        let e = entry(Some(ReadableValue::Hash(vec![("name".into(), "a".into())])));
        assert_eq!(
            entry_to_json(&e).to_string(),
            r#"{"key":"user:1","type":"hash","ttl_ms":60000,"value":{"name":"a"}}"#
        );

        let zset = ReadableEntry {
            key: "rank".into(),
            key_type: "zset".into(),
            pttl_ms: -1,
            value: Some(ReadableValue::Zset(vec![("a".into(), 1.5)])),
            truncated: false,
        };
        let json = entry_to_json(&zset);
        assert!(json["ttl_ms"].is_null());
        assert_eq!(json["value"][0]["member"], "a");
        assert_eq!(json["value"][0]["score"], 1.5);

        // Module type: value stays null but the key is still recorded.
        let module = entry(None);
        assert!(entry_to_json(&module)["value"].is_null());
    }

    #[test]
    fn truncation_is_marked_in_json_and_csv() {
        // Complete entries carry no "truncated" field at all — existing
        // consumers see byte-identical output.
        let complete = entry(Some(ReadableValue::Hash(vec![("name".into(), "a".into())])));
        assert!(entry_to_json(&complete).get("truncated").is_none());

        let mut cut = entry(Some(ReadableValue::Hash(vec![("name".into(), "a".into())])));
        cut.truncated = true;
        assert_eq!(
            entry_to_json(&cut).to_string(),
            r#"{"key":"user:1","type":"hash","ttl_ms":60000,"value":{"name":"a"},"truncated":true}"#
        );
        assert_eq!(
            entry_to_csv(&cut),
            "user:1,hash,60000,\"{\"\"name\"\":\"\"a\"\"}\",true\r\n"
        );
    }

    #[test]
    fn csv_rows_quote_and_embed_json() {
        let text = ReadableEntry {
            key: "greeting,1".into(),
            key_type: "string".into(),
            pttl_ms: -1,
            value: Some(ReadableValue::Text("hello \"world\"".into())),
            truncated: false,
        };
        assert_eq!(
            entry_to_csv(&text),
            "\"greeting,1\",string,,\"hello \"\"world\"\"\",\r\n"
        );

        let list = ReadableEntry {
            key: "l".into(),
            key_type: "list".into(),
            pttl_ms: 5,
            value: Some(ReadableValue::List(vec!["a".into(), "b".into()])),
            truncated: false,
        };
        assert_eq!(entry_to_csv(&list), "l,list,5,\"[\"\"a\"\",\"\"b\"\"]\",\r\n");

        assert_eq!(csv_header(), "key,type,ttl_ms,value,truncated\r\n");
    }

    #[test]
    fn next_stream_id_steps_the_sequence() {
        assert_eq!(next_stream_id("1526985054069-3").as_deref(), Some("1526985054069-4"));
        // Not `ms-seq`, or a sequence that cannot step: no next id.
        assert!(next_stream_id("garbage").is_none());
        assert!(next_stream_id("5-18446744073709551615").is_none());
    }
}

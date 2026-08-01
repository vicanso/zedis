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

/// A fully-fetched value in display form.
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
pub struct ReadableEntry {
    pub key: String,
    pub key_type: String,
    /// Remaining TTL in milliseconds, `-1` for none.
    pub pttl_ms: i64,
    pub value: Option<ReadableValue>,
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

fn lossy_pairs(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<(String, String)> {
    pairs.into_iter().map(|(a, b)| (lossy(a), lossy(b))).collect()
}

/// Fetch one chunk of keys with full values. Keys that vanished between
/// SCAN and the fetch (`TYPE` = none) are silently dropped, matching the
/// binary exporter. Cluster-safe: every command is keyed.
pub async fn read_readable_chunk(conn: &mut RedisAsyncConn, keys: &[String]) -> Result<Vec<ReadableEntry>> {
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
        let value = match key_type.as_str() {
            "none" => continue,
            "string" => Some(ReadableValue::Text(lossy(
                cmd("GET").arg(key).query_async::<Vec<u8>>(conn).await?,
            ))),
            "list" => Some(ReadableValue::List(
                cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(-1)
                    .query_async::<Vec<Vec<u8>>>(conn)
                    .await?
                    .into_iter()
                    .map(lossy)
                    .collect(),
            )),
            "set" => Some(ReadableValue::Set(
                cmd("SMEMBERS")
                    .arg(key)
                    .query_async::<Vec<Vec<u8>>>(conn)
                    .await?
                    .into_iter()
                    .map(lossy)
                    .collect(),
            )),
            "hash" => Some(ReadableValue::Hash(lossy_pairs(
                cmd("HGETALL")
                    .arg(key)
                    .query_async::<Vec<(Vec<u8>, Vec<u8>)>>(conn)
                    .await?,
            ))),
            "zset" => Some(ReadableValue::Zset(
                cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(-1)
                    .arg("WITHSCORES")
                    .query_async::<Vec<(Vec<u8>, f64)>>(conn)
                    .await?
                    .into_iter()
                    .map(|(member, score)| (lossy(member), score))
                    .collect(),
            )),
            "stream" => Some(ReadableValue::Stream(
                cmd("XRANGE")
                    .arg(key)
                    .arg("-")
                    .arg("+")
                    .query_async::<Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)>>(conn)
                    .await?
                    .into_iter()
                    .map(|(id, fields)| (id, lossy_pairs(fields)))
                    .collect(),
            )),
            // Module types (ReJSON, Bloom, TimeSeries, ...) have no
            // generic readable form — keep the key visible, value null.
            _ => None,
        };
        entries.push(ReadableEntry {
            key: key.clone(),
            key_type,
            pttl_ms,
            value,
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
pub fn entry_to_json(entry: &ReadableEntry) -> serde_json::Value {
    serde_json::json!({
        "key": entry.key,
        "type": entry.key_type,
        "ttl_ms": if entry.pttl_ms >= 0 { Some(entry.pttl_ms) } else { None },
        "value": entry.value.as_ref().map(value_to_json),
    })
}

/// Column header for the CSV export (CRLF-terminated).
pub fn csv_header() -> String {
    build_csv_record(&["key", "type", "ttl_ms", "value"])
}

/// One CSV row per key; non-string values are JSON-encoded into the
/// value cell (the pragmatic standard for nested data in flat CSV).
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
    build_csv_record(&[&entry.key, &entry.key_type, &ttl, &value])
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
    fn csv_rows_quote_and_embed_json() {
        let text = ReadableEntry {
            key: "greeting,1".into(),
            key_type: "string".into(),
            pttl_ms: -1,
            value: Some(ReadableValue::Text("hello \"world\"".into())),
        };
        assert_eq!(
            entry_to_csv(&text),
            "\"greeting,1\",string,,\"hello \"\"world\"\"\"\r\n"
        );

        let list = ReadableEntry {
            key: "l".into(),
            key_type: "list".into(),
            pttl_ms: 5,
            value: Some(ReadableValue::List(vec!["a".into(), "b".into()])),
        };
        assert_eq!(entry_to_csv(&list), "l,list,5,\"[\"\"a\"\",\"\"b\"\"]\"\r\n");

        assert_eq!(csv_header(), "key,type,ttl_ms,value\r\n");
    }
}

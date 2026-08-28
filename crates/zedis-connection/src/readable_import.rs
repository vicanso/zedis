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

//! Import half of the human-readable bulk formats: parse the JSON / CSV
//! files `readable_export.rs` writes — hand-edited or not — back into
//! entries and write them with type-native commands (`SET` / `RPUSH` /
//! `SADD` / `HSET` / `ZADD` / `XADD`), restoring TTLs via `PEXPIRE`.
//!
//! Readable files are parsed fully in memory: they exist for hand-edited,
//! human-scale data. The framed binary bundle (`dump_restore.rs`) stays
//! the streaming, full-fidelity path for big migrations.

use crate::async_connection::RedisAsyncConn;
use crate::dump_restore::{ConflictMode, ConflictPreview, keys_exist, preview_dump_conflicts};
use crate::error::Error;
use crate::manager::get_connection_manager;
use crate::readable_export::{ReadableEntry, ReadableValue};
use futures::future::try_join_all;
use redis::cmd;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use zedis_core::csv::parse_csv;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Elements per write command when a collection is written back, so one
/// imported 100k-element list becomes many bounded `RPUSH`es instead of
/// a single million-argument command.
const WRITE_PAGE: usize = 1_000;

/// Bytes read from the head of a file to sniff its format.
const SNIFF_LEN: usize = 4 * 1024;

/// On-disk format of an import file. Sniffed from content, never from
/// the extension — a renamed file still imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    /// The framed `DUMP`/`RESTORE` bundle (`.zedis-dump`).
    Binary,
    /// The JSON array written by the readable export.
    Json,
    /// The CSV table written by the readable export.
    Csv,
}

/// Classify the first bytes of an import file. `Binary` is the `ZDIS`
/// magic; a JSON array starts with `[`; CSV must open with the export's
/// own header row (`key,type,ttl_ms,value…`).
pub fn sniff_import_format(head: &[u8]) -> Result<ImportFormat> {
    if head.starts_with(crate::dump_restore::MAGIC_HEADER) {
        return Ok(ImportFormat::Binary);
    }
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('[') {
        return Ok(ImportFormat::Json);
    }
    if let Some(header) = parse_csv(trimmed).into_iter().next()
        && header.len() >= 4
        && header[..4] == ["key", "type", "ttl_ms", "value"]
    {
        return Ok(ImportFormat::Csv);
    }
    Err(Error::Invalid {
        message: "unrecognized import file — expected a .zedis-dump bundle, a JSON array export, or a CSV export"
            .to_string(),
    })
}

/// Sniff the format of the file at `path` (reads at most the first 4 KiB).
pub fn detect_import_format(path: &Path) -> Result<ImportFormat> {
    let mut head = vec![0u8; SNIFF_LEN];
    let mut file = fs::File::open(path)?;
    let mut filled = 0;
    while filled < head.len() {
        let n = file.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    sniff_import_format(&head)
}

/// A scalar cell/element: strings verbatim, numbers and booleans by their
/// JSON text — forgiving to hand edits like `"value": 42`.
fn scalar(value: &Value, context: &str) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other => Err(Error::Invalid {
            message: format!("{context}: expected a string, got {other}"),
        }),
    }
}

fn scalar_items(values: &[Value], key: &str) -> Result<Vec<String>> {
    values
        .iter()
        .map(|v| scalar(v, &format!("key `{key}`: collection element")))
        .collect()
}

/// Map one JSON `value` into a typed [`ReadableValue`], mirroring the
/// shapes `entry_to_json` writes. `None` for `null` (module types).
fn value_from_json(key: &str, key_type: &str, value: &Value) -> Result<Option<ReadableValue>> {
    if value.is_null() {
        return Ok(None);
    }
    let invalid = |expected: &str| Error::Invalid {
        message: format!("key `{key}` ({key_type}): expected {expected}"),
    };
    let parsed = match key_type {
        "string" => ReadableValue::Text(scalar(value, &format!("key `{key}` (string)"))?),
        "list" => ReadableValue::List(scalar_items(value.as_array().ok_or_else(|| invalid("an array"))?, key)?),
        "set" => ReadableValue::Set(scalar_items(value.as_array().ok_or_else(|| invalid("an array"))?, key)?),
        "hash" => {
            let object = value.as_object().ok_or_else(|| invalid("an object"))?;
            let mut pairs = Vec::with_capacity(object.len());
            for (field, v) in object {
                pairs.push((field.clone(), scalar(v, &format!("key `{key}`: hash field `{field}`"))?));
            }
            ReadableValue::Hash(pairs)
        }
        "zset" => {
            let items = value.as_array().ok_or_else(|| invalid("an array"))?;
            let mut pairs = Vec::with_capacity(items.len());
            for item in items {
                let member = item
                    .get("member")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("array items of {\"member\", \"score\"}"))?;
                let score = item
                    .get("score")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| invalid("array items of {\"member\", \"score\"}"))?;
                pairs.push((member.to_string(), score));
            }
            ReadableValue::Zset(pairs)
        }
        "stream" => {
            let items = value.as_array().ok_or_else(|| invalid("an array"))?;
            let mut entries = Vec::with_capacity(items.len());
            for item in items {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("array items of {\"id\", \"fields\"}"))?;
                let fields = item
                    .get("fields")
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid("array items of {\"id\", \"fields\"}"))?;
                let mut pairs = Vec::with_capacity(fields.len());
                for (field, v) in fields {
                    pairs.push((
                        field.clone(),
                        scalar(v, &format!("key `{key}`: stream field `{field}`"))?,
                    ));
                }
                entries.push((id.to_string(), pairs));
            }
            ReadableValue::Stream(entries)
        }
        other => {
            return Err(Error::Invalid {
                message: format!(
                    "key `{key}`: cannot import values of type `{other}` — remove the entry or set its value to null"
                ),
            });
        }
    };
    Ok(Some(parsed))
}

fn entry_from_json(item: &Value, index: usize) -> Result<ReadableEntry> {
    let object = item.as_object().ok_or_else(|| Error::Invalid {
        message: format!("entry #{}: expected a JSON object", index + 1),
    })?;
    let key = object
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid {
            message: format!("entry #{}: missing string field \"key\"", index + 1),
        })?
        .to_string();
    let key_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid {
            message: format!("key `{key}`: missing string field \"type\""),
        })?
        .to_string();
    let pttl_ms = match object.get("ttl_ms") {
        None | Some(Value::Null) => -1,
        Some(v) => v.as_i64().ok_or_else(|| Error::Invalid {
            message: format!("key `{key}`: \"ttl_ms\" must be a number or null"),
        })?,
    };
    let value = value_from_json(&key, &key_type, object.get("value").unwrap_or(&Value::Null))?;
    let truncated = object.get("truncated").and_then(Value::as_bool).unwrap_or(false);
    Ok(ReadableEntry {
        key,
        key_type,
        pttl_ms,
        value,
        truncated,
    })
}

fn entries_from_json(text: &str) -> Result<Vec<ReadableEntry>> {
    let root: Value = serde_json::from_str(text)?;
    let items = root.as_array().ok_or_else(|| Error::Invalid {
        message: "expected a JSON array of entries (the shape the JSON export writes)".to_string(),
    })?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| entry_from_json(item, index))
        .collect()
}

fn entries_from_csv(text: &str) -> Result<Vec<ReadableEntry>> {
    let mut records = parse_csv(text).into_iter();
    let header = records.next().ok_or_else(|| Error::Invalid {
        message: "empty CSV file".to_string(),
    })?;
    if header.len() < 4 || header[..4] != ["key", "type", "ttl_ms", "value"] {
        return Err(Error::Invalid {
            message: "CSV header must start with key,type,ttl_ms,value (the shape the CSV export writes)".to_string(),
        });
    }
    let mut entries = Vec::new();
    for (index, record) in records.enumerate() {
        let row = index + 2; // 1-based, after the header
        // 4 columns is the pre-`truncated` export shape; 5 the current one.
        if record.len() < 4 || record.len() > header.len().max(5) {
            return Err(Error::Invalid {
                message: format!("CSV row {row}: expected 4-5 fields, got {}", record.len()),
            });
        }
        let key = record[0].clone();
        let key_type = record[1].clone();
        let pttl_ms = if record[2].is_empty() {
            -1
        } else {
            record[2].parse::<i64>().map_err(|_| Error::Invalid {
                message: format!("CSV row {row} (key `{key}`): ttl_ms `{}` is not a number", record[2]),
            })?
        };
        let cell = record[3].as_str();
        let value = if key_type == "string" {
            // The string cell is the raw value, never JSON-encoded.
            Some(ReadableValue::Text(cell.to_string()))
        } else if cell.is_empty() {
            // Module types export an empty cell.
            None
        } else {
            let json: Value = serde_json::from_str(cell).map_err(|e| Error::Invalid {
                message: format!("CSV row {row} (key `{key}`): value cell is not valid JSON: {e}"),
            })?;
            value_from_json(&key, &key_type, &json)?
        };
        let truncated = record.get(4).map(|c| c == "true").unwrap_or(false);
        entries.push(ReadableEntry {
            key,
            key_type,
            pttl_ms,
            value,
            truncated,
        });
    }
    Ok(entries)
}

/// Parse a whole readable export file into entries. Fails fast with a
/// key- or row-addressed message — nothing has been written yet, so a
/// hand-edit typo surfaces before the import touches the server.
pub fn parse_readable_entries(text: &str, format: ImportFormat) -> Result<Vec<ReadableEntry>> {
    match format {
        ImportFormat::Json => entries_from_json(text),
        ImportFormat::Csv => entries_from_csv(text),
        ImportFormat::Binary => Err(Error::Invalid {
            message: "binary dumps go through DumpReader, not the readable parser".to_string(),
        }),
    }
}

/// Outcome of writing one readable entry. Unlike the binary path's
/// `RestoreStatus`, skips carry their reason — the readable formats have
/// several (conflict, truncated export, module type, empty collection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadableWriteStatus {
    Written,
    /// Destination key exists and the conflict mode is Skip.
    SkippedExists,
    /// The export marked this entry truncated — importing knowingly
    /// partial data would corrupt; delete the `truncated` marker in the
    /// file to force it.
    SkippedTruncated,
    /// Module type (`value` is null) — nothing to write.
    SkippedUnsupported,
    /// An empty collection cannot be materialized as a key.
    SkippedEmpty,
    Failed(String),
}

/// Write a chunk of parsed entries, one future per entry (mirrors
/// `restore_keys_chunk`). Statuses come back in entry order.
pub async fn write_readable_chunk(
    conn: &mut RedisAsyncConn,
    entries: &[ReadableEntry],
    conflict: ConflictMode,
) -> Result<Vec<ReadableWriteStatus>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let futures = entries.iter().map(|entry| {
        let mut c = conn.clone();
        async move { write_single_entry(&mut c, entry, conflict).await }
    });
    try_join_all(futures).await
}

async fn write_single_entry(
    conn: &mut RedisAsyncConn,
    entry: &ReadableEntry,
    conflict: ConflictMode,
) -> Result<ReadableWriteStatus> {
    let Some(value) = &entry.value else {
        return Ok(ReadableWriteStatus::SkippedUnsupported);
    };
    if entry.truncated {
        return Ok(ReadableWriteStatus::SkippedTruncated);
    }
    let empty = match value {
        ReadableValue::Text(_) => false,
        ReadableValue::List(items) | ReadableValue::Set(items) => items.is_empty(),
        ReadableValue::Hash(pairs) => pairs.is_empty(),
        ReadableValue::Zset(pairs) => pairs.is_empty(),
        ReadableValue::Stream(items) => items.is_empty(),
    };
    if empty {
        return Ok(ReadableWriteStatus::SkippedEmpty);
    }

    // Unlike RESTORE's atomic BUSYKEY, a check-then-write races a
    // concurrent writer — acceptable for a hand-run import job.
    match conflict {
        ConflictMode::Overwrite => {
            cmd("DEL").arg(&entry.key).exec_async(conn).await?;
        }
        ConflictMode::Skip | ConflictMode::Abort => {
            let exists: i64 = cmd("EXISTS").arg(&entry.key).query_async(conn).await?;
            if exists > 0 {
                return match conflict {
                    ConflictMode::Skip => Ok(ReadableWriteStatus::SkippedExists),
                    _ => Err(Error::Invalid {
                        message: format!("key {} already exists at destination", entry.key),
                    }),
                };
            }
        }
    }

    match write_value(conn, &entry.key, value, entry.pttl_ms).await {
        Ok(()) => Ok(ReadableWriteStatus::Written),
        Err(e) => Ok(ReadableWriteStatus::Failed(e.to_string())),
    }
}

async fn write_value(conn: &mut RedisAsyncConn, key: &str, value: &ReadableValue, pttl_ms: i64) -> Result<()> {
    match value {
        ReadableValue::Text(text) => {
            cmd("SET").arg(key).arg(text).exec_async(conn).await?;
        }
        ReadableValue::List(items) => {
            for page in items.chunks(WRITE_PAGE) {
                let mut push = cmd("RPUSH");
                push.arg(key);
                for item in page {
                    push.arg(item);
                }
                push.exec_async(conn).await?;
            }
        }
        ReadableValue::Set(items) => {
            for page in items.chunks(WRITE_PAGE) {
                let mut add = cmd("SADD");
                add.arg(key);
                for item in page {
                    add.arg(item);
                }
                add.exec_async(conn).await?;
            }
        }
        ReadableValue::Hash(pairs) => {
            for page in pairs.chunks(WRITE_PAGE) {
                let mut set = cmd("HSET");
                set.arg(key);
                for (field, v) in page {
                    set.arg(field).arg(v);
                }
                set.exec_async(conn).await?;
            }
        }
        ReadableValue::Zset(pairs) => {
            for page in pairs.chunks(WRITE_PAGE) {
                let mut add = cmd("ZADD");
                add.arg(key);
                for (member, score) in page {
                    add.arg(*score).arg(member);
                }
                add.exec_async(conn).await?;
            }
        }
        ReadableValue::Stream(entries) => {
            // Original ids preserved; XRANGE order is ascending, which
            // XADD requires.
            for (id, fields) in entries {
                let mut add = cmd("XADD");
                add.arg(key).arg(id);
                for (field, v) in fields {
                    add.arg(field).arg(v);
                }
                add.exec_async(conn).await?;
            }
        }
    }
    if pttl_ms > 0 {
        cmd("PEXPIRE").arg(key).arg(pttl_ms).exec_async(conn).await?;
    }
    Ok(())
}

/// Format-aware dry-run conflict scan for the import window: binary
/// bundles stream through [`preview_dump_conflicts`]; readable files are
/// parsed and their keys batch-`EXISTS`-checked.
pub async fn preview_import_conflicts(
    server_id: &str,
    db: usize,
    input_path: PathBuf,
    sample_limit: usize,
    cancel: &AtomicBool,
) -> Result<ConflictPreview> {
    let path_for_sniff = input_path.clone();
    let format = smol::unblock(move || detect_import_format(&path_for_sniff)).await?;
    if format == ImportFormat::Binary {
        return preview_dump_conflicts(server_id, db, input_path, sample_limit, cancel).await;
    }

    let entries = smol::unblock(move || -> Result<Vec<ReadableEntry>> {
        let text = fs::read_to_string(&input_path)?;
        parse_readable_entries(&text, format)
    })
    .await?;

    let client = get_connection_manager().get_client(server_id, db).await?;
    let mut conn = client.connection();
    let mut preview = ConflictPreview::default();
    const BATCH: usize = 64;
    for chunk in entries.chunks(BATCH) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let keys: Vec<Vec<u8>> = chunk.iter().map(|e| e.key.as_bytes().to_vec()).collect();
        let exists = keys_exist(&mut conn, &keys).await?;
        for (entry, is_there) in chunk.iter().zip(exists) {
            preview.total += 1;
            if is_there {
                preview.conflicting += 1;
                if preview.sample_keys.len() < sample_limit {
                    preview.sample_keys.push(entry.key.clone());
                }
            } else {
                preview.free += 1;
            }
        }
    }
    preview.cancelled = cancel.load(Ordering::Acquire);
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readable_export::{csv_header, entry_to_csv, entry_to_json};

    fn sample_entries() -> Vec<ReadableEntry> {
        vec![
            ReadableEntry {
                key: "s".into(),
                key_type: "string".into(),
                pttl_ms: 60_000,
                value: Some(ReadableValue::Text("hello \"world\",\nline2".into())),
                truncated: false,
            },
            ReadableEntry {
                key: "l".into(),
                key_type: "list".into(),
                pttl_ms: -1,
                value: Some(ReadableValue::List(vec!["a".into(), "b".into()])),
                truncated: false,
            },
            ReadableEntry {
                key: "h".into(),
                key_type: "hash".into(),
                pttl_ms: -1,
                value: Some(ReadableValue::Hash(vec![("f".into(), "v".into())])),
                truncated: false,
            },
            ReadableEntry {
                key: "z".into(),
                key_type: "zset".into(),
                pttl_ms: -1,
                value: Some(ReadableValue::Zset(vec![("m".into(), 1.5)])),
                truncated: true,
            },
            ReadableEntry {
                key: "x".into(),
                key_type: "stream".into(),
                pttl_ms: -1,
                value: Some(ReadableValue::Stream(vec![(
                    "1-1".into(),
                    vec![("n".into(), "0".into())],
                )])),
                truncated: false,
            },
            ReadableEntry {
                key: "mod".into(),
                key_type: "ReJSON-RL".into(),
                pttl_ms: -1,
                value: None,
                truncated: false,
            },
        ]
    }

    fn assert_entries_match(parsed: &[ReadableEntry], expected: &[ReadableEntry]) {
        assert_eq!(parsed.len(), expected.len());
        for (a, b) in parsed.iter().zip(expected) {
            assert_eq!(a.key, b.key);
            assert_eq!(a.key_type, b.key_type);
            assert_eq!(a.pttl_ms, b.pttl_ms);
            assert_eq!(a.truncated, b.truncated);
            match (&a.value, &b.value) {
                (None, None) => {}
                (Some(ReadableValue::Text(x)), Some(ReadableValue::Text(y))) => assert_eq!(x, y),
                (Some(ReadableValue::List(x)), Some(ReadableValue::List(y))) => assert_eq!(x, y),
                (Some(ReadableValue::Set(x)), Some(ReadableValue::Set(y))) => assert_eq!(x, y),
                (Some(ReadableValue::Hash(x)), Some(ReadableValue::Hash(y))) => assert_eq!(x, y),
                (Some(ReadableValue::Zset(x)), Some(ReadableValue::Zset(y))) => assert_eq!(x, y),
                (Some(ReadableValue::Stream(x)), Some(ReadableValue::Stream(y))) => assert_eq!(x, y),
                other => panic!("value shape mismatch for `{}`: {other:?}", a.key),
            }
        }
    }

    #[test]
    fn json_round_trips_through_the_export_shape() {
        let entries = sample_entries();
        let doc = serde_json::Value::Array(entries.iter().map(entry_to_json).collect()).to_string();
        assert_eq!(sniff_import_format(doc.as_bytes()).expect("sniff"), ImportFormat::Json);
        let parsed = parse_readable_entries(&doc, ImportFormat::Json).expect("parse");
        assert_entries_match(&parsed, &entries);
    }

    #[test]
    fn csv_round_trips_through_the_export_shape() {
        let entries = sample_entries();
        let mut doc = csv_header();
        for entry in &entries {
            doc.push_str(&entry_to_csv(entry));
        }
        assert_eq!(sniff_import_format(doc.as_bytes()).expect("sniff"), ImportFormat::Csv);
        let parsed = parse_readable_entries(&doc, ImportFormat::Csv).expect("parse");
        assert_entries_match(&parsed, &entries);
    }

    #[test]
    fn csv_accepts_the_old_four_column_shape() {
        let doc = "key,type,ttl_ms,value\r\na,string,,hello\r\n";
        let parsed = parse_readable_entries(doc, ImportFormat::Csv).expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].pttl_ms, -1);
        assert!(!parsed[0].truncated);
        assert!(matches!(&parsed[0].value, Some(ReadableValue::Text(s)) if s == "hello"));
    }

    #[test]
    fn parse_errors_name_the_offending_entry() {
        let bad_ttl = r#"[{"key":"a","type":"string","ttl_ms":"soon","value":"x"}]"#;
        let err = parse_readable_entries(bad_ttl, ImportFormat::Json).expect_err("bad ttl");
        assert!(err.to_string().contains("`a`"), "{err}");

        let bad_shape = r#"[{"key":"b","type":"list","value":"not-an-array"}]"#;
        let err = parse_readable_entries(bad_shape, ImportFormat::Json).expect_err("bad shape");
        assert!(err.to_string().contains("`b`"), "{err}");

        let bad_row = "key,type,ttl_ms,value\r\nonly-one-field\r\n";
        let err = parse_readable_entries(bad_row, ImportFormat::Csv).expect_err("bad row");
        assert!(err.to_string().contains("row 2"), "{err}");

        let module_value = r#"[{"key":"c","type":"ReJSON-RL","value":{"x":1}}]"#;
        let err = parse_readable_entries(module_value, ImportFormat::Json).expect_err("module value");
        assert!(err.to_string().contains("ReJSON-RL"), "{err}");
    }

    #[test]
    fn sniff_rejects_unknown_content() {
        assert!(sniff_import_format(b"ZDIS\x01\x00").is_ok());
        assert!(sniff_import_format(b"random text").is_err());
        assert!(sniff_import_format(b"a,b,c\r\n1,2,3\r\n").is_err());
    }
}

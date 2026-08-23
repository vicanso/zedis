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

//! Local recycle bin for deleted keys.
//!
//! A soft-deleted key is stored as its `DUMP` payload plus a small JSON
//! meta document, keyed by `(server_id, id)` where `id` is
//! `"{deleted_at_ms:020}:{db}:{key}"` — the zero-padded timestamp prefix
//! keeps range scans in deletion order. Values are framed as
//! `[u32 LE meta_len][meta_json][payload]` so listings can decode the
//! meta without copying the (potentially large) payload.

use super::{Result, TRASH_TABLE, get_database};
use crate::error::Error;
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

/// How long a trashed key stays restorable.
pub const TRASH_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
/// Keys whose `DUMP` payload exceeds this are deleted permanently instead
/// of trashed. Deliberately small: the bin protects against fat-finger
/// deletes of ordinary keys (sessions, configs, JSON documents — almost
/// always well under 1MB), while deleting a multi-MB big key is usually
/// intentional cleanup that should not leave a copy on local disk. It also
/// bounds the redb file's high-water mark, which never shrinks.
pub const TRASH_MAX_PAYLOAD: usize = 1024 * 1024;
/// Pre-DUMP gate: skip trashing entirely when `MEMORY USAGE` estimates the
/// value above this, so a huge value is never serialized/transferred just
/// to be discarded. 4× the payload cap because the in-memory footprint
/// typically overestimates the (serialized, compressed) DUMP size several
/// fold; mid-size values fall through to the exact [`TRASH_MAX_PAYLOAD`]
/// check after DUMP.
pub const TRASH_MAX_VALUE_MEMORY: i64 = 4 * 1024 * 1024;

/// `#[serde(default)]` for the same reason as [`crate::ProtoConfig`], even
/// though these rows expire after 24h.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct TrashMetaDoc {
    v: u8,
    key: String,
    db: usize,
    /// Remaining TTL at deletion (ms); `<= 0` means no expiry.
    pttl_ms: i64,
    deleted_at_ms: i64,
}

/// Listing row: everything except the payload.
#[derive(Debug, Clone)]
pub struct TrashMeta {
    /// Table row id, pass back to restore/remove.
    pub id: String,
    pub key: String,
    pub db: usize,
    pub pttl_ms: i64,
    pub deleted_at_ms: i64,
}

/// One full trashed key (meta + `DUMP` payload).
#[derive(Debug)]
pub struct TrashEntry {
    pub key: String,
    pub db: usize,
    pub pttl_ms: i64,
    pub deleted_at_ms: i64,
    pub payload: Vec<u8>,
}

fn trash_id(deleted_at_ms: i64, db: usize, key: &str) -> String {
    format!("{deleted_at_ms:020}:{db}:{key}")
}

fn encode_value(entry: &TrashEntry) -> Result<Vec<u8>> {
    let meta = serde_json::to_vec(&TrashMetaDoc {
        v: 1,
        key: entry.key.clone(),
        db: entry.db,
        pttl_ms: entry.pttl_ms,
        deleted_at_ms: entry.deleted_at_ms,
    })?;
    let mut value = Vec::with_capacity(4 + meta.len() + entry.payload.len());
    value.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    value.extend_from_slice(&meta);
    value.extend_from_slice(&entry.payload);
    Ok(value)
}

/// Split a stored value into its meta document and payload slice.
fn decode_value(value: &[u8]) -> Result<(TrashMetaDoc, &[u8])> {
    let invalid = || Error::Invalid {
        message: "corrupted trash entry".to_string(),
    };
    let len_bytes: [u8; 4] = value.get(..4).and_then(|b| b.try_into().ok()).ok_or_else(invalid)?;
    let meta_len = u32::from_le_bytes(len_bytes) as usize;
    let meta_bytes = value.get(4..4 + meta_len).ok_or_else(invalid)?;
    let payload = value.get(4 + meta_len..).ok_or_else(invalid)?;
    let meta: TrashMetaDoc = serde_json::from_slice(meta_bytes)?;
    Ok((meta, payload))
}

pub fn insert_trash_entry(server_id: &str, entry: &TrashEntry) -> Result<()> {
    let id = trash_id(entry.deleted_at_ms, entry.db, &entry.key);
    let value = encode_value(entry)?;
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(TRASH_TABLE)?;
        table.insert((server_id, id.as_str()), value.as_slice())?;
    }
    write_txn.commit()?;
    Ok(())
}

/// Metas of every trashed key for `server_id`, newest first. Payloads are
/// not copied.
pub fn list_trash_meta(server_id: &str) -> Result<Vec<TrashMeta>> {
    let db = get_database()?;
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(TRASH_TABLE)?;
    let mut rows = vec![];
    for item in table.range((server_id, "")..=(server_id, "\u{10FFFF}"))? {
        let (key, value) = item?;
        let Ok((meta, _payload)) = decode_value(value.value()) else {
            continue;
        };
        rows.push(TrashMeta {
            id: key.value().1.to_string(),
            key: meta.key,
            db: meta.db,
            pttl_ms: meta.pttl_ms,
            deleted_at_ms: meta.deleted_at_ms,
        });
    }
    rows.reverse();
    Ok(rows)
}

/// The full entry (payload included) for a listing row id.
pub fn get_trash_entry(server_id: &str, id: &str) -> Result<Option<TrashEntry>> {
    let db = get_database()?;
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(TRASH_TABLE)?;
    let Some(value) = table.get((server_id, id))? else {
        return Ok(None);
    };
    let (meta, payload) = decode_value(value.value())?;
    Ok(Some(TrashEntry {
        key: meta.key,
        db: meta.db,
        pttl_ms: meta.pttl_ms,
        deleted_at_ms: meta.deleted_at_ms,
        payload: payload.to_vec(),
    }))
}

pub fn remove_trash_entry(server_id: &str, id: &str) -> Result<()> {
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(TRASH_TABLE)?;
        table.remove((server_id, id))?;
    }
    write_txn.commit()?;
    Ok(())
}

/// Delete entries of `server_id` trashed before `before_ms`; returns how
/// many were removed. The id's zero-padded timestamp prefix makes this a
/// prefix comparison, no value decoding needed.
pub fn purge_trash(server_id: &str, before_ms: i64) -> Result<usize> {
    let cutoff = format!("{before_ms:020}");
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    let removed = {
        let mut table = write_txn.open_table(TRASH_TABLE)?;
        let expired: Vec<String> = table
            .range((server_id, "")..(server_id, cutoff.as_str()))?
            .filter_map(|item| item.ok())
            .map(|(key, _)| key.value().1.to_string())
            .collect();
        for id in &expired {
            table.remove((server_id, id.as_str()))?;
        }
        expired.len()
    };
    write_txn.commit()?;
    Ok(removed)
}

/// Delete expired entries across **all** servers. Called once at startup
/// so the 24h retention holds even when no other purge trigger (a new
/// soft delete, opening the bin) ever fires for a server.
pub fn purge_all_trash(before_ms: i64) -> Result<usize> {
    let cutoff = format!("{before_ms:020}");
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    let removed = {
        let mut table = write_txn.open_table(TRASH_TABLE)?;
        let expired: Vec<(String, String)> = table
            .iter()?
            .filter_map(|item| item.ok())
            .filter_map(|(key, _)| {
                let (server_id, id) = key.value();
                // The zero-padded timestamp prefix makes expiry a plain
                // string comparison.
                if id < cutoff.as_str() {
                    Some((server_id.to_string(), id.to_string()))
                } else {
                    None
                }
            })
            .collect();
        for (server_id, id) in &expired {
            table.remove((server_id.as_str(), id.as_str()))?;
        }
        expired.len()
    };
    write_txn.commit()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::fs::override_config_dir;

    fn entry(key: &str, db: usize, deleted_at_ms: i64) -> TrashEntry {
        TrashEntry {
            key: key.to_string(),
            db,
            pttl_ms: 5000,
            deleted_at_ms,
            payload: format!("dump-of-{key}").into_bytes(),
        }
    }

    #[test]
    fn trash_roundtrip_purge_and_isolation() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        insert_trash_entry("srv-t", &entry("user:1", 0, 1000)).expect("insert old");
        insert_trash_entry("srv-t", &entry("user:2", 3, 2000)).expect("insert new");
        insert_trash_entry("srv-other", &entry("user:9", 0, 1500)).expect("insert other");

        // Newest first, payload-free metas.
        let metas = list_trash_meta("srv-t").expect("list");
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].key, "user:2");
        assert_eq!(metas[0].db, 3);
        assert_eq!(metas[1].key, "user:1");

        // Full entry retrieval restores the payload bytes.
        let full = get_trash_entry("srv-t", &metas[0].id).expect("get").expect("present");
        assert_eq!(full.payload, b"dump-of-user:2");
        assert_eq!(full.pttl_ms, 5000);

        // Purge removes only the old row of the given server.
        assert_eq!(purge_trash("srv-t", 1500).expect("purge"), 1);
        assert_eq!(list_trash_meta("srv-t").expect("list").len(), 1);
        assert_eq!(list_trash_meta("srv-other").expect("list").len(), 1);

        // Explicit removal empties the bin.
        remove_trash_entry("srv-t", &metas[0].id).expect("remove");
        assert!(list_trash_meta("srv-t").expect("list").is_empty());
    }

    #[test]
    fn purge_all_trash_sweeps_every_server() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        // Timestamps below every other trash test's rows (>= 1000) so this
        // sweep can't interfere with them when tests run in parallel.
        insert_trash_entry("srv-pa-1", &entry("old:1", 0, 100)).expect("insert old 1");
        insert_trash_entry("srv-pa-2", &entry("old:2", 1, 200)).expect("insert old 2");
        insert_trash_entry("srv-pa-1", &entry("fresh:1", 0, 800)).expect("insert fresh");

        let removed = purge_all_trash(500).expect("purge all");
        assert!(removed >= 2, "expected both expired rows swept, removed {removed}");
        let remaining = list_trash_meta("srv-pa-1").expect("list srv-pa-1");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, "fresh:1");
        assert!(list_trash_meta("srv-pa-2").expect("list srv-pa-2").is_empty());
    }
}

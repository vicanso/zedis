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

//! Framed dump/restore codec and Redis I/O helpers.
//!
//! File layout (little-endian throughout):
//!
//! ```text
//! "ZDIS"          (4 bytes)  magic
//! u16             format version
//! u16             flags (reserved)
//! u32             header_len
//! header_len B    header JSON
//! repeated entries:
//!   u32           key_len
//!   key_len B     key bytes (raw)
//!   i64           pttl_ms (-1 = no TTL)
//!   u8            type_hint (0=unknown,1=string,2=list,3=set,4=zset,5=hash,6=stream)
//!   u32           payload_len
//!   payload_len B payload bytes (Redis DUMP output)
//! "ZEND"          (4 bytes)  footer magic
//! u32             CRC32 over every byte before the footer magic
//! ```

use super::conn::RedisAsyncConn;
use super::manager::get_connection_manager;
#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::error::Error;
use futures::future::try_join_all;
use redis::cmd;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

type Result<T, E = Error> = std::result::Result<T, E>;

// The `.zdis` file format — its reader, writer and checksum — is desktop
// only: there is no file in a browser tab. The commands behind it (`DUMP`,
// `RESTORE`, `EXISTS`) are what copy-key runs, and those travel through the
// bridge like any other (ADR 9). One gated module rather than a gate per
// item (CLAUDE.md, *Desktop first*).
#[cfg(not(target_family = "wasm"))]
mod file;
#[cfg(not(target_family = "wasm"))]
pub(crate) use file::MAGIC_HEADER;
#[cfg(not(target_family = "wasm"))]
pub use file::{DumpHeader, DumpReader, DumpWriter, preview_dump_conflicts};

/// Type hint stored alongside each entry. Display-only; restore does not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TypeHint {
    Unknown = 0,
    String = 1,
    List = 2,
    Set = 3,
    ZSet = 4,
    Hash = 5,
    Stream = 6,
}

impl TypeHint {
    #[cfg(not(target_family = "wasm"))]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::String,
            2 => Self::List,
            3 => Self::Set,
            4 => Self::ZSet,
            5 => Self::Hash,
            6 => Self::Stream,
            _ => Self::Unknown,
        }
    }

    fn from_redis_type(s: &str) -> Self {
        match s {
            "string" => Self::String,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::ZSet,
            "hash" => Self::Hash,
            "stream" => Self::Stream,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DumpEntry {
    pub key: Vec<u8>,
    /// -1 means no TTL.
    pub pttl_ms: i64,
    pub type_hint: TypeHint,
    pub payload: Vec<u8>,
}

/// What to do when `RESTORE` hits an existing key (`BUSYKEY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictMode {
    /// Leave the destination key unchanged (default).
    #[default]
    Skip,
    /// `RESTORE … REPLACE` — overwrite the destination.
    Overwrite,
    /// Stop the whole import on the first conflict.
    Abort,
}

impl ConflictMode {
    pub const ALL: [ConflictMode; 3] = [ConflictMode::Skip, ConflictMode::Overwrite, ConflictMode::Abort];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(ConflictMode::Skip)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&m| m == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreStatus {
    Written,
    Skipped,
    Failed(String),
}

// ---------------------------------------------------------------------------
// Redis I/O
// ---------------------------------------------------------------------------

/// Dumps a slice of keys with bounded concurrency. Missing or expired keys are skipped.
pub async fn dump_keys_chunk(conn: &mut RedisAsyncConn, keys: &[String]) -> Result<Vec<DumpEntry>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let futures = keys.iter().map(|key| {
        let mut c = conn.clone();
        let key = key.clone();
        async move { dump_single_key(&mut c, key).await }
    });
    let results = try_join_all(futures).await?;
    Ok(results.into_iter().flatten().collect())
}

async fn dump_single_key(conn: &mut RedisAsyncConn, key: String) -> Result<Option<DumpEntry>> {
    let key_str = key.as_str();
    // All three commands target the same key, so they hit the same cluster slot —
    // safe to pipeline. Folding them into one round-trip is ~3x faster than three
    // sequential awaits.
    let (pttl, ty, payload): (i64, String, Option<Vec<u8>>) = redis::pipe()
        .cmd("PTTL")
        .arg(key_str)
        .cmd("TYPE")
        .arg(key_str)
        .cmd("DUMP")
        .arg(key_str)
        .query_async(conn)
        .await?;
    // DUMP returning nil is the authoritative "key gone" signal; PTTL == -2 / TYPE == "none"
    // can race against expiration but DUMP cannot lie about whether it produced bytes.
    let Some(payload) = payload else {
        return Ok(None);
    };
    Ok(Some(DumpEntry {
        key: key_str.as_bytes().to_vec(),
        pttl_ms: if pttl < 0 { -1 } else { pttl },
        type_hint: TypeHint::from_redis_type(&ty),
        payload,
    }))
}

/// Restores a slice of entries. Concurrency is bounded by the slice length.
pub async fn restore_keys_chunk(
    conn: &mut RedisAsyncConn,
    entries: &[DumpEntry],
    conflict: ConflictMode,
) -> Result<Vec<RestoreStatus>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let futures = entries.iter().map(|entry| {
        let mut c = conn.clone();
        let entry = entry.clone();
        async move { restore_single_key(&mut c, entry, conflict).await }
    });
    try_join_all(futures).await
}

async fn restore_single_key(
    conn: &mut RedisAsyncConn,
    entry: DumpEntry,
    conflict: ConflictMode,
) -> Result<RestoreStatus> {
    let ttl_arg: i64 = if entry.pttl_ms < 0 { 0 } else { entry.pttl_ms };
    let mut command = cmd("RESTORE");
    command.arg(&entry.key).arg(ttl_arg).arg(&entry.payload);
    if matches!(conflict, ConflictMode::Overwrite) {
        command.arg("REPLACE");
    }
    match command.query_async::<()>(conn).await {
        Ok(()) => Ok(RestoreStatus::Written),
        Err(err) => {
            // Redis returns `BUSYKEY Target key name already exists.` when a key is present
            // and REPLACE wasn't supplied.
            let msg = err.to_string();
            if msg.contains("BUSYKEY") {
                match conflict {
                    ConflictMode::Skip => Ok(RestoreStatus::Skipped),
                    ConflictMode::Abort => Err(Error::Invalid {
                        message: format!(
                            "key {} already exists at destination",
                            String::from_utf8_lossy(&entry.key)
                        ),
                    }),
                    ConflictMode::Overwrite => Ok(RestoreStatus::Failed(msg)),
                }
            } else {
                Ok(RestoreStatus::Failed(msg))
            }
        }
    }
}

/// Batch `EXISTS` for binary key names. Returns one bool per key (order preserved).
pub async fn keys_exist(conn: &mut RedisAsyncConn, keys: &[Vec<u8>]) -> Result<Vec<bool>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut pipe = redis::pipe();
    for key in keys {
        pipe.cmd("EXISTS").arg(key.as_slice());
    }
    let results: Vec<i64> = pipe.query_async(conn).await?;
    Ok(results.into_iter().map(|n| n > 0).collect())
}

/// Dry-run conflict scan for a server-to-server copy: `EXISTS` for every
/// key on the destination, nothing written. The counterpart of
/// [`preview_dump_conflicts`] for keys that are still on a source server
/// rather than in a file.
pub async fn preview_key_conflicts(
    server_id: &str,
    db: usize,
    keys: &[String],
    sample_limit: usize,
    cancel: &AtomicBool,
) -> Result<ConflictPreview> {
    const BATCH: usize = 64;
    let client = get_connection_manager().get_client(server_id, db).await?;
    let mut conn = client.connection();
    let mut preview = ConflictPreview {
        total: keys.len() as u64,
        ..Default::default()
    };
    for chunk in keys.chunks(BATCH) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let batch: Vec<Vec<u8>> = chunk.iter().map(|key| key.as_bytes().to_vec()).collect();
        let exists = keys_exist(&mut conn, &batch).await?;
        for (key, is_there) in chunk.iter().zip(exists) {
            if is_there {
                preview.conflicting += 1;
                if preview.sample_keys.len() < sample_limit {
                    preview.sample_keys.push(key.clone());
                }
            } else {
                preview.free += 1;
            }
        }
    }
    preview.cancelled = cancel.load(Ordering::Acquire);
    Ok(preview)
}

/// Result of a dry-run conflict scan against a dump file, or of the keys
/// of a server-to-server copy against the destination.
#[derive(Debug, Clone, Default)]
pub struct ConflictPreview {
    pub total: u64,
    pub conflicting: u64,
    pub free: u64,
    /// First N conflicting key names for the UI list.
    pub sample_keys: Vec<String>,
    pub cancelled: bool,
}

/// Copy a single key's value (and remaining TTL) to another server / db
/// via `DUMP` on the source and `RESTORE` on the target. Source and target
/// may be the same server (e.g. a cross-db copy). Returns `Ok(None)` when
/// the source key no longer exists, otherwise the restore outcome.
pub async fn copy_key(
    source_id: String,
    source_db: usize,
    target_id: String,
    target_db: usize,
    key: String,
    conflict: ConflictMode,
) -> Result<Option<RestoreStatus>> {
    let mut src = super::get_connection_manager()
        .get_connection(&source_id, source_db)
        .await?;
    let entries = dump_keys_chunk(&mut src, std::slice::from_ref(&key)).await?;
    let Some(entry) = entries.into_iter().next() else {
        return Ok(None);
    };
    let mut dst = super::get_connection_manager()
        .get_connection(&target_id, target_db)
        .await?;
    let mut statuses = restore_keys_chunk(&mut dst, std::slice::from_ref(&entry), conflict).await?;
    Ok(statuses.pop())
}

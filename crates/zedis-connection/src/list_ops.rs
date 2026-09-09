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

//! Deleting List elements by position.
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

use crate::async_connection::RedisAsyncConn;
use crate::error::Error;
use redis::pipe;
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Removes the elements at `indexes` from the list at `key`.
///
/// Runs as one `MULTI`/`EXEC`, so no other client can observe the marker or
/// interleave a write between the stamping and the removal. Duplicate
/// indexes are harmless (the second `LSET` writes the same marker).
///
/// Returns how many elements `LREM` actually removed.
pub async fn remove_list_indexes(conn: &mut RedisAsyncConn, key: &str, indexes: &[usize]) -> Result<u64> {
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
    let mut removed: Vec<u64> = pipeline.query_async(conn).await?;
    Ok(removed.pop().unwrap_or(0))
}

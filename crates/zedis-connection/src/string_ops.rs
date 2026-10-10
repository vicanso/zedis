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

//! The two key types read and written whole: a String and a RedisJSON
//! document.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::error::Error;
use crate::server_db::ServerDb;
use redis::{Value, cmd};

type Result<T, E = Error> = std::result::Result<T, E>;

/// `GET`, as bytes: a String is whatever was stored, and decoding it is the
/// editor's business.
pub async fn string_get(at: &ServerDb, key: &str) -> Result<Vec<u8>> {
    Ok(cmd("GET").arg(key).query_async(&mut at.connection().await?).await?)
}

/// The first `len` bytes of a string and its whole length (`STRLEN` +
/// `GETRANGE`) — the preview of a value too large to load whole. A string
/// shorter than `len` comes back whole, a missing key empty with length 0.
pub async fn string_prefix(at: &ServerDb, key: &str, len: usize) -> Result<(Vec<u8>, u64)> {
    let mut conn = at.connection().await?;
    let total: u64 = cmd("STRLEN").arg(key).query_async(&mut conn).await?;
    if len == 0 {
        return Ok((Vec::new(), total));
    }
    let head: Vec<u8> = cmd("GETRANGE")
        .arg(key)
        .arg(0)
        .arg(len - 1)
        .query_async(&mut conn)
        .await?;
    Ok((head, total))
}

/// `JSON.GET key` — the whole document, as the server serialises it.
pub async fn json_get(at: &ServerDb, key: &str) -> Result<String> {
    Ok(cmd("JSON.GET")
        .arg(key)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// What a string write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringWrite {
    /// Written, with the fresh `MEMORY USAGE` where the server answered one.
    Saved(Option<u64>),
    /// `SET … IFEQ` answered nil: the value on the server is no longer the
    /// one this client loaded, so nothing was written.
    Conflict,
}

/// `SET key value`, the way the editor saves.
///
/// The TTL is kept: `KEEPTTL` where the server has it (Redis 6.0) and can be
/// trusted with it (`RedisClient::keeps_ttl_on_set` — Garnet answers OK and
/// drops the key), else `PX ttl_ms` re-applied by hand — a `SET` without
/// either would drop the expiry. `cas_baseline` is the bytes the editor loaded: where the server
/// offers `IFEQ` (`floors::SET_IFEQ`) the write is refused rather than
/// clobbering a concurrent writer's change, and where it does not, the
/// baseline is simply not sent — the same write, without the guard.
pub async fn string_set(
    at: &ServerDb,
    key: &str,
    value: &[u8],
    ttl_ms: i64,
    cas_baseline: Option<&[u8]>,
) -> Result<StringWrite> {
    let client = at.client().await?;
    let mut conn = client.connection();
    let mut c = cmd("SET");
    c.arg(key).arg(value);
    if client.keeps_ttl_on_set() {
        c.arg("KEEPTTL");
    } else if ttl_ms > 0 {
        c.arg("PX").arg(ttl_ms);
    }
    let cas = cas_baseline.filter(|_| client.supports_set_ifeq());
    if let Some(baseline) = cas {
        c.arg("IFEQ").arg(baseline);
    }
    let reply: Value = c.query_async(&mut conn).await?;
    if cas.is_some() && matches!(reply, Value::Nil) {
        return Ok(StringWrite::Conflict);
    }
    // The fresh size for the header. A server that refuses `MEMORY USAGE`
    // leaves the old number in place rather than failing the save.
    let size = cmd("MEMORY")
        .arg("USAGE")
        .arg(key)
        .query_async::<u64>(&mut conn)
        .await
        .ok();
    Ok(StringWrite::Saved(size))
}

/// `JSON.MERGE key $ patch` — write only the fields that changed.
pub async fn json_merge(at: &ServerDb, key: &str, patch: &str) -> Result<()> {
    Ok(cmd("JSON.MERGE")
        .arg(key)
        .arg("$")
        .arg(patch)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `JSON.SET key $ document` — replace the whole document.
pub async fn json_set(at: &ServerDb, key: &str, document: &str) -> Result<()> {
    Ok(cmd("JSON.SET")
        .arg(key)
        .arg("$")
        .arg(document)
        .query_async(&mut at.connection().await?)
        .await?)
}

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
use crate::floors;
use crate::reply;
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
    string_range(at, key, 0, len).await
}

/// Up to `len` bytes of a string starting at `offset`, and its whole length
/// (`STRLEN` + `GETRANGE`) — how the editor pages a value too large to load
/// whole. An `offset` at or past the end returns empty bytes with the current
/// length; a missing key returns empty with length 0.
pub async fn string_range(at: &ServerDb, key: &str, offset: u64, len: usize) -> Result<(Vec<u8>, u64)> {
    let mut conn = at.connection().await?;
    let total: u64 = cmd("STRLEN").arg(key).query_async(&mut conn).await?;
    if len == 0 || offset >= total {
        return Ok((Vec::new(), total));
    }
    let end = offset.saturating_add(len as u64 - 1).min(total - 1);
    let slice: Vec<u8> = cmd("GETRANGE")
        .arg(key)
        .arg(offset)
        .arg(end)
        .query_async(&mut conn)
        .await?;
    Ok((slice, total))
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

const CROSS_SLOT_LCS_MAX_BYTES: u64 = 4_096;

/// `LCS key1 key2` — longest common subsequence of two string keys (Redis 7.0+).
///
/// When a Redis Cluster rejects `LCS` with `CROSSSLOT` because `key1` and `key2`
/// hash to different slots, falls back to reading both keys and computing the
/// byte-level LCS client-side (for strings up to [`CROSS_SLOT_LCS_MAX_BYTES`]).
pub async fn string_lcs(at: &ServerDb, key1: &str, key2: &str) -> Result<String> {
    let client = at.client().await?;
    if !client.supports(floors::LCS) {
        return Err(Error::Invalid {
            message: "LCS requires Redis 7.0 or Valkey 7.2+".to_string(),
        });
    }
    let mut conn = client.connection();
    match cmd("LCS").arg(key1).arg(key2).query_async::<Value>(&mut conn).await {
        Ok(val) => Ok(reply::text_lossy(&val).unwrap_or_default()),
        Err(err) if err.code() == Some("CROSSSLOT") || err.to_string().contains("CROSSSLOT") => {
            let len1: u64 = cmd("STRLEN").arg(key1).query_async(&mut conn).await?;
            let len2: u64 = cmd("STRLEN").arg(key2).query_async(&mut conn).await?;
            if len1 > CROSS_SLOT_LCS_MAX_BYTES || len2 > CROSS_SLOT_LCS_MAX_BYTES {
                return Err(err.into());
            }
            let a: Vec<u8> = cmd("GET").arg(key1).query_async(&mut conn).await?;
            let b: Vec<u8> = cmd("GET").arg(key2).query_async(&mut conn).await?;
            Ok(String::from_utf8_lossy(&bytes_lcs(&a, &b)).into_owned())
        }
        Err(err) => Err(err.into()),
    }
}

fn bytes_lcs(a: &[u8], b: &[u8]) -> Vec<u8> {
    let m = a.len();
    let n = b.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }
    let stride = n + 1;
    let mut dp = vec![0u16; (m + 1) * stride];
    for i in 1..=m {
        for j in 1..=n {
            dp[i * stride + j] = if a[i - 1] == b[j - 1] {
                dp[(i - 1) * stride + (j - 1)] + 1
            } else {
                dp[(i - 1) * stride + j].max(dp[i * stride + (j - 1)])
            };
        }
    }
    let mut out = Vec::with_capacity(dp[m * stride + n] as usize);
    let (mut i, mut j) = (m, n);
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push(a[i - 1]);
            i -= 1;
            j -= 1;
        } else if dp[(i - 1) * stride + j] >= dp[i * stride + (j - 1)] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out.reverse();
    out
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

#[cfg(test)]
mod tests {
    use super::bytes_lcs;

    #[test]
    fn bytes_lcs_matches_redis_lcs_semantics() {
        assert_eq!(bytes_lcs(b"ohmytext", b"mynewtext"), b"mytext");
        assert_eq!(bytes_lcs(b"", b"nonempty"), b"");
        assert_eq!(bytes_lcs(b"abc", b"xyz"), b"");
        assert_eq!(bytes_lcs(b"identical", b"identical"), b"identical");
    }
}

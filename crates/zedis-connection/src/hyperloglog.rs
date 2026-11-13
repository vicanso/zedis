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

//! HyperLogLog: what the HLL viewer shows and does.

#[cfg(target_family = "wasm")]
use crate::bridge::BridgeQuery as _;
use crate::conn::RedisAsyncConn;
use crate::error::Error;
use crate::reply::{is_cross_slot, same_slot_hash_tag};
use crate::server_db::ServerDb;
use redis::cmd;

type Result<T, E = Error> = std::result::Result<T, E>;

/// How the sketch is stored. Redis starts sparse and converts to dense once
/// the sparse form would be larger (`hll-sparse-max-bytes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HllEncoding {
    Dense,
    Sparse,
}

impl HllEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            HllEncoding::Dense => "dense",
            HllEncoding::Sparse => "sparse",
        }
    }

    /// From the header byte at offset 4 of the value (`HYLL` magic, then the
    /// encoding): 0 = dense, 1 = sparse.
    fn from_header_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(HllEncoding::Dense),
            1 => Some(HllEncoding::Sparse),
            _ => None,
        }
    }
}

/// A HyperLogLog key as the viewer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HllInfo {
    /// `PFCOUNT`: the estimated number of distinct elements.
    pub cardinality: i64,
    /// `None` when the header could not be read (a proxy that denies
    /// `GETRANGE`, a value that is not a sketch after all).
    pub encoding: Option<HllEncoding>,
    /// `STRLEN`, in bytes; 0 when it could not be read.
    pub size: u64,
}

/// The estimated cardinality, the encoding and the byte size of `key`. Only
/// `PFCOUNT` is fatal; the other two are best-effort extras.
pub async fn hll_info(at: &ServerDb, key: &str) -> Result<HllInfo> {
    let mut conn = at.connection().await?;
    let cardinality: i64 = cmd("PFCOUNT").arg(key).query_async(&mut conn).await?;
    let size: i64 = cmd("STRLEN").arg(key).query_async(&mut conn).await.unwrap_or(0);
    let header: Option<Vec<u8>> = cmd("GETRANGE").arg(key).arg(4).arg(4).query_async(&mut conn).await.ok();
    Ok(HllInfo {
        cardinality,
        encoding: header
            .and_then(|bytes| bytes.first().copied())
            .and_then(HllEncoding::from_header_byte),
        size: size.max(0) as u64,
    })
}

/// `PFADD key element [element …]` — fold elements into the sketch. `true`
/// when the estimate changed. No elements is not sent: a bare `PFADD key`
/// would *create* an empty sketch, which nobody asked for.
pub async fn pf_add(at: &ServerDb, key: &str, elements: &[String]) -> Result<bool> {
    if elements.is_empty() {
        return Ok(false);
    }
    let mut command = cmd("PFADD");
    command.arg(key);
    for element in elements {
        command.arg(element);
    }
    let changed: i64 = command.query_async(&mut at.connection().await?).await?;
    Ok(changed != 0)
}

/// Cross-slot fallback when `PFMERGE` is refused on a cluster because
/// `destination` and `sources` hash to different slots: stages each existing
/// source sketch into a short-lived temporary key sharing `destination`'s
/// hash slot, runs `PFMERGE` within that slot, and deletes the temporary keys.
async fn pf_merge_cross_slot(conn: &mut RedisAsyncConn, destination: &str, sources: &[String]) -> Result<()> {
    let tag_prefix = same_slot_hash_tag(destination);
    let nonce = uuid::Uuid::now_v7().simple();
    let mut temp_keys: Vec<String> = Vec::new();
    for (idx, source) in sources.iter().enumerate() {
        if source == destination {
            continue;
        }
        let bytes: Option<Vec<u8>> = cmd("GET").arg(source).query_async(conn).await?;
        let Some(bytes) = bytes else {
            continue;
        };
        let temp_key = format!("{tag_prefix}:zedis-pfmerge:{nonce}:{idx}");
        let res: std::result::Result<(), redis::RedisError> = cmd("SET")
            .arg(&temp_key)
            .arg(&bytes)
            .arg("PX")
            .arg(60_000)
            .query_async(conn)
            .await;
        if let Err(err) = res {
            if !temp_keys.is_empty() {
                let _ = cmd("DEL").arg(&temp_keys).query_async::<()>(conn).await;
            }
            return Err(err.into());
        }
        temp_keys.push(temp_key);
    }

    let mut merge = cmd("PFMERGE");
    merge.arg(destination);
    for temp_key in &temp_keys {
        merge.arg(temp_key);
    }
    let merge_res: std::result::Result<(), redis::RedisError> = merge.query_async(conn).await;
    if !temp_keys.is_empty() {
        let _ = cmd("DEL").arg(&temp_keys).query_async::<()>(conn).await;
    }
    Ok(merge_res?)
}

/// `PFMERGE destination source [source …]`.
///
/// The destination is *included* in the merge by Redis, so this folds the
/// sources into what is already there rather than replacing it — which is
/// what "merge into this key" should mean, and worth saying in the dialog
/// because the opposite reading is just as natural. On a cluster where
/// `destination` and `sources` hash to different slots (`CrossSlot`), stages
/// the source sketches into temporary keys in `destination`'s slot before
/// merging.
pub async fn pf_merge(at: &ServerDb, destination: &str, sources: &[String]) -> Result<()> {
    if sources.is_empty() {
        return Ok(());
    }
    let conn = &mut at.connection().await?;
    let mut command = cmd("PFMERGE");
    command.arg(destination);
    for source in sources {
        command.arg(source);
    }
    match command.query_async::<()>(conn).await {
        Ok(()) => Ok(()),
        Err(err) if is_cross_slot(&err) => pf_merge_cross_slot(conn, destination, sources).await,
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::HllEncoding;

    #[test]
    fn the_encoding_is_the_header_byte_redis_documents() {
        assert_eq!(HllEncoding::from_header_byte(0), Some(HllEncoding::Dense));
        assert_eq!(HllEncoding::from_header_byte(1), Some(HllEncoding::Sparse));
        assert_eq!(HllEncoding::from_header_byte(b'x'), None, "not a sketch header");
        assert_eq!(HllEncoding::Sparse.as_str(), "sparse");
    }
}

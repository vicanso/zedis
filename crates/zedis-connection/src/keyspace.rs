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

//! Keys as the key tree and the key header see them: scanned, typed, sized,
//! renamed, given a TTL, created, deleted — one at a time or by the prefix.
//!
//! What happens *inside* a key's value is the business of the module for its
//! type (`hash_fields.rs`, `list_ops.rs`, …, `key_ops.rs` for the one-shot
//! operations of the key menu).

#[cfg(target_family = "wasm")]
use crate::bridge::{BridgePipeline as _, BridgeQuery as _};
use crate::error::Error;
use crate::floors;
use crate::manager::{ExpireCondition, HeatMetric, HeatProbe};
use crate::server_db::ServerDb;
use futures::stream::{self, StreamExt};
use redis::{cmd, pipe};
use tracing::{debug, warn};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Keys matched per `SCAN` round when an operation walks a whole prefix.
const PREFIX_SCAN_COUNT: u64 = 10_000;
/// Rounds a prefix walk makes before it stops: it bounds one click, and what
/// is left is reached by clicking again.
const PREFIX_SCAN_ROUNDS: usize = 20;
/// The same bound for a delete — and for the count that precedes it, which
/// has to make the same walk or the confirmation would show one number and
/// the click remove another. Wider than the TTL walk's: a delete is the
/// operation people run on a whole prefix, and a click that leaves most of
/// it standing is the surprise the confirmation exists to prevent.
const DELETE_SCAN_ROUNDS: usize = 50;
/// Keys `MEMORY USAGE` is asked about when a prefix is sized before it is
/// deleted: enough for an average, few enough that the question costs
/// nothing next to the walk.
const MEMORY_SAMPLE: usize = 32;
/// `TYPE` commands per pipeline when typing a page of keys.
const TYPE_PIPELINE_CHUNK: usize = 500;
/// `TYPE` commands in flight on a cluster, where keys cannot be pipelined
/// across nodes.
const TYPE_CONCURRENCY: usize = 100;

/// A page of a scan: the cursors to continue from (one per master — all zero
/// when the scan is complete) and `(key, type, ttl)` rows.
pub type ScanPage = (Vec<u64>, Vec<(String, String, i64)>);

/// One round of the key tree's scan; `cursors` is `None` to start one.
pub async fn scan_page(
    at: &ServerDb,
    cursors: Option<Vec<u64>>,
    pattern: &str,
    count: u64,
    with_ttl: bool,
    type_filter: Option<&str>,
) -> Result<ScanPage> {
    at.client()
        .await?
        .scan(cursors, pattern, count, with_ttl, type_filter)
        .await
}

/// `TYPE` of every key, in order. Pipelined where one connection reaches
/// every key; on a cluster the keys live on different nodes, so they go out
/// concurrently instead, and a key whose `TYPE` fails reads as `""`.
pub async fn key_types(at: &ServerDb, keys: Vec<String>) -> Result<Vec<String>> {
    let client = at.client().await?;
    let conn = client.connection();
    if client.is_cluster() {
        return Ok(stream::iter(keys)
            .map(|key| {
                let mut conn = conn.clone();
                async move {
                    cmd("TYPE")
                        .arg(&key)
                        .query_async::<String>(&mut conn)
                        .await
                        .unwrap_or_default()
                }
            })
            .buffered(TYPE_CONCURRENCY)
            .collect()
            .await);
    }
    let mut conn = conn;
    let mut types = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(TYPE_PIPELINE_CHUNK) {
        let mut pipeline = pipe();
        for key in chunk {
            pipeline.cmd("TYPE").arg(key);
        }
        let answered: Vec<String> = pipeline.query_async(&mut conn).await?;
        types.extend(answered);
    }
    Ok(types)
}

/// `TYPE` and `TTL` of one key, in one round trip. A TTL of `-2` is the
/// server saying the key is not there.
pub async fn key_type_and_ttl(at: &ServerDb, key: &str) -> Result<(String, i64)> {
    Ok(pipe()
        .cmd("TYPE")
        .arg(key)
        .cmd("TTL")
        .arg(key)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// What the key weighs: `MEMORY USAGE`, or the cheaper exact answer its type
/// has (`STRLEN`).
pub async fn key_memory_usage(at: &ServerDb, key: &str, key_type: &str) -> Result<u64> {
    at.client().await?.memory_usage(key, key_type).await
}

/// The header's decorations: `OBJECT ENCODING` (when asked for) and whichever
/// of `OBJECT FREQ` / `IDLETIME` the eviction policy makes meaningful. Never
/// fails — a server without them simply has no chip.
pub async fn key_object_meta(at: &ServerDb, key: &str, with_encoding: bool, heat: HeatProbe) -> (String, HeatMetric) {
    match at.client().await {
        Ok(client) => client.object_meta(key, with_encoding, heat).await,
        Err(e) => {
            debug!(key, error = %e, "object meta skipped");
            (String::new(), HeatMetric::default())
        }
    }
}

/// `DUMP key`.
pub async fn dump_key(at: &ServerDb, key: &str) -> Result<Vec<u8>> {
    Ok(cmd("DUMP").arg(key).query_async(&mut at.connection().await?).await?)
}

/// What the recycle bin keeps of a key about to be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySnapshot {
    /// The `DUMP` payload, as `RESTORE` takes it back.
    pub payload: Vec<u8>,
    /// `PTTL` at the time: `-1` for no expiry.
    pub pttl_ms: i64,
}

/// `DUMP` + `PTTL` of a key, for the recycle bin — or `None`, with the reason
/// logged, whenever there is nothing worth keeping: the value is estimated
/// (`MEMORY USAGE`, asked *before* the `DUMP` so a huge value is never
/// serialised just to be thrown away) or measured to be over its cap, the key
/// is already gone, or `DUMP` failed. Never an error: the bin is a safety net
/// and must not turn a working delete into a failed one.
pub async fn snapshot_key(at: &ServerDb, key: &str, max_memory: i64, max_payload: usize) -> Option<KeySnapshot> {
    let conn = &mut match at.connection().await {
        Ok(conn) => conn,
        Err(e) => {
            warn!(key, error = %e, "trash: no connection, deleting permanently");
            return None;
        }
    };
    match cmd("MEMORY")
        .arg("USAGE")
        .arg(key)
        .query_async::<Option<i64>>(conn)
        .await
    {
        Ok(Some(estimated)) if estimated > max_memory => {
            warn!(key, estimated, "trash: value too large, deleting permanently");
            return None;
        }
        // Nil: the key is already gone; the DUMP below settles it.
        Ok(_) => {}
        Err(e) => debug!(key, error = %e, "trash: MEMORY USAGE unavailable, relying on post-DUMP cap"),
    }
    let payload: Option<Vec<u8>> = match cmd("DUMP").arg(key).query_async(conn).await {
        Ok(payload) => payload,
        Err(e) => {
            warn!(key, error = %e, "trash: DUMP failed, deleting permanently");
            return None;
        }
    };
    // Nil reply: the key vanished between the delete request and now.
    let payload = payload?;
    if payload.len() > max_payload {
        warn!(
            key,
            size = payload.len(),
            "trash: payload too large, deleting permanently"
        );
        return None;
    }
    let pttl_ms: i64 = cmd("PTTL").arg(key).query_async(conn).await.unwrap_or(-1);
    Some(KeySnapshot { payload, pttl_ms })
}

/// `DEL key`.
pub async fn delete_key(at: &ServerDb, key: &str) -> Result<()> {
    Ok(cmd("DEL").arg(key).query_async(&mut at.connection().await?).await?)
}

/// Delete the given keys, wherever in a cluster each one lives (`UNLINK`
/// where the server has it).
pub async fn delete_keys(at: &ServerDb, keys: Vec<String>) -> Result<()> {
    at.client().await?.unlike_keys_scattered(keys).await
}

/// Delete every key matching `pattern`: scan a round, delete what it found,
/// again — bounded, so one click cannot run for ever on a huge prefix.
/// What one pass over a prefix reaches — the answer to "delete this
/// folder?" before it is asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefixImpact {
    /// Keys the pass matched.
    pub keys: u64,
    /// Whether the cursors came back to zero. `false` means the pass stopped
    /// at its round limit and the prefix holds more than `keys`.
    pub complete: bool,
    /// `MEMORY USAGE` over up to [`MEMORY_SAMPLE`] of the matched keys —
    /// (keys asked, bytes) — or `None` where the server would not say.
    pub sampled: Option<(u64, u64)>,
}

impl PrefixImpact {
    /// The prefix's size, scaled up from the sample; `None` without one.
    pub fn estimated_bytes(&self) -> Option<u64> {
        let (asked, bytes) = self.sampled.filter(|(asked, _)| *asked > 0)?;
        Some((bytes as f64 / asked as f64 * self.keys as f64) as u64)
    }
}

/// What [`delete_keys_matching`] would reach: the same walk, counting
/// instead of deleting. The number is the pass's, not the prefix's — a
/// walk that stops at its round limit says so in `complete` — so what the
/// confirmation shows is what the click does.
pub async fn count_keys_matching(at: &ServerDb, pattern: &str) -> Result<PrefixImpact> {
    let client = at.client().await?;
    let mut cursors: Option<Vec<u64>> = None;
    let mut keys = 0u64;
    let mut complete = false;
    let mut sample: Vec<String> = Vec::new();
    for _ in 0..DELETE_SCAN_ROUNDS {
        let (next, keys_per_node) = client.scan_nodes(cursors, pattern, PREFIX_SCAN_COUNT, None).await?;
        for node_keys in keys_per_node {
            keys += node_keys.len() as u64;
            let room = MEMORY_SAMPLE.saturating_sub(sample.len());
            sample.extend(node_keys.into_iter().take(room));
        }
        if next.iter().sum::<u64>() == 0 {
            complete = true;
            break;
        }
        cursors = Some(next);
    }
    let sampled = if client.supports(floors::MEMORY_USAGE) {
        sample_memory(at, &sample).await
    } else {
        None
    };
    Ok(PrefixImpact {
        keys,
        complete,
        sampled,
    })
}

/// `MEMORY USAGE` over `keys`: (keys asked, bytes), or `None` when the
/// server refuses — a proxy without the command, an ACL without `@memory`.
/// Never an error: a count without a size is still a count.
async fn sample_memory(at: &ServerDb, keys: &[String]) -> Option<(u64, u64)> {
    if keys.is_empty() {
        return None;
    }
    let conn = &mut at.connection().await.ok()?;
    let (mut asked, mut bytes) = (0u64, 0u64);
    for key in keys {
        match cmd("MEMORY")
            .arg("USAGE")
            .arg(key)
            .query_async::<Option<u64>>(conn)
            .await
        {
            Ok(Some(size)) => {
                asked += 1;
                bytes += size;
            }
            // Gone since the scan listed it.
            Ok(None) => {}
            Err(e) => {
                debug!(key, error = %e, "MEMORY USAGE unavailable, the prefix goes unsized");
                return None;
            }
        }
    }
    (asked > 0).then_some((asked, bytes))
}

pub async fn delete_keys_matching(at: &ServerDb, pattern: &str) -> Result<()> {
    let client = at.client().await?;
    let mut cursors: Option<Vec<u64>> = None;
    for _ in 0..DELETE_SCAN_ROUNDS {
        let (next, keys_per_node) = client.scan_nodes(cursors, pattern, PREFIX_SCAN_COUNT, None).await?;
        client.unlike_keys(keys_per_node).await?;
        if next.iter().sum::<u64>() == 0 {
            break;
        }
        cursors = Some(next);
    }
    Ok(())
}

/// `RENAME`, or `RENAMENX` unless `overwrite` — then `false` means the new
/// name is taken and nothing moved.
pub async fn rename_key(at: &ServerDb, old: &str, new: &str, overwrite: bool) -> Result<bool> {
    let conn = &mut at.connection().await?;
    if overwrite {
        let _: () = cmd("RENAME").arg(old).arg(new).query_async(conn).await?;
        return Ok(true);
    }
    let renamed: i64 = cmd("RENAMENX").arg(old).arg(new).query_async(conn).await?;
    Ok(renamed == 1)
}

/// `EXPIRE key seconds`.
pub async fn expire_key(at: &ServerDb, key: &str, seconds: u64) -> Result<()> {
    Ok(cmd("EXPIRE")
        .arg(key)
        .arg(seconds)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// `EXPIREAT key unix-seconds`.
pub async fn expire_key_at(at: &ServerDb, key: &str, unix_secs: i64) -> Result<()> {
    Ok(cmd("EXPIREAT")
        .arg(key)
        .arg(unix_secs)
        .query_async(&mut at.connection().await?)
        .await?)
}

/// Set (or, with `None`, remove) the TTL of each key; one answer per key, in
/// order — `false` where `condition` (`NX` / `XX` / `GT` / `LT`) held it back
/// or the key is gone.
pub async fn set_keys_ttl(
    at: &ServerDb,
    keys: Vec<String>,
    ttl_secs: Option<u64>,
    condition: Option<ExpireCondition>,
) -> Result<Vec<bool>> {
    at.client()
        .await?
        .set_ttl_keys_scattered(keys, ttl_secs, condition)
        .await
}

/// [`set_keys_ttl`] for every key matching `pattern`: the keys it changed,
/// and how many it left alone. Bounded like [`delete_keys_matching`].
pub async fn set_ttl_matching(
    at: &ServerDb,
    pattern: &str,
    ttl_secs: Option<u64>,
    condition: Option<ExpireCondition>,
) -> Result<(Vec<String>, usize)> {
    let client = at.client().await?;
    let mut cursors: Option<Vec<u64>> = None;
    let mut changed = Vec::new();
    let mut skipped = 0usize;
    for _ in 0..PREFIX_SCAN_ROUNDS {
        let (next, keys_per_node) = client.scan_nodes(cursors, pattern, PREFIX_SCAN_COUNT, None).await?;
        let keys: Vec<String> = keys_per_node.into_iter().flatten().collect();
        let applied = client.set_ttl_keys_scattered(keys.clone(), ttl_secs, condition).await?;
        for (key, done) in keys.into_iter().zip(applied) {
            if done {
                changed.push(key);
            } else {
                skipped += 1;
            }
        }
        if next.iter().sum::<u64>() == 0 {
            break;
        }
        cursors = Some(next);
    }
    Ok((changed, skipped))
}

/// Create a key by running its type's first write (`SET k v`, `HSET k f v`,
/// `RPUSH k item`, …) and, when given, `EXPIRE`. `false` — and nothing sent —
/// when a key of that name already exists.
pub async fn create_key(
    at: &ServerDb,
    key: &str,
    command: &str,
    args: &[String],
    ttl_secs: Option<u64>,
) -> Result<bool> {
    let conn = &mut at.connection().await?;
    let exists: bool = cmd("EXISTS").arg(key).query_async(conn).await?;
    if exists {
        return Ok(false);
    }
    let _: () = cmd(command).arg(key).arg(args).query_async(conn).await?;
    if let Some(seconds) = ttl_secs {
        let _: () = cmd("EXPIRE").arg(key).arg(seconds).query_async(conn).await?;
    }
    Ok(true)
}

/// `PUBLISH`, or `SPUBLISH` for a sharded channel; answers how many
/// subscribers received it.
pub async fn publish(at: &ServerDb, channel: &str, message: &str, sharded: bool) -> Result<u64> {
    Ok(cmd(if sharded { "SPUBLISH" } else { "PUBLISH" })
        .arg(channel)
        .arg(message)
        .query_async(&mut at.connection().await?)
        .await?)
}

#[cfg(test)]
mod prefix_impact_tests {
    use super::PrefixImpact;

    #[test]
    fn the_estimate_scales_the_sample_and_is_absent_without_one() {
        let sized = PrefixImpact {
            keys: 1_000,
            complete: true,
            sampled: Some((10, 2_500)),
        };
        assert_eq!(sized.estimated_bytes(), Some(250_000));
        let without_sample = PrefixImpact {
            keys: 1_000,
            complete: false,
            sampled: None,
        };
        assert_eq!(without_sample.estimated_bytes(), None);
        // A sample that asked nothing (every key gone by the time it ran).
        let empty = PrefixImpact {
            sampled: Some((0, 0)),
            ..sized
        };
        assert_eq!(empty.estimated_bytes(), None);
    }
}

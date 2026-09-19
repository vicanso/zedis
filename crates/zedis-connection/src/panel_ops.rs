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

//! Operations of the pooled [`RedisClient`](crate::manager::RedisClient),
//! offered to a caller that holds a [`ServerDb`] and not a client.
//!
//! The methods themselves stay where they are — several are used by the
//! state layer on a client it already has. A view has no client and should
//! not take one to call a single method (ADR 10), so each method a panel
//! needs has a function here that finds the client and forwards. No logic
//! belongs in this file: a function that grows some should become an
//! operation in the module of its Redis feature.

use crate::error::Error;
use crate::hotkeys::HotkeysReport;
use crate::manager::{CommandStat, HeatProbe, KeyMemoryUsage, PubsubChannelsSnapshot, ValueSearchRound};
use crate::server_db::ServerDb;
use crate::slot_stats::{SlotStatMetric, SlotStatRow};
use zedis_core::keysizes::KeysizesDist;

type Result<T, E = Error> = std::result::Result<T, E>;

/// The raw bytes of `key` (`GET` / `DUMP`-free read used by the value diff).
pub async fn key_bytes(at: &ServerDb, key: &str) -> Result<Vec<u8>> {
    at.client().await?.get_key_bytes(key).await
}

/// A short, printable preview of `key`'s value, whatever its type.
pub async fn value_preview(at: &ServerDb, key: &str) -> Result<String> {
    at.client().await?.get_value_preview(key).await
}

/// `HOTKEYS START` with the chosen metrics and list length.
pub async fn hotkeys_start(at: &ServerDb, cpu: bool, net: bool, count: u64) -> Result<()> {
    at.client().await?.hotkeys_start(cpu, net, count).await
}

pub async fn hotkeys_stop(at: &ServerDb) -> Result<()> {
    at.client().await?.hotkeys_stop().await
}

pub async fn hotkeys_reset(at: &ServerDb) -> Result<()> {
    at.client().await?.hotkeys_reset().await
}

pub async fn hotkeys_report(at: &ServerDb) -> Result<HotkeysReport> {
    at.client().await?.hotkeys_report().await
}

/// `INFO keysizes` as per-type distributions — empty on a server that has no
/// such section, which is not an error: the panel simply has no histogram.
pub async fn key_size_distributions(at: &ServerDb) -> Result<Vec<KeysizesDist>> {
    let client = at.client().await?;
    if !client.supports_info_keysizes() {
        return Ok(Vec::new());
    }
    client.info_keysizes().await
}

pub async fn maxmemory_policy(at: &ServerDb) -> Result<String> {
    at.client().await?.maxmemory_policy().await
}

/// One round of the memory analyzer's sampling scan: the keys scanned, the
/// cursors to continue from, and the sampled keys' memory usage.
pub async fn sample_memory_usage(
    at: &ServerDb,
    ratio: f32,
    count: u64,
    cursors: Option<Vec<u64>>,
    heat: HeatProbe,
    with_encoding: bool,
) -> Result<(u64, Vec<u64>, Vec<KeyMemoryUsage>)> {
    at.client()
        .await?
        .sample_scan_memory_usage(ratio, count, cursors, heat, with_encoding)
        .await
}

pub async fn pubsub_channels(at: &ServerDb, pattern: &str, sharded: bool) -> Result<PubsubChannelsSnapshot> {
    at.client().await?.pubsub_channels(pattern, sharded).await
}

/// `INFO commandstats`, summed over the masters.
pub async fn command_stats(at: &ServerDb) -> Result<Vec<CommandStat>> {
    at.client().await?.command_stats().await
}

pub async fn cluster_slot_stats(at: &ServerDb, metric: SlotStatMetric, limit: u64) -> Result<Vec<SlotStatRow>> {
    at.client().await?.cluster_slot_stats(metric, limit).await
}

/// One round of a value search; `cursors` from the previous round continue it.
pub async fn scan_values_round(
    at: &ServerDb,
    pattern: &str,
    needle_lower: &str,
    max_value_bytes: u64,
    max_container_elems: u64,
    cursors: Option<Vec<u64>>,
    page_count: u64,
) -> Result<ValueSearchRound> {
    at.client()
        .await?
        .scan_values_round(
            pattern,
            needle_lower,
            max_value_bytes,
            max_container_elems,
            cursors,
            page_count,
        )
        .await
}

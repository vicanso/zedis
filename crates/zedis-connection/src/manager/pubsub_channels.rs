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

//! Channel discovery for the Pub/Sub panel: `PUBSUB CHANNELS` + `PUBSUB
//! NUMSUB`, or `PUBSUB SHARDCHANNELS` + `PUBSUB SHARDNUMSUB` in sharded
//! mode (Redis 7+), so a channel can be picked instead of typed.
//!
//! Redis knows a channel only while someone is subscribed to it, and it
//! knows it *per node*: a classic subscription shows up on the node the
//! subscriber is connected to, a shard channel on its slot owner. The
//! listing is therefore the union over the masters with the counts summed.
//! The pooled client fans out to masters only, so a subscriber connected
//! to a replica is not seen — the panel says so on a cluster.

use super::RedisClient;
use crate::error::Error;
use redis::cmd;
use std::collections::BTreeMap;

type Result<T, E = Error> = std::result::Result<T, E>;

/// `NUMSUB` takes the channels as arguments: bounded so one reply stays
/// small on a server with thousands of live channels.
const NUMSUB_CHUNK: usize = 200;

/// Listing cap. `PUBSUB CHANNELS` has no paging, so its reply is whole
/// either way; the cap bounds the `NUMSUB` follow-ups and the table.
pub const MAX_PUBSUB_CHANNELS: usize = 10_000;

/// One channel with a subscriber, as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubChannel {
    pub name: String,
    /// Exact-name subscribers summed over the masters — `NUMSUB` never
    /// counts pattern subscribers.
    pub subscribers: u64,
}

/// What the server knows about live channels right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PubsubChannelsSnapshot {
    /// Most subscribed first, ties by name.
    pub channels: Vec<PubsubChannel>,
    /// `PUBSUB NUMPAT` summed over the masters; `None` in sharded mode,
    /// which has no pattern subscriptions.
    pub pattern_subscriptions: Option<u64>,
    /// Masters the listing came from — more than one on a cluster.
    pub nodes: usize,
    /// The union ran past [`MAX_PUBSUB_CHANNELS`] and was cut there.
    pub truncated: bool,
}

impl RedisClient {
    /// The channels with at least one subscriber whose name matches
    /// `pattern` (a glob; empty lists every channel), with their counts.
    pub async fn pubsub_channels(&self, pattern: &str, sharded: bool) -> Result<PubsubChannelsSnapshot> {
        let (list_sub, count_sub) = if sharded {
            ("SHARDCHANNELS", "SHARDNUMSUB")
        } else {
            ("CHANNELS", "NUMSUB")
        };
        let mut list = cmd("PUBSUB");
        list.arg(list_sub);
        let pattern = pattern.trim();
        if !pattern.is_empty() {
            list.arg(pattern);
        }
        let (masters, listed): (_, Vec<Vec<String>>) = self.query_async_masters(vec![list]).await?;
        let nodes = masters.len();
        let (names, truncated) = channel_union(listed);

        let mut counted: Vec<Vec<(String, u64)>> = Vec::new();
        for chunk in names.chunks(NUMSUB_CHUNK) {
            let mut count = cmd("PUBSUB");
            count.arg(count_sub);
            for name in chunk {
                count.arg(name);
            }
            let (_, replies): (_, Vec<Vec<(String, u64)>>) = self.query_async_masters(vec![count]).await?;
            counted.extend(replies);
        }

        let pattern_subscriptions = if sharded {
            None
        } else {
            let (_, replies): (_, Vec<u64>) = self
                .query_async_masters(vec![cmd("PUBSUB").arg("NUMPAT").clone()])
                .await?;
            Some(replies.into_iter().sum())
        };

        Ok(PubsubChannelsSnapshot {
            channels: merge_subscribers(counted),
            pattern_subscriptions,
            nodes,
            truncated,
        })
    }
}

/// The distinct names over every node's listing, sorted, cut at the cap.
fn channel_union(listed: Vec<Vec<String>>) -> (Vec<String>, bool) {
    let mut names: Vec<String> = listed.into_iter().flatten().collect();
    names.sort_unstable();
    names.dedup();
    let truncated = names.len() > MAX_PUBSUB_CHANNELS;
    names.truncate(MAX_PUBSUB_CHANNELS);
    (names, truncated)
}

/// Sums each channel's count over the nodes' replies and drops the ones
/// that lost their last subscriber between the listing and the count.
fn merge_subscribers(counted: Vec<Vec<(String, u64)>>) -> Vec<PubsubChannel> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for (name, subscribers) in counted.into_iter().flatten() {
        *totals.entry(name).or_default() += subscribers;
    }
    let mut channels: Vec<PubsubChannel> = totals
        .into_iter()
        .filter(|(_, subscribers)| *subscribers > 0)
        .map(|(name, subscribers)| PubsubChannel { name, subscribers })
        .collect();
    channels.sort_by(|a, b| b.subscribers.cmp(&a.subscribers).then_with(|| a.name.cmp(&b.name)));
    channels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
        pairs.iter().map(|(name, n)| (name.to_string(), *n)).collect()
    }

    #[test]
    fn the_union_is_deduplicated_sorted_and_capped() {
        let listed = vec![
            vec!["news".to_string(), "alerts".to_string()],
            vec!["news".to_string(), "chat".to_string()],
        ];
        let (names, truncated) = channel_union(listed);
        assert_eq!(names, vec!["alerts", "chat", "news"]);
        assert!(!truncated);

        let many: Vec<String> = (0..MAX_PUBSUB_CHANNELS + 1).map(|i| format!("c{i:05}")).collect();
        let (names, truncated) = channel_union(vec![many]);
        assert_eq!(names.len(), MAX_PUBSUB_CHANNELS);
        assert!(truncated);
    }

    #[test]
    fn counts_are_summed_over_nodes_and_ordered_by_subscribers() {
        // Two cluster masters each holding a subscriber of `news`; `gone`
        // lost its subscriber between CHANNELS and NUMSUB.
        let merged = merge_subscribers(vec![
            counts(&[("news", 1), ("alerts", 3), ("gone", 0)]),
            counts(&[("news", 1), ("chat", 3)]),
        ]);
        assert_eq!(
            merged,
            vec![
                PubsubChannel {
                    name: "alerts".into(),
                    subscribers: 3
                },
                PubsubChannel {
                    name: "chat".into(),
                    subscribers: 3
                },
                PubsubChannel {
                    name: "news".into(),
                    subscribers: 2
                },
            ]
        );
    }
}

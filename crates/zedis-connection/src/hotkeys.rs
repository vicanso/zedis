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

//! `HOTKEYS` (Redis 8.6) report parsing.
//!
//! The tracking itself is per node, so the [`RedisClient`] methods run the
//! subcommands on every master (`query_async_masters`) and this module merges
//! the per-node `HOTKEYS GET` replies into one report — cluster slots are
//! disjoint, so the per-node top-K lists concatenate without double counting.
//!
//! [`RedisClient`]: crate::manager::get_connection_manager

use redis::Value;

/// One tracked key with its metric value (CPU µs or network bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEntry {
    pub key: String,
    pub value: u64,
}

/// The merged `HOTKEYS GET` report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeysReport {
    /// Any node still tracking.
    pub tracking_active: bool,
    /// `SAMPLE` ratio the collection ran with (1 = every key).
    pub sample_ratio: u64,
    /// Wall-clock length of the collection (max across nodes).
    pub collection_duration_ms: u64,
    /// `all-commands-all-slots-us` summed across nodes — the CPU-time
    /// denominator for a key's share.
    pub total_cpu_us: u64,
    /// `net-bytes-all-commands-all-slots` summed across nodes — the
    /// network denominator for a key's share.
    pub total_net_bytes: u64,
    /// Top keys by CPU time (µs), descending. Empty when the collection
    /// ran without the CPU metric.
    pub by_cpu: Vec<HotkeyEntry>,
    /// Top keys by network bytes, descending. Empty when the collection
    /// ran without the NET metric.
    pub by_net: Vec<HotkeyEntry>,
}

impl HotkeysReport {
    /// Fold one node's `HOTKEYS GET` reply in. `Nil` (tracking never
    /// started / reset on that node) contributes nothing.
    pub fn merge_node_reply(&mut self, reply: &Value) {
        let Some(pairs) = reply_pairs(reply) else {
            return;
        };
        for (field, value) in pairs {
            match field.as_str() {
                "tracking-active" => self.tracking_active |= as_u64(value) == Some(1),
                "sample-ratio" => self.sample_ratio = self.sample_ratio.max(as_u64(value).unwrap_or(0)),
                "collection-duration-ms" => {
                    self.collection_duration_ms = self.collection_duration_ms.max(as_u64(value).unwrap_or(0))
                }
                "all-commands-all-slots-us" => {
                    self.total_cpu_us = self.total_cpu_us.saturating_add(as_u64(value).unwrap_or(0))
                }
                "net-bytes-all-commands-all-slots" => {
                    self.total_net_bytes = self.total_net_bytes.saturating_add(as_u64(value).unwrap_or(0))
                }
                "by-cpu-time-us" => self.by_cpu.extend(entry_list(value)),
                "by-net-bytes" => self.by_net.extend(entry_list(value)),
                // selected-slots, collection-start-time-unix-ms,
                // total-cpu-time-{user,sys}-ms, total-net-bytes and any
                // future field: not shown, skipped by name.
                _ => {}
            }
        }
    }

    /// Order both lists hottest-first — call once after every node merged.
    pub fn sort(&mut self) {
        self.by_cpu.sort_by_key(|e| std::cmp::Reverse(e.value));
        self.by_net.sort_by_key(|e| std::cmp::Reverse(e.value));
    }

    /// True when every node answered `Nil` — nothing was ever collected.
    pub fn is_empty(&self) -> bool {
        self.by_cpu.is_empty() && self.by_net.is_empty() && self.collection_duration_ms == 0
    }
}

/// The field/value pairs of a map-shaped reply, whether it arrived as a
/// RESP3 map or a RESP2 flat array. `None` for `Nil` or anything else.
fn reply_pairs(value: &Value) -> Option<Vec<(String, &Value)>> {
    match value {
        Value::Map(items) => Some(items.iter().filter_map(|(k, v)| Some((as_string(k)?, v))).collect()),
        // The server wraps the report in a one-element outer array (an
        // array of collections; at most one exists) — seen on the wire as
        // `*1` around the `*24` field list on 8.6.1. Unwrap it. A flat
        // pair list can never match: its elements are strings/ints.
        Value::Array(items) if items.len() == 1 && matches!(items[0], Value::Array(_) | Value::Map(_)) => {
            reply_pairs(&items[0])
        }
        Value::Array(items) => Some(
            items
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|[k, v]| Some((as_string(k)?, v)))
                .collect(),
        ),
        _ => None,
    }
}

/// A `key value key value …` (or map) list into entries; non-numeric
/// values are dropped rather than failing the report.
fn entry_list(value: &Value) -> Vec<HotkeyEntry> {
    reply_pairs(value)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, v)| Some(HotkeyEntry { key, value: as_u64(v)? }))
        .collect()
}

fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(n) => u64::try_from(*n).ok(),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().ok(),
        Value::SimpleString(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    /// The RESP2 shape of one node's reply, as captured on the wire from a
    /// live 8.6.1 server (`HOTKEYS START METRICS 2 CPU NET`): the field
    /// list rides inside a one-element outer array (`*1` around `*24`).
    fn node_reply(active: i64, cpu: &[(&str, i64)], net: &[(&str, i64)]) -> Value {
        let pairs =
            |list: &[(&str, i64)]| Value::Array(list.iter().flat_map(|(k, v)| [bulk(k), Value::Int(*v)]).collect());
        Value::Array(vec![Value::Array(vec![
            bulk("tracking-active"),
            Value::Int(active),
            bulk("sample-ratio"),
            Value::Int(1),
            bulk("selected-slots"),
            Value::Array(vec![Value::Int(0), Value::Int(16383)]),
            bulk("all-commands-all-slots-us"),
            Value::Int(cpu.iter().map(|(_, v)| *v).sum::<i64>() * 2),
            bulk("net-bytes-all-commands-all-slots"),
            Value::Int(net.iter().map(|(_, v)| *v).sum::<i64>() * 2),
            bulk("collection-start-time-unix-ms"),
            Value::Int(1_788_249_634_243),
            bulk("collection-duration-ms"),
            Value::Int(257),
            bulk("total-cpu-time-user-ms"),
            Value::Int(0),
            bulk("total-cpu-time-sys-ms"),
            Value::Int(2),
            bulk("total-net-bytes"),
            Value::Int(3351),
            bulk("by-cpu-time-us"),
            pairs(cpu),
            bulk("by-net-bytes"),
            pairs(net),
        ])])
    }

    #[test]
    fn merges_and_sorts_two_nodes() {
        let mut report = HotkeysReport::default();
        report.merge_node_reply(&node_reply(1, &[("a", 10), ("b", 30)], &[("a", 100)]));
        report.merge_node_reply(&node_reply(0, &[("c", 20)], &[("c", 900)]));
        report.merge_node_reply(&Value::Nil);
        report.sort();

        assert!(report.tracking_active);
        assert_eq!(report.sample_ratio, 1);
        assert_eq!(report.collection_duration_ms, 257);
        assert_eq!(report.total_cpu_us, 80 + 40);
        assert_eq!(report.total_net_bytes, 200 + 1800);
        let cpu: Vec<_> = report.by_cpu.iter().map(|e| (e.key.as_str(), e.value)).collect();
        assert_eq!(cpu, vec![("b", 30), ("c", 20), ("a", 10)]);
        let net: Vec<_> = report.by_net.iter().map(|e| (e.key.as_str(), e.value)).collect();
        assert_eq!(net, vec![("c", 900), ("a", 100)]);
        assert!(!report.is_empty());
    }

    #[test]
    fn all_nil_reads_as_empty() {
        let mut report = HotkeysReport::default();
        report.merge_node_reply(&Value::Nil);
        report.sort();
        assert!(report.is_empty());
        assert!(!report.tracking_active);
    }

    #[test]
    fn resp3_map_shape_parses_too() {
        let mut report = HotkeysReport::default();
        report.merge_node_reply(&Value::Map(vec![
            (bulk("tracking-active"), Value::Int(1)),
            (bulk("all-commands-all-slots-us"), Value::Int(50)),
            (bulk("by-cpu-time-us"), Value::Map(vec![(bulk("k"), Value::Int(7))])),
        ]));
        assert!(report.tracking_active);
        assert_eq!(report.total_cpu_us, 50);
        assert_eq!(
            report.by_cpu,
            vec![HotkeyEntry {
                key: "k".into(),
                value: 7
            }]
        );
    }
}

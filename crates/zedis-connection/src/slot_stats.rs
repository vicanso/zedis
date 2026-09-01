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

//! `CLUSTER SLOT-STATS` (Redis 8.2) reply parsing.
//!
//! Each node reports only the slots assigned to it, so the caller queries
//! every master with the same `ORDERBY <metric> LIMIT n DESC` and merges —
//! slots are disjoint across masters, concatenation never double counts.
//! `key-count` is always present; `memory-bytes` / `cpu-usec` /
//! `network-bytes-in` / `network-bytes-out` appear only when the server
//! runs with `cluster-slot-stats-enabled yes` (start-time only config).

use redis::Value;

/// The sortable metrics `ORDERBY` accepts. `KeyCount` is the only one a
/// server without `cluster-slot-stats-enabled` understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlotStatMetric {
    #[default]
    KeyCount,
    MemoryBytes,
    CpuUsec,
    NetworkBytesIn,
    NetworkBytesOut,
}

impl SlotStatMetric {
    pub const ALL: &'static [SlotStatMetric] = &[
        SlotStatMetric::KeyCount,
        SlotStatMetric::MemoryBytes,
        SlotStatMetric::CpuUsec,
        SlotStatMetric::NetworkBytesIn,
        SlotStatMetric::NetworkBytesOut,
    ];

    /// The metric name as `ORDERBY` (and the reply) spells it.
    pub const fn word(self) -> &'static str {
        match self {
            SlotStatMetric::KeyCount => "key-count",
            SlotStatMetric::MemoryBytes => "memory-bytes",
            SlotStatMetric::CpuUsec => "cpu-usec",
            SlotStatMetric::NetworkBytesIn => "network-bytes-in",
            SlotStatMetric::NetworkBytesOut => "network-bytes-out",
        }
    }
}

/// One slot's usage row, tagged with the master that owns it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotStatRow {
    pub slot: u16,
    /// `host:port` of the owning master.
    pub node: String,
    pub key_count: u64,
    /// `None` when the server runs without `cluster-slot-stats-enabled`.
    pub memory_bytes: Option<u64>,
    pub cpu_usec: Option<u64>,
    pub network_bytes_in: Option<u64>,
    pub network_bytes_out: Option<u64>,
}

impl SlotStatRow {
    /// The row's value for `metric` (missing extended metrics read as 0).
    pub fn metric(&self, metric: SlotStatMetric) -> u64 {
        match metric {
            SlotStatMetric::KeyCount => self.key_count,
            SlotStatMetric::MemoryBytes => self.memory_bytes.unwrap_or(0),
            SlotStatMetric::CpuUsec => self.cpu_usec.unwrap_or(0),
            SlotStatMetric::NetworkBytesIn => self.network_bytes_in.unwrap_or(0),
            SlotStatMetric::NetworkBytesOut => self.network_bytes_out.unwrap_or(0),
        }
    }

    /// Whether the reply carried the extended (enable-gated) metrics.
    pub fn has_extended_metrics(&self) -> bool {
        self.cpu_usec.is_some()
    }
}

/// Parse one node's `CLUSTER SLOT-STATS` reply into rows tagged `node`.
/// Malformed entries are skipped, not fatal — a future server may append
/// fields or reshape an entry.
pub fn parse_slot_stats(reply: &Value, node: &str) -> Vec<SlotStatRow> {
    let Value::Array(entries) = reply else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let Value::Array(pair) = entry else { return None };
            let slot = u16::try_from(as_u64(pair.first()?)?).ok()?;
            let mut row = SlotStatRow {
                slot,
                node: node.to_string(),
                ..Default::default()
            };
            for (name, value) in stat_pairs(pair.get(1)?) {
                let value = as_u64(value);
                match name.as_str() {
                    "key-count" => row.key_count = value.unwrap_or(0),
                    "memory-bytes" => row.memory_bytes = value,
                    "cpu-usec" => row.cpu_usec = value,
                    "network-bytes-in" => row.network_bytes_in = value,
                    "network-bytes-out" => row.network_bytes_out = value,
                    _ => {}
                }
            }
            Some(row)
        })
        .collect()
}

/// `metric-name value …` pairs, RESP3 map or RESP2 flat array.
fn stat_pairs(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Map(items) => items.iter().filter_map(|(k, v)| Some((as_string(k)?, v))).collect(),
        Value::Array(items) => items
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|[k, v]| Some((as_string(k)?, v)))
            .collect(),
        _ => Vec::new(),
    }
}

fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(n) => u64::try_from(*n).ok(),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    fn entry(slot: i64, stats: &[(&str, i64)]) -> Value {
        Value::Array(vec![
            Value::Int(slot),
            Value::Array(stats.iter().flat_map(|(k, v)| [bulk(k), Value::Int(*v)]).collect()),
        ])
    }

    #[test]
    fn parses_extended_reply_and_tags_node() {
        // Shape captured live from 8.6.1 with cluster-slot-stats-enabled.
        let reply = Value::Array(vec![
            entry(
                12182,
                &[
                    ("key-count", 1),
                    ("memory-bytes", 32),
                    ("cpu-usec", 42),
                    ("network-bytes-in", 1162),
                    ("network-bytes-out", 275),
                ],
            ),
            entry(0, &[("key-count", 0)]),
        ]);
        let rows = parse_slot_stats(&reply, "127.0.0.1:17000");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].slot, 12182);
        assert_eq!(rows[0].node, "127.0.0.1:17000");
        assert_eq!(rows[0].key_count, 1);
        assert_eq!(rows[0].cpu_usec, Some(42));
        assert!(rows[0].has_extended_metrics());
        assert_eq!(rows[0].metric(SlotStatMetric::NetworkBytesOut), 275);
        // key-count-only server: extended metrics stay None, not 0.
        assert!(!rows[1].has_extended_metrics());
        assert_eq!(rows[1].memory_bytes, None);
    }

    #[test]
    fn malformed_entries_are_skipped() {
        let reply = Value::Array(vec![bulk("junk"), entry(3, &[("key-count", 9)])]);
        let rows = parse_slot_stats(&reply, "n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slot, 3);
        assert_eq!(rows[0].key_count, 9);
    }

    #[test]
    fn orderby_words_match_the_wire_spelling() {
        let words: Vec<_> = SlotStatMetric::ALL.iter().map(|m| m.word()).collect();
        assert_eq!(
            words,
            vec![
                "key-count",
                "memory-bytes",
                "cpu-usec",
                "network-bytes-in",
                "network-bytes-out"
            ]
        );
    }
}

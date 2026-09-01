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

//! `INFO keysizes` parsing — the server-side per-type key-size histogram
//! (Redis 8+; extended in 8.6).
//!
//! Section lines look like
//! `db0_distrib_strings_sizes:2=3,1K=20` — per database, one line per data
//! type, holding power-of-two buckets as `label=count` pairs. Strings are
//! bucketed by value **bytes** (`sizes`), containers by **element count**
//! (`items`). Labels come pre-humanized (`2`, `512`, `1K`, `16M`); they are
//! kept verbatim for display and ordered by their numeric value.

/// What a type's buckets count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysizesUnit {
    /// Value size in bytes (strings).
    Bytes,
    /// Element count (lists, sets, zsets, hashes, streams).
    Items,
}

/// One data type's bucket histogram for one database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeysizesDist {
    /// Type name as the section spells it: `strings`, `lists`, `sets`,
    /// `zsets`, `hashes`, ….
    pub type_name: String,
    pub unit: KeysizesUnit,
    /// `(bucket label, key count)`, ascending by bucket value. The label is
    /// the bucket's lower bound.
    pub buckets: Vec<(String, u64)>,
}

impl KeysizesDist {
    /// Total keys of this type.
    pub fn total(&self) -> u64 {
        self.buckets.iter().map(|(_, count)| count).sum()
    }
}

/// A bucket label's numeric value (`512` → 512, `1K` → 1024, `16M` → 16 Mi),
/// for ordering. Unknown labels sort last.
fn bucket_value(label: &str) -> u64 {
    let label = label.trim();
    let (digits, multiplier) = match label.as_bytes().last() {
        Some(b'K' | b'k') => (&label[..label.len() - 1], 1u64 << 10),
        Some(b'M' | b'm') => (&label[..label.len() - 1], 1u64 << 20),
        Some(b'G' | b'g') => (&label[..label.len() - 1], 1u64 << 30),
        Some(b'T' | b't') => (&label[..label.len() - 1], 1u64 << 40),
        _ => (label, 1),
    };
    digits
        .parse::<u64>()
        .map(|n| n.saturating_mul(multiplier))
        .unwrap_or(u64::MAX)
}

/// Parse the `# Keysizes` lines of an `INFO` reply for one database.
/// Returns one distribution per data type, types in the order the section
/// lists them, buckets ascending. Unparsable lines are skipped.
pub fn parse_keysizes(info: &str, db: usize) -> Vec<KeysizesDist> {
    let prefix = format!("db{db}_distrib_");
    let mut dists = Vec::new();
    for line in info.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let Some((name, payload)) = rest.split_once(':') else {
            continue;
        };
        // `strings_sizes` / `lists_items` → type + unit.
        let (type_name, unit) = match name.rsplit_once('_') {
            Some((t, "sizes")) => (t, KeysizesUnit::Bytes),
            Some((t, "items")) => (t, KeysizesUnit::Items),
            _ => continue,
        };
        let mut buckets: Vec<(String, u64)> = payload
            .split(',')
            .filter_map(|pair| {
                let (label, count) = pair.split_once('=')?;
                Some((label.trim().to_string(), count.trim().parse().ok()?))
            })
            .collect();
        if buckets.is_empty() {
            continue;
        }
        buckets.sort_by_key(|(label, _)| bucket_value(label));
        dists.push(KeysizesDist {
            type_name: type_name.to_string(),
            unit,
            buckets,
        });
    }
    dists
}

/// Merge per-node distributions (cluster masters) by summing counts for the
/// same `(type, bucket)`. Type order follows first appearance; buckets are
/// re-sorted ascending.
pub fn merge_keysizes(per_node: Vec<Vec<KeysizesDist>>) -> Vec<KeysizesDist> {
    let mut merged: Vec<KeysizesDist> = Vec::new();
    for dists in per_node {
        for dist in dists {
            match merged.iter_mut().find(|m| m.type_name == dist.type_name) {
                None => merged.push(dist),
                Some(existing) => {
                    for (label, count) in dist.buckets {
                        match existing.buckets.iter_mut().find(|(l, _)| *l == label) {
                            Some((_, c)) => *c = c.saturating_add(count),
                            None => existing.buckets.push((label, count)),
                        }
                    }
                    existing.buckets.sort_by_key(|(label, _)| bucket_value(label));
                }
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured live from Redis 8.6.1.
    const INFO: &str = "# Keysizes\r\n\
        db0_distrib_strings_sizes:2=3,1K=20\r\n\
        db0_distrib_lists_items:8=1\r\n\
        db0_distrib_zsets_items:2=1\r\n\
        db0_distrib_hashes_items:1=1\r\n\
        db1_distrib_strings_sizes:4=7\r\n";

    #[test]
    fn parses_only_the_requested_db() {
        let dists = parse_keysizes(INFO, 0);
        let names: Vec<_> = dists.iter().map(|d| d.type_name.as_str()).collect();
        assert_eq!(names, vec!["strings", "lists", "zsets", "hashes"]);
        assert_eq!(dists[0].unit, KeysizesUnit::Bytes);
        assert_eq!(dists[0].buckets, vec![("2".to_string(), 3), ("1K".to_string(), 20)]);
        assert_eq!(dists[0].total(), 23);
        assert_eq!(dists[1].unit, KeysizesUnit::Items);

        let db1 = parse_keysizes(INFO, 1);
        assert_eq!(db1.len(), 1);
        assert_eq!(db1[0].buckets, vec![("4".to_string(), 7)]);
        assert!(parse_keysizes(INFO, 2).is_empty());
    }

    #[test]
    fn buckets_order_by_value_not_lexicographically() {
        let dists = parse_keysizes("db0_distrib_strings_sizes:1K=1,16=2,2M=3,512=4\n", 0);
        let labels: Vec<_> = dists[0].buckets.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["16", "512", "1K", "2M"]);
    }

    #[test]
    fn merge_sums_matching_buckets_across_nodes() {
        let a = parse_keysizes("db0_distrib_strings_sizes:2=3,1K=20\ndb0_distrib_lists_items:8=1\n", 0);
        let b = parse_keysizes("db0_distrib_strings_sizes:2=5,64=1\n", 0);
        let merged = merge_keysizes(vec![a, b]);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].buckets,
            vec![("2".to_string(), 8), ("64".to_string(), 1), ("1K".to_string(), 20)]
        );
        assert_eq!(merged[1].type_name, "lists");
    }

    #[test]
    fn junk_lines_are_skipped() {
        assert!(parse_keysizes("db0_distrib_strings:oops\r\nnot_a_line\r\n", 0).is_empty());
        assert!(parse_keysizes("db0_distrib_strings_sizes:\r\n", 0).is_empty());
    }
}

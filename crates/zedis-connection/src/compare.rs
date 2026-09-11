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

//! Prefix-level comparison of two databases: which keys exist only on one
//! side, and which exist on both with a different value.
//!
//! Values are compared in their readable form, read type-aware on both
//! sides — never as `DUMP` bytes, which differ between encodings
//! (`listpack` vs `hashtable`) of the same content. A set's members and a
//! hash's fields compare regardless of order; a list, a sorted set and a
//! stream are ordered and compare as such. Values past the read limits and
//! module types are reported as not compared rather than guessed at.

use super::readable_export::{ReadLimits, ReadableEntry, ReadableValue, read_readable_chunk};
use crate::error::Error;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Keys a `SCAN` round asks for.
const SCAN_COUNT: u64 = 1000;
/// Keys read per round trip when comparing values.
const COMPARE_CHUNK: usize = 64;

/// One side of a comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareSide {
    pub server_id: String,
    pub db: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOptions {
    /// Keys starting with this; empty means every key.
    pub prefix: String,
    /// Keys scanned per side before the scan stops and the report is
    /// marked partial for that side.
    pub limit: usize,
}

/// Where a comparison is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompareStage {
    #[default]
    ScanningSource,
    ScanningTarget,
    Comparing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareProgress {
    pub stage: CompareStage,
    pub source_keys: u64,
    pub target_keys: u64,
    /// Keys on both sides whose values were read so far, of `to_compare`.
    pub compared: u64,
    pub to_compare: u64,
}

/// Why a key on both sides is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDifference {
    /// Same type, different content.
    Value,
    /// A different type on each side.
    Type { source: String, target: String },
    /// Not compared: past the read limits on a side, or a module type
    /// with no readable form.
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferingKey {
    pub key: String,
    /// The type on the source.
    pub key_type: String,
    pub difference: KeyDifference,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareReport {
    /// `(key, type)`, sorted by key.
    pub only_source: Vec<(String, String)>,
    pub only_target: Vec<(String, String)>,
    pub differing: Vec<DifferingKey>,
    /// Keys on both sides with the same value.
    pub same: u64,
    /// The scan stopped at the limit on this side, so its lists are partial.
    pub source_capped: bool,
    pub target_capped: bool,
    pub cancelled: bool,
}

/// `prefix` as a `SCAN MATCH` pattern: the glob characters a key may
/// contain are escaped so the prefix matches literally.
pub fn prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('*');
    pattern
}

/// Every key under `prefix` on one side with its type, up to `limit`.
/// The second value says whether the limit cut the scan short.
async fn scan_side(
    server_id: &str,
    db: usize,
    pattern: &str,
    limit: usize,
    cancel: &AtomicBool,
    mut on_count: impl FnMut(u64),
) -> Result<(BTreeMap<String, String>, bool)> {
    let client = super::get_connection_manager().get_client(server_id, db).await?;
    let mut keys = BTreeMap::new();
    let mut cursors: Option<Vec<u64>> = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok((keys, false));
        }
        let (next, page) = client.scan(cursors, pattern, SCAN_COUNT, false, None).await?;
        for (key, key_type, _) in page {
            keys.insert(key, key_type);
        }
        on_count(keys.len() as u64);
        if keys.len() >= limit {
            return Ok((keys, true));
        }
        if next.iter().all(|cursor| *cursor == 0) {
            return Ok((keys, false));
        }
        cursors = Some(next);
    }
}

/// Compare the keys under `options.prefix` on `source` against `target`.
/// `progress` is called as the scans and the value reads advance; `cancel`
/// stops the work between rounds and marks the report cancelled.
pub async fn compare_prefix(
    source: &CompareSide,
    target: &CompareSide,
    options: &CompareOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(CompareProgress),
) -> Result<CompareReport> {
    let pattern = prefix_pattern(&options.prefix);
    let limit = options.limit.max(1);
    let mut state = CompareProgress::default();

    let (source_keys, source_capped) = scan_side(&source.server_id, source.db, &pattern, limit, cancel, |count| {
        state.source_keys = count;
        progress(state.clone());
    })
    .await?;
    state.stage = CompareStage::ScanningTarget;
    progress(state.clone());
    let (target_keys, target_capped) = scan_side(&target.server_id, target.db, &pattern, limit, cancel, |count| {
        state.target_keys = count;
        progress(state.clone());
    })
    .await?;

    let mut report = CompareReport {
        source_capped,
        target_capped,
        ..Default::default()
    };
    let mut both: Vec<(String, String, String)> = Vec::new();
    for (key, key_type) in &source_keys {
        match target_keys.get(key) {
            Some(target_type) => both.push((key.clone(), key_type.clone(), target_type.clone())),
            None => report.only_source.push((key.clone(), key_type.clone())),
        }
    }
    for (key, key_type) in &target_keys {
        if !source_keys.contains_key(key) {
            report.only_target.push((key.clone(), key_type.clone()));
        }
    }
    if cancel.load(Ordering::Acquire) {
        report.cancelled = true;
        return Ok(report);
    }

    state.stage = CompareStage::Comparing;
    state.to_compare = both.len() as u64;
    progress(state.clone());

    // A type mismatch needs no read; everything else is read on both sides.
    let mut to_read: Vec<(String, String)> = Vec::with_capacity(both.len());
    for (key, source_type, target_type) in both {
        if source_type != target_type {
            report.differing.push(DifferingKey {
                key,
                key_type: source_type.clone(),
                difference: KeyDifference::Type {
                    source: source_type,
                    target: target_type,
                },
            });
            state.compared += 1;
        } else {
            to_read.push((key, source_type));
        }
    }

    if !to_read.is_empty() {
        let source_client = super::get_connection_manager()
            .get_client(&source.server_id, source.db)
            .await?;
        let target_client = super::get_connection_manager()
            .get_client(&target.server_id, target.db)
            .await?;
        let mut source_conn = source_client.connection();
        let mut target_conn = target_client.connection();
        for chunk in to_read.chunks(COMPARE_CHUNK) {
            if cancel.load(Ordering::Acquire) {
                report.cancelled = true;
                break;
            }
            let keys: Vec<String> = chunk.iter().map(|(key, _)| key.clone()).collect();
            let source_entries =
                index_entries(read_readable_chunk(&mut source_conn, &keys, ReadLimits::default()).await?);
            let target_entries =
                index_entries(read_readable_chunk(&mut target_conn, &keys, ReadLimits::default()).await?);
            for (key, key_type) in chunk {
                match (source_entries.get(key), target_entries.get(key)) {
                    (Some(a), Some(b)) => match compare_entries(a, b) {
                        Some(true) => report.same += 1,
                        Some(false) => report.differing.push(DifferingKey {
                            key: key.clone(),
                            key_type: key_type.clone(),
                            difference: KeyDifference::Value,
                        }),
                        None => report.differing.push(DifferingKey {
                            key: key.clone(),
                            key_type: key_type.clone(),
                            difference: KeyDifference::Unchecked,
                        }),
                    },
                    // Gone from a side since the scan: report it as such.
                    (Some(_), None) => report.only_source.push((key.clone(), key_type.clone())),
                    (None, Some(_)) => report.only_target.push((key.clone(), key_type.clone())),
                    (None, None) => {}
                }
            }
            state.compared += chunk.len() as u64;
            progress(state.clone());
        }
    }
    report.only_source.sort();
    report.only_target.sort();
    report.differing.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(report)
}

fn index_entries(entries: Vec<ReadableEntry>) -> BTreeMap<String, ReadableEntry> {
    entries.into_iter().map(|entry| (entry.key.clone(), entry)).collect()
}

/// `Some(equal)`, or `None` when a side could not be compared — cut by the
/// read limits, or a type with no readable form.
fn compare_entries(a: &ReadableEntry, b: &ReadableEntry) -> Option<bool> {
    if a.truncated || b.truncated {
        return None;
    }
    match (&a.value, &b.value) {
        (Some(x), Some(y)) => Some(values_equal(x, y)),
        _ => None,
    }
}

/// Equality in the terms the type defines: a set and a hash have no order,
/// the rest do.
pub fn values_equal(a: &ReadableValue, b: &ReadableValue) -> bool {
    match (a, b) {
        (ReadableValue::Set(x), ReadableValue::Set(y)) => {
            let mut x = x.clone();
            let mut y = y.clone();
            x.sort();
            y.sort();
            x == y
        }
        (ReadableValue::Hash(x), ReadableValue::Hash(y)) => {
            let mut x = x.clone();
            let mut y = y.clone();
            x.sort();
            y.sort();
            x == y
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn sets_and_hashes_compare_regardless_of_order_and_lists_do_not() {
        let set_a = ReadableValue::Set(vec![text("a"), text("b")]);
        let set_b = ReadableValue::Set(vec![text("b"), text("a")]);
        assert!(values_equal(&set_a, &set_b));
        let hash_a = ReadableValue::Hash(vec![(text("f"), text("1")), (text("g"), text("2"))]);
        let hash_b = ReadableValue::Hash(vec![(text("g"), text("2")), (text("f"), text("1"))]);
        assert!(values_equal(&hash_a, &hash_b));
        let list_a = ReadableValue::List(vec![text("a"), text("b")]);
        let list_b = ReadableValue::List(vec![text("b"), text("a")]);
        assert!(!values_equal(&list_a, &list_b));
        assert!(!values_equal(&set_a, &list_a), "different kinds never match");
        assert!(values_equal(
            &ReadableValue::Text(text("x")),
            &ReadableValue::Text(text("x"))
        ));
    }

    #[test]
    fn a_cut_or_unreadable_side_is_not_compared() {
        let entry = |value: Option<ReadableValue>, truncated: bool| ReadableEntry {
            key: text("k"),
            key_type: text("hash"),
            pttl_ms: -1,
            value,
            truncated,
        };
        let full = entry(Some(ReadableValue::Text(text("x"))), false);
        assert_eq!(compare_entries(&full, &full), Some(true));
        assert_eq!(
            compare_entries(&full, &entry(Some(ReadableValue::Text(text("y"))), false)),
            Some(false)
        );
        assert_eq!(
            compare_entries(&full, &entry(Some(ReadableValue::Text(text("x"))), true)),
            None
        );
        assert_eq!(compare_entries(&full, &entry(None, false)), None);
    }

    #[test]
    fn the_prefix_matches_literally() {
        assert_eq!(prefix_pattern("user:"), "user:*");
        assert_eq!(prefix_pattern(""), "*");
        assert_eq!(prefix_pattern("a*b?[c]"), "a\\*b\\?\\[c\\]*");
    }
}

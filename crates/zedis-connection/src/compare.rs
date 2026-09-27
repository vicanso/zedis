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
use crate::keyspace::key_types;
use crate::server_db::ServerDb;
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

/// The keys one side listed and the other did not, with the other side's
/// type for each where it has the key after all — asked only when that
/// side's scan was `capped`; a complete scan already said it has none.
async fn present_on(
    other: &ServerDb,
    capped: bool,
    unlisted: Vec<(String, String)>,
) -> Result<Vec<(String, String, Option<String>)>> {
    if !capped || unlisted.is_empty() {
        return Ok(unlisted.into_iter().map(|(key, ty)| (key, ty, None)).collect());
    }
    let types = key_types(other, unlisted.iter().map(|(key, _)| key.clone()).collect()).await?;
    Ok(unlisted
        .into_iter()
        .zip(types)
        .map(|((key, ty), other_type)| (key, ty, (other_type != "none").then_some(other_type)))
        .collect())
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
    let mut unlisted_on_target: Vec<(String, String)> = Vec::new();
    for (key, key_type) in &source_keys {
        match target_keys.get(key) {
            Some(target_type) => both.push((key.clone(), key_type.clone(), target_type.clone())),
            None => unlisted_on_target.push((key.clone(), key_type.clone())),
        }
    }
    let unlisted_on_source: Vec<(String, String)> = target_keys
        .iter()
        .filter(|(key, _)| !source_keys.contains_key(*key))
        .map(|(key, key_type)| (key.clone(), key_type.clone()))
        .collect();
    // A side the limit cut short listed only part of its keys: one the other
    // side has and it did not list may still be there, and calling it
    // missing was a false difference. Ask that side for those keys by name.
    let source_at = ServerDb::new(source.server_id.as_str(), source.db);
    let target_at = ServerDb::new(target.server_id.as_str(), target.db);
    for (key, source_type, target_type) in present_on(&target_at, target_capped, unlisted_on_target).await? {
        match target_type {
            Some(target_type) => both.push((key, source_type, target_type)),
            None => report.only_source.push((key, source_type)),
        }
    }
    for (key, target_type, source_type) in present_on(&source_at, source_capped, unlisted_on_source).await? {
        match source_type {
            Some(source_type) => both.push((key, source_type, target_type)),
            None => report.only_target.push((key, target_type)),
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
        let source_at = ServerDb::new(source.server_id.as_str(), source.db);
        let target_at = ServerDb::new(target.server_id.as_str(), target.db);
        for chunk in to_read.chunks(COMPARE_CHUNK) {
            if cancel.load(Ordering::Acquire) {
                report.cancelled = true;
                break;
            }
            let keys: Vec<String> = chunk.iter().map(|(key, _)| key.clone()).collect();
            let source_entries = index_entries(read_readable_chunk(&source_at, &keys, ReadLimits::default()).await?);
            let target_entries = index_entries(read_readable_chunk(&target_at, &keys, ReadLimits::default()).await?);
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
        // The values come as text with every byte that is not UTF-8 replaced
        // by U+FFFD, so two binary values differing only in those bytes read
        // the same — `\x80\x01` and `\x81\x01` were counted as equal. Where
        // a replacement shows, the text cannot say: unchecked, not "same".
        (Some(x), Some(y)) if has_replacement(x) || has_replacement(y) => None,
        (Some(x), Some(y)) => Some(values_equal(x, y)),
        _ => None,
    }
}

/// Whether any text in the value carries U+FFFD, the mark a lossy decoding
/// leaves (a value that really holds one is left unchecked too — the safe
/// side of the doubt).
fn has_replacement(value: &ReadableValue) -> bool {
    fn marked<'a>(mut texts: impl Iterator<Item = &'a String>) -> bool {
        texts.any(|text| text.contains(char::REPLACEMENT_CHARACTER))
    }
    match value {
        ReadableValue::Text(text) => marked(std::iter::once(text)),
        ReadableValue::List(items) | ReadableValue::Set(items) => marked(items.iter()),
        ReadableValue::Hash(pairs) => marked(pairs.iter().flat_map(|(f, v)| [f, v])),
        ReadableValue::Zset(members) => marked(members.iter().map(|(m, _)| m)),
        ReadableValue::Stream(entries) => marked(
            entries
                .iter()
                .flat_map(|(_, fields)| fields.iter().flat_map(|(f, v)| [f, v])),
        ),
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

    #[test]
    fn binary_values_that_decode_alike_are_not_called_the_same() {
        let entry = |bytes: &[u8]| ReadableEntry {
            key: "k".to_string(),
            key_type: "string".to_string(),
            pttl_ms: -1,
            value: Some(ReadableValue::Text(String::from_utf8_lossy(bytes).into_owned())),
            truncated: false,
        };
        // Both read as "\u{FFFD}\u{1}".
        assert_eq!(compare_entries(&entry(b"\x80\x01"), &entry(b"\x81\x01")), None);
        assert_eq!(compare_entries(&entry(b"plain"), &entry(b"plain")), Some(true));
        assert_eq!(compare_entries(&entry(b"plain"), &entry(b"other")), Some(false));
    }

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

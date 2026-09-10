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

//! Session change log for collection keys (Hash / List / Set / ZSet).
//!
//! String history snapshots the whole value before each save. That model
//! cannot work for a collection: it is loaded a page at a time and edited
//! one field at a time, and snapshotting a million-field hash per edit is
//! not an option. So a collection records *changes* instead — "field `f`
//! went from `a` to `b`" — which is also the more useful unit: it says what
//! was edited, not just that something was.
//!
//! The structured diff falls out of the log. [`net_snapshots`] reduces the
//! entries to "before the first change" and "after the last change" per
//! field, and `diff::kv_diff` turns those into Added / Removed / Changed.
//! Only edits made in this app, in this session, are known — the log never
//! claims to describe changes made by other clients.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

/// Entries kept per key; the oldest is evicted past this.
pub const CHANGE_LOG_CAPACITY: usize = 200;

/// Values longer than this (in bytes) are stored as a fingerprint instead of
/// the text, so a log of edits to large values stays small.
pub const CHANGE_VALUE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// One field, member or position went from `old` to `new`. `None` on a
    /// side means absent: `old: None` is an add, `new: None` a removal.
    Element,
    /// A whole-key operation (`LTRIM`, a pop, `HINCRBY`) whose effect on
    /// individual elements was not read back. Listed in the log, left out of
    /// the net diff — pretending to know its element-level effect would make
    /// the diff lie.
    Operation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    /// Unix seconds.
    pub at: i64,
    pub kind: ChangeKind,
    /// The field (hash), member (set / zset), position (list), or the
    /// operation's own description.
    pub target: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

impl ChangeEntry {
    pub fn element(at: i64, target: impl Into<String>, old: Option<&str>, new: Option<&str>) -> Self {
        Self {
            at,
            kind: ChangeKind::Element,
            target: target.into(),
            old: old.map(stored_value),
            new: new.map(stored_value),
        }
    }

    pub fn operation(at: i64, description: impl Into<String>) -> Self {
        Self {
            at,
            kind: ChangeKind::Operation,
            target: description.into(),
            old: None,
            new: None,
        }
    }
}

/// The value as the log keeps it: verbatim up to [`CHANGE_VALUE_LIMIT`],
/// beyond that a length plus a content fingerprint. The fingerprint is what
/// keeps two different large values of the same length from comparing equal
/// and silently dropping out of the net diff as "unchanged".
fn stored_value(value: &str) -> String {
    if value.len() <= CHANGE_VALUE_LIMIT {
        return value.to_string();
    }
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("<{} bytes, #{:016x}>", value.len(), hasher.finish())
}

/// Append `entry` (oldest first), evicting from the front past the capacity.
pub fn push_change(log: &mut VecDeque<ChangeEntry>, entry: ChangeEntry) {
    // An edit that changes nothing is not a change worth a row.
    if entry.kind == ChangeKind::Element && entry.old == entry.new {
        return;
    }
    if log.len() >= CHANGE_LOG_CAPACITY {
        log.pop_front();
    }
    log.push_back(entry);
}

/// One side of a before/after comparison: `(field, value)` pairs, the shape
/// `diff::kv_diff` takes.
pub type Snapshot = Vec<(String, String)>;

/// Before-and-after snapshots of every element the log touched, ready for
/// `kv_diff`: `old` holds each target's value before its first change (when
/// it existed), `new` its value after its last change (when it still does).
///
/// A target edited back to where it started, or added and then removed,
/// appears identically in both or in neither, so `kv_diff` drops it — the
/// net diff shows what is different now, not every step taken to get there.
/// Operation entries are skipped; see [`ChangeKind::Operation`].
pub fn net_snapshots(log: &VecDeque<ChangeEntry>) -> (Snapshot, Snapshot) {
    let mut order: Vec<&str> = Vec::new();
    let mut first_old: HashMap<&str, Option<&str>> = HashMap::new();
    let mut last_new: HashMap<&str, Option<&str>> = HashMap::new();
    for entry in log.iter().filter(|e| e.kind == ChangeKind::Element) {
        let target = entry.target.as_str();
        if !first_old.contains_key(target) {
            order.push(target);
            first_old.insert(target, entry.old.as_deref());
        }
        last_new.insert(target, entry.new.as_deref());
    }
    let old = order
        .iter()
        .filter_map(|t| {
            first_old
                .get(t)
                .copied()
                .flatten()
                .map(|v| (t.to_string(), v.to_string()))
        })
        .collect();
    let new = order
        .iter()
        .filter_map(|t| {
            last_new
                .get(t)
                .copied()
                .flatten()
                .map(|v| (t.to_string(), v.to_string()))
        })
        .collect();
    (old, new)
}

/// How many entries the net diff could not account for.
pub fn operation_count(log: &VecDeque<ChangeEntry>) -> usize {
    log.iter().filter(|e| e.kind == ChangeKind::Operation).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{KvDelta, kv_diff};

    fn log_of(entries: Vec<ChangeEntry>) -> VecDeque<ChangeEntry> {
        let mut log = VecDeque::new();
        for entry in entries {
            push_change(&mut log, entry);
        }
        log
    }

    fn deltas(log: &VecDeque<ChangeEntry>) -> Vec<(String, KvDelta)> {
        let (old, new) = net_snapshots(log);
        kv_diff(&old, &new).into_iter().map(|e| (e.key, e.delta)).collect()
    }

    #[test]
    fn the_oldest_entry_goes_first_past_capacity() {
        let mut log = VecDeque::new();
        for n in 0..(CHANGE_LOG_CAPACITY + 5) {
            push_change(
                &mut log,
                ChangeEntry::element(n as i64, "f", None, Some(&n.to_string())),
            );
        }
        assert_eq!(log.len(), CHANGE_LOG_CAPACITY);
        assert_eq!(log.front().map(|e| e.at), Some(5));
    }

    #[test]
    fn a_no_op_edit_is_not_recorded() {
        let log = log_of(vec![ChangeEntry::element(1, "f", Some("a"), Some("a"))]);
        assert!(log.is_empty());
    }

    #[test]
    fn edits_that_end_where_they_started_leave_no_net_change() {
        let log = log_of(vec![
            ChangeEntry::element(1, "f", Some("a"), Some("b")),
            ChangeEntry::element(2, "f", Some("b"), Some("a")),
            // Added, then removed again.
            ChangeEntry::element(3, "g", None, Some("x")),
            ChangeEntry::element(4, "g", Some("x"), None),
        ]);
        assert_eq!(log.len(), 4, "every step stays in the log");
        assert!(deltas(&log).is_empty(), "but none of it is a net change");
    }

    #[test]
    fn the_net_diff_names_what_is_different_now() {
        let log = log_of(vec![
            ChangeEntry::element(1, "changed", Some("a"), Some("b")),
            ChangeEntry::element(2, "changed", Some("b"), Some("c")),
            ChangeEntry::element(3, "added", None, Some("x")),
            ChangeEntry::element(4, "removed", Some("y"), None),
        ]);
        let (old, new) = net_snapshots(&log);
        // Before the first change and after the last, per field.
        assert!(old.contains(&("changed".to_string(), "a".to_string())));
        assert!(new.contains(&("changed".to_string(), "c".to_string())));
        let mut got = deltas(&log);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("added".to_string(), KvDelta::Added),
                ("changed".to_string(), KvDelta::Changed),
                ("removed".to_string(), KvDelta::Removed),
            ]
        );
    }

    #[test]
    fn a_rename_is_a_removal_and_an_addition() {
        let log = log_of(vec![
            ChangeEntry::element(1, "old", Some("v"), None),
            ChangeEntry::element(1, "new", None, Some("v")),
        ]);
        let mut got = deltas(&log);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("new".to_string(), KvDelta::Added),
                ("old".to_string(), KvDelta::Removed)
            ]
        );
    }

    #[test]
    fn operations_are_logged_but_kept_out_of_the_net_diff() {
        let log = log_of(vec![
            ChangeEntry::element(1, "f", Some("1"), Some("2")),
            ChangeEntry::operation(2, "HINCRBY f 5"),
        ]);
        assert_eq!(log.len(), 2);
        assert_eq!(operation_count(&log), 1);
        assert_eq!(deltas(&log), vec![("f".to_string(), KvDelta::Changed)]);
    }

    #[test]
    fn large_values_keep_distinct_fingerprints() {
        let a = "a".repeat(CHANGE_VALUE_LIMIT + 1);
        let b = "b".repeat(CHANGE_VALUE_LIMIT + 1);
        // Same length, different content: must still read as a change.
        let log = log_of(vec![ChangeEntry::element(1, "big", Some(&a), Some(&b))]);
        assert_eq!(log.len(), 1, "not collapsed into a no-op");
        let stored = log[0].old.as_deref().unwrap_or_default();
        assert!(
            stored.starts_with('<') && stored.len() < 64,
            "stored as a fingerprint: {stored}"
        );
        assert_eq!(deltas(&log), vec![("big".to_string(), KvDelta::Changed)]);
    }
}

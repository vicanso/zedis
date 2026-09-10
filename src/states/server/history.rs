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

//! In-memory write history for string-key values.
//!
//! Each time the user saves a value through the bytes editor we snapshot
//! the *previous* bytes here so the user can roll back a few versions
//! without leaving the app. The history is purely local — it is never
//! persisted to disk and is cleared on app exit, on key delete, and on
//! server switch. Behavior is roughly that of an undo stack with a
//! bounded ring buffer per key.
//!
//! [`KeyHistories`] owns that buffer together with the collection change log
//! and puts both under one memory budget. The app is usually left running for
//! days, so "cleared on exit" is not a bound: without a ceiling the history of
//! every key ever edited stays for the life of the session. Past the budget
//! the key used least recently loses its history as a whole.
//! Separately, a key not used for [`HISTORY_IDLE_EXPIRY_SECS`] loses its
//! history whether or not the budget is reached.

use ahash::AHashMap;
use bytes::Bytes;
use gpui::SharedString;
use std::collections::VecDeque;
use zedis_core::change_log::{ChangeEntry, push_change};

/// Maximum number of historical versions kept per key. Older entries get
/// evicted FIFO once this is exceeded.
pub const VALUE_HISTORY_CAPACITY: usize = 10;

/// A single past version of a string-key value.
#[derive(Debug, Clone)]
pub struct ValueHistoryEntry {
    /// The bytes that were overwritten by the SET this entry was captured for.
    pub bytes: Bytes,
    /// Unix timestamp (seconds) of when the overwrite happened.
    pub at: i64,
}

impl ValueHistoryEntry {
    pub fn size(&self) -> usize {
        self.bytes.len()
    }
}

/// Push a new entry onto the front of `buffer`, evicting the oldest from
/// the back if it would exceed `VALUE_HISTORY_CAPACITY`. Newest-first
/// ordering keeps the rendering code straightforward — index 0 is "most
/// recent".
///
/// Skips identical consecutive entries: if the same bytes are saved twice
/// in a row, the second push is a no-op. This avoids history bloat from
/// users hitting save without actually changing anything.
pub fn push_history(buffer: &mut VecDeque<ValueHistoryEntry>, entry: ValueHistoryEntry) {
    if let Some(front) = buffer.front()
        && front.bytes == entry.bytes
    {
        return;
    }
    if buffer.len() >= VALUE_HISTORY_CAPACITY {
        buffer.pop_back();
    }
    buffer.push_front(entry);
}

/// A single saved version larger than this is not kept. Values up to
/// `MAX_INLINE_VALUE_SIZE` (5 MB) load into the editor, but ten copies of one
/// of those would be 50 MB of undo for a single key — for a blob that large a
/// version history is the wrong tool, and it would spend the budget below on
/// one key.
pub const VALUE_HISTORY_MAX_VERSION_BYTES: usize = 1024 * 1024;

/// What all the histories of one connection may hold in total.
pub const HISTORY_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// A key whose history has not been used — written to, or opened in the
/// editor — for this long loses it, budget or not. The budget bounds the
/// peak; this is what gives the memory back when Zedis is left open and
/// idle, which would otherwise keep everything under the budget
/// indefinitely. A week, so an edit from last Monday can still be compared
/// on Friday.
pub const HISTORY_IDLE_EXPIRY_SECS: i64 = 7 * 24 * 60 * 60;

/// Rough per-entry cost beyond the payload (the entry itself, allocation
/// headers), so a flood of tiny entries still counts against the budget.
const ENTRY_OVERHEAD_BYTES: usize = 48;

fn value_entry_bytes(entry: &ValueHistoryEntry) -> usize {
    entry.bytes.len() + ENTRY_OVERHEAD_BYTES
}

fn change_entry_bytes(entry: &ChangeEntry) -> usize {
    entry.target.len()
        + entry.old.as_ref().map_or(0, String::len)
        + entry.new.as_ref().map_or(0, String::len)
        + ENTRY_OVERHEAD_BYTES
}

/// When a key's history was last used. `tick` orders keys for budget
/// eviction — timestamps alone tie within a second — and `at` (unix seconds)
/// decides idle expiry.
#[derive(Debug, Clone, Copy)]
struct Usage {
    tick: u64,
    at: i64,
}

/// Per-key String versions and collection change logs for one connection,
/// under a shared memory budget with least-recently-used eviction, plus an
/// idle expiry.
///
/// "Used" means written to, or opened in the editor: a key someone keeps
/// coming back to keeps its history, and one edited once and never opened
/// again goes first — or after a week, whichever comes sooner. Eviction and
/// expiry drop a key's history as a whole rather than trimming every key a
/// little, because a history with its oldest versions shaved off looks
/// complete and is not.
#[derive(Debug, Clone)]
pub struct KeyHistories {
    values: AHashMap<SharedString, VecDeque<ValueHistoryEntry>>,
    changes: AHashMap<SharedString, VecDeque<ChangeEntry>>,
    /// Last use of every key that has history. Kept in step by every method
    /// that adds, moves or drops history, so eviction and expiry never miss a
    /// key and never track one that has nothing left.
    touched: AHashMap<SharedString, Usage>,
    tick: u64,
    budget: usize,
    max_version: usize,
    expiry_secs: i64,
}

impl Default for KeyHistories {
    fn default() -> Self {
        Self {
            values: AHashMap::default(),
            changes: AHashMap::default(),
            touched: AHashMap::default(),
            tick: 0,
            budget: HISTORY_BUDGET_BYTES,
            max_version: VALUE_HISTORY_MAX_VERSION_BYTES,
            expiry_secs: HISTORY_IDLE_EXPIRY_SECS,
        }
    }
}

impl KeyHistories {
    #[cfg(test)]
    fn with_limits(budget: usize, max_version: usize) -> Self {
        Self {
            budget,
            max_version,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_expiry(mut self, secs: i64) -> Self {
        self.expiry_secs = secs;
        self
    }

    pub fn value_history_for(&self, key: &SharedString) -> Option<&VecDeque<ValueHistoryEntry>> {
        self.values.get(key).filter(|history| !history.is_empty())
    }

    pub fn change_log_for(&self, key: &SharedString) -> Option<&VecDeque<ChangeEntry>> {
        self.changes.get(key).filter(|log| !log.is_empty())
    }

    /// Mark `key` as used at `now` (unix seconds). A key with no history is
    /// not tracked: browsing a thousand keys must not grow a thousand-entry
    /// map.
    pub fn touch(&mut self, key: &SharedString, now: i64) {
        if self.values.contains_key(key) || self.changes.contains_key(key) {
            self.tick += 1;
            self.touched.insert(
                key.clone(),
                Usage {
                    tick: self.tick,
                    at: now,
                },
            );
        }
    }

    /// Save the bytes about to be overwritten at `at`, unless they exceed the
    /// per-version cap.
    pub fn push_value(&mut self, key: SharedString, bytes: Bytes, at: i64) {
        if bytes.len() > self.max_version {
            return;
        }
        push_history(
            self.values.entry(key.clone()).or_default(),
            ValueHistoryEntry { bytes, at },
        );
        self.touch(&key, at);
        self.enforce_budget(&key);
    }

    pub fn record_changes(&mut self, key: SharedString, entries: Vec<ChangeEntry>, now: i64) {
        if entries.is_empty() {
            return;
        }
        let log = self.changes.entry(key.clone()).or_default();
        for entry in entries {
            push_change(log, entry);
        }
        // Every entry can be dropped as a no-op, and an empty log is not
        // history worth tracking.
        if log.is_empty() {
            self.changes.remove(&key);
            return;
        }
        self.touch(&key, now);
        self.enforce_budget(&key);
    }

    /// Drop everything recorded for `key` (the key was deleted).
    pub fn forget(&mut self, key: &SharedString) {
        self.values.remove(key);
        self.changes.remove(key);
        self.touched.remove(key);
    }

    /// Keep only the keys `keep` accepts (a prefix or batch delete).
    pub fn retain_keys(&mut self, keep: impl Fn(&SharedString) -> bool) {
        self.values.retain(|key, _| keep(key));
        self.changes.retain(|key, _| keep(key));
        self.touched.retain(|key, _| keep(key));
    }

    /// Move `old`'s history to `new`, keeping its last-use time and order.
    pub fn rename(&mut self, old: &SharedString, new: SharedString) {
        if let Some(history) = self.values.remove(old) {
            self.values.insert(new.clone(), history);
        }
        if let Some(log) = self.changes.remove(old) {
            self.changes.insert(new.clone(), log);
        }
        if let Some(usage) = self.touched.remove(old) {
            self.touched.insert(new, usage);
        }
    }

    pub fn clear(&mut self) {
        self.values.clear();
        self.changes.clear();
        self.touched.clear();
    }

    /// Drop the history of every key not used within the idle expiry.
    /// Called periodically rather than on writes: a Zedis nobody is touching
    /// makes no writes, and that is exactly when the memory should come back.
    /// Returns whether any key lost its history.
    pub fn sweep_expired(&mut self, now: i64) -> bool {
        let expiry = self.expiry_secs;
        let expired: Vec<SharedString> = self
            .touched
            .iter()
            .filter(|(_, usage)| now.saturating_sub(usage.at) >= expiry)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &expired {
            self.forget(key);
        }
        !expired.is_empty()
    }

    fn key_bytes(&self, key: &SharedString) -> usize {
        let values: usize = self
            .values
            .get(key)
            .map_or(0, |history| history.iter().map(value_entry_bytes).sum());
        let changes: usize = self
            .changes
            .get(key)
            .map_or(0, |log| log.iter().map(change_entry_bytes).sum());
        values + changes
    }

    fn bytes_used(&self) -> usize {
        let values: usize = self.values.values().flatten().map(value_entry_bytes).sum();
        let changes: usize = self.changes.values().flatten().map(change_entry_bytes).sum();
        values + changes
    }

    /// Evict least-recently-used keys until the total fits. `protect` is the
    /// key just written: dropping it would lose the edit that was recorded a
    /// moment ago, so when it alone is over budget it simply stays.
    fn enforce_budget(&mut self, protect: &SharedString) {
        let mut used = self.bytes_used();
        while used > self.budget {
            let Some(victim) = self
                .touched
                .iter()
                .filter(|(key, _)| *key != protect)
                .min_by_key(|(_, usage)| usage.tick)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            used = used.saturating_sub(self.key_bytes(&victim));
            self.forget(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(s: &[u8], ts: i64) -> ValueHistoryEntry {
        ValueHistoryEntry {
            bytes: Bytes::copy_from_slice(s),
            at: ts,
        }
    }

    #[test]
    fn pushes_newest_first() {
        let mut buf = VecDeque::new();
        push_history(&mut buf, entry(b"v1", 1));
        push_history(&mut buf, entry(b"v2", 2));
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0].bytes.as_ref(), b"v2");
        assert_eq!(buf[1].bytes.as_ref(), b"v1");
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut buf = VecDeque::new();
        for i in 0..(VALUE_HISTORY_CAPACITY + 5) {
            // Vary the bytes to avoid the consecutive-dedup optimization.
            let s = format!("v{i}");
            push_history(&mut buf, entry(s.as_bytes(), i as i64));
        }
        assert_eq!(buf.len(), VALUE_HISTORY_CAPACITY);
        // Newest is index 0, oldest survivor should be entry index 5
        // (entries 0..=4 evicted).
        assert_eq!(
            buf.front().expect("buffer should have a front entry").at,
            (VALUE_HISTORY_CAPACITY + 4) as i64,
        );
        assert_eq!(buf.back().expect("buffer should have a back entry").at, 5);
    }

    #[test]
    fn dedups_consecutive_identical_writes() {
        let mut buf = VecDeque::new();
        push_history(&mut buf, entry(b"same", 1));
        push_history(&mut buf, entry(b"same", 2));
        push_history(&mut buf, entry(b"same", 3));
        assert_eq!(buf.len(), 1, "identical consecutive writes should collapse");
        // But a change in between still creates two entries.
        push_history(&mut buf, entry(b"diff", 4));
        push_history(&mut buf, entry(b"same", 5));
        assert_eq!(buf.len(), 3);
    }

    fn key(name: &str) -> SharedString {
        SharedString::from(name.to_string())
    }

    fn bytes_of(len: usize) -> Bytes {
        Bytes::from(vec![b'x'; len])
    }

    #[test]
    fn a_version_over_the_size_cap_is_not_kept() {
        let mut histories = KeyHistories::with_limits(HISTORY_BUDGET_BYTES, 10);
        histories.push_value(key("big"), bytes_of(11), 1);
        assert!(histories.value_history_for(&key("big")).is_none());
        histories.push_value(key("big"), bytes_of(10), 2);
        assert_eq!(histories.value_history_for(&key("big")).map(VecDeque::len), Some(1));
    }

    #[test]
    fn the_least_recently_used_key_is_evicted_first() {
        // Each entry costs 100 + 48; two fit in 350, a third does not.
        let mut histories = KeyHistories::with_limits(350, 1_000);
        histories.push_value(key("a"), bytes_of(100), 1);
        histories.push_value(key("b"), bytes_of(100), 2);
        // Opened again, so `a` is now more recent than `b`.
        histories.touch(&key("a"), 3);
        histories.push_value(key("c"), bytes_of(100), 4);
        assert!(histories.value_history_for(&key("a")).is_some());
        assert!(
            histories.value_history_for(&key("b")).is_none(),
            "b was the least recently used"
        );
        assert!(histories.value_history_for(&key("c")).is_some());
        assert!(!histories.touched.contains_key(&key("b")));
    }

    #[test]
    fn the_key_just_written_is_never_the_one_evicted() {
        // A budget smaller than one entry: nothing else to drop, and dropping
        // what was just written would lose the edit.
        let mut histories = KeyHistories::with_limits(100, 1_000);
        histories.push_value(key("only"), bytes_of(200), 1);
        assert!(histories.value_history_for(&key("only")).is_some());
    }

    #[test]
    fn change_logs_count_against_the_same_budget() {
        let mut histories = KeyHistories::with_limits(350, 1_000);
        histories.push_value(key("string"), bytes_of(100), 1);
        let big = "y".repeat(150);
        histories.record_changes(
            key("hash"),
            vec![ChangeEntry::element(2, "f", None, Some(big.as_str()))],
            2,
        );
        histories.record_changes(
            key("zset"),
            vec![ChangeEntry::element(3, "m", None, Some(big.as_str()))],
            3,
        );
        // 148 + 199 + 199 is over 350: the String key goes first, and the
        // total is still over, so the hash goes after it.
        assert!(histories.value_history_for(&key("string")).is_none());
        assert!(histories.change_log_for(&key("hash")).is_none());
        assert!(histories.change_log_for(&key("zset")).is_some());
    }

    #[test]
    fn forget_retain_and_rename_keep_the_usage_in_step() {
        let mut histories = KeyHistories::default();
        histories.push_value(key("a"), bytes_of(1), 1);
        histories.push_value(key("b"), bytes_of(1), 2);
        histories.push_value(key("c:1"), bytes_of(1), 3);

        histories.rename(&key("a"), key("z"));
        assert!(histories.value_history_for(&key("z")).is_some());
        assert!(histories.touched.contains_key(&key("z")));
        assert!(!histories.touched.contains_key(&key("a")));

        histories.forget(&key("b"));
        assert!(!histories.touched.contains_key(&key("b")));

        histories.retain_keys(|k| !k.starts_with("c:"));
        assert!(histories.value_history_for(&key("c:1")).is_none());
        assert!(!histories.touched.contains_key(&key("c:1")));
    }

    #[test]
    fn opening_a_key_without_history_does_not_track_it() {
        let mut histories = KeyHistories::default();
        histories.touch(&key("browsed"), 0);
        assert!(histories.touched.is_empty());
    }

    #[test]
    fn a_batch_of_no_op_changes_leaves_no_empty_log() {
        let mut histories = KeyHistories::default();
        histories.record_changes(key("k"), vec![ChangeEntry::element(1, "f", Some("a"), Some("a"))], 1);
        assert!(histories.change_log_for(&key("k")).is_none());
        assert!(!histories.changes.contains_key(&key("k")));
        assert!(histories.touched.is_empty());
    }

    #[test]
    fn a_key_unused_for_the_expiry_loses_its_history() {
        let mut histories = KeyHistories::default().with_expiry(100);
        histories.push_value(key("stale"), bytes_of(1), 0);
        histories.push_value(key("recent"), bytes_of(1), 50);
        assert!(histories.sweep_expired(120), "the stale key was dropped");
        assert!(
            histories.value_history_for(&key("stale")).is_none(),
            "unused for 120s, past the 100s expiry"
        );
        assert!(
            histories.value_history_for(&key("recent")).is_some(),
            "unused for only 70s"
        );
        assert!(!histories.touched.contains_key(&key("stale")));
    }

    #[test]
    fn opening_a_key_restarts_its_expiry() {
        let mut histories = KeyHistories::default().with_expiry(100);
        histories.push_value(key("k"), bytes_of(1), 0);
        // Opened at 90: the clock restarts from there, not from the write.
        histories.touch(&key("k"), 90);
        assert!(!histories.sweep_expired(150), "nothing was due yet");
        assert!(histories.value_history_for(&key("k")).is_some());
        assert!(histories.sweep_expired(190));
        assert!(histories.value_history_for(&key("k")).is_none());
    }
}

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

//! Persisted key timeline snapshots (`key_timeline` redb table).
//!
//! Stores bounded per-key historical value snapshots keyed by
//! `"{server_id}:{db}:{key}"` so reopening the Key Timeline dialog shows
//! previous session snapshots and diffs across restarts.
//!
//! Retention limits enforced on both load and save:
//! - At most [`KEY_TIMELINE_MAX_SNAPSHOTS`] (50) snapshots per key (newest first).
//! - Payload size capped at [`zedis_core::MAX_INLINE_VALUE_SIZE`].
//! - Snapshots older than [`KEY_TIMELINE_RETENTION_SECS`] (7 days) are pruned.

use super::{KEY_TIMELINE_TABLE, Result, get_database};
#[cfg(target_family = "wasm")]
use crate::mem_store::{ReadableDatabase, ReadableTable};
use dashmap::DashMap;
#[cfg(not(target_family = "wasm"))]
use redb::ReadableDatabase;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tracing::warn;
use zedis_core::{MAX_INLINE_VALUE_SIZE, now_unix_secs};

/// Maximum number of snapshots retained per `(server_id, db, key)`.
pub const KEY_TIMELINE_MAX_SNAPSHOTS: usize = 50;

/// Retention window for persisted key timeline snapshots (7 days in seconds).
pub const KEY_TIMELINE_RETENTION_SECS: i64 = 7 * 86_400;

static FALLBACK_TIMELINES: LazyLock<DashMap<String, Vec<StoredKeyTimelineSnapshot>>> = LazyLock::new(DashMap::new);

/// One persisted snapshot of a key's state at a point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredKeyTimelineSnapshot {
    /// Unix timestamp in seconds when the snapshot was captured.
    pub recorded_at: i64,
    /// Display clock string (`HH:MM:SS`).
    pub timestamp: String,
    /// Event label (`initial`, `manual`, `poll`, `set`, `hset`, `expire`, etc.).
    pub event: String,
    /// Whether the key existed at capture time.
    pub exists: bool,
    /// Redis key type (`string`, `hash`, `list`, `set`, `zset`, `stream`, `none`, ...).
    pub key_type: String,
    /// Remaining TTL in seconds (`-1` for no expiry, `-2` for missing/expired).
    pub ttl_secs: i64,
    /// Formatted text representation of the key's value for line diffing.
    pub snapshot_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct KeyTimelineDoc {
    pub snapshots: Vec<StoredKeyTimelineSnapshot>,
}

/// Formats the redb table key `"{server_id}:{db}:{key}"`.
pub fn key_timeline_scope(server_id: &str, db: usize, key: &str) -> String {
    format!("{server_id}:{db}:{key}")
}

fn is_within_size_limit(snapshot: &StoredKeyTimelineSnapshot) -> bool {
    (snapshot.snapshot_text.len() as u64) <= MAX_INLINE_VALUE_SIZE
}

fn is_within_retention(snapshot: &StoredKeyTimelineSnapshot, now_secs: i64) -> bool {
    if snapshot.recorded_at <= 0 || now_secs <= 0 {
        return true;
    }
    now_secs.saturating_sub(snapshot.recorded_at) <= KEY_TIMELINE_RETENTION_SECS
}

fn prune_snapshots(snapshots: &mut Vec<StoredKeyTimelineSnapshot>, now_secs: i64) {
    snapshots.retain(|s| is_within_size_limit(s) && is_within_retention(s, now_secs));
    if snapshots.len() > KEY_TIMELINE_MAX_SNAPSHOTS {
        snapshots.truncate(KEY_TIMELINE_MAX_SNAPSHOTS);
    }
}

pub struct KeyTimelineManager;

impl KeyTimelineManager {
    /// Lists persisted snapshots for `(server_id, db, key)` in newest-first order,
    /// pruning expired (> 7 days) or oversized entries and capping at 50.
    /// Unreadable rows are logged and skipped without deleting them.
    pub fn list_snapshots(server_id: &str, db: usize, key: &str) -> Vec<StoredKeyTimelineSnapshot> {
        Self::list_snapshots_at(server_id, db, key, now_unix_secs())
    }

    pub fn list_snapshots_at(server_id: &str, db: usize, key: &str, now_secs: i64) -> Vec<StoredKeyTimelineSnapshot> {
        if server_id.is_empty() || key.is_empty() {
            return Vec::new();
        }
        let scope = key_timeline_scope(server_id, db, key);
        let Ok(database) = get_database() else {
            let Some(mut entry) = FALLBACK_TIMELINES.get_mut(&scope) else {
                return Vec::new();
            };
            prune_snapshots(&mut entry, now_secs);
            return entry.clone();
        };

        let Ok(read_txn) = database.begin_read() else {
            return Vec::new();
        };
        let Ok(table) = read_txn.open_table(KEY_TIMELINE_TABLE) else {
            return Vec::new();
        };
        let Ok(Some(raw)) = table.get(scope.as_str()) else {
            return Vec::new();
        };

        let mut doc: KeyTimelineDoc = match serde_json::from_slice(raw.value()) {
            Ok(doc) => doc,
            Err(e) => {
                warn!(scope = %scope, error = %e, "unreadable key timeline record, skipped");
                return Vec::new();
            }
        };
        prune_snapshots(&mut doc.snapshots, now_secs);
        doc.snapshots
    }

    /// Prepends a new snapshot (newest first) for `(server_id, db, key)` and persists
    /// the bounded list. Snapshots whose payload exceeds [`MAX_INLINE_VALUE_SIZE`]
    /// are skipped so oversized payloads never bloat local storage.
    pub fn append_snapshot(server_id: &str, db: usize, key: &str, snapshot: StoredKeyTimelineSnapshot) -> Result<()> {
        Self::append_snapshot_at(server_id, db, key, snapshot, now_unix_secs())
    }

    pub fn append_snapshot_at(
        server_id: &str,
        db: usize,
        key: &str,
        mut snapshot: StoredKeyTimelineSnapshot,
        now_secs: i64,
    ) -> Result<()> {
        if server_id.is_empty() || key.is_empty() {
            return Ok(());
        }
        if !is_within_size_limit(&snapshot) {
            return Ok(());
        }
        if snapshot.recorded_at <= 0 {
            snapshot.recorded_at = now_secs;
        }
        if !is_within_retention(&snapshot, now_secs) {
            return Ok(());
        }

        let scope = key_timeline_scope(server_id, db, key);
        let Ok(database) = get_database() else {
            let mut list = FALLBACK_TIMELINES.entry(scope).or_default();
            list.insert(0, snapshot);
            prune_snapshots(&mut list, now_secs);
            return Ok(());
        };

        let mut snapshots = Self::list_snapshots_at(server_id, db, key, now_secs);
        snapshots.insert(0, snapshot);
        prune_snapshots(&mut snapshots, now_secs);

        let doc = KeyTimelineDoc { snapshots };
        let payload = serde_json::to_vec(&doc)?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(KEY_TIMELINE_TABLE)?;
            table.insert(scope.as_str(), payload.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Replaces the most recent persisted snapshot's event label and TTL (used when a
    /// push notification arrives immediately after a poll captured the identical value).
    pub fn update_latest_event(server_id: &str, db: usize, key: &str, event: &str, ttl_secs: i64) -> Result<()> {
        let now_secs = now_unix_secs();
        if server_id.is_empty() || key.is_empty() {
            return Ok(());
        }
        let scope = key_timeline_scope(server_id, db, key);
        let Ok(database) = get_database() else {
            if let Some(mut list) = FALLBACK_TIMELINES.get_mut(&scope)
                && let Some(first) = list.first_mut()
            {
                first.event = event.to_string();
                first.ttl_secs = ttl_secs;
            }
            return Ok(());
        };

        let mut snapshots = Self::list_snapshots_at(server_id, db, key, now_secs);
        let Some(first) = snapshots.first_mut() else {
            return Ok(());
        };
        first.event = event.to_string();
        first.ttl_secs = ttl_secs;

        let doc = KeyTimelineDoc { snapshots };
        let payload = serde_json::to_vec(&doc)?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(KEY_TIMELINE_TABLE)?;
            table.insert(scope.as_str(), payload.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Clears all persisted timeline snapshots for `(server_id, db, key)`.
    pub fn clear_snapshots(server_id: &str, db: usize, key: &str) -> Result<()> {
        if server_id.is_empty() || key.is_empty() {
            return Ok(());
        }
        let scope = key_timeline_scope(server_id, db, key);
        let Ok(database) = get_database() else {
            FALLBACK_TIMELINES.remove(&scope);
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(KEY_TIMELINE_TABLE)?;
            table.remove(scope.as_str())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Moves persisted timeline snapshots from `old_key` to `new_key` when a key is
    /// renamed. If `old_key` had no persisted snapshots, `new_key` is left untouched.
    pub fn rename(server_id: &str, db: usize, old_key: &str, new_key: &str) -> Result<()> {
        if server_id.is_empty() || old_key.is_empty() || new_key.is_empty() || old_key == new_key {
            return Ok(());
        }
        let old_scope = key_timeline_scope(server_id, db, old_key);
        let new_scope = key_timeline_scope(server_id, db, new_key);

        let Ok(database) = get_database() else {
            if let Some((_, snapshots)) = FALLBACK_TIMELINES.remove(&old_scope) {
                FALLBACK_TIMELINES.insert(new_scope, snapshots);
            }
            return Ok(());
        };

        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(KEY_TIMELINE_TABLE)?;
            let old_bytes = table.remove(old_scope.as_str())?.map(|v| v.value().to_vec());
            if let Some(bytes) = old_bytes {
                table.insert(new_scope.as_str(), bytes.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::fs::override_config_dir;

    fn setup() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();
    }

    fn make_snap(idx: usize, recorded_at: i64, text: &str) -> StoredKeyTimelineSnapshot {
        StoredKeyTimelineSnapshot {
            recorded_at,
            timestamp: format!("12:00:{idx:02}"),
            event: "set".to_string(),
            exists: true,
            key_type: "string".to_string(),
            ttl_secs: -1,
            snapshot_text: text.to_string(),
        }
    }

    #[test]
    fn round_trip_append_list_and_clear() {
        setup();
        let srv = "timeline-srv-1";
        let key = "user:100";
        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("clear");

        let base_now = 1_750_000_000;
        KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(1, base_now, "v1"), base_now).expect("append 1");
        KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(2, base_now + 5, "v2"), base_now + 5)
            .expect("append 2");

        let listed = KeyTimelineManager::list_snapshots_at(srv, 0, key, base_now + 10);
        assert_eq!(listed.len(), 2);
        // Newest first.
        assert_eq!(listed[0].snapshot_text, "v2");
        assert_eq!(listed[1].snapshot_text, "v1");

        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("clear");
        assert!(KeyTimelineManager::list_snapshots_at(srv, 0, key, base_now + 10).is_empty());
    }

    #[test]
    fn caps_at_50_snapshots_keeping_most_recent() {
        setup();
        let srv = "timeline-srv-cap";
        let key = "counter:key";
        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("clear");

        let base_now = 1_750_000_000;
        for i in 0..60 {
            let t = base_now + i as i64;
            KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(i, t, &format!("val-{i}")), t)
                .expect("append");
        }

        let listed = KeyTimelineManager::list_snapshots_at(srv, 0, key, base_now + 100);
        assert_eq!(listed.len(), KEY_TIMELINE_MAX_SNAPSHOTS);
        assert_eq!(listed[0].snapshot_text, "val-59");
        assert_eq!(listed[49].snapshot_text, "val-10");

        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("cleanup");
    }

    #[test]
    fn prunes_snapshots_older_than_seven_days() {
        setup();
        let srv = "timeline-srv-ttl";
        let key = "session:abc";
        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("clear");

        let day_zero = 1_750_000_000;
        let eight_days_later = day_zero + KEY_TIMELINE_RETENTION_SECS + 3600;

        KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(1, day_zero, "old-value"), day_zero)
            .expect("append old");
        KeyTimelineManager::append_snapshot_at(
            srv,
            0,
            key,
            make_snap(2, eight_days_later - 100, "fresh-value"),
            eight_days_later,
        )
        .expect("append fresh");

        let listed = KeyTimelineManager::list_snapshots_at(srv, 0, key, eight_days_later);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].snapshot_text, "fresh-value");

        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("cleanup");
    }

    #[test]
    fn skips_payloads_exceeding_max_inline_value_size() {
        setup();
        let srv = "timeline-srv-size";
        let key = "blob:huge";
        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("clear");

        let now = 1_750_000_000;
        KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(1, now, "small-ok"), now).expect("append small");

        let oversized = "x".repeat((MAX_INLINE_VALUE_SIZE as usize) + 1);
        KeyTimelineManager::append_snapshot_at(srv, 0, key, make_snap(2, now + 1, &oversized), now + 1)
            .expect("append oversized is no-op");

        let listed = KeyTimelineManager::list_snapshots_at(srv, 0, key, now + 2);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].snapshot_text, "small-ok");

        KeyTimelineManager::clear_snapshots(srv, 0, key).expect("cleanup");
    }

    #[test]
    fn rename_moves_snapshots_to_new_key_and_preserves_unreadable_rows_on_load() {
        setup();
        let srv = "timeline-srv-rename";
        let old_key = "order:old";
        let new_key = "order:new";
        KeyTimelineManager::clear_snapshots(srv, 0, old_key).expect("clear old");
        KeyTimelineManager::clear_snapshots(srv, 0, new_key).expect("clear new");

        let now = 1_750_000_000;
        KeyTimelineManager::append_snapshot_at(srv, 0, old_key, make_snap(1, now, "payload-1"), now).expect("append");

        crate::move_local_records(srv, 0, old_key, new_key);

        assert!(KeyTimelineManager::list_snapshots_at(srv, 0, old_key, now).is_empty());
        let moved = KeyTimelineManager::list_snapshots_at(srv, 0, new_key, now);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].snapshot_text, "payload-1");

        // Verify unreadable rows are skipped rather than deleted.
        let corrupt_scope = key_timeline_scope(srv, 0, "corrupt:key");
        {
            let db = get_database().expect("db");
            let txn = db.begin_write().expect("write txn");
            {
                let mut table = txn.open_table(KEY_TIMELINE_TABLE).expect("open table");
                table
                    .insert(corrupt_scope.as_str(), b"{not valid json".as_slice())
                    .expect("insert corrupt");
            }
            txn.commit().expect("commit");
        }
        assert!(KeyTimelineManager::list_snapshots_at(srv, 0, "corrupt:key", now).is_empty());
        {
            let db = get_database().expect("db");
            let txn = db.begin_read().expect("read txn");
            let table = txn.open_table(KEY_TIMELINE_TABLE).expect("open table");
            assert!(
                table.get(corrupt_scope.as_str()).expect("get").is_some(),
                "unreadable row must not be deleted on load"
            );
        }

        KeyTimelineManager::clear_snapshots(srv, 0, new_key).expect("cleanup");
        KeyTimelineManager::clear_snapshots(srv, 0, "corrupt:key").expect("cleanup corrupt");
    }
}

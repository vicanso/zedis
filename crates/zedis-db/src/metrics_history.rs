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

//! Persisted per-server metrics samples.
//!
//! Byte-level storage only: the payload is an opaque JSON blob so this
//! layer stays independent of the `RedisMetrics` shape — serialization,
//! the once-per-minute write throttle and the retention policy live in
//! `states/server/stat.rs`.

use super::{METRICS_HISTORY_TABLE, Result, get_database};
use redb::{ReadableDatabase, ReadableTable};

/// Store one sample keyed by `(server_id, timestamp_ms)`.
pub fn insert_metrics_sample(server_id: &str, timestamp_ms: i64, payload: &[u8]) -> Result<()> {
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(METRICS_HISTORY_TABLE)?;
        table.insert((server_id, timestamp_ms), payload)?;
    }
    write_txn.commit()?;
    Ok(())
}

/// All samples for `server_id` from `from_ms` (inclusive) onwards, in
/// ascending timestamp order.
pub fn list_metrics_samples(server_id: &str, from_ms: i64) -> Result<Vec<Vec<u8>>> {
    let db = get_database()?;
    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(METRICS_HISTORY_TABLE)?;
    let mut samples = vec![];
    for item in table.range((server_id, from_ms)..(server_id, i64::MAX))? {
        let (_, value) = item?;
        samples.push(value.value().to_vec());
    }
    Ok(samples)
}

/// Delete samples of `server_id` older than `before_ms`; returns how many
/// were removed.
pub fn prune_metrics_history(server_id: &str, before_ms: i64) -> Result<usize> {
    let db = get_database()?;
    let write_txn = db.begin_write()?;
    let removed = {
        let mut table = write_txn.open_table(METRICS_HISTORY_TABLE)?;
        let expired: Vec<i64> = table
            .range((server_id, i64::MIN)..(server_id, before_ms))?
            .filter_map(|item| item.ok())
            .map(|(key, _)| key.value().1)
            .collect();
        for timestamp_ms in &expired {
            table.remove((server_id, *timestamp_ms))?;
        }
        expired.len()
    };
    write_txn.commit()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::fs::override_config_dir;

    #[test]
    fn metrics_history_roundtrip_and_prune() {
        // Same per-process temp dir the app-state tests use; the inserts
        // below overwrite their keys so the assertions are deterministic
        // even if the database file already exists.
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        insert_metrics_sample("srv-a", 1000, b"a1").expect("insert a1");
        insert_metrics_sample("srv-a", 2000, b"a2").expect("insert a2");
        insert_metrics_sample("srv-b", 1500, b"b1").expect("insert b1");

        // Ascending order, server-scoped, from_ms inclusive.
        let samples = list_metrics_samples("srv-a", 0).expect("list srv-a");
        assert_eq!(samples, vec![b"a1".to_vec(), b"a2".to_vec()]);
        assert_eq!(
            list_metrics_samples("srv-a", 1500).expect("list from 1500"),
            vec![b"a2".to_vec()]
        );

        // Prune only touches the given server and only below the cutoff.
        assert_eq!(prune_metrics_history("srv-a", 2000).expect("prune"), 1);
        assert_eq!(
            list_metrics_samples("srv-a", 0).expect("list pruned"),
            vec![b"a2".to_vec()]
        );
        assert_eq!(
            list_metrics_samples("srv-b", 0).expect("list srv-b"),
            vec![b"b1".to_vec()]
        );
    }
}

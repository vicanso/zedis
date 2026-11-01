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

//! Generic persisted job store (`jobs` redb table).
//!
//! Stores resumable background job checkpoints keyed by `id` with a `kind`
//! discriminator (e.g. `"migration"`), human-readable `summary`, timestamps,
//! and a kind-specific `payload_json`. Every struct carries `#[serde(default)]`
//! and unreadable rows are skipped rather than deleted so forward/backward
//! schema changes never destroy user state.

use super::{JOBS_TABLE, Result, get_database};
use crate::error::Error;
#[cfg(target_family = "wasm")]
use crate::mem_store::{ReadableDatabase, ReadableTable};
use dashmap::DashMap;
#[cfg(not(target_family = "wasm"))]
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::sync::LazyLock;
use tracing::warn;

static FALLBACK_JOBS: LazyLock<DashMap<String, StoredJobRecord>> = LazyLock::new(DashMap::new);

/// A persisted job checkpoint stored in `JOBS_TABLE`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredJobRecord {
    pub id: String,
    pub kind: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub summary: String,
    pub payload_json: String,
}

pub struct JobManager;

impl JobManager {
    /// Lists all persisted jobs matching `kind`, sorted by `updated_at`
    /// descending (most recently updated first). Unreadable or incomplete
    /// rows are logged and skipped without deleting them from the table.
    pub fn list_by_kind(kind: &str) -> Vec<StoredJobRecord> {
        let Ok(db) = get_database() else {
            let mut records: Vec<StoredJobRecord> = FALLBACK_JOBS
                .iter()
                .filter(|entry| entry.value().kind == kind)
                .map(|entry| entry.value().clone())
                .collect();
            records.sort_by_key(|r| Reverse(r.updated_at));
            return records;
        };
        let Ok(read_txn) = db.begin_read() else {
            return Vec::new();
        };
        let Ok(table) = read_txn.open_table(JOBS_TABLE) else {
            return Vec::new();
        };
        let Ok(iter) = table.iter() else {
            return Vec::new();
        };

        let mut records = Vec::new();
        for item in iter {
            let Ok((key, value)) = item else {
                continue;
            };
            let table_id = key.value();
            let mut record: StoredJobRecord = match serde_json::from_slice(value.value()) {
                Ok(r) => r,
                Err(e) => {
                    warn!(id = table_id, error = %e, "unreadable job record, skipped");
                    continue;
                }
            };
            if record.id.trim().is_empty() {
                record.id = table_id.to_string();
            }
            if record.kind.trim().is_empty() {
                warn!(id = table_id, "job record missing kind, skipped");
                continue;
            }
            if record.kind == kind {
                records.push(record);
            }
        }
        records.sort_by_key(|r| Reverse(r.updated_at));
        records
    }

    /// Inserts or updates a job checkpoint in `JOBS_TABLE`.
    pub fn upsert(record: &StoredJobRecord) -> Result<()> {
        let id = record.id.trim();
        if id.is_empty() {
            return Err(Error::Invalid {
                message: "job id is empty".to_string(),
            });
        }
        if record.kind.trim().is_empty() {
            return Err(Error::Invalid {
                message: "job kind is empty".to_string(),
            });
        }
        let Ok(db) = get_database() else {
            let mut normalized = record.clone();
            normalized.id = id.to_string();
            FALLBACK_JOBS.insert(id.to_string(), normalized);
            return Ok(());
        };
        let payload = serde_json::to_vec(record)?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.insert(id, payload.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Removes a completed or discarded job checkpoint from `JOBS_TABLE`.
    pub fn delete(id: &str) -> Result<()> {
        let Ok(db) = get_database() else {
            FALLBACK_JOBS.remove(id);
            return Ok(());
        };
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(JOBS_TABLE)?;
            table.remove(id)?;
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

    fn write_raw(id: &str, bytes: &[u8]) {
        let db = get_database().expect("database");
        let txn = db.begin_write().expect("begin write");
        {
            let mut table = txn.open_table(JOBS_TABLE).expect("open jobs table");
            table.insert(id, bytes).expect("insert raw");
        }
        txn.commit().expect("commit");
    }

    fn raw_exists(id: &str) -> bool {
        let db = get_database().expect("database");
        let txn = db.begin_read().expect("begin read");
        let table = txn.open_table(JOBS_TABLE).expect("open jobs table");
        table.get(id).expect("get").is_some()
    }

    #[test]
    fn round_trip_upsert_list_and_delete_while_skipping_unreadable_rows() {
        setup();

        let job_a = StoredJobRecord {
            id: "mig-test-1".to_string(),
            kind: "migration".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_010,
            summary: "Copy 100 keys from dev to staging".to_string(),
            payload_json: r#"{"offset":64}"#.to_string(),
        };
        let job_b = StoredJobRecord {
            id: "mig-test-2".to_string(),
            kind: "migration".to_string(),
            created_at: 1_700_000_005,
            updated_at: 1_700_000_020,
            summary: "Export user:* to file".to_string(),
            payload_json: r#"{"pattern_index":0}"#.to_string(),
        };
        let other_kind = StoredJobRecord {
            id: "other-test-1".to_string(),
            kind: "other".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_030,
            summary: "Other task".to_string(),
            payload_json: "{}".to_string(),
        };

        JobManager::upsert(&job_a).expect("upsert job_a");
        JobManager::upsert(&job_b).expect("upsert job_b");
        JobManager::upsert(&other_kind).expect("upsert other_kind");
        write_raw("corrupt-job-row", b"{not valid json");

        let listed = JobManager::list_by_kind("migration");
        let ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["mig-test-2", "mig-test-1"]);
        assert!(
            raw_exists("corrupt-job-row"),
            "unreadable row must be skipped rather than deleted"
        );

        JobManager::delete("mig-test-1").expect("delete job_a");
        JobManager::delete("mig-test-2").expect("delete job_b");
        JobManager::delete("other-test-1").expect("delete other_kind");
        let _ = JobManager::delete("corrupt-job-row");

        assert!(JobManager::list_by_kind("migration").is_empty());
    }
}

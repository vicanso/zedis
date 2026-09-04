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

use crate::error::Error;
#[cfg(test)]
use redb::ReadableDatabase;
use redb::{Database, DatabaseError, ReadableTable, StorageError, TableDefinition, WriteTransaction};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;
use zedis_core::fs::get_or_create_config_dir;

pub mod error;

mod backup;

mod cmd_history_manager;
mod favorites_manager;
mod history_manager;
mod key_metadata_manager;
mod lua_scripts;
mod metrics_history;
mod protos;
mod recent_keys_manager;
mod scripts;
mod search_history_manager;
mod trash;

pub use backup::*;
pub use cmd_history_manager::*;
pub use favorites_manager::*;
pub use key_metadata_manager::*;
pub use lua_scripts::*;
pub use metrics_history::*;
pub use protos::*;
pub use recent_keys_manager::*;
pub use scripts::*;
pub use search_history_manager::*;
pub use trash::*;

const SEARCH_HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("search_history");
const PROTO_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("proto");
const SCRIPT_VIEWER_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("script_viewer");
const CMD_HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("cmd_history");
const FAVORITY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("favority");
/// Per-(server, db) MRU of recently opened keys (JSON array of key names).
const RECENT_KEYS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("recent_keys");
// Saved Lua scripts: globally shared across servers, persisted to disk.
const LUA_SCRIPT_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("lua_script");
// Per-server client-side key tags + free-form notes. Value is a JSON
// document keyed by Redis key name:
//   {"v": 1, "entries": {"<key>": {"tag": "<color>"|null, "note": "..."}}}
// Versioned at the document level so future additions (multi-tag,
// per-tag note, ...) can migrate the in-memory cache without breaking
// disk compatibility.
const KEY_METADATA_TABLE: TableDefinition<&str, &str> = TableDefinition::new("key_metadata");
// Per-server metrics history samples: key (server_id, timestamp_ms), value =
// JSON of one `RedisMetrics`. Written at most once per minute per server and
// pruned to the retention window, so the table stays small (~10k rows per
// server) — see `states/server/stat.rs` for the write policy.
const METRICS_HISTORY_TABLE: TableDefinition<(&str, i64), &[u8]> = TableDefinition::new("metrics_history");
// Local recycle bin for soft-deleted keys: key (server_id, id) where id is
// "{deleted_at_ms:020}:{db}:{key}"; value = framed meta JSON + DUMP payload
// (see `db/trash.rs`). Entries are purged after 24h.
const TRASH_TABLE: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("key_trash");
/// Database-level bookkeeping; today just the schema version.
const META_TABLE: TableDefinition<&str, u32> = TableDefinition::new("meta");
const SCHEMA_VERSION_KEY: &str = "schema_version";
/// Version of the on-disk layout this build writes. Bump it together with a
/// matching step in [`migrate_step`] whenever a table's key/value encoding
/// changes in a way old rows can't be read as-is. Adding a brand-new table
/// needs no bump — `ensure_schema` creates missing tables on every open.
/// Files without a `meta` table predate versioning and are treated as v1.
pub const SCHEMA_VERSION: u32 = 1;

type Result<T, E = Error> = std::result::Result<T, E>;

static DATABASE: OnceLock<Database> = OnceLock::new();

fn get_database() -> Result<&'static Database> {
    DATABASE.get().ok_or(Error::Invalid {
        message: "database not initialized".to_string(),
    })
}

/// `<config_dir>/zedis.redb`. Same file name in both environments — a
/// development run is isolated by its own config *directory*
/// (`<config_dir>/dev`), not by a `-dev` file suffix.
pub fn database_path() -> Result<PathBuf> {
    Ok(get_or_create_config_dir()?.join("zedis.redb"))
}

pub fn init_database() -> Result<()> {
    let db_path = database_path()?;
    debug!(path = db_path.display().to_string(), "create database");
    let db = Database::create(&db_path)?;
    ensure_schema(&db)?;
    debug!(path = db_path.display().to_string(), "database initialized success");
    DATABASE.set(db).map_err(|_| Error::Invalid {
        message: "database initialized failed".to_string(),
    })?;
    Ok(())
}

/// Brings an opened file up to [`SCHEMA_VERSION`] in one write transaction:
/// refuses a newer file, runs the pending [`migrate_step`]s on an older one,
/// creates any missing table, and stamps the version. Nothing is committed if
/// any step fails, so a half-migrated file can't exist.
fn ensure_schema(db: &Database) -> Result<()> {
    let write_txn = db.begin_write()?;
    let stored = {
        let table = write_txn.open_table(META_TABLE)?;
        table.get(SCHEMA_VERSION_KEY)?.map(|v| v.value())
    };
    match stored {
        Some(found) if found > SCHEMA_VERSION => {
            return Err(Error::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Some(found) => {
            for from in found..SCHEMA_VERSION {
                debug!(from, to = from + 1, "migrating local database schema");
                migrate_step(&write_txn, from)?;
            }
        }
        // A fresh file, or one written before versioning (the v1 layout).
        None => {}
    }
    {
        write_txn.open_table(SEARCH_HISTORY_TABLE)?;
        write_txn.open_table(PROTO_TABLE)?;
        write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
        write_txn.open_table(CMD_HISTORY_TABLE)?;
        write_txn.open_table(FAVORITY_TABLE)?;
        write_txn.open_table(RECENT_KEYS_TABLE)?;
        write_txn.open_table(LUA_SCRIPT_TABLE)?;
        write_txn.open_table(KEY_METADATA_TABLE)?;
        write_txn.open_table(METRICS_HISTORY_TABLE)?;
        write_txn.open_table(TRASH_TABLE)?;
        write_txn
            .open_table(META_TABLE)?
            .insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION)?;
    }
    write_txn.commit()?;
    Ok(())
}

/// Migrates the layout from schema `from` to `from + 1` inside the caller's
/// transaction. Every bump of [`SCHEMA_VERSION`] adds its step here, in
/// order; reaching this without one is a programming error and fails the
/// open rather than silently stamping data the new code can't read.
fn migrate_step(_txn: &WriteTransaction, from: u32) -> Result<()> {
    Err(Error::Invalid {
        message: format!("no migration from local database schema v{from}"),
    })
}

/// Why [`init_database`] failed, reduced to what the UI can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbOpenFailure {
    /// Another process (usually a second Zedis) holds the exclusive lock.
    Locked,
    /// Written by a newer Zedis — update, or rebuild from scratch.
    SchemaTooNew { found: u32, supported: u32 },
    /// The file is unreadable as a redb database and auto-repair gave up.
    /// Rebuilding (after moving the file aside) is the only way forward.
    Damaged(String),
    /// Plain I/O failure (permissions, read-only volume, missing folder);
    /// rebuilding would hit the same wall.
    Inaccessible(String),
}

/// Classifies an [`init_database`] error for the recovery window.
pub fn open_failure_kind(error: &Error) -> DbOpenFailure {
    match error {
        Error::SchemaTooNew { found, supported } => DbOpenFailure::SchemaTooNew {
            found: *found,
            supported: *supported,
        },
        Error::RedbDatabase {
            source: DatabaseError::DatabaseAlreadyOpen,
        } => DbOpenFailure::Locked,
        // redb reports a file that isn't a redb database (bad magic, short
        // header, truncated page) as an `InvalidData` I/O error, not as
        // `Corrupted` — that's damage, and a rebuild is the cure.
        Error::RedbDatabase {
            source: DatabaseError::Storage(StorageError::Io(io)),
        }
        | Error::Io { source: io }
            if !matches!(io.kind(), ErrorKind::InvalidData | ErrorKind::UnexpectedEof) =>
        {
            DbOpenFailure::Inaccessible(io.to_string())
        }
        other => DbOpenFailure::Damaged(other.to_string()),
    }
}

/// Moves the database file aside as `zedis.redb.corrupt-<unix-secs>` so a
/// following [`init_database`] starts from a fresh file. Nothing is deleted:
/// the old file stays next to the new one for manual salvage. Only valid
/// while no database is open in this process.
pub fn quarantine_database() -> Result<PathBuf> {
    if DATABASE.get().is_some() {
        return Err(Error::Invalid {
            message: "database is open; cannot quarantine it".to_string(),
        });
    }
    let path = database_path()?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = path.with_file_name(format!("{name}.corrupt-{secs}"));
    std::fs::rename(&path, &target)?;
    Ok(target)
}

/// Test-only: initialize the database exactly once per process. `Once`
/// blocks concurrent callers until the first initialization finishes —
/// racing two `init_database` calls trips over redb's exclusive file lock.
/// Callers must redirect the config dir (`override_config_dir`) first.
#[cfg(test)]
pub fn init_database_for_tests() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Err(e) = init_database() {
            panic!("init test database: {e}");
        }
    });
}

fn add_normalize_history(history: &mut Vec<String>, keyword: String, max: usize) {
    history.retain(|x| *x != keyword);

    history.insert(0, keyword);

    if history.len() > max {
        history.truncate(max);
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use redb::TableHandle;

    /// A scratch database file per test, removed on drop.
    struct ScratchDb(PathBuf);

    impl ScratchDb {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("zedis-db-schema-{}-{name}.redb", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for ScratchDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn stored_version(db: &Database) -> Option<u32> {
        let txn = db.begin_read().expect("begin read");
        // No `meta` table at all is the pre-versioning layout.
        let Ok(table) = txn.open_table(META_TABLE) else {
            return None;
        };
        table.get(SCHEMA_VERSION_KEY).expect("get").map(|v| v.value())
    }

    #[test]
    fn fresh_and_pre_versioning_files_are_stamped_with_the_current_schema() {
        let scratch = ScratchDb::new("fresh");
        // Pre-versioning layout: tables exist, no `meta` table.
        {
            let db = Database::create(&scratch.0).expect("create");
            let txn = db.begin_write().expect("begin write");
            txn.open_table(FAVORITY_TABLE).expect("open");
            txn.commit().expect("commit");
            assert_eq!(stored_version(&db), None);
            ensure_schema(&db).expect("ensure schema");
            assert_eq!(stored_version(&db), Some(SCHEMA_VERSION));
        }
        // Reopening an up-to-date file is a no-op.
        let db = Database::open(&scratch.0).expect("reopen");
        ensure_schema(&db).expect("ensure schema again");
        assert_eq!(stored_version(&db), Some(SCHEMA_VERSION));
    }

    #[test]
    fn every_table_this_crate_defines_exists_after_an_open() {
        // The read paths open tables in a *read* transaction, which cannot
        // create one — so a table missing here fails every read until the first
        // write happens to create it. `cmd_history` was missing exactly this
        // way. A new `TableDefinition` belongs in `ensure_schema` and here.
        let scratch = ScratchDb::new("tables");
        let db = Database::create(&scratch.0).expect("create");
        ensure_schema(&db).expect("ensure schema");

        let txn = db.begin_read().expect("begin read");
        let mut found: Vec<String> = txn
            .list_tables()
            .expect("list tables")
            .map(|t| t.name().to_string())
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                "cmd_history",
                "favority",
                "key_metadata",
                "key_trash",
                "lua_script",
                "meta",
                "metrics_history",
                "proto",
                "recent_keys",
                "script_viewer",
                "search_history",
            ]
        );
    }

    #[test]
    fn an_older_file_with_no_migration_step_fails_the_open_rather_than_being_stamped() {
        // The whole point of the version stamp: reaching a layout this build has
        // no step for must abort, not quietly claim the file is current and let
        // the managers write a mix of both shapes into it.
        let scratch = ScratchDb::new("older");
        let db = Database::create(&scratch.0).expect("create");
        {
            let txn = db.begin_write().expect("begin write");
            txn.open_table(META_TABLE)
                .expect("open meta")
                .insert(SCHEMA_VERSION_KEY, 0u32)
                .expect("insert");
            txn.commit().expect("commit");
        }

        let err = ensure_schema(&db).expect_err("must refuse");
        assert!(err.to_string().contains("no migration"), "{err}");
        // Aborted transaction: the stamp is untouched and no table was created.
        assert_eq!(stored_version(&db), Some(0));
        let txn = db.begin_read().expect("begin read");
        assert!(matches!(
            txn.open_table(FAVORITY_TABLE),
            Err(redb::TableError::TableDoesNotExist(_))
        ));
    }

    #[test]
    fn a_newer_schema_is_refused_without_touching_the_file() {
        let scratch = ScratchDb::new("newer");
        let db = Database::create(&scratch.0).expect("create");
        {
            let txn = db.begin_write().expect("begin write");
            txn.open_table(META_TABLE)
                .expect("open meta")
                .insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION + 5)
                .expect("insert");
            txn.commit().expect("commit");
        }
        let err = ensure_schema(&db).expect_err("must refuse");
        assert!(
            matches!(err, Error::SchemaTooNew { found, supported } if found == SCHEMA_VERSION + 5 && supported == SCHEMA_VERSION)
        );
        assert_eq!(
            open_failure_kind(&err),
            DbOpenFailure::SchemaTooNew {
                found: SCHEMA_VERSION + 5,
                supported: SCHEMA_VERSION
            }
        );
        // The refused transaction was aborted: version untouched, no new tables.
        assert_eq!(stored_version(&db), Some(SCHEMA_VERSION + 5));
        let txn = db.begin_read().expect("begin read");
        assert!(matches!(
            txn.open_table(FAVORITY_TABLE),
            Err(redb::TableError::TableDoesNotExist(_))
        ));
    }

    #[test]
    fn open_failures_are_classified_for_the_recovery_window() {
        let locked = Error::RedbDatabase {
            source: DatabaseError::DatabaseAlreadyOpen,
        };
        assert_eq!(open_failure_kind(&locked), DbOpenFailure::Locked);
        let corrupt = Error::RedbDatabase {
            source: DatabaseError::Storage(StorageError::Corrupted("bad header".into())),
        };
        assert!(matches!(open_failure_kind(&corrupt), DbOpenFailure::Damaged(m) if m.contains("bad header")));
        let denied = Error::Io {
            source: std::io::Error::from(ErrorKind::PermissionDenied),
        };
        assert!(matches!(open_failure_kind(&denied), DbOpenFailure::Inaccessible(_)));
        // What redb actually returns for garbage in place of the file.
        let not_redb = Error::RedbDatabase {
            source: DatabaseError::Storage(StorageError::Io(std::io::Error::new(
                ErrorKind::InvalidData,
                "Not a redb database: magic number mismatch",
            ))),
        };
        assert!(matches!(open_failure_kind(&not_redb), DbOpenFailure::Damaged(m) if m.contains("magic number")));
    }

    #[test]
    fn a_garbage_file_counts_as_damaged() {
        let scratch = ScratchDb::new("garbage");
        std::fs::write(&scratch.0, b"definitely not a redb file, just some bytes").expect("write garbage");
        let err: Error = Database::create(&scratch.0).expect_err("garbage must not open").into();
        assert!(matches!(open_failure_kind(&err), DbOpenFailure::Damaged(_)), "{err}");
    }

    #[test]
    fn a_second_opener_sees_the_lock() {
        let scratch = ScratchDb::new("locked");
        let _first = Database::create(&scratch.0).expect("create");
        let err: Error = Database::create(&scratch.0).expect_err("second open must fail").into();
        assert_eq!(open_failure_kind(&err), DbOpenFailure::Locked);
    }
}

#[cfg(test)]
mod history_normalize_tests {
    use super::add_normalize_history;

    #[test]
    fn moves_a_repeat_to_the_front_and_enforces_the_cap() {
        let mut history = vec!["b".to_string(), "a".to_string()];
        add_normalize_history(&mut history, "a".to_string(), 5);
        assert_eq!(history, vec!["a", "b"], "a repeat moves up instead of duplicating");

        let mut history: Vec<String> = Vec::new();
        for keyword in ["1", "2", "3", "4"] {
            add_normalize_history(&mut history, keyword.to_string(), 3);
        }
        assert_eq!(history, vec!["4", "3", "2"], "the oldest entry falls off the end");
    }
}

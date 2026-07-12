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
use redb::{Database, TableDefinition};
use std::sync::OnceLock;
use tracing::debug;
use zedis_core::env::is_development;
use zedis_core::fs::get_or_create_config_dir;

pub mod error;

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

type Result<T, E = Error> = std::result::Result<T, E>;

static DATABASE: OnceLock<Database> = OnceLock::new();

fn get_database() -> Result<&'static Database> {
    DATABASE.get().ok_or(Error::Invalid {
        message: "database not initialized".to_string(),
    })
}

pub fn init_database() -> Result<()> {
    let dir = get_or_create_config_dir()?;
    let db_path = if is_development() {
        dir.join("zedis-dev.redb")
    } else {
        dir.join("zedis.redb")
    };
    debug!(path = db_path.display().to_string(), "create database");
    let db = Database::create(&db_path)?;
    let write_txn = db.begin_write()?;
    {
        write_txn.open_table(SEARCH_HISTORY_TABLE)?;
        write_txn.open_table(PROTO_TABLE)?;
        write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
        write_txn.open_table(FAVORITY_TABLE)?;
        write_txn.open_table(RECENT_KEYS_TABLE)?;
        write_txn.open_table(LUA_SCRIPT_TABLE)?;
        write_txn.open_table(KEY_METADATA_TABLE)?;
        write_txn.open_table(METRICS_HISTORY_TABLE)?;
        write_txn.open_table(TRASH_TABLE)?;
    }
    write_txn.commit()?;
    debug!(path = db_path.display().to_string(), "database initialized success");
    DATABASE.set(db).map_err(|_| Error::Invalid {
        message: "database initialized failed".to_string(),
    })?;
    Ok(())
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

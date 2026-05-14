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

//! Locally persisted Lua script library.
//!
//! Each script entry stores its source code, a precomputed SHA1
//! (matching Redis's own `SCRIPT LOAD` hash so we can `EVALSHA`
//! against it), and lifetime usage counters. Scripts are global —
//! the same library is visible from every server connection; the
//! decision to do this rather than per-server isolation came from
//! the observation that Lua scripts are portable, you typically
//! reuse the same `incr-then-expire` snippet on prod / dev / staging.

use super::{LUA_SCRIPT_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tracing::{info, warn};

type Result<T, E = Error> = std::result::Result<T, E>;

static LUA_SCRIPT_CACHE: LazyLock<DashMap<String, LuaScript>> = LazyLock::new(DashMap::new);

/// One saved Lua script. `id` is the redb key, kept out of the value
/// body so it doesn't get serialized twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuaScript {
    pub name: String,
    pub code: String,
    /// Hex SHA1 of `code`, computed locally via `redis::Script::new` so
    /// it matches what Redis would return from `SCRIPT LOAD`. Cached
    /// here to avoid a hash roundtrip on every run.
    pub sha: String,
    /// User-saved default arguments. The run panel pre-fills these so
    /// repeated invocations don't have to retype.
    #[serde(default)]
    pub default_keys: Vec<String>,
    #[serde(default)]
    pub default_args: Vec<String>,
    /// Total `EVALSHA` / `EVAL` invocations attempted via this entry.
    #[serde(default)]
    pub calls: u64,
    /// Successful `EVALSHA` invocations (no NOSCRIPT fallback).
    /// `evalsha_hits / calls` is the hit-rate displayed in the UI —
    /// a low ratio usually means the script gets evicted by a busy
    /// `SCRIPT FLUSH` or restart cycle.
    #[serde(default)]
    pub evalsha_hits: u64,
    /// Unix timestamp seconds.
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

pub struct LuaScriptManager;

impl LuaScriptManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(LUA_SCRIPT_TABLE)?;

        let mut invalid: Vec<String> = Vec::new();
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            let script: LuaScript = match serde_json::from_slice(value.value()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(id, error = %e, "invalid lua script entry, will be removed");
                    invalid.push(id.to_string());
                    continue;
                }
            };
            LUA_SCRIPT_CACHE.insert(id.to_string(), script);
        }
        drop(read_txn);

        if !invalid.is_empty() {
            let write_txn = db.begin_write()?;
            {
                let mut table = write_txn.open_table(LUA_SCRIPT_TABLE)?;
                for id in &invalid {
                    table.remove(id.as_str())?;
                }
            }
            write_txn.commit()?;
            info!(count = invalid.len(), "removed invalid lua script entries");
        }

        info!(count = LUA_SCRIPT_CACHE.len(), "load lua scripts success");
        Ok(())
    }

    pub fn list_with_id() -> Vec<(String, LuaScript)> {
        let mut items: Vec<(String, LuaScript)> = LUA_SCRIPT_CACHE
            .iter()
            .map(|item| (item.key().clone(), item.value().clone()))
            .collect();
        // Newest-saved-first so freshly added scripts surface at the
        // top of the list without the user scrolling. Negate the key
        // because `sort_by_key` is ascending and we want descending.
        items.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));
        items
    }

    pub fn get(id: &str) -> Result<LuaScript> {
        if let Some(s) = LUA_SCRIPT_CACHE.get(id) {
            return Ok(s.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(LUA_SCRIPT_TABLE)?;
        let Some(v) = table.get(id)? else {
            return Err(Error::Invalid {
                message: "lua script not found".to_string(),
            });
        };
        Ok(serde_json::from_slice(v.value())?)
    }

    pub fn upsert(id: &str, script: LuaScript) -> Result<()> {
        if script.name.trim().is_empty() {
            return Err(Error::Invalid {
                message: "script name is empty".to_string(),
            });
        }
        if script.code.trim().is_empty() {
            return Err(Error::Invalid {
                message: "script code is empty".to_string(),
            });
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(LUA_SCRIPT_TABLE)?;
            let json = serde_json::to_string(&script)?;
            table.insert(id, json.as_bytes())?;
        }
        write_txn.commit()?;
        LUA_SCRIPT_CACHE.insert(id.to_string(), script);
        Ok(())
    }

    pub fn delete(id: &str) -> Result<()> {
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(LUA_SCRIPT_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        LUA_SCRIPT_CACHE.remove(id);
        Ok(())
    }

    /// Increment usage counters atomically. `was_hit=true` when the
    /// run completed via `EVALSHA` without falling back to a fresh
    /// `SCRIPT LOAD + EVAL`. The two counters live on the script
    /// record so the hit-rate stays paired with the source they
    /// describe even across renames or code edits.
    pub fn record_call(id: &str, was_hit: bool) -> Result<()> {
        let mut script = Self::get(id)?;
        script.calls = script.calls.saturating_add(1);
        if was_hit {
            script.evalsha_hits = script.evalsha_hits.saturating_add(1);
        }
        // Don't bump `updated_at` here — that's reserved for code
        // edits. Otherwise running a script would shuffle it to the
        // top of the list, which is surprising.
        Self::upsert(id, script)
    }
}

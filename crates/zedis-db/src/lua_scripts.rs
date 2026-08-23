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
/// `#[serde(default)]` is the upgrade contract (see [`crate::ProtoConfig`]): a
/// script saved by an earlier build must still load after a field is added.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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

        let mut skipped = 0usize;
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            // Skipped, not deleted — the library is the user's own work and
            // exists nowhere else. See `ProtoConfig` for the contract that
            // keeps a version bump from landing here in the first place.
            let script: LuaScript = match serde_json::from_slice(value.value()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(id, error = %e, "unreadable lua script entry, skipped");
                    skipped += 1;
                    continue;
                }
            };
            if script.name.trim().is_empty() || script.code.trim().is_empty() {
                warn!(id, "incomplete lua script entry, skipped");
                skipped += 1;
                continue;
            }
            LUA_SCRIPT_CACHE.insert(id.to_string(), script);
        }
        drop(read_txn);

        info!(count = LUA_SCRIPT_CACHE.len(), skipped, "load lua scripts success");
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

    /// Whether another entry (excluding `except_id`) already uses this
    /// display name. Used for soft duplicate-name warnings in the form.
    pub fn name_taken(name: &str, except_id: Option<&str>) -> bool {
        let needle = name.trim();
        if needle.is_empty() {
            return false;
        }
        LUA_SCRIPT_CACHE.iter().any(|item| {
            if except_id.is_some_and(|id| item.key() == id) {
                return false;
            }
            item.value().name.trim() == needle
        })
    }

    /// Portable export payload (no id / counters / timestamps).
    pub fn export_all() -> Vec<LuaScriptExport> {
        let mut items: Vec<LuaScriptExport> = LUA_SCRIPT_CACHE
            .iter()
            .map(|item| {
                let s = item.value();
                LuaScriptExport {
                    name: s.name.clone(),
                    code: s.code.clone(),
                    default_keys: s.default_keys.clone(),
                    default_args: s.default_args.clone(),
                }
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }
}

/// Wire shape for clipboard / file import-export. Intentionally omits
/// runtime stats so a dump is re-importable on another machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LuaScriptExport {
    pub name: String,
    pub code: String,
    #[serde(default)]
    pub default_keys: Vec<String>,
    #[serde(default)]
    pub default_args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_database_for_tests, lua_scripts::LUA_SCRIPT_TABLE};
    use zedis_core::fs::override_config_dir;

    fn setup() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();
    }

    fn script(name: &str, code: &str) -> LuaScript {
        LuaScript {
            name: name.to_string(),
            code: code.to_string(),
            sha: "deadbeef".to_string(),
            ..Default::default()
        }
    }

    /// Writes a raw row, bypassing `upsert` — the shape an older build, or a
    /// half-written file, leaves behind.
    fn write_raw(id: &str, json: &[u8]) {
        let db = get_database().expect("database");
        let txn = db.begin_write().expect("begin write");
        {
            let mut table = txn.open_table(LUA_SCRIPT_TABLE).expect("open");
            table.insert(id, json).expect("insert");
        }
        txn.commit().expect("commit");
    }

    fn raw_exists(id: &str) -> bool {
        let db = get_database().expect("database");
        let txn = db.begin_read().expect("begin read");
        let table = txn.open_table(LUA_SCRIPT_TABLE).expect("open");
        table.get(id).expect("get").is_some()
    }

    #[test]
    fn a_script_saved_by_an_older_build_still_loads() {
        // Everything except the source itself was added after the first
        // release; the contract is that the shape below keeps working.
        let legacy = br#"{"name":"incr","code":"return 1","sha":"abc"}"#;
        let parsed: LuaScript = serde_json::from_slice(legacy).expect("legacy row parses");
        assert_eq!(parsed.name, "incr");
        assert_eq!(parsed.calls, 0);
        assert!(parsed.default_keys.is_empty());

        // And the same holds for a field this build itself does not know yet.
        let future = br#"{"name":"incr","code":"return 1","sha":"abc","invented_later":true}"#;
        assert_eq!(
            serde_json::from_slice::<LuaScript>(future)
                .expect("unknown field ignored")
                .name,
            "incr"
        );
    }

    #[test]
    fn upsert_get_and_delete_round_trip() {
        setup();
        LuaScriptManager::upsert("lua-rt", script("round", "return redis.call('PING')")).expect("upsert");

        let got = LuaScriptManager::get("lua-rt").expect("get");
        assert_eq!(got.name, "round");
        assert_eq!(got.code, "return redis.call('PING')");

        LuaScriptManager::delete("lua-rt").expect("delete");
        assert!(LuaScriptManager::get("lua-rt").is_err());
        assert!(!raw_exists("lua-rt"));
    }

    #[test]
    fn refuses_an_entry_with_nothing_in_it() {
        setup();
        assert!(LuaScriptManager::upsert("lua-bad", script("  ", "return 1")).is_err());
        assert!(LuaScriptManager::upsert("lua-bad", script("named", "   ")).is_err());
        assert!(!raw_exists("lua-bad"), "a rejected script must not reach the file");
    }

    #[test]
    fn record_call_counts_runs_without_reordering_the_library() {
        setup();
        let mut s = script("counted", "return 1");
        s.updated_at = 42;
        LuaScriptManager::upsert("lua-calls", s).expect("upsert");

        LuaScriptManager::record_call("lua-calls", true).expect("hit");
        LuaScriptManager::record_call("lua-calls", false).expect("miss");

        let got = LuaScriptManager::get("lua-calls").expect("get");
        assert_eq!((got.calls, got.evalsha_hits), (2, 1));
        // Running a script must not shuffle it to the top of the list.
        assert_eq!(got.updated_at, 42);
    }

    #[test]
    fn name_taken_ignores_the_entry_being_edited() {
        setup();
        LuaScriptManager::upsert("lua-nt-1", script("shared-name", "return 1")).expect("upsert");
        assert!(LuaScriptManager::name_taken("shared-name", None));
        assert!(LuaScriptManager::name_taken(" shared-name ", None), "compared trimmed");
        assert!(!LuaScriptManager::name_taken("shared-name", Some("lua-nt-1")));
        assert!(
            !LuaScriptManager::name_taken("   ", None),
            "a blank name is never taken"
        );
    }

    #[test]
    fn export_drops_the_runtime_stats() {
        setup();
        let mut s = script("exported-zz", "return 1");
        s.calls = 9;
        s.evalsha_hits = 4;
        s.default_keys = vec!["k".to_string()];
        LuaScriptManager::upsert("lua-export", s).expect("upsert");

        let exported = LuaScriptManager::export_all();
        let mine = exported.iter().find(|e| e.name == "exported-zz").expect("present");
        assert_eq!(mine.code, "return 1");
        assert_eq!(mine.default_keys, vec!["k".to_string()]);
        // Sorted by name so a dump is stable across machines.
        let mut sorted = exported.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = exported.iter().map(|e| e.name.as_str()).collect();
        let sorted_names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, sorted_names);
    }

    #[test]
    fn init_skips_a_row_it_cannot_read_and_leaves_it_on_disk() {
        setup();
        write_raw("lua-init-broken", b"{ this is not json");
        LuaScriptManager::upsert("lua-init-good", script("survivor", "return 1")).expect("upsert");
        LUA_SCRIPT_CACHE.remove("lua-init-good");

        LuaScriptManager::init().expect("init");

        assert!(
            LUA_SCRIPT_CACHE.contains_key("lua-init-good"),
            "a valid row still loads"
        );
        assert!(
            !LUA_SCRIPT_CACHE.contains_key("lua-init-broken"),
            "an unreadable row is skipped"
        );
        // The row survives: it is the user's own work, and a later build may
        // read it again. Deleting it here is how a schema change turns into
        // silent data loss.
        assert!(raw_exists("lua-init-broken"));
    }

    #[test]
    fn init_skips_a_row_with_no_source_in_it() {
        setup();
        // Reachable now that `#[serde(default)]` makes a truncated row parse.
        write_raw("lua-init-empty", br#"{"name":"ghost"}"#);
        LuaScriptManager::init().expect("init");
        assert!(!LUA_SCRIPT_CACHE.contains_key("lua-init-empty"));
        assert!(raw_exists("lua-init-empty"));
    }
}

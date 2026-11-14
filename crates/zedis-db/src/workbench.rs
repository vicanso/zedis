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

//! Locally persisted parameterized Query Workbench snippets.
//!
//! Snippets hold multi-statement Redis command scripts with optional
//! `{{param}}` placeholders and saved default values. Like [`crate::LuaScript`],
//! snippets are global across connections so reusable operational playbooks
//! work on dev, staging, and production servers alike.

use super::{WORKBENCH_TABLE, get_database};
use crate::error::Error;
#[cfg(target_family = "wasm")]
use crate::mem_store::{ReadableDatabase, ReadableTable};
use dashmap::DashMap;
#[cfg(not(target_family = "wasm"))]
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use tracing::{info, warn};

type Result<T, E = Error> = std::result::Result<T, E>;

static WORKBENCH_CACHE: LazyLock<DashMap<String, WorkbenchSnippet>> = LazyLock::new(DashMap::new);

/// One saved workbench query snippet. `id` is the table key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkbenchSnippet {
    pub name: String,
    pub script: String,
    #[serde(default)]
    pub description: String,
    /// Saved default values for `{{param}}` placeholders in `script`.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Unix timestamp seconds.
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// Extract distinct `{{param}}` placeholder names from `script`, preserving
/// first-appearance order. Placeholders containing whitespace/newlines or
/// nested braces are ignored so JSON payloads like `{"a":{"b":1}}` never
/// trigger false-positive parameter inputs.
pub fn extract_workbench_params(script: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut rest = script;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            break;
        };
        let candidate = after_open[..close].trim();
        let valid = !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if valid {
            let name = candidate.to_string();
            if !seen.contains(&name) {
                seen.push(name);
            }
            rest = &after_open[close + 2..];
        } else {
            rest = &rest[open + 2..];
        }
    }
    seen
}

/// Substitute every valid `{{param}}` placeholder in `script` with its value
/// from `values` (leaving unbound placeholders untouched if absent).
pub fn render_workbench_script(script: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let candidate = after_open[..close].trim();
        let valid = !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if valid && let Some(val) = values.get(candidate) {
            out.push_str(val);
            rest = &after_open[close + 2..];
        } else if valid {
            out.push_str(&rest[open..open + 2 + close + 2]);
            rest = &after_open[close + 2..];
        } else {
            out.push_str("{{");
            rest = after_open;
        }
    }
    out.push_str(rest);
    out
}

pub struct WorkbenchManager;

impl WorkbenchManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(WORKBENCH_TABLE)?;

        let mut skipped = 0usize;
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            let snippet: WorkbenchSnippet = match serde_json::from_slice(value.value()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(id, error = %e, "unreadable workbench snippet, skipped");
                    skipped += 1;
                    continue;
                }
            };
            if snippet.name.trim().is_empty() || snippet.script.trim().is_empty() {
                warn!(id, "incomplete workbench snippet, skipped");
                skipped += 1;
                continue;
            }
            WORKBENCH_CACHE.insert(id.to_string(), snippet);
        }
        drop(read_txn);

        info!(
            count = WORKBENCH_CACHE.len(),
            skipped, "load workbench snippets success"
        );
        Ok(())
    }

    pub fn reload() -> Result<()> {
        WORKBENCH_CACHE.clear();
        Self::init()
    }

    pub fn list_with_id() -> Vec<(String, WorkbenchSnippet)> {
        let mut items: Vec<(String, WorkbenchSnippet)> = WORKBENCH_CACHE
            .iter()
            .map(|item| (item.key().clone(), item.value().clone()))
            .collect();
        items.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));
        items
    }

    pub fn get(id: &str) -> Result<WorkbenchSnippet> {
        if let Some(s) = WORKBENCH_CACHE.get(id) {
            return Ok(s.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(WORKBENCH_TABLE)?;
        let Some(v) = table.get(id)? else {
            return Err(Error::Invalid {
                message: "workbench snippet not found".to_string(),
            });
        };
        Ok(serde_json::from_slice(v.value())?)
    }

    pub fn upsert(id: &str, snippet: WorkbenchSnippet) -> Result<()> {
        if snippet.name.trim().is_empty() {
            return Err(Error::Invalid {
                message: "snippet name is empty".to_string(),
            });
        }
        if snippet.script.trim().is_empty() {
            return Err(Error::Invalid {
                message: "snippet script is empty".to_string(),
            });
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(WORKBENCH_TABLE)?;
            let json = serde_json::to_string(&snippet)?;
            table.insert(id, json.as_bytes())?;
        }
        write_txn.commit()?;
        WORKBENCH_CACHE.insert(id.to_string(), snippet);
        Ok(())
    }

    pub fn delete(id: &str) -> Result<()> {
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(WORKBENCH_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        WORKBENCH_CACHE.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::fs::override_config_dir;

    #[test]
    fn params_extract_in_order_and_ignore_nested_json_braces() {
        let script = "GET user:{{user_id}}\nJSON.SET doc:{{user_id}} $ '{\"nested\":{\"a\":1}}'\nEXPIRE user:{{user_id}} {{ ttl_sec }}";
        assert_eq!(extract_workbench_params(script), vec!["user_id", "ttl_sec"]);

        let mut values = BTreeMap::new();
        values.insert("user_id".to_string(), "42".to_string());
        values.insert("ttl_sec".to_string(), "300".to_string());
        let rendered = render_workbench_script(script, &values);
        assert_eq!(
            rendered,
            "GET user:42\nJSON.SET doc:42 $ '{\"nested\":{\"a\":1}}'\nEXPIRE user:42 300"
        );
    }

    #[test]
    fn snippet_crud_round_trips_through_database() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "100".to_string());
        let snippet = WorkbenchSnippet {
            name: "Inspect User".to_string(),
            script: "HGETALL user:{{id}}".to_string(),
            description: "Fetch user profile".to_string(),
            params,
            created_at: 10,
            updated_at: 20,
        };
        WorkbenchManager::upsert("wb-test-1", snippet.clone()).expect("upsert");
        assert_eq!(WorkbenchManager::get("wb-test-1").expect("get"), snippet);
        assert!(WorkbenchManager::list_with_id().iter().any(|(id, _)| id == "wb-test-1"));

        WorkbenchManager::delete("wb-test-1").expect("delete");
        assert!(WorkbenchManager::get("wb-test-1").is_err());
    }
}

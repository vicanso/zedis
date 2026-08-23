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

use super::{add_normalize_history, get_database};
use crate::error::Error;
use dashmap::DashMap;
use redb::TableDefinition;
use redb::{ReadableDatabase, ReadableTable};

type Result<T, E = Error> = std::result::Result<T, E>;

pub struct HistoryManager {
    max_history_size: usize,
    history_cache: DashMap<String, Vec<String>>,
    definition: TableDefinition<'static, &'static str, &'static str>,
}

impl HistoryManager {
    pub fn new(definition: TableDefinition<'static, &'static str, &'static str>) -> Self {
        Self {
            max_history_size: 20,
            history_cache: DashMap::new(),
            definition,
        }
    }
    pub fn set_max_history_size(mut self, max_history_size: usize) -> Self {
        self.max_history_size = max_history_size;
        self
    }
    pub fn add_record(&self, server_id: &str, keyword: &str) -> Result<Vec<String>> {
        let keyword = keyword.trim();
        let db = get_database()?;
        let write_txn = db.begin_write()?;

        let history = {
            let mut table = write_txn.open_table(self.definition)?;
            let mut history = if let Some(history) = self.history_cache.get(server_id) {
                history.clone()
            } else if let Some(v) = table.get(server_id)? {
                serde_json::from_str(v.value())?
            } else {
                Vec::new()
            };
            if !keyword.is_empty() {
                add_normalize_history(&mut history, keyword.to_string(), self.max_history_size);

                self.history_cache.insert(server_id.to_string(), history.clone());

                let json_val = serde_json::to_string(&history)?;
                table.insert(server_id, json_val.as_str())?;
            }
            history
        };

        write_txn.commit()?;
        Ok(history)
    }

    pub fn records(&self, server_id: &str) -> Result<Vec<String>> {
        if let Some(history) = self.history_cache.get(server_id) {
            return Ok(history.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(self.definition)?;
        let Some(v) = table.get(server_id)? else {
            return Ok(Vec::new());
        };
        let history: Vec<String> = serde_json::from_str(v.value())?;
        self.history_cache.insert(server_id.to_string(), history.clone());
        Ok(history)
    }

    pub fn remove_record(&self, server_id: &str, keyword: &str) -> Result<Vec<String>> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return self.records(server_id);
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;

        let history = {
            let mut table = write_txn.open_table(self.definition)?;
            let mut history = if let Some(history) = self.history_cache.get(server_id) {
                history.clone()
            } else if let Some(v) = table.get(server_id)? {
                serde_json::from_str(v.value())?
            } else {
                Vec::new()
            };
            let len_before = history.len();
            history.retain(|x| x.as_str() != keyword);
            if history.len() != len_before {
                self.history_cache.insert(server_id.to_string(), history.clone());
                let json_val = serde_json::to_string(&history)?;
                table.insert(server_id, json_val.as_str())?;
            }
            history
        };

        write_txn.commit()?;
        Ok(history)
    }

    pub fn clear_history(&self, server_id: &str) -> Result<()> {
        self.history_cache.remove(server_id);
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(self.definition)?;
            table.remove(server_id)?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SEARCH_HISTORY_TABLE, get_cmd_history_manager, get_favorites_manager, get_recent_keys_manager,
        get_search_history_manager, init_database_for_tests, recent_keys_scope,
    };
    use zedis_core::fs::override_config_dir;

    /// Every manager in this crate is a `HistoryManager` over a different table,
    /// so the tests drive fresh instances over one table with ids of their own —
    /// a fresh instance also starts with an empty cache, which is what makes the
    /// cache-versus-disk checks below mean anything.
    fn manager(max: usize) -> HistoryManager {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();
        HistoryManager::new(SEARCH_HISTORY_TABLE).set_max_history_size(max)
    }

    /// The row as it actually sits on disk, bypassing the in-memory cache.
    fn on_disk(server_id: &str) -> Option<Vec<String>> {
        let db = get_database().expect("database");
        let txn = db.begin_read().expect("begin read");
        let table = txn.open_table(SEARCH_HISTORY_TABLE).expect("open table");
        let value = table.get(server_id).expect("get")?;
        Some(serde_json::from_str(value.value()).expect("stored json"))
    }

    #[test]
    fn keeps_the_most_recent_first_without_duplicates() {
        let m = manager(20);
        m.add_record("hm-mru", "alpha").expect("add");
        m.add_record("hm-mru", "beta").expect("add");
        assert_eq!(m.records("hm-mru").expect("records"), vec!["beta", "alpha"]);

        // Re-adding moves the entry to the front instead of duplicating it.
        let after = m.add_record("hm-mru", "alpha").expect("add again");
        assert_eq!(after, vec!["alpha", "beta"]);
        assert_eq!(m.records("hm-mru").expect("records"), vec!["alpha", "beta"]);
    }

    #[test]
    fn drops_the_oldest_entry_once_the_cap_is_reached() {
        let m = manager(3);
        for keyword in ["one", "two", "three", "four"] {
            m.add_record("hm-cap", keyword).expect("add");
        }
        assert_eq!(m.records("hm-cap").expect("records"), vec!["four", "three", "two"]);
        assert_eq!(on_disk("hm-cap").expect("row"), vec!["four", "three", "two"]);
    }

    #[test]
    fn trims_the_keyword_and_ignores_an_empty_one() {
        let m = manager(20);
        m.add_record("hm-trim", "  spaced  ").expect("add");
        assert_eq!(m.records("hm-trim").expect("records"), vec!["spaced"]);

        // An empty keyword is the "user pressed enter on a blank box" case: it
        // returns the current list and must not write a row.
        assert_eq!(m.add_record("hm-trim", "   ").expect("add blank"), vec!["spaced"]);
        assert_eq!(m.records("hm-trim").expect("records"), vec!["spaced"]);
    }

    #[test]
    fn what_the_cache_returns_is_what_is_on_disk() {
        let writer = manager(20);
        writer.add_record("hm-disk", "first").expect("add");
        writer.add_record("hm-disk", "second").expect("add");
        writer.remove_record("hm-disk", "first").expect("remove");

        // A second manager over the same table starts cold, so this reads the
        // committed row rather than the cache the writes populated.
        let reader = manager(20);
        assert_eq!(reader.records("hm-disk").expect("records"), vec!["second"]);
        assert_eq!(on_disk("hm-disk").expect("row"), vec!["second"]);
    }

    #[test]
    fn removing_and_clearing_leave_no_stale_cache_behind() {
        let m = manager(20);
        m.add_record("hm-clear", "keep").expect("add");
        m.add_record("hm-clear", "drop").expect("add");

        // Removing something that was never there is a no-op, not an error.
        assert_eq!(
            m.remove_record("hm-clear", "absent").expect("remove"),
            vec!["drop", "keep"]
        );
        assert_eq!(m.remove_record("hm-clear", "drop").expect("remove"), vec!["keep"]);

        m.clear_history("hm-clear").expect("clear");
        assert!(m.records("hm-clear").expect("records").is_empty());
        assert!(on_disk("hm-clear").is_none());
        assert!(manager(20).records("hm-clear").expect("cold records").is_empty());
    }

    #[test]
    fn one_servers_history_never_leaks_into_another() {
        let m = manager(20);
        m.add_record("hm-iso-a", "only-a").expect("add");
        m.add_record("hm-iso-b", "only-b").expect("add");
        assert_eq!(m.records("hm-iso-a").expect("records"), vec!["only-a"]);
        assert_eq!(m.records("hm-iso-b").expect("records"), vec!["only-b"]);

        m.clear_history("hm-iso-a").expect("clear");
        assert_eq!(m.records("hm-iso-b").expect("records"), vec!["only-b"]);
    }

    #[test]
    fn the_shipped_managers_keep_their_own_caps_and_tables() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();

        // Recent keys is the small one (10) — the key-tree dropdown shows the
        // whole list — while command history keeps 100.
        let scope = recent_keys_scope("hm-shipped", 3);
        assert_eq!(scope, "hm-shipped/3");
        for i in 0..15 {
            get_recent_keys_manager()
                .add_record(&scope, &format!("key:{i}"))
                .expect("add");
        }
        let recent = get_recent_keys_manager().records(&scope).expect("records");
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0], "key:14");

        // Separate tables: the same id in another manager is a separate list.
        get_favorites_manager().add_record(&scope, "fav:1").expect("add");
        assert_eq!(get_favorites_manager().records(&scope).expect("records"), vec!["fav:1"]);
        assert_eq!(get_recent_keys_manager().records(&scope).expect("records").len(), 10);
        assert!(get_cmd_history_manager().records(&scope).expect("records").is_empty());
        assert!(
            get_search_history_manager()
                .records(&scope)
                .expect("records")
                .is_empty()
        );
    }
}

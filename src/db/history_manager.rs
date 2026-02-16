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
use gpui::SharedString;
use redb::TableDefinition;
use redb::{ReadableDatabase, ReadableTable};

type Result<T, E = Error> = std::result::Result<T, E>;

pub struct HistoryManager {
    max_history_size: usize,
    history_cache: DashMap<String, Vec<SharedString>>,
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
    pub fn add_record(&self, server_id: &str, keyword: &str) -> Result<Vec<SharedString>> {
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
                add_normalize_history(&mut history, keyword.to_string().into(), self.max_history_size);

                self.history_cache.insert(server_id.to_string(), history.clone());

                let json_val = serde_json::to_string(&history)?;
                table.insert(server_id, json_val.as_str())?;
            }
            history
        };

        write_txn.commit()?;
        Ok(history)
    }

    pub fn records(&self, server_id: &str) -> Result<Vec<SharedString>> {
        if let Some(history) = self.history_cache.get(server_id) {
            return Ok(history.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(self.definition)?;
        let Some(v) = table.get(server_id)? else {
            return Ok(Vec::new());
        };
        let history: Vec<SharedString> = serde_json::from_str(v.value())?;
        self.history_cache.insert(server_id.to_string(), history.clone());
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

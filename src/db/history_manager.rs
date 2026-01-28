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

use super::{HISTORY_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use gpui::SharedString;
use redb::{ReadableDatabase, ReadableTable};
use std::sync::LazyLock;

type Result<T, E = Error> = std::result::Result<T, E>;

const MAX_HISTORY_SIZE: usize = 20;
static HISTORY_CACHE: LazyLock<DashMap<String, Vec<SharedString>>> = LazyLock::new(DashMap::new);

pub struct HistoryManager;

pub fn add_normalize_history(history: &mut Vec<SharedString>, keyword: SharedString) {
    history.retain(|x| *x != keyword);

    history.insert(0, keyword);

    if history.len() > MAX_HISTORY_SIZE {
        history.truncate(MAX_HISTORY_SIZE);
    }
}

impl HistoryManager {
    pub fn add_record(server_id: &str, keyword: &str) -> Result<()> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Ok(());
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;

        {
            let mut table = write_txn.open_table(HISTORY_TABLE)?;
            let mut history = if let Some(history) = HISTORY_CACHE.get(server_id) {
                history.clone()
            } else if let Some(v) = table.get(server_id)? {
                serde_json::from_str(v.value())?
            } else {
                Vec::new()
            };
            add_normalize_history(&mut history, keyword.to_string().into());

            HISTORY_CACHE.insert(server_id.to_string(), history.clone());

            let json_val = serde_json::to_string(&history)?;
            table.insert(server_id, json_val.as_str())?;
        }

        write_txn.commit()?;
        Ok(())
    }

    pub fn records(server_id: &str) -> Result<Vec<SharedString>> {
        if let Some(history) = HISTORY_CACHE.get(server_id) {
            return Ok(history.clone());
        }
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(HISTORY_TABLE)?;
        let Some(v) = table.get(server_id)? else {
            return Ok(Vec::new());
        };
        let history: Vec<SharedString> = serde_json::from_str(v.value())?;
        HISTORY_CACHE.insert(server_id.to_string(), history.clone());
        Ok(history)
    }

    pub fn clear_history(server_id: &str) -> Result<()> {
        HISTORY_CACHE.remove(server_id);
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(HISTORY_TABLE)?;
            table.remove(server_id)?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

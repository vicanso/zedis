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
use crate::helpers::{get_or_create_config_dir, is_development};
use gpui::SharedString;
use redb::{Database, TableDefinition};
use std::sync::OnceLock;
use tracing::debug;

mod cmd_history_manager;
mod history_manager;
mod protos;
mod search_history_manager;

pub use cmd_history_manager::*;
pub use protos::*;
pub use search_history_manager::*;

const SEARCH_HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("search_history");
const PROTO_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("proto");
const CMD_HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("cmd_history");

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
    }
    write_txn.commit()?;
    debug!(path = db_path.display().to_string(), "database initialized success");
    DATABASE.set(db).map_err(|_| Error::Invalid {
        message: "database initialized failed".to_string(),
    })?;
    Ok(())
}

fn add_normalize_history(history: &mut Vec<SharedString>, keyword: SharedString, max: usize) {
    history.retain(|x| *x != keyword);

    history.insert(0, keyword);

    if history.len() > max {
        history.truncate(max);
    }
}

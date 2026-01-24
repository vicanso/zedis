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
use redb::{Database, TableDefinition};
use std::sync::OnceLock;

mod history_manager;

pub use history_manager::*;

const HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("search_history");

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
    let db = Database::create(db_path)?;
    let write_txn = db.begin_write()?;
    {
        write_txn.open_table(HISTORY_TABLE)?;
    }
    write_txn.commit()?;
    DATABASE.set(db).map_err(|_| Error::Invalid {
        message: "database initialized failed".to_string(),
    })?;
    Ok(())
}

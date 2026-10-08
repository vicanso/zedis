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

//! Backup / restore of the local redb store (tags, favorites, script
//! viewers, Lua scripts, proto bindings) as one JSON file — the Settings
//! "Local data" section. The document itself is `zedis_db::backup`; this
//! is the file plumbing. Settings picks the path with a save dialog.

use crate::error::Error;
use chrono::Local;
use std::path::Path;
use zedis_core::fs::write_file_atomic;
use zedis_db::{ImportSummary, LocalDataBackup, export_local_data, import_local_data};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds the backup JSON and a suggested filename. Settings asks where to
/// write it — a machine with no Downloads folder must not silently land the
/// file in the config directory.
pub fn export_local_data_json() -> Result<(String, Vec<u8>)> {
    let now = Local::now();
    let backup = export_local_data(env!("CARGO_PKG_VERSION"), now.timestamp())?;
    let json = serde_json::to_vec_pretty(&backup)?;
    let name = format!("zedis-local-data-{}.json", now.format("%Y%m%d-%H%M%S"));
    Ok((name, json))
}

/// Writes a backup produced by [`export_local_data_json`].
pub fn write_local_data_file(path: &Path, json: &[u8]) -> Result<()> {
    Ok(write_file_atomic(path, json)?)
}

/// Reads a backup file and merges it into the store.
pub fn import_local_data_file(path: &Path) -> Result<ImportSummary> {
    let bytes = std::fs::read(path)?;
    let backup: LocalDataBackup = serde_json::from_slice(&bytes)?;
    Ok(import_local_data(&backup)?)
}

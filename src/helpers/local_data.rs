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
//! is only the file plumbing, laid out like the diagnostics bundle.

use crate::error::Error;
use chrono::Local;
use std::io;
use std::path::{Path, PathBuf};
use zedis_core::fs::{get_download_dir, get_or_create_config_dir, write_file_atomic};
use zedis_db::{ImportSummary, LocalDataBackup, export_local_data, import_local_data};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Writes `zedis-local-data-<stamp>.json` to Downloads (the config dir when
/// there is none — App Store sandbox) and returns its path.
pub fn export_local_data_file() -> Result<PathBuf> {
    let dir = get_download_dir()
        .or_else(|| get_or_create_config_dir().ok())
        .ok_or_else(|| io::Error::other("no directory to write the backup to"))?;
    let now = Local::now();
    let backup = export_local_data(env!("CARGO_PKG_VERSION"), now.timestamp())?;
    let json = serde_json::to_vec_pretty(&backup)?;
    let path = dir.join(format!("zedis-local-data-{}.json", now.format("%Y%m%d-%H%M%S")));
    write_file_atomic(&path, &json)?;
    Ok(path)
}

/// Reads a backup file and merges it into the store.
pub fn import_local_data_file(path: &Path) -> Result<ImportSummary> {
    let bytes = std::fs::read(path)?;
    let backup: LocalDataBackup = serde_json::from_slice(&bytes)?;
    Ok(import_local_data(&backup)?)
}

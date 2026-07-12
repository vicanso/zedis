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

//! Custom script viewer support.
//!
//! Allows users to process a Redis value through an external shell command and
//! display the stdout as the formatted value.  `shell_command` is a template
//! string executed via `sh -c` (Unix/macOS) or `cmd /c` (Windows) where the
//! following placeholders are substituted before execution:
//!
//! | Placeholder  | Replaced with                                             |
//! |-------------|-------------------------------------------------------------|
//! | `{KEY}`     | The Redis key name                                          |
//! | `{VALUE}`   | The raw value as a UTF-8 string (lossy)                     |
//! | `{HEX}`     | Lower-hex encoding of the raw bytes                         |
//! | `{HEX_FILE}`| Path to a temp file containing the hex-encoded bytes        |
//! | `{RAW_FILE}`| Path to a temp file containing the raw bytes (binary-safe)  |
//!
//! `{RAW_FILE}` is the recommended way to feed binary data to a command without
//! relying on `echo` or shell quoting.  Shell pipes and redirections work
//! naturally because the command runs through the shell interpreter.
//!
//! ```text
//! base64 --decode {RAW_FILE}           # binary-safe, no echo needed
//! base64 --decode < {RAW_FILE}         # redirect as stdin
//! cat {HEX_FILE} | xxd -r -p | jq .   # hex → binary → JSON
//! jq -r .name {RAW_FILE}               # parse JSON value directly
//! ```

use super::{SCRIPT_VIEWER_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use redb::{ReadableDatabase, ReadableTable};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use tempfile::NamedTempFile;
use tracing::info;

pub use super::protos::MatchMode;

type Result<T, E = Error> = std::result::Result<T, E>;

/// HEX strings longer than this are written to a temp file instead of inlined.
const HEX_INLINE_MAX: usize = 8000;

static SCRIPT_CACHE: LazyLock<DashMap<String, ScriptConfig>> = LazyLock::new(DashMap::new);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptConfig {
    pub server_id: String,
    pub name: String,
    /// Full shell command template executed via `sh -c` (Unix) or `cmd /c` (Windows).
    /// Supports placeholders: `{KEY}`, `{VALUE}`, `{HEX}`, `{HEX_FILE}`, `{RAW_FILE}`.
    /// Shell features (pipes, redirects, etc.) work naturally.
    pub shell_command: String,
    pub match_pattern: String,
    pub mode: MatchMode,
}

pub struct ScriptManager;

impl ScriptManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(SCRIPT_VIEWER_TABLE)?;

        let mut invalid_ids: Vec<String> = Vec::new();
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            let config: ScriptConfig = match serde_json::from_slice(value.value()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(id, error = %e, "invalid script entry, will be removed");
                    invalid_ids.push(id.to_string());
                    continue;
                }
            };
            info!(id, name = config.name, server_id = config.server_id, "load script");
            SCRIPT_CACHE.insert(id.to_string(), config);
        }
        drop(read_txn);

        if !invalid_ids.is_empty() {
            let write_txn = db.begin_write()?;
            {
                let mut table = write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
                for id in &invalid_ids {
                    table.remove(id.as_str())?;
                }
            }
            write_txn.commit()?;
            info!(count = invalid_ids.len(), "removed invalid script entries");
        }

        info!(count = SCRIPT_CACHE.len(), "load scripts success");
        Ok(())
    }

    pub fn list_with_id() -> Vec<(String, ScriptConfig)> {
        SCRIPT_CACHE
            .iter()
            .map(|item| (item.key().clone(), item.value().clone()))
            .collect()
    }

    pub fn get(id: &str) -> Result<ScriptConfig> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(SCRIPT_VIEWER_TABLE)?;
        let Some(v) = table.get(id)? else {
            return Err(Error::Invalid {
                message: "script viewer not found".to_string(),
            });
        };
        Ok(serde_json::from_slice(v.value())?)
    }

    pub fn upsert(id: &str, config: ScriptConfig) -> Result<()> {
        if config.name.is_empty() {
            return Err(Error::Invalid {
                message: "script viewer name is empty".to_string(),
            });
        }
        if config.shell_command.is_empty() {
            return Err(Error::Invalid {
                message: "script viewer shell command is empty".to_string(),
            });
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
            let json = serde_json::to_string(&config)?;
            table.insert(id, json.as_bytes())?;
        }
        write_txn.commit()?;
        SCRIPT_CACHE.insert(id.to_string(), config);
        Ok(())
    }

    pub fn delete(id: &str) -> Result<()> {
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(SCRIPT_VIEWER_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        SCRIPT_CACHE.remove(id);
        Ok(())
    }

    /// Returns the script ID whose pattern matches `key` for `server_id`.
    pub fn match_key_to_id(server_id: &str, key: &str) -> Option<String> {
        let item = SCRIPT_CACHE.iter().find(|item| {
            if item.server_id != server_id {
                return false;
            }
            match item.mode {
                MatchMode::Exact => key == item.match_pattern,
                MatchMode::Prefix => key.starts_with(&item.match_pattern),
                MatchMode::Suffix => key.ends_with(&item.match_pattern),
                MatchMode::Regex => Regex::new(&item.match_pattern).is_ok_and(|re| re.is_match(key)),
            }
        })?;
        Some(item.key().clone())
    }

    /// Executes the shell command for the given script ID with `data` as input.
    ///
    /// Substitutes template placeholders in `shell_command`, then runs it via
    /// `sh -c` and returns the captured stdout.
    pub fn execute(id: &str, key: &str, data: &[u8]) -> Result<String> {
        let config = {
            if let Some(c) = SCRIPT_CACHE.get(id) {
                c.clone()
            } else {
                Self::get(id)?
            }
        };

        let hex = bytes_to_hex(data);
        let value_str = String::from_utf8_lossy(data);
        let tmpl = &config.shell_command;

        // {RAW_FILE}: temp file with the raw bytes — binary-safe, no echo needed.
        let raw_file: Option<NamedTempFile> = if tmpl.contains("{RAW_FILE}") {
            let mut f = NamedTempFile::new().map_err(|e| Error::Invalid {
                message: format!("failed to create raw temp file: {e}"),
            })?;
            f.write_all(data).map_err(|e| Error::Invalid {
                message: format!("failed to write raw temp file: {e}"),
            })?;
            Some(f)
        } else {
            None
        };

        // {HEX_FILE}: temp file with hex-encoded bytes.
        // Also used when {HEX} is present but the hex string is too long to inline.
        let hex_file: Option<NamedTempFile> =
            if tmpl.contains("{HEX_FILE}") || (tmpl.contains("{HEX}") && hex.len() > HEX_INLINE_MAX) {
                let mut f = NamedTempFile::new().map_err(|e| Error::Invalid {
                    message: format!("failed to create hex temp file: {e}"),
                })?;
                f.write_all(hex.as_bytes()).map_err(|e| Error::Invalid {
                    message: format!("failed to write hex temp file: {e}"),
                })?;
                Some(f)
            } else {
                None
            };

        // Substitute all placeholders in the shell command string.
        let mut cmd_str = tmpl.replace("{KEY}", key);
        cmd_str = cmd_str.replace("{VALUE}", value_str.as_ref());
        if let Some(ref f) = raw_file {
            cmd_str = cmd_str.replace("{RAW_FILE}", &f.path().to_string_lossy());
        } else {
            cmd_str = cmd_str.replace("{RAW_FILE}", "");
        }
        if let Some(ref f) = hex_file {
            let path = f.path().to_string_lossy();
            cmd_str = cmd_str.replace("{HEX_FILE}", path.as_ref());
            if hex.len() > HEX_INLINE_MAX {
                cmd_str = cmd_str.replace("{HEX}", path.as_ref());
            } else {
                cmd_str = cmd_str.replace("{HEX}", &hex);
            }
        } else {
            cmd_str = cmd_str.replace("{HEX}", &hex);
            cmd_str = cmd_str.replace("{HEX_FILE}", "");
        }

        // Use the platform shell: sh -c on Unix/macOS, cmd /c on Windows.
        #[cfg(target_os = "windows")]
        let (shell, flag) = ("cmd", "/c");
        #[cfg(not(target_os = "windows"))]
        let (shell, flag) = ("sh", "-c");

        let output = Command::new(shell)
            .args([flag, &cmd_str])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| Error::Invalid {
                message: format!("failed to run shell command: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Invalid {
                message: format!("script exited with error: {}", stderr.trim()),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn bytes_to_hex(data: &[u8]) -> String {
    data.iter().fold(String::with_capacity(data.len() * 2), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
        s
    })
}

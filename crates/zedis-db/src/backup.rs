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

//! Portable backup of the user-authored local data: key tags / notes,
//! favorites, script viewers, the Lua script library and proto bindings.
//!
//! The redb file is the only copy of all of this, so a backup is one JSON
//! document the user can keep, move to another machine, or restore after
//! a reinstall. Histories (commands, searches, recent keys), metrics and
//! the trash are deliberately left out — they are caches, not authored
//! data. Import *merges*: existing rows stay, rows with the same id / key
//! are overwritten, and favorites keep their order. Every container carries
//! `#[serde(default)]` so a backup from an older build still reads.

use crate::error::Error;
use crate::{
    KeyMetadata, LuaScript, LuaScriptManager, ProtoConfig, ProtoManager, ScriptConfig, ScriptManager,
    get_favorites_manager, get_key_metadata_manager,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Bumped when the document shape changes incompatibly; an import refuses
/// a newer format instead of guessing.
pub const BACKUP_FORMAT: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalDataBackup {
    pub format: u32,
    /// Unix seconds when the backup was written.
    pub exported_at: i64,
    pub app_version: String,
    pub key_metadata: Vec<ServerKeyMetadata>,
    pub favorites: Vec<ServerFavorites>,
    pub script_viewers: Vec<Stored<ScriptConfig>>,
    pub lua_scripts: Vec<Stored<LuaScript>>,
    pub protos: Vec<Stored<ProtoConfig>>,
}

/// One server's tags and notes, keyed by Redis key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerKeyMetadata {
    pub server_id: String,
    pub entries: HashMap<String, KeyMetadata>,
}

/// One server's favorites, most recent first (the order the sidebar shows).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerFavorites {
    pub server_id: String,
    pub keys: Vec<String>,
}

/// A row with its store id, so a re-import overwrites the same row instead
/// of duplicating it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored<T: Default> {
    pub id: String,
    pub value: T,
}

/// What an import wrote, for the confirmation notice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub key_metadata: usize,
    pub favorites: usize,
    pub script_viewers: usize,
    pub lua_scripts: usize,
    pub protos: usize,
    /// Rows the store rejected (a proto that no longer compiles, …); logged
    /// individually.
    pub skipped: usize,
}

impl ImportSummary {
    pub fn total(&self) -> usize {
        self.key_metadata + self.favorites + self.script_viewers + self.lua_scripts + self.protos
    }
}

/// Snapshot every authored table into one document.
pub fn export_local_data(app_version: &str, exported_at: i64) -> Result<LocalDataBackup> {
    let key_metadata = get_key_metadata_manager()
        .all_records()?
        .into_iter()
        .map(|(server_id, entries)| ServerKeyMetadata { server_id, entries })
        .collect();
    let favorites = get_favorites_manager()
        .all_records()?
        .into_iter()
        .map(|(server_id, keys)| ServerFavorites { server_id, keys })
        .collect();
    let mut script_viewers: Vec<Stored<ScriptConfig>> = ScriptManager::list_with_id()
        .into_iter()
        .map(|(id, value)| Stored { id, value })
        .collect();
    script_viewers.sort_by(|a, b| a.id.cmp(&b.id));
    let mut lua_scripts: Vec<Stored<LuaScript>> = LuaScriptManager::list_with_id()
        .into_iter()
        .map(|(id, value)| Stored { id, value })
        .collect();
    lua_scripts.sort_by(|a, b| a.id.cmp(&b.id));
    let mut protos: Vec<Stored<ProtoConfig>> = ProtoManager::list_protos_with_id()
        .into_iter()
        .map(|(id, value)| Stored { id, value })
        .collect();
    protos.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(LocalDataBackup {
        format: BACKUP_FORMAT,
        exported_at,
        app_version: app_version.to_string(),
        key_metadata,
        favorites,
        script_viewers,
        lua_scripts,
        protos,
    })
}

/// Merge a document into the store. Storage errors abort (the store is
/// then partially updated, which a re-import repairs — every write is an
/// upsert); a row the store rejects is skipped and counted.
pub fn import_local_data(backup: &LocalDataBackup) -> Result<ImportSummary> {
    if backup.format > BACKUP_FORMAT {
        return Err(Error::Invalid {
            message: format!(
                "backup format {} is newer than this build understands ({BACKUP_FORMAT})",
                backup.format
            ),
        });
    }
    let mut summary = ImportSummary::default();

    let metadata = get_key_metadata_manager();
    for server in &backup.key_metadata {
        let entries: Vec<(String, KeyMetadata)> = server
            .entries
            .iter()
            .filter(|(_, metadata)| !metadata.is_empty())
            .map(|(key, metadata)| (key.clone(), metadata.clone()))
            .collect();
        summary.key_metadata += entries.len();
        metadata.set_many(&server.server_id, entries)?;
    }

    let favorites = get_favorites_manager();
    for server in &backup.favorites {
        // `add_record` moves a key to the front, so replaying oldest-first
        // leaves the backup's order intact.
        for key in server.keys.iter().rev() {
            favorites.add_record(&server.server_id, key)?;
            summary.favorites += 1;
        }
    }

    for item in &backup.script_viewers {
        ScriptManager::upsert(&item.id, item.value.clone())?;
        summary.script_viewers += 1;
    }
    for item in &backup.lua_scripts {
        LuaScriptManager::upsert(&item.id, item.value.clone())?;
        summary.lua_scripts += 1;
    }
    for item in &backup.protos {
        match ProtoManager::upsert_proto(&item.id, item.value.clone()) {
            Ok(()) => summary.protos += 1,
            Err(e) => {
                warn!(id = %item.id, name = %item.value.name, error = %e, "proto binding skipped");
                summary.skipped += 1;
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TagColor, init_database_for_tests};
    use zedis_core::fs::override_config_dir;

    fn setup() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();
    }

    #[test]
    fn export_then_import_round_trips_every_table() {
        setup();
        let server = "backup-test-server";
        get_key_metadata_manager()
            .set_many(
                server,
                [(
                    "user:1".to_string(),
                    KeyMetadata {
                        tag: Some(TagColor::Red),
                        note: "vip".to_string(),
                    },
                )],
            )
            .expect("set metadata");
        get_favorites_manager().add_record(server, "older").expect("fav");
        get_favorites_manager().add_record(server, "newer").expect("fav");
        ScriptManager::upsert(
            "backup-viewer",
            ScriptConfig {
                server_id: server.to_string(),
                name: "viewer".to_string(),
                shell_command: "cat".to_string(),
                match_pattern: "user:*".to_string(),
                ..Default::default()
            },
        )
        .expect("viewer");
        LuaScriptManager::upsert(
            "backup-lua",
            LuaScript {
                name: "ping".to_string(),
                code: "return 1".to_string(),
                sha: "abc".to_string(),
                ..Default::default()
            },
        )
        .expect("lua");

        let backup = export_local_data("0.0.0-test", 42).expect("export");
        assert_eq!(backup.format, BACKUP_FORMAT);
        assert_eq!(backup.exported_at, 42);
        let tags = backup
            .key_metadata
            .iter()
            .find(|item| item.server_id == server)
            .expect("server tags exported");
        assert_eq!(tags.entries["user:1"].note, "vip");
        let favs = backup
            .favorites
            .iter()
            .find(|item| item.server_id == server)
            .expect("favorites exported");
        assert_eq!(favs.keys, vec!["newer".to_string(), "older".to_string()]);
        assert!(backup.script_viewers.iter().any(|item| item.id == "backup-viewer"));
        assert!(backup.lua_scripts.iter().any(|item| item.id == "backup-lua"));

        // The JSON round trip is what a file restore does.
        let json = serde_json::to_string(&backup).expect("json");
        let parsed: LocalDataBackup = serde_json::from_str(&json).expect("parse");
        let summary = import_local_data(&parsed).expect("import");
        assert!(summary.key_metadata >= 1);
        assert!(summary.favorites >= 2);
        assert!(summary.script_viewers >= 1);
        assert!(summary.lua_scripts >= 1);
        assert_eq!(summary.skipped, 0);
        // Merged, not duplicated, and the favorites order survived.
        let favs = get_favorites_manager().records(server).expect("records");
        assert_eq!(favs, vec!["newer".to_string(), "older".to_string()]);
        assert_eq!(
            get_key_metadata_manager()
                .records(server)
                .expect("records")
                .get("user:1")
                .map(|m| m.note.clone()),
            Some("vip".to_string())
        );
    }

    #[test]
    fn a_newer_format_is_refused_and_an_old_document_still_reads() {
        setup();
        let newer = LocalDataBackup {
            format: BACKUP_FORMAT + 1,
            ..Default::default()
        };
        assert!(import_local_data(&newer).is_err());
        // A minimal document from before any optional section existed.
        let old: LocalDataBackup = serde_json::from_str(r#"{"format":1}"#).expect("parse");
        let summary = import_local_data(&old).expect("import");
        assert_eq!(summary.total(), 0);
    }
}

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

use super::{PROTO_TABLE, get_database};
use crate::error::Error;
use dashmap::DashMap;
use prost_reflect::{DescriptorPool, DynamicMessage};
use redb::{ReadableDatabase, ReadableTable};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;
use tempfile::TempDir;
use tracing::{info, warn};
use zedis_core::fs::resolve_path;

type Result<T, E = Error> = std::result::Result<T, E>;

static PROTO_META_CACHE: LazyLock<DashMap<String, ProtoConfig>> = LazyLock::new(DashMap::new);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum MatchMode {
    #[default]
    Prefix,
    Suffix,
    Regex,
    Exact,
}

impl From<usize> for MatchMode {
    fn from(value: usize) -> Self {
        match value {
            1 => MatchMode::Suffix,
            2 => MatchMode::Regex,
            3 => MatchMode::Exact,
            _ => MatchMode::Prefix,
        }
    }
}

impl From<MatchMode> for usize {
    fn from(value: MatchMode) -> Self {
        match value {
            MatchMode::Prefix => 0,
            MatchMode::Suffix => 1,
            MatchMode::Regex => 2,
            MatchMode::Exact => 3,
        }
    }
}
/// One saved proto viewer.
///
/// `#[serde(default)]` is the upgrade contract every value this crate stores in
/// redb follows: a row written by an earlier build must still deserialize after
/// a field is added, because the loaders below skip what they cannot read — and
/// a decoder definition the user typed is not something a version bump gets to
/// drop. New fields are therefore always optional or defaulted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtoConfig {
    pub server_id: String,
    pub name: String,
    pub match_pattern: String,
    pub mode: MatchMode,
    pub includes: Option<String>,
    pub content: Option<String>,
    pub target_message: Option<String>,
}

fn proto_to_json(pool: &DescriptorPool, message_name: &str, bytes: &[u8]) -> Result<String> {
    let message_descriptor = pool.get_message_by_name(message_name).ok_or(Error::Invalid {
        message: "message not found".to_string(),
    })?;

    let dynamic_msg = DynamicMessage::decode(message_descriptor, bytes)?;

    let json_output = serde_json::to_string_pretty(&dynamic_msg)?;

    Ok(json_output)
}

fn parse_protobuf(content: &str, includes: &str) -> Result<(DescriptorPool, Vec<String>)> {
    if content.is_empty() {
        return Err(Error::Invalid {
            message: "content is empty".to_string(),
        });
    }
    let temp_dir = TempDir::new()?;
    let temp_path = temp_dir.path();
    let mut files = Vec::new();
    let mut dirs = includes
        .split(",")
        .map(|item| Path::new(&resolve_path(item)).to_path_buf())
        .collect::<Vec<_>>();
    let is_proto_file = Path::new(content)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("proto"));
    if is_proto_file {
        let file = resolve_path(content);
        let file_path = Path::new(&file);
        if !file_path.exists() {
            return Err(Error::Invalid {
                message: "proto file not found".to_string(),
            });
        }
        files.push(file_path.to_path_buf());
        if let Some(parent) = file_path.parent() {
            dirs.push(parent.to_path_buf());
        }
    } else {
        let file_path = temp_path.join("main.proto");
        fs::write(&file_path, content)?;
        files.push(file_path);
        dirs.push(temp_path.to_path_buf());
    }
    let file_descriptor_set = protox::compile(files, dirs)?;
    let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(file_descriptor_set)?;
    let messages = pool
        .all_messages()
        .map(|message| message.full_name().to_string())
        .collect::<Vec<_>>();
    Ok((pool, messages))
}

pub struct ProtoManager;

impl ProtoManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(PROTO_TABLE)?;

        let mut skipped = 0usize;
        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            // A row this build cannot read is skipped and left on disk, never
            // deleted — it holds a definition the user wrote, and a later build
            // may well read it again. Skipping per row also stops one bad entry
            // from hiding every entry after it, which propagating here did.
            let mut config: ProtoConfig = match serde_json::from_slice(value.value()) {
                Ok(c) => c,
                Err(e) => {
                    warn!(id, error = %e, "unreadable proto entry, skipped");
                    skipped += 1;
                    continue;
                }
            };
            if config.name.is_empty() {
                warn!(id, "incomplete proto entry, skipped");
                skipped += 1;
                continue;
            }
            info!(
                id,
                name = config.name,
                server_id = config.server_id,
                match_pattern = config.match_pattern,
                "load proto"
            );
            config.content = None;
            PROTO_META_CACHE.insert(id.to_string(), config);
        }
        info!(count = PROTO_META_CACHE.len(), skipped, "load protos success");

        Ok(())
    }
    pub fn list_protos_with_id() -> Vec<(String, ProtoConfig)> {
        let cache = &PROTO_META_CACHE;
        cache
            .iter()
            .map(|item| (item.key().clone(), item.value().clone()))
            .collect::<Vec<_>>()
    }
    pub fn parse_protobuf(content: &str, includes: &str) -> Result<(DescriptorPool, Vec<String>)> {
        parse_protobuf(content, includes)
    }
    pub fn get_proto(id: &str) -> Result<ProtoConfig> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(PROTO_TABLE)?;
        let Some(v) = table.get(id)? else {
            return Err(Error::Invalid {
                message: "proto not found".to_string(),
            });
        };
        let proto: ProtoConfig = serde_json::from_slice(v.value())?;
        Ok(proto)
    }
    pub fn delete_proto(id: &str) -> Result<()> {
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(PROTO_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        PROTO_META_CACHE.remove(id);
        Ok(())
    }
    pub fn match_key_to_name(server_id: &str, key: &str) -> Option<String> {
        let cache = &PROTO_META_CACHE;
        let item = cache.iter().find(|item| {
            if item.server_id != server_id {
                return false;
            }
            match item.mode {
                MatchMode::Exact => key == item.match_pattern,
                MatchMode::Prefix => key.starts_with(&item.match_pattern),
                MatchMode::Suffix => key.ends_with(&item.match_pattern),
                MatchMode::Regex => {
                    if let Ok(re) = Regex::new(&item.match_pattern) {
                        re.is_match(key)
                    } else {
                        false
                    }
                }
            }
        })?;
        Some(item.key().to_string())
    }
    pub fn upsert_proto(id: &str, mut proto: ProtoConfig) -> Result<()> {
        if proto.name.is_empty() {
            return Err(Error::Invalid {
                message: "proto name is empty".to_string(),
            });
        }
        let db = get_database()?;
        let write_txn = db.begin_write()?;
        {
            let mut table = write_txn.open_table(PROTO_TABLE)?;
            let json_val = serde_json::to_string(&proto)?;
            table.insert(id, json_val.as_bytes())?;
        }
        write_txn.commit()?;
        proto.content = None;
        PROTO_META_CACHE.insert(id.to_string(), proto);
        Ok(())
    }
    pub fn decode_data(id: &str, data: &[u8]) -> Result<String> {
        let proto = {
            let db = get_database()?;
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(PROTO_TABLE)?;
            let Some(v) = table.get(id)? else {
                return Err(Error::Invalid {
                    message: "proto not found".to_string(),
                });
            };
            let proto: ProtoConfig = serde_json::from_slice(v.value())?;
            proto
        };
        let content = proto.content.unwrap_or_default();
        if content.trim().is_empty() {
            return Err(Error::Invalid {
                message: "proto content is empty".to_string(),
            });
        };
        let includes = proto.includes.unwrap_or_default();
        let (pool, messages) = parse_protobuf(&content, &includes)?;
        let mut target_message = proto.target_message.unwrap_or_default();
        if target_message.is_empty() {
            target_message = messages.first().map(|item| item.to_string()).unwrap_or_default();
        }

        if target_message.is_empty() {
            return Err(Error::Invalid {
                message: "target message is empty".to_string(),
            });
        }
        proto_to_json(&pool, &target_message, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_database_for_tests;
    use zedis_core::fs::override_config_dir;

    fn setup() {
        override_config_dir(std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())));
        init_database_for_tests();
    }

    fn config(server_id: &str, pattern: &str, mode: MatchMode) -> ProtoConfig {
        ProtoConfig {
            server_id: server_id.to_string(),
            name: "orders".to_string(),
            match_pattern: pattern.to_string(),
            mode,
            includes: None,
            content: Some("syntax = \"proto3\";".to_string()),
            target_message: Some("Order".to_string()),
        }
    }

    fn write_raw(id: &str, json: &[u8]) {
        let db = get_database().expect("database");
        let txn = db.begin_write().expect("begin write");
        {
            let mut table = txn.open_table(PROTO_TABLE).expect("open");
            table.insert(id, json).expect("insert");
        }
        txn.commit().expect("commit");
    }

    fn raw_exists(id: &str) -> bool {
        let db = get_database().expect("database");
        let txn = db.begin_read().expect("begin read");
        let table = txn.open_table(PROTO_TABLE).expect("open");
        table.get(id).expect("get").is_some()
    }

    #[test]
    fn a_proto_saved_by_an_older_build_still_loads() {
        let legacy = br#"{"server_id":"s1","name":"orders","match_pattern":"order:"}"#;
        let parsed: ProtoConfig = serde_json::from_slice(legacy).expect("legacy row parses");
        assert_eq!(parsed.match_pattern, "order:");
        assert_eq!(parsed.mode, MatchMode::Prefix);
        assert!(parsed.content.is_none());
    }

    #[test]
    fn the_schema_text_stays_on_disk_and_out_of_the_cache() {
        setup();
        ProtoManager::upsert_proto("pr-rt", config("pr-rt-srv", "order:", MatchMode::Prefix)).expect("upsert");

        // The cache is a metadata index — a .proto body can be large and is only
        // needed when something is actually decoded.
        let cached = ProtoManager::list_protos_with_id()
            .into_iter()
            .find(|(id, _)| id == "pr-rt")
            .expect("cached");
        assert!(cached.1.content.is_none());

        let stored = ProtoManager::get_proto("pr-rt").expect("get");
        assert_eq!(stored.content.as_deref(), Some("syntax = \"proto3\";"));
        assert_eq!(stored.target_message.as_deref(), Some("Order"));

        ProtoManager::delete_proto("pr-rt").expect("delete");
        assert!(ProtoManager::get_proto("pr-rt").is_err());
        assert!(!raw_exists("pr-rt"));
        assert!(ProtoManager::match_key_to_name("pr-rt-srv", "order:1").is_none());
    }

    #[test]
    fn matches_a_key_by_every_mode_and_only_for_its_own_server() {
        setup();
        ProtoManager::upsert_proto("pr-pre", config("pr-m-srv", "order:", MatchMode::Prefix)).expect("upsert");
        ProtoManager::upsert_proto("pr-suf", config("pr-m-srv", ":pb", MatchMode::Suffix)).expect("upsert");
        ProtoManager::upsert_proto("pr-exa", config("pr-m-srv", "exactly", MatchMode::Exact)).expect("upsert");
        ProtoManager::upsert_proto("pr-re", config("pr-m-srv", "^ev[0-9]+$", MatchMode::Regex)).expect("upsert");

        assert_eq!(
            ProtoManager::match_key_to_name("pr-m-srv", "order:1").as_deref(),
            Some("pr-pre")
        );
        assert_eq!(
            ProtoManager::match_key_to_name("pr-m-srv", "blob:pb").as_deref(),
            Some("pr-suf")
        );
        assert_eq!(
            ProtoManager::match_key_to_name("pr-m-srv", "exactly").as_deref(),
            Some("pr-exa")
        );
        assert_eq!(
            ProtoManager::match_key_to_name("pr-m-srv", "ev7").as_deref(),
            Some("pr-re")
        );
        assert!(ProtoManager::match_key_to_name("pr-m-srv", "nothing").is_none());
        assert!(ProtoManager::match_key_to_name("pr-other-srv", "order:1").is_none());
    }

    #[test]
    fn refuses_an_unnamed_proto() {
        setup();
        let mut unnamed = config("pr-bad-srv", "k", MatchMode::Prefix);
        unnamed.name = String::new();
        assert!(ProtoManager::upsert_proto("pr-bad", unnamed).is_err());
        assert!(!raw_exists("pr-bad"));
    }

    #[test]
    fn one_unreadable_row_no_longer_hides_every_row_after_it() {
        setup();
        // `init` used to propagate the first parse error, so whichever entries
        // sorted after the bad one silently disappeared from the picker. The
        // ids here bracket the broken one in key order on purpose.
        write_raw("pr-init-1-broken", b"} not json {");
        ProtoManager::upsert_proto("pr-init-2-good", config("pr-init-srv", "after:", MatchMode::Prefix))
            .expect("upsert");
        PROTO_META_CACHE.remove("pr-init-2-good");

        ProtoManager::init().expect("init");

        assert!(PROTO_META_CACHE.contains_key("pr-init-2-good"), "later rows still load");
        assert!(!PROTO_META_CACHE.contains_key("pr-init-1-broken"));
        assert!(raw_exists("pr-init-1-broken"), "an unreadable row is never deleted");
    }
}

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
use tracing::info;

type Result<T, E = Error> = std::result::Result<T, E>;

static PROTO_META_CACHE: LazyLock<DashMap<String, ProtoConfig>> = LazyLock::new(DashMap::new);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MatchMode {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtoConfig {
    pub server_id: String,
    pub name: String,
    pub match_pattern: String,
    pub mode: MatchMode,
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

pub struct ProtoManager;

impl ProtoManager {
    pub fn init() -> Result<()> {
        let db = get_database()?;
        let read_txn = db.begin_read()?;
        let table = read_txn.open_table(PROTO_TABLE)?;

        for item in table.iter()? {
            let (key, value) = item?;
            let id = key.value();
            let mut config: ProtoConfig = serde_json::from_slice(value.value())?;
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
        info!(count = PROTO_META_CACHE.len(), "load protos success");

        Ok(())
    }
    pub fn list_protos() -> Vec<ProtoConfig> {
        let cache = &PROTO_META_CACHE;
        cache.iter().map(|item| item.value().clone()).collect::<Vec<_>>()
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
    pub fn add_proto(id: &str, mut proto: ProtoConfig) -> Result<()> {
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
        let Some(content) = proto.content else {
            return Err(Error::Invalid {
                message: "proto content is empty".to_string(),
            });
        };
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();
        let content = content.trim();
        let mut files = Vec::new();
        if content.ends_with(".proto") {
            files.push(Path::new(content).to_path_buf());
        } else {
            let file_path = temp_path.join(proto.name);
            fs::write(&file_path, content)?;
            files.push(file_path);
        }
        let file_descriptor_set = protox::compile(files, [temp_path])?;
        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(file_descriptor_set)?;
        let mut target_message = proto.target_message.unwrap_or_default();
        if target_message.is_empty()
            && let Some(message) = pool.all_messages().next()
        {
            target_message = message.name().to_string();
        }
        if target_message.is_empty() {
            return Err(Error::Invalid {
                message: "target message is empty".to_string(),
            });
        }
        proto_to_json(&pool, &target_message, data)
    }
}

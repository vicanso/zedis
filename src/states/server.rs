// Copyright 2025 Tree xie.
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

use crate::connection::RedisServer;
use crate::connection::get_connection_manager;
use crate::connection::{get_servers, save_servers};
use crate::error::Error;
use ahash::AHashMap;
use ahash::AHashSet;
use chrono::Local;
use gpui::prelude::*;
use gpui_component::tree::TreeItem;
use pretty_hex::{HexConfig, config_hex};
use redis::{cmd, pipe};
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;
use tracing::error;
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

const DEFAULT_SCAN_RESULT_MAX: usize = 1_000;
// string, list, set, zset, hash, stream, and vectorset.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum KeyType {
    #[default]
    Unknown,
    String,
    List,
    Set,
    Zset,
    Hash,
    Stream,
    Vectorset,
}

impl KeyType {
    /// 返回单字符的字符串切片
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyType::String => "S",
            KeyType::List => "L",
            KeyType::Hash => "H",
            KeyType::Zset => "Z",
            // 冲突解决策略：
            KeyType::Set => "T",    // seT
            KeyType::Stream => "M", // streaM
            KeyType::Vectorset => "V",
            KeyType::Unknown => "",
        }
    }
}

fn unix_ts() -> u64 {
    if let Ok(value) = SystemTime::now().duration_since(UNIX_EPOCH) {
        value.as_secs()
    } else {
        0
    }
}

// KeyNode is a node in the key tree.
#[derive(Debug, Default)]
struct KeyNode {
    /// full path (e.g. "dir1:dir2")
    full_path: String,

    /// is this node a real key?
    is_key: bool,

    /// children nodes (key is short name, e.g. "dir2")
    children: AHashMap<String, KeyNode>,
}

impl KeyNode {
    /// create a new child node
    fn new(full_path: String) -> Self {
        Self {
            full_path,
            is_key: false,
            children: AHashMap::new(),
        }
    }

    /// recursively insert a key (by parts) into this node.
    /// 'self' is the parent node (e.g. "dir1")
    /// 'mut parts' is the remaining parts (e.g. ["dir2", "name"])
    fn insert(&mut self, mut parts: std::str::Split<'_, &str>) {
        let Some(part_name) = parts.next() else {
            self.is_key = true;
            return;
        };

        let child_full_path = if self.full_path.is_empty() {
            part_name.to_string()
        } else {
            format!("{}:{}", self.full_path, part_name)
        };

        let child_node = self
            .children
            .entry(part_name.to_string()) // Key in map is short name
            .or_insert_with(|| KeyNode::new(child_full_path));

        child_node.insert(parts);
    }
}

#[derive(Debug, Clone, Default)]
pub struct RedisValue {
    key_type: KeyType,
    data: Option<String>,
    expire_at: Option<u64>,
    size: usize,
}

impl RedisValue {
    pub fn data(&self) -> Option<&String> {
        self.data.as_ref()
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn ttl(&self) -> Option<Duration> {
        self.expire_at
            .map(|expire_at| Duration::from_secs(expire_at - unix_ts()))
    }
}

impl From<&str> for KeyType {
    fn from(value: &str) -> Self {
        match value {
            "list" => KeyType::List,
            "set" => KeyType::Set,
            "zset" => KeyType::Zset,
            "hash" => KeyType::Hash,
            "stream" => KeyType::Stream,
            "vectorset" => KeyType::Vectorset,
            "string" => KeyType::String,
            _ => KeyType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    server: String,
    dbsize: Option<u64>,
    latency: Option<Duration>,
    servers: Option<Vec<RedisServer>>,
    key: Option<String>,
    value: Option<RedisValue>,
    // scan
    keyword: String,
    cursors: Option<Vec<u64>>,
    scaning: bool,
    scan_completed: bool,
    scan_times: usize,
    key_tree_id: String,
    loaded_prefixes: AHashSet<String>,
    keys: AHashMap<String, KeyType>,
}

impl ZedisServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            ..Default::default()
        }
    }
    fn reset_scan(&mut self) {
        self.keyword = "".to_string();
        self.cursors = None;
        self.keys.clear();
        self.key_tree_id = Uuid::now_v7().to_string();
        self.scaning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        self.loaded_prefixes.clear();
    }
    fn reset(&mut self) {
        self.server = "".to_string();
        self.dbsize = None;
        self.latency = None;
        self.key = None;
        self.reset_scan();
    }
    fn extend_keys(&mut self, keys: Vec<String>) {
        self.keys.reserve(keys.len());
        let mut insert_count = 0;
        for key in keys {
            self.keys.entry(key).or_insert_with(|| {
                insert_count += 1;
                KeyType::Unknown
            });
        }
        if insert_count != 0 {
            self.key_tree_id = Uuid::now_v7().to_string();
        }
    }
    pub fn key_type(&self, key: &str) -> Option<&KeyType> {
        self.keys.get(key)
    }
    pub fn key_tree_id(&self) -> &str {
        &self.key_tree_id
    }
    pub fn key_tree(&self, expanded_items: &AHashSet<String>) -> Vec<TreeItem> {
        let keys = self.keys.keys();
        let mut root_trie_node = KeyNode {
            full_path: "".to_string(),
            is_key: false,
            children: AHashMap::new(),
        };

        for key in keys {
            root_trie_node.insert(key.split(":"));
        }

        fn convert_map_to_vec_tree(
            children_map: &AHashMap<String, KeyNode>,
            expanded_items: &AHashSet<String>,
        ) -> Vec<TreeItem> {
            let mut children_vec = Vec::new();

            for (short_name, internal_node) in children_map {
                let mut node = TreeItem::new(internal_node.full_path.clone(), short_name.clone());
                if expanded_items.contains(&internal_node.full_path) {
                    node = node.expanded(true);
                }
                let node = node.children(convert_map_to_vec_tree(
                    &internal_node.children,
                    expanded_items,
                ));
                children_vec.push(node);
            }

            children_vec.sort_unstable_by(|a, b| {
                let a_is_dir = !a.children.is_empty();
                let b_is_dir = !b.children.is_empty();

                let type_ordering = a_is_dir.cmp(&b_is_dir).reverse();

                type_ordering.then_with(|| a.id.cmp(&b.id))
            });

            children_vec
        }

        convert_map_to_vec_tree(&root_trie_node.children, expanded_items)
    }
    pub fn scan_completed(&self) -> bool {
        self.scan_completed
    }
    pub fn scaning(&self) -> bool {
        self.scaning
    }
    pub fn dbsize(&self) -> Option<u64> {
        self.dbsize
    }
    pub fn scan_count(&self) -> usize {
        self.keys.len()
    }
    pub fn latency(&self) -> Option<Duration> {
        self.latency
    }
    pub fn server(&self) -> &str {
        &self.server
    }
    pub fn servers(&self) -> Option<&[RedisServer]> {
        self.servers.as_deref()
    }
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
    }
    pub fn remove_server(&mut self, server: &str, cx: &mut Context<Self>) {
        let mut servers = self.servers.clone().unwrap_or_default();
        servers.retain(|s| s.name != server);
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                save_servers(servers.clone())?;

                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            match result {
                Ok(servers) => handle.update(cx, |this, cx| {
                    this.servers = Some(servers);
                    cx.notify();
                }),
                Err(e) => {
                    // TODO
                    println!("error: {e:?}");
                    Ok(())
                }
            }
        })
        .detach();
    }
    pub fn update_or_insrt_server(&mut self, cx: &mut Context<Self>, mut server: RedisServer) {
        let mut servers = self.servers.clone().unwrap_or_default();
        server.updated_at = Some(Local::now().to_rfc3339());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if let Some(existing_server) = servers.iter_mut().find(|s| s.name == server.name) {
                    *existing_server = server;
                } else {
                    servers.push(server);
                }
                save_servers(servers.clone())?;

                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            match result {
                Ok(servers) => handle.update(cx, |this, cx| {
                    this.servers = Some(servers);
                    cx.notify();
                }),
                Err(e) => {
                    // TODO
                    println!("error: {e:?}");
                    Ok(())
                }
            }
        })
        .detach();
    }
    pub fn fetch_servers(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let servers = get_servers()?;
                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(servers) => {
                        this.servers = Some(servers);
                    }
                    Err(e) => {
                        println!("error: {e:?}");
                    }
                };
                cx.notify();
            })
        })
        .detach();
    }
    fn fill_key_types(&mut self, cx: &mut Context<Self>, prefix: String) {
        let mut keys = self
            .keys
            .iter()
            .filter_map(|(key, value)| {
                if *value != KeyType::Unknown {
                    return None;
                }
                let suffix = key.strip_prefix(&prefix)?;
                if suffix.contains(":") {
                    return None;
                }
                Some(key.clone())
            })
            .collect::<Vec<String>>();
        if keys.is_empty() {
            return;
        }
        let server = self.server.clone();
        keys.sort_unstable();
        let keys_clone = keys.clone();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let mut conn = client.get_connection()?;
                let mut cmd = pipe();
                for key in keys.iter().take(1000) {
                    cmd.cmd("TYPE").arg(key);
                }
                let types: Vec<String> = cmd.query(&mut conn)?;
                Ok(types)
            });
            let result: Result<Vec<String>> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(types) => {
                        for (index, t) in types.iter().enumerate() {
                            let Some(key) = keys_clone.get(index) else {
                                continue;
                            };
                            this.keys
                                .get_mut(key)
                                .map(|k| *k = KeyType::from(t.as_str()));
                        }
                        this.key_tree_id = Uuid::now_v7().to_string();
                    }
                    Err(e) => {
                        // TODO 出错的处理
                        error!(error = %e, "fill key types fail");
                    }
                }
                cx.notify();
            })
        })
        .detach();
    }
    fn scan_keys(&mut self, cx: &mut Context<Self>, server: String, keyword: String) {
        if self.server != server || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        let max = (self.scan_times + 1) * DEFAULT_SCAN_RESULT_MAX;
        cx.spawn(async move |handle, cx| {
            let processing_server = server.clone();
            let processing_keyword = keyword.clone();
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let pattern = format!("*{}*", keyword);
                let count = if keyword.is_empty() { 2_000 } else { 10_000 };
                if let Some(cursors) = cursors {
                    client.scan(cursors, &pattern, count)
                } else {
                    client.first_scan(&pattern, count)
                }
            });
            let result = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok((cursors, keys)) => {
                        debug!("cursors: {cursors:?}, keys count: {}", keys.len());
                        if cursors.iter().sum::<u64>() == 0 {
                            this.scan_completed = true;
                            this.cursors = None;
                        } else {
                            this.cursors = Some(cursors);
                        }
                        this.extend_keys(keys);
                    }
                    Err(e) => {
                        // TODO 出错的处理
                        println!("error: {e:?}");
                        // this.error = Some(e.to_string());
                        this.cursors = None;
                    }
                };
                if this.cursors.is_some() && this.keys.len() < max {
                    // run again
                    this.scan_keys(cx, processing_server, processing_keyword);
                    return cx.notify();
                }
                this.scaning = false;
                cx.notify();
                this.fill_key_types(cx, "".to_string());
            })
        })
        .detach();
    }
    pub fn scan(&mut self, cx: &mut Context<Self>, keyword: String) {
        self.reset_scan();
        self.scaning = true;
        self.keyword = keyword.clone();
        cx.notify();
        self.scan_keys(cx, self.server.clone(), keyword);
    }
    pub fn scan_next(&mut self, cx: &mut Context<Self>) {
        if self.scan_completed {
            return;
        }
        self.scan_times += 1;
        self.scan_keys(cx, self.server.clone(), self.keyword.clone());
        cx.notify();
    }
    pub fn scan_prefix(&mut self, cx: &mut Context<Self>, prefix: String) {
        if self.loaded_prefixes.contains(&prefix) || self.scan_completed {
            return;
        }
        let server = self.server.clone();
        cx.spawn(async move |handle, cx| {
            let pattern = format!("{}*", prefix);
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let count = 10_000;
                // let mut cursors: Option<Vec<u64>>,
                let mut cursors: Option<Vec<u64>> = None;
                let mut result_keys = vec![];
                // 最多执行x次
                for _ in 0..20 {
                    let (new_cursor, keys) = if let Some(cursors) = cursors.clone() {
                        client.scan(cursors, &pattern, count)?
                    } else {
                        client.first_scan(&pattern, count)?
                    };
                    result_keys.extend(keys);
                    if new_cursor.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursor);
                }

                Ok(result_keys)
            });
            let result: Result<Vec<String>> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(keys) => {
                        debug!(prefix, count = keys.len(), "scan prefix success");
                        this.loaded_prefixes.insert(prefix.clone());
                        this.extend_keys(keys);
                    }
                    Err(e) => {
                        error!(err = %e, "scan prefix fail");
                    }
                };
                cx.notify();
                this.fill_key_types(cx, prefix);
            })
        })
        .detach();
        cx.notify();
    }
    pub fn select(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.server != server {
            self.reset();
            self.server = server.to_string();
            debug!(server = self.server, "select server");
            cx.notify();
            if self.server.is_empty() {
                return;
            }
            self.scaning = true;
            cx.notify();
            let server_clone = server.to_string();
            cx.spawn(async move |handle, cx| {
                let counting_server = server_clone.clone();
                let task = cx.background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_clone)?;
                    let dbsize = client.dbsize()?;
                    let start = Instant::now();
                    client.ping()?;
                    Ok((dbsize, start.elapsed()))
                });
                let result: Result<(u64, Duration)> = task.await;
                handle.update(cx, move |this, cx| {
                    if this.server != counting_server {
                        return;
                    }
                    match result {
                        Ok((dbsize, latency)) => {
                            this.latency = Some(latency);
                            this.dbsize = Some(dbsize);
                        }
                        Err(e) => {
                            // TODO 出错的处理
                            error!(error = %e, "get redis info fail");
                            this.dbsize = None;
                            this.latency = None;
                        }
                    };
                    let server = this.server.clone();
                    cx.notify();
                    this.scan_keys(cx, server, "".to_string());
                })
            })
            .detach();
        }
    }
    pub fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        if self.key.clone().unwrap_or_default() != key {
            self.key = Some(key.clone());
            cx.notify();
            if key.is_empty() {
                return;
            }
            let server = self.server.clone();
            cx.spawn(async move |handle, cx| {
                // TODO判断key的类型
                let task = cx.background_spawn(async move {
                    let client = get_connection_manager().get_client(&server)?;
                    let mut conn = client.get_connection()?;
                    let t: String = cmd("TYPE").arg(&key).query(&mut conn)?;
                    let mut redis_value = RedisValue {
                        key_type: KeyType::from(t.as_str()),
                        ..Default::default()
                    };
                    // TODO 根据类型选择对应的函数
                    let (value, ttl): (Vec<u8>, i64) =
                        pipe().get(&key).ttl(&key).query(&mut conn)?;
                    if ttl >= 0 {
                        redis_value.expire_at = Some(unix_ts() + ttl as u64);
                    }
                    redis_value.size = value.len();
                    if value.is_empty() {
                        return Ok(redis_value);
                    }
                    if let Ok(value) = std::str::from_utf8(&value) {
                        if let Ok(value) = serde_json::from_str::<Value>(value)
                            && let Ok(pretty_value) = serde_json::to_string_pretty(&value)
                        {
                            redis_value.data = Some(pretty_value);
                        } else {
                            redis_value.data = Some(value.to_string());
                        }
                    } else {
                        // TODO 根据窗口宽度使用width:16/32
                        let cfg = HexConfig {
                            title: false,
                            width: 32,
                            group: 0,
                            ..HexConfig::default()
                        };

                        redis_value.data = Some(config_hex(&value, cfg));
                    }
                    Ok(redis_value)
                });
                let result: Result<RedisValue, Error> = task.await;
                handle.update(cx, move |this, cx| {
                    match result {
                        Ok(value) => {
                            this.value = Some(value);
                        }
                        Err(e) => {
                            // TODO 出错的处理
                            this.value = None;
                            error!(error = %e, "get redis info fail");
                        }
                    };
                    cx.notify();
                })
            })
            .detach();
        }
    }
    pub fn delete_key(&mut self, key: String, cx: &mut Context<Self>) {
        let server = self.server.clone();
        cx.spawn(async move |handle, cx| {
            let remove_key = key.clone();
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let mut conn = client.get_connection()?;
                let _: () = cmd("DEL").arg(&key).query(&mut conn)?;
                Ok(())
            });
            let result: Result<(), Error> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(()) => {
                        this.keys.remove(&remove_key);
                        this.key_tree_id = Uuid::now_v7().to_string();
                        this.key = None;
                    }
                    Err(e) => {
                        // TODO 出错的处理
                        error!(error = %e, "delete key fail");
                    }
                };
                cx.notify();
            })
        })
        .detach();
    }
}

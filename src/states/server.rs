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
use crate::connection::save_servers;
use crate::error::Error;
use crate::helpers::unix_ts;
use ahash::AHashMap;
use ahash::AHashSet;
use chrono::Local;
use gpui::SharedString;
use gpui::prelude::*;
use gpui_component::tree::TreeItem;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tracing::debug;
use tracing::error;
use uuid::Uuid;
use value::{KeyType, RedisValue, RedisValueData};

pub mod key;
pub mod list;
pub mod string;
pub mod value;

type Result<T, E = Error> = std::result::Result<T, E>;

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

#[derive(Debug, Clone)]
pub struct ErrorMessage {
    pub category: SharedString,
    pub message: SharedString,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    server: SharedString,
    dbsize: Option<u64>,
    latency: Option<Duration>,
    servers: Option<Vec<RedisServer>>,
    key: Option<SharedString>,
    value: Option<RedisValue>,
    updating: bool,
    deleting: bool,
    // scan
    keyword: SharedString,
    cursors: Option<Vec<u64>>,
    scaning: bool,
    scan_completed: bool,
    scan_times: usize,
    key_tree_id: SharedString,
    loaded_prefixes: AHashSet<SharedString>,
    keys: AHashMap<SharedString, KeyType>,

    last_operated_at: i64,
    // error
    error_messages: Arc<RwLock<Vec<ErrorMessage>>>,
}

impl ZedisServerState {
    pub fn new() -> Self {
        Self::default()
    }
    fn reset_scan(&mut self) {
        self.keyword = "".into();
        self.cursors = None;
        self.keys.clear();
        self.key_tree_id = Uuid::now_v7().to_string().into();
        self.scaning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        self.loaded_prefixes.clear();
    }
    fn reset(&mut self) {
        self.server = "".into();
        self.dbsize = None;
        self.latency = None;
        self.key = None;
        self.reset_scan();
    }
    fn extend_keys(&mut self, keys: Vec<String>) {
        self.keys.reserve(keys.len());
        let mut insert_count = 0;
        for key in keys {
            self.keys.entry(key.into()).or_insert_with(|| {
                insert_count += 1;
                KeyType::Unknown
            });
        }
        if insert_count != 0 {
            self.key_tree_id = Uuid::now_v7().to_string().into();
        }
    }
    fn add_error_message(&mut self, category: String, message: String) {
        let mut guard = self.error_messages.write();
        if guard.len() >= 10 {
            guard.remove(0);
        }
        guard.push(ErrorMessage {
            category: category.into(),
            message: message.into(),
            created_at: unix_ts(),
        });
    }
    pub fn get_error_message(&self) -> Option<ErrorMessage> {
        if let Some(last) = self.error_messages.read().last()
            && last.created_at >= self.last_operated_at
        {
            return Some(last.clone());
        }
        None
    }
    fn spawn<T, Fut>(
        &mut self,
        cx: &mut Context<Self>,
        task_name: &str,
        task: impl FnOnce() -> Fut + Send + 'static,
        callback: impl FnOnce(&mut Self, Result<T>, &mut Context<Self>) + Send + 'static,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let name = task_name.to_string();
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;
            handle.update(cx, move |this, cx| {
                if let Err(e) = &result {
                    // TODO 出错的处理
                    let message = format!("{name} fail");
                    error!(error = %e, message);
                    this.add_error_message(name, e.to_string());
                }
                callback(this, result, cx);
            })
        })
        .detach();
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
    pub fn updating(&self) -> bool {
        self.updating
    }
    pub fn deleting(&self) -> bool {
        self.deleting
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
    pub fn set_servers(&mut self, servers: Vec<RedisServer>) {
        self.servers = Some(servers);
    }
    pub fn servers(&self) -> Option<&[RedisServer]> {
        self.servers.as_deref()
    }
    pub fn key(&self) -> Option<SharedString> {
        self.key.clone()
    }
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
    }
    pub fn value_key_type(&self) -> Option<KeyType> {
        self.value.as_ref().map(|value| value.key_type())
    }
    pub fn remove_server(&mut self, server: &str, cx: &mut Context<Self>) {
        let mut servers = self.servers.clone().unwrap_or_default();
        servers.retain(|s| s.name != server);
        self.last_operated_at = unix_ts();
        self.spawn(
            cx,
            "remove_server",
            move || async move {
                save_servers(servers.clone()).await?;
                Ok(servers)
            },
            move |this, result, cx| {
                if let Ok(servers) = result {
                    this.servers = Some(servers);
                }
                cx.notify();
            },
        );
    }
    pub fn update_or_insrt_server(&mut self, cx: &mut Context<Self>, mut server: RedisServer) {
        let mut servers = self.servers.clone().unwrap_or_default();
        server.updated_at = Some(Local::now().to_rfc3339());
        self.last_operated_at = unix_ts();
        self.spawn(
            cx,
            "update_or_insert_server",
            move || async move {
                if let Some(existing_server) = servers.iter_mut().find(|s| s.name == server.name) {
                    *existing_server = server;
                } else {
                    servers.push(server);
                }
                save_servers(servers.clone()).await?;

                Ok(servers)
            },
            move |this, result, cx| {
                if let Ok(servers) = result {
                    this.servers = Some(servers);
                }
                cx.notify();
            },
        );
    }

    pub fn ping(&mut self, cx: &mut Context<Self>) {
        if self.server.is_empty() {
            return;
        }
        let server = self.server.clone();
        self.spawn(
            cx,
            "ping",
            move || async move {
                let client = get_connection_manager().get_client(&server).await?;
                let start = Instant::now();
                client.ping().await?;
                Ok(start.elapsed())
            },
            move |this, result, cx| {
                if let Ok(latency) = result {
                    this.latency = Some(latency);
                };
                cx.notify();
            },
        );
    }
    pub fn select(&mut self, server: SharedString, cx: &mut Context<Self>) {
        if self.server != server {
            self.reset();
            self.server = server;
            debug!(server = self.server.as_str(), "select server");
            cx.notify();
            if self.server.is_empty() {
                return;
            }
            self.scaning = true;
            cx.notify();
            let server_clone = self.server.clone();
            self.last_operated_at = unix_ts();
            let counting_server = self.server.clone();
            self.spawn(
                cx,
                "select_server",
                move || async move {
                    let client = get_connection_manager().get_client(&server_clone).await?;
                    let dbsize = client.dbsize().await?;
                    let start = Instant::now();
                    client.ping().await?;
                    Ok((dbsize, start.elapsed()))
                },
                move |this, result, cx| {
                    if this.server != counting_server {
                        return;
                    }
                    if let Ok((dbsize, latency)) = result {
                        this.latency = Some(latency);
                        this.dbsize = Some(dbsize);
                    };
                    let server = this.server.clone();
                    cx.notify();
                    this.scan_keys(server, "".into(), cx);
                },
            );
        }
    }
}

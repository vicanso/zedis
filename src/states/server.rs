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
use crate::states::QueryMode;
use ahash::AHashMap;
use ahash::AHashSet;
use chrono::Local;
use gpui::EventEmitter;
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

#[derive(Clone, PartialEq, Default, Debug)]
pub enum RedisServerStatus {
    #[default]
    Idle,
    Loading,
}

#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    server: SharedString,
    query_mode: QueryMode,
    server_status: RedisServerStatus,
    dbsize: Option<u64>,
    nodes: (usize, usize),
    version: SharedString,
    latency: Option<Duration>,
    servers: Option<Vec<RedisServer>>,
    key: Option<SharedString>,
    value: Option<RedisValue>,
    // scan
    keyword: SharedString,
    cursors: Option<Vec<u64>>,
    scaning: bool,
    scan_completed: bool,
    scan_times: usize,
    key_tree_id: SharedString,
    loaded_prefixes: AHashSet<SharedString>,
    keys: AHashMap<SharedString, KeyType>,

    // error
    error_messages: Arc<RwLock<Vec<ErrorMessage>>>,
}

pub enum ServerEvent {
    Spawn(SharedString),
    TaskFinish(SharedString),
    Selectkey(SharedString),
    ValueUpdated(SharedString),
    SelectServer(SharedString),
    ServerUpdated(SharedString),
    ScanStart(SharedString),
    ScanNext(SharedString),
    ScanFinish(SharedString),
    Error(ErrorMessage),
    Heartbeat(Duration),
}

impl EventEmitter<ServerEvent> for ZedisServerState {}

impl ZedisServerState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset_scan(&mut self) {
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
        self.version = "".into();
        self.nodes = (0, 0);
        self.dbsize = None;
        self.latency = None;
        self.key = None;
        self.reset_scan();
    }
    fn extend_keys(&mut self, keys: Vec<SharedString>) {
        self.keys.reserve(keys.len());
        let mut insert_count = 0;
        for key in keys {
            self.keys.entry(key).or_insert_with(|| {
                insert_count += 1;
                KeyType::Unknown
            });
        }
        if insert_count != 0 {
            self.key_tree_id = Uuid::now_v7().to_string().into();
        }
    }
    fn add_error_message(&mut self, category: String, message: String, cx: &mut Context<Self>) {
        let mut guard = self.error_messages.write();
        if guard.len() >= 10 {
            guard.remove(0);
        }
        let info = ErrorMessage {
            category: category.into(),
            message: message.into(),
            created_at: unix_ts(),
        };
        guard.push(info.clone());
        cx.emit(ServerEvent::Error(info));
    }
    fn spawn<T, Fut>(
        &mut self,
        task_name: &str,
        task: impl FnOnce() -> Fut + Send + 'static,
        callback: impl FnOnce(&mut Self, Result<T>, &mut Context<Self>) + Send + 'static,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let name = task_name.to_string();
        cx.emit(ServerEvent::Spawn(name.clone().into()));
        debug!(name, "spawn task");
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;
            handle.update(cx, move |this, cx| {
                if let Err(e) = &result {
                    // TODO 出错的处理
                    let message = format!("{name} fail");
                    error!(error = %e, message);
                    this.add_error_message(name, e.to_string(), cx);
                }
                callback(this, result, cx);
            })
        })
        .detach();
    }
    pub fn is_busy(&self) -> bool {
        !matches!(self.server_status, RedisServerStatus::Idle)
    }
    pub fn key_type(&self, key: &str) -> Option<&KeyType> {
        self.keys.get(key)
    }
    pub fn key_tree_id(&self) -> &str {
        &self.key_tree_id
    }
    pub fn set_query_mode(&mut self, mode: QueryMode) {
        self.query_mode = mode;
    }
    pub fn key_tree(&self, expanded_items: &AHashSet<String>, expand_all: bool) -> Vec<TreeItem> {
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
            expand_all: bool,
        ) -> Vec<TreeItem> {
            let mut children_vec = Vec::new();

            for (short_name, internal_node) in children_map {
                let mut node = TreeItem::new(internal_node.full_path.clone(), short_name.clone());
                if expand_all || expanded_items.contains(&internal_node.full_path) {
                    node = node.expanded(true);
                }
                let node = node.children(convert_map_to_vec_tree(
                    &internal_node.children,
                    expanded_items,
                    expand_all,
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

        convert_map_to_vec_tree(&root_trie_node.children, expanded_items, expand_all)
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
    pub fn nodes(&self) -> (usize, usize) {
        self.nodes
    }
    pub fn version(&self) -> &str {
        &self.version
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
        self.spawn(
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
            cx,
        );
    }
    pub fn update_or_insrt_server(
        &mut self,
        mut server: RedisServer,
        is_new: bool,
        cx: &mut Context<Self>,
    ) {
        let mut servers = self.servers.clone().unwrap_or_default();
        server.updated_at = Some(Local::now().to_rfc3339());
        self.spawn(
            "update_or_insert_server",
            move || async move {
                if let Some(existing_server) = servers.iter_mut().find(|s| s.name == server.name) {
                    if is_new {
                        return Err(Error::Invalid {
                            message: "server already exists".to_string(),
                        });
                    }
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
            cx,
        );
    }

    pub fn ping(&mut self, cx: &mut Context<Self>) {
        if self.server.is_empty() {
            return;
        }
        let server = self.server.clone();
        self.spawn(
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
                    cx.emit(ServerEvent::Heartbeat(latency));
                };
            },
            cx,
        );
    }
    pub fn select(&mut self, server: SharedString, mode: QueryMode, cx: &mut Context<Self>) {
        if self.server != server {
            self.reset();
            self.server = server.clone();
            self.query_mode = mode;
            debug!(server = self.server.as_str(), "select server");
            cx.emit(ServerEvent::SelectServer(server));
            cx.notify();
            if self.server.is_empty() {
                return;
            }
            self.server_status = RedisServerStatus::Loading;
            self.scaning = true;
            cx.notify();
            let server_clone = self.server.clone();
            let counting_server = server_clone.clone();
            self.spawn(
                "select_server",
                move || async move {
                    let client = get_connection_manager().get_client(&server_clone).await?;
                    let dbsize = client.dbsize().await?;
                    let start = Instant::now();
                    let version = client.version().to_string();
                    client.ping().await?;
                    Ok((dbsize, start.elapsed(), client.nodes(), version))
                },
                move |this, result, cx| {
                    if this.server != counting_server {
                        return;
                    }
                    if let Ok((dbsize, latency, nodes, version)) = result {
                        this.latency = Some(latency);
                        this.dbsize = Some(dbsize);
                        this.nodes = nodes;
                        this.version = version.into();
                    };
                    let server = this.server.clone();
                    this.server_status = RedisServerStatus::Idle;
                    cx.emit(ServerEvent::ServerUpdated(server.clone()));
                    cx.notify();
                    if this.query_mode == QueryMode::All {
                        this.scan_keys(server, "".into(), cx);
                    } else {
                        this.scaning = false;
                        cx.notify();
                    }
                },
                cx,
            );
        }
    }
}

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

use crate::connection::QueryMode;
use crate::connection::RedisServer;
use crate::connection::get_connection_manager;
use crate::connection::save_servers;
use crate::error::Error;
use crate::helpers::unix_ts;
use ahash::AHashMap;
use ahash::AHashSet;
use chrono::Local;
use gpui::EventEmitter;
use gpui::SharedString;
use gpui::prelude::*;
use gpui_component::tree::TreeItem;
use parking_lot::RwLock;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tracing::debug;
use tracing::error;
use uuid::Uuid;
use value::{KeyType, RedisValue, RedisValueData};

pub mod key;
pub mod list;
pub mod set;
pub mod string;
pub mod value;

type Result<T, E = Error> = std::result::Result<T, E>;

// Constants for state management
const MAX_ERROR_MESSAGES: usize = 10; // Maximum error messages to keep in memory
const KEY_SEPARATOR: &str = ":"; // Redis key namespace separator

/// Node in the hierarchical key tree structure
///
/// Uses a trie-like structure to organize Redis keys by their colon-separated
/// namespaces. For example, "user:123:profile" becomes a tree:
/// - user (folder)
///   - 123 (folder)
///     - profile (key)
#[derive(Debug, Default)]
struct KeyNode {
    /// Full path from root (e.g., "dir1:dir2")
    full_path: SharedString,

    /// Whether this node represents an actual Redis key (vs just a namespace folder)
    is_key: bool,

    /// Child nodes (map key is the short name, e.g., "dir2")
    children: AHashMap<SharedString, KeyNode>,
}

impl KeyNode {
    /// Create a new child node with the given full path
    fn new(full_path: SharedString) -> Self {
        Self {
            full_path,
            is_key: false,
            children: AHashMap::new(),
        }
    }

    /// Recursively insert a key (by parts) into the tree
    ///
    /// # Arguments
    /// * `parts` - Iterator of remaining path parts (e.g., ["dir2", "name"])
    ///
    /// # Example
    /// For key "user:123:profile" split by ":", inserts:
    /// - "user" as folder -> "123" as folder -> "profile" as key
    fn insert(&mut self, mut parts: std::str::Split<'_, &str>) {
        // If no more parts, this node is a terminal key
        let Some(part_name) = parts.next() else {
            self.is_key = true;
            return;
        };

        // Build full path for the child node
        let child_full_path = if self.full_path.is_empty() {
            part_name.to_string().into()
        } else {
            format!("{}:{}", self.full_path, part_name).into()
        };

        // Get or create child node and continue insertion
        let child_node = self
            .children
            .entry(part_name.to_string().into())
            .or_insert_with(|| KeyNode::new(child_full_path));

        child_node.insert(parts);
    }
}

/// Error message with categorization and timestamp
#[derive(Debug, Clone)]
pub struct ErrorMessage {
    /// Category of error (e.g., task name like "ping", "scan_keys")
    pub category: SharedString,

    /// Human-readable error message
    pub message: SharedString,

    /// Unix timestamp when error occurred
    pub created_at: i64,
}

/// Redis server connection status
#[derive(Clone, PartialEq, Default, Debug)]
pub enum RedisServerStatus {
    /// Server is idle and ready for operations
    #[default]
    Idle,

    /// Server is loading initial data (connecting, fetching metadata)
    Loading,
}

/// Main state management for Redis server operations
///
/// This struct manages:
/// - Server connection and metadata (version, latency, dbsize)
/// - Key scanning and tree structure
/// - Selected key and its value
/// - Error message history
/// - Async task spawning and coordination
#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    /// Currently selected server id
    server_id: SharedString,

    /// Query mode (All/Prefix/Exact) for key filtering
    query_mode: QueryMode,

    /// Whether to soft wrap the editor
    soft_wrap: bool,

    /// Current server status
    server_status: RedisServerStatus,

    /// Total number of keys in the database (from DBSIZE command)
    dbsize: Option<u64>,

    /// Number of Redis nodes (master, replica) for cluster info
    nodes: (usize, usize),

    /// Redis server version string
    version: SharedString,

    /// Last measured latency to server
    latency: Option<Duration>,

    /// List of all configured servers
    servers: Option<Vec<RedisServer>>,

    /// Currently selected key name
    key: Option<SharedString>,

    /// Value data for the currently selected key
    value: Option<RedisValue>,

    // ===== Key scanning state =====
    /// Search keyword for filtering keys
    keyword: SharedString,

    /// SCAN cursors for cluster nodes (one per node)
    cursors: Option<Vec<u64>>,

    /// Whether a scan operation is in progress
    scaning: bool,

    /// Whether the current scan has completed
    scan_completed: bool,

    /// Number of scan iterations performed
    scan_times: usize,

    /// Unique ID for current key tree (changes when keys are reloaded)
    key_tree_id: SharedString,

    /// Set of prefixes that have been scanned (for lazy loading folders)
    loaded_prefixes: AHashSet<SharedString>,

    /// Map of all loaded keys and their types
    keys: AHashMap<SharedString, KeyType>,

    // ===== Error tracking =====
    /// Recent error messages (limited to MAX_ERROR_MESSAGES)
    error_messages: Arc<RwLock<Vec<ErrorMessage>>>,
}

/// Background task types for Redis operations
///
/// Each variant represents a specific async operation that runs in the background
#[derive(Clone, PartialEq, Debug)]
pub enum ServerTask {
    /// Health check - ping the Redis server
    Ping,

    /// Connect to and load metadata from a server
    SelectServer,

    /// Remove a server from configuration
    RemoveServer,

    /// Update the server query mode
    UpdateServerQueryMode,

    /// Update the server soft wrap
    UpdateServerSoftWrap,

    /// Add new server or update existing server configuration
    UpdateOrInsertServer,

    /// Fill in key types for unknown keys
    FillKeyTypes,

    /// Load value data for a selected key
    Selectkey,

    /// Delete a key from Redis
    DeleteKey,

    /// Scan for keys matching pattern
    ScanKeys,

    /// Scan keys with a specific prefix (for lazy folder loading)
    ScanPrefix,

    /// Update TTL (time-to-live) for a key
    UpdateKeyTtl,

    /// Delete an item from a list
    DeleteListItem,

    /// Update a value in a list
    UpdateListValue,

    /// Push a value to a list
    PushListValue,

    /// Add a value to a set
    AddSetValue,

    /// Load more items
    LoadMoreValue,

    /// Save edited value back to Redis
    SaveValue,
}

impl ServerTask {
    /// Get string representation of task (for logging and error messages)
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerTask::Ping => "ping",
            ServerTask::SelectServer => "select_server",
            ServerTask::RemoveServer => "remove_server",
            ServerTask::UpdateOrInsertServer => "update_or_insert_server",
            ServerTask::FillKeyTypes => "fill_key_types",
            ServerTask::Selectkey => "select_key",
            ServerTask::DeleteKey => "delete_key",
            ServerTask::ScanKeys => "scan_keys",
            ServerTask::ScanPrefix => "scan_prefix",
            ServerTask::UpdateKeyTtl => "update_key_ttl",
            ServerTask::DeleteListItem => "delete_list_item",
            ServerTask::UpdateListValue => "update_list_value",
            ServerTask::LoadMoreValue => "load_more_value",
            ServerTask::SaveValue => "save_value",
            ServerTask::UpdateServerQueryMode => "update_server_query_mode",
            ServerTask::UpdateServerSoftWrap => "update_server_soft_wrap",
            ServerTask::PushListValue => "push_list_value",
            ServerTask::AddSetValue => "add_set_value",
        }
    }
}

/// Events emitted by server state for reactive UI updates
pub enum ServerEvent {
    /// A new background task has started.
    TaskStarted(ServerTask),
    /// A background task has completed.
    TaskFinished(SharedString),

    /// A key has been selected for viewing/editing
    KeySelected(SharedString),
    /// Key scan operation has started
    KeyScanStarted(SharedString),
    /// Key scan found a new batch of keys.
    KeyScanPaged(SharedString),
    /// Key scan operation has fully completed.
    KeyScanFinished(SharedString),

    /// A key's value has been fetched (initial load).
    ValueLoaded(SharedString),
    /// A key's value has been updated
    ValueUpdated(SharedString),
    /// Load more value
    ValuePaginationStarted(SharedString),
    /// Load more value
    ValuePaginationFinished(SharedString),
    /// Add a value to a set、list、hash、zset
    ValueAdded(SharedString),

    /// User selected a different server
    ServerSelected(SharedString),
    /// Server list config has been modified (add/remove/edit).
    ServerListUpdated,
    /// Server metadata (info/dbsize) has been refreshed.
    ServerInfoUpdated(SharedString),
    /// Periodic heartbeat received with latency.
    HeartbeatReceived(Duration),

    /// Soft wrap changed
    SoftWrapToggled(bool),
    /// An error occurred.
    ErrorOccurred(ErrorMessage),
}

impl EventEmitter<ServerEvent> for ZedisServerState {}

impl ZedisServerState {
    /// Create a new server state instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all scan-related state (clears keys, cursors, etc.)
    ///
    /// Called when switching servers or starting a new scan
    pub fn reset_scan(&mut self) {
        self.keyword = SharedString::default();
        self.cursors = None;
        self.keys.clear();
        self.key_tree_id = Uuid::now_v7().to_string().into();
        self.scaning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        self.loaded_prefixes.clear();
    }

    /// Reset all state when switching to a different server
    fn reset(&mut self) {
        self.server_id = SharedString::default();
        self.version = SharedString::default();
        self.nodes = (0, 0);
        self.dbsize = None;
        self.latency = None;
        self.key = None;
        self.reset_scan();
    }

    /// Add new keys to the key map (deduplicating automatically)
    ///
    /// If any new keys were added, generates a new tree ID to trigger UI refresh
    fn extend_keys(&mut self, keys: Vec<SharedString>) {
        self.keys.reserve(keys.len());
        let mut insert_count = 0;

        for key in keys {
            self.keys.entry(key).or_insert_with(|| {
                insert_count += 1;
                KeyType::Unknown
            });
        }

        // Update tree ID only if new keys were added
        if insert_count != 0 {
            self.key_tree_id = Uuid::now_v7().to_string().into();
        }
    }

    /// Add an error message to the history and emit error event
    ///
    /// Maintains a rolling window of MAX_ERROR_MESSAGES most recent errors
    fn add_error_message(&mut self, category: String, message: String, cx: &mut Context<Self>) {
        let mut guard = self.error_messages.write();

        // Remove oldest error if at capacity
        if guard.len() >= MAX_ERROR_MESSAGES {
            guard.remove(0);
        }

        let info = ErrorMessage {
            category: category.into(),
            message: message.into(),
            created_at: unix_ts(),
        };
        guard.push(info.clone());
        cx.emit(ServerEvent::ErrorOccurred(info));
    }
    /// Spawn an async background task with error handling
    ///
    /// This is the core async task dispatcher that:
    /// 1. Emits a Spawn event for UI feedback
    /// 2. Runs the task in a background thread pool
    /// 3. Captures errors and adds them to error history
    /// 4. Calls the callback with the result
    ///
    /// # Type Parameters
    /// * `T` - The success return type of the task
    /// * `Fut` - The future type returned by the task closure
    ///
    /// # Arguments
    /// * `name` - Task identifier for logging and error tracking
    /// * `task` - Async closure that performs the operation
    /// * `callback` - Called with the result when task completes
    /// * `cx` - Context for spawning and state updates
    fn spawn<T, Fut>(
        &mut self,
        name: ServerTask,
        task: impl FnOnce() -> Fut + Send + 'static,
        callback: impl FnOnce(&mut Self, Result<T>, &mut Context<Self>) + Send + 'static,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        cx.emit(ServerEvent::TaskStarted(name.clone()));
        debug!(name = name.as_str(), "Spawning background task");

        cx.spawn(async move |handle, cx| {
            // Run task in background executor (thread pool)
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;

            // Update state with result on main thread
            handle.update(cx, move |this, cx| {
                if let Err(e) = &result {
                    let message = format!("{} failed", name.as_str());
                    error!(error = %e, message);
                    this.add_error_message(name.as_str().to_string(), e.to_string(), cx);
                }
                callback(this, result, cx);
            })
        })
        .detach();
    }
    /// Update and save server configuration
    fn update_and_save_server_config<F>(&mut self, task_name: ServerTask, cx: &mut Context<Self>, modifier: F)
    where
        F: FnOnce(&mut RedisServer),
    {
        let mut servers = self.servers.clone().unwrap_or_default();

        if let Some(s) = servers.iter_mut().find(|s| s.id == self.server_id) {
            modifier(s);
        }

        self.spawn(
            task_name,
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

    // ===== Public accessor methods =====

    /// Check if the server is currently busy with an operation
    pub fn is_busy(&self) -> bool {
        !matches!(self.server_status, RedisServerStatus::Idle)
    }

    /// Get the type of a specific key (if known)
    pub fn key_type(&self, key: &str) -> Option<&KeyType> {
        self.keys.get(key)
    }

    /// Get the current key tree ID (changes when keys are reloaded)
    pub fn key_tree_id(&self) -> &str {
        &self.key_tree_id
    }

    /// Set the query mode (All/Prefix/Exact)
    pub fn set_query_mode(&mut self, mode: QueryMode, cx: &mut Context<Self>) {
        self.query_mode = mode;

        self.update_and_save_server_config(ServerTask::UpdateServerQueryMode, cx, move |server| {
            server.query_mode = Some(mode.to_string());
        });
    }
    /// Set whether to soft wrap the editor
    pub fn set_soft_wrap(&mut self, soft_wrap: bool, cx: &mut Context<Self>) {
        self.soft_wrap = soft_wrap;
        cx.emit(ServerEvent::SoftWrapToggled(self.soft_wrap));

        self.update_and_save_server_config(ServerTask::UpdateServerSoftWrap, cx, move |server| {
            server.soft_wrap = Some(soft_wrap);
        });
    }
    /// Get the current query mode (All/Prefix/Exact)
    pub fn query_mode(&self) -> QueryMode {
        self.query_mode
    }
    /// Build hierarchical tree structure from flat Redis keys
    ///
    /// Converts keys like "user:123:name", "user:456:age" into a tree:
    /// - user (folder)
    ///   - 123 (folder)
    ///     - name (key)
    ///   - 456 (folder)
    ///     - age (key)
    ///
    /// # Arguments
    /// * `expanded_items` - Set of folder paths that should be shown expanded
    /// * `expand_all` - If true, expand all folders regardless of expanded_items
    ///
    /// # Returns
    /// Vector of tree items sorted with folders first, then alphabetically
    pub fn key_tree(&self, expanded_items: &AHashSet<SharedString>, expand_all: bool) -> Vec<TreeItem> {
        let keys = self.keys.keys();

        // Build trie structure from all keys
        let mut root_trie_node = KeyNode {
            full_path: SharedString::default(),
            is_key: false,
            children: AHashMap::new(),
        };

        for key in keys {
            root_trie_node.insert(key.split(KEY_SEPARATOR));
        }

        /// Convert the trie structure to a flat vector of TreeItems
        ///
        /// Recursively processes children and sorts them:
        /// 1. Folders (directories) before keys
        /// 2. Alphabetically by name within each category
        fn convert_map_to_vec_tree(
            children_map: &AHashMap<SharedString, KeyNode>,
            expanded_items: &AHashSet<SharedString>,
            expand_all: bool,
        ) -> Vec<TreeItem> {
            let mut children_vec = Vec::new();

            for (short_name, internal_node) in children_map {
                // Create tree item with full path as ID and short name as label
                let mut node = TreeItem::new(internal_node.full_path.clone(), short_name.clone());

                // Set expanded state
                if expand_all || expanded_items.contains(&internal_node.full_path) {
                    node = node.expanded(true);
                }

                // Recursively build children
                let node = node.children(convert_map_to_vec_tree(
                    &internal_node.children,
                    expanded_items,
                    expand_all,
                ));
                children_vec.push(node);
            }

            // Sort: folders first (reverse), then alphabetically by ID
            children_vec.sort_unstable_by(|a, b| {
                let a_is_dir = !a.children.is_empty();
                let b_is_dir = !b.children.is_empty();

                // Folders before files (reverse comparison)
                let type_ordering = a_is_dir.cmp(&b_is_dir).reverse();

                // Then alphabetically
                type_ordering.then_with(|| a.id.cmp(&b.id))
            });

            children_vec
        }

        convert_map_to_vec_tree(&root_trie_node.children, expanded_items, expand_all)
    }
    /// Check if the current scan has completed
    pub fn scan_completed(&self) -> bool {
        self.scan_completed
    }

    /// Check if a scan is currently in progress
    pub fn scaning(&self) -> bool {
        self.scaning
    }

    /// Get the total database size (number of keys)
    pub fn dbsize(&self) -> Option<u64> {
        self.dbsize
    }

    /// Get the count of scanned/loaded keys
    pub fn scan_count(&self) -> usize {
        self.keys.len()
    }

    /// Get the last measured latency to the server
    pub fn latency(&self) -> Option<Duration> {
        self.latency
    }

    /// Get cluster node counts (master, replica)
    pub fn nodes(&self) -> (usize, usize) {
        self.nodes
    }

    /// Get the Redis server version string
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get the currently selected server id
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Get whether to soft wrap the editor
    pub fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    /// Set the list of configured servers
    pub fn set_servers(&mut self, servers: Vec<RedisServer>) {
        self.servers = Some(servers);
    }

    /// Get a server by id
    pub fn server(&self, server_id: &str) -> Option<&RedisServer> {
        self.servers
            .as_deref()
            .and_then(|servers| servers.iter().find(|s| s.id == server_id))
    }

    /// Get the list of all configured servers
    pub fn servers(&self) -> Option<&[RedisServer]> {
        self.servers.as_deref()
    }

    /// Get the currently selected key name
    pub fn key(&self) -> Option<SharedString> {
        self.key.clone()
    }

    /// Get the value data for the currently selected key
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
    }

    /// Get the key type of the currently selected value
    pub fn value_key_type(&self) -> Option<KeyType> {
        self.value.as_ref().map(|value| value.key_type())
    }
    // ===== Server management operations =====

    /// Remove a server from the configuration
    ///
    /// Persists the change to disk and emits UpdateServers event
    pub fn remove_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut servers = self.servers.clone().unwrap_or_default();
        servers.retain(|s| s.id != id);

        self.spawn(
            ServerTask::RemoveServer,
            move || async move {
                save_servers(servers.clone()).await?;
                Ok(servers)
            },
            move |this, result, cx| {
                if let Ok(servers) = result {
                    cx.emit(ServerEvent::ServerListUpdated);
                    this.servers = Some(servers);
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Add new server or update existing server configuration
    ///
    /// # Arguments
    /// * `server` - Server configuration to add/update
    /// * `cx` - Context for spawning async task
    pub fn update_or_insrt_server(&mut self, mut server: RedisServer, cx: &mut Context<Self>) {
        let mut servers = self.servers.clone().unwrap_or_default();
        if server.id.is_empty() {
            server.id = Uuid::now_v7().to_string();
        }
        server.updated_at = Some(Local::now().to_rfc3339());

        self.spawn(
            ServerTask::UpdateOrInsertServer,
            move || async move {
                if server.name.is_empty() {
                    return Err(Error::Invalid {
                        message: "Server name is required".to_string(),
                    });
                }
                if let Some(existing_server) = servers.iter_mut().find(|s| s.id == server.id) {
                    *existing_server = server;
                } else {
                    servers.push(server);
                }
                save_servers(servers.clone()).await?;

                Ok(servers)
            },
            move |this, result, cx| {
                if let Ok(servers) = result {
                    cx.emit(ServerEvent::ServerListUpdated);
                    this.servers = Some(servers);
                }
                cx.notify();
            },
            cx,
        );
    }

    // ===== Redis operations =====

    /// Send a PING command to check server health and measure latency
    ///
    /// If ping fails, removes the cached client connection (it will be recreated on next use)
    pub fn ping(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }

        let server_id = self.server_id.clone();
        let remove_server_id = server_id.clone();

        self.spawn(
            ServerTask::Ping,
            move || async move {
                let client = get_connection_manager().get_client(&server_id).await?;
                let start = Instant::now();
                client.ping().await?;
                Ok(start.elapsed())
            },
            move |this, result, cx| match result {
                Ok(latency) => {
                    this.latency = Some(latency);
                    cx.emit(ServerEvent::HeartbeatReceived(latency));
                }
                Err(e) => {
                    // Connection is invalid, remove cached client
                    get_connection_manager().remove_client(&remove_server_id);
                    error!(error = %e, "Ping failed, client connection removed");
                }
            },
            cx,
        );
    }
    /// Select and connect to a Redis server
    ///
    /// This initiates a connection and loads server metadata:
    /// - Database size (DBSIZE)
    /// - Server version
    /// - Latency measurement (PING)
    /// - Cluster node counts
    ///
    /// If query_mode is QueryMode::All, automatically starts scanning all keys.
    ///
    /// # Arguments
    /// * `server_id` - Server id to connect to
    /// * `cx` - Context for spawning async tasks and state updates
    pub fn select(&mut self, server_id: SharedString, cx: &mut Context<Self>) {
        // Only proceed if selecting a different server
        if self.server_id != server_id {
            self.reset();
            self.server_id = server_id.clone();
            let (query_mode, soft_wrap) = self
                .server(server_id.as_str())
                .map(|server_config| {
                    let mode = server_config
                        .query_mode
                        .as_deref()
                        .and_then(|s| QueryMode::from_str(s).ok())
                        .unwrap_or_default();

                    let wrap = server_config.soft_wrap.unwrap_or(true);

                    // 返回一个元组，包含所有需要更新的值
                    (mode, wrap)
                })
                .unwrap_or((QueryMode::All, true));
            self.query_mode = query_mode;
            self.soft_wrap = soft_wrap;

            debug!(server_id = self.server_id.as_str(), "Selecting server");
            cx.emit(ServerEvent::ServerSelected(server_id));
            cx.notify();

            if self.server_id.is_empty() {
                return;
            }

            // Set loading state
            self.server_status = RedisServerStatus::Loading;
            self.scaning = true;
            cx.notify();

            let server_id_clone = self.server_id.clone();
            let counting_server_id = server_id_clone.clone();

            self.spawn(
                ServerTask::SelectServer,
                move || async move {
                    let client = get_connection_manager().get_client(&server_id_clone).await?;

                    // Gather server metadata
                    let dbsize = client.dbsize().await?;
                    let start = Instant::now();
                    let version = client.version().to_string();
                    client.ping().await?;
                    let latency = start.elapsed();
                    let nodes = client.nodes();

                    Ok((dbsize, latency, nodes, version))
                },
                move |this, result, cx| {
                    // Ignore if user switched to a different server while loading
                    if this.server_id != counting_server_id {
                        return;
                    }

                    // Update metadata if successful
                    if let Ok((dbsize, latency, nodes, version)) = result {
                        this.latency = Some(latency);
                        this.dbsize = Some(dbsize);
                        this.nodes = nodes;
                        this.version = version.into();
                    };

                    let server_id = this.server_id.clone();
                    this.server_status = RedisServerStatus::Idle;
                    cx.emit(ServerEvent::ServerInfoUpdated(server_id.clone()));
                    cx.notify();

                    // Auto-scan keys if in All mode
                    if this.query_mode == QueryMode::All {
                        this.scan_keys(server_id, SharedString::default(), cx);
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

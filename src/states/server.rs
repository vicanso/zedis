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

use crate::connection::{AccessMode, RedisClientDescription, get_connection_manager};
use crate::db::HistoryManager;
use crate::error::Error;
use crate::states::server::event::{ServerEvent, ServerTask};
use crate::states::server::stat::RedisInfo;
use crate::states::{QueryMode, get_session_option};
use ahash::AHashMap;
use ahash::AHashSet;
use gpui::SharedString;
use gpui::prelude::*;
use parking_lot::RwLock;
use std::str::FromStr;
use std::sync::Arc;
use tracing::debug;
use tracing::error;
use uuid::Uuid;
use value::{KeyType, RedisValue, RedisValueData};

pub mod event;
pub mod hash;
pub mod key;
pub mod list;
pub mod set;
pub mod stat;
pub mod string;
pub mod value;
pub mod zset;

type Result<T, E = Error> = std::result::Result<T, E>;

// Constants for state management
const MAX_ERROR_MESSAGES: usize = 10; // Maximum error messages to keep in memory
/// Error message with categorization and timestamp
#[derive(Debug, Clone)]
pub struct ErrorMessage {
    /// Category of error (e.g., task name like "ping", "scan_keys")
    pub category: SharedString,

    /// Human-readable error message
    pub message: SharedString,
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
    redis_info: Option<RedisInfo>,

    /// Whether the terminal is open
    terminal: bool,

    /// Currently selected server id
    server_id: SharedString,

    /// Search history
    search_history: Vec<SharedString>,

    /// Whether the server supports database selection
    supports_db_selection: bool,

    /// Currently selected database
    db: usize,

    /// Access mode
    access_mode: AccessMode,

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
    /// Description of the nodes
    nodes_description: Arc<RedisClientDescription>,

    /// Redis server version string
    version: SharedString,

    /// List of all configured servers
    // servers: Option<Vec<RedisServer>>,

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
    scanning: bool,

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
        self.scanning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        self.loaded_prefixes.clear();
    }

    /// Reset all state when switching to a different server
    fn reset(&mut self) {
        self.server_id = SharedString::default();
        self.version = SharedString::default();
        self.nodes = (0, 0);
        self.keys.clear();
        self.key_tree_id = SharedString::default();
        self.nodes_description = Arc::new(RedisClientDescription::default());
        self.dbsize = None;
        self.key = None;
        self.redis_info = None;
        self.value = None;
        self.reset_scan();
        self.terminal = false;
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
        };
        guard.push(info.clone());
        self.emit_error_notification(info.message.clone(), cx);
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
        let server_id = self.server_id.clone();

        cx.spawn(async move |handle, cx| {
            // Run task in background executor (thread pool)
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;

            // Update state with result on main thread
            handle.update(cx, move |this, cx| {
                if let Err(e) = &result {
                    let message = format!("{} failed", name.as_str());
                    error!(error = %e, message);
                    // only add error message if the server id is the same as the current server id
                    if this.server_id == server_id {
                        this.add_error_message(name.as_str().to_string(), e.to_string(), cx);
                    }
                }
                callback(this, result, cx);
            })
        })
        .detach();
    }

    fn try_get_mut_key_value(&mut self) -> Option<(SharedString, &mut RedisValue)> {
        let key = self.key.as_ref().filter(|k| !k.is_empty())?.clone();
        let value = self.value.as_mut()?;
        if value.is_busy() {
            return None;
        }
        Some((key, value))
    }

    // ===== Public accessor methods =====

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn toggle_terminal(&mut self, cx: &mut Context<Self>) {
        self.terminal = !self.terminal;
        cx.emit(ServerEvent::TerminalToggled(self.terminal));
    }

    /// Check if the server is currently busy with an operation
    pub fn is_busy(&self) -> bool {
        !matches!(self.server_status, RedisServerStatus::Idle)
    }

    /// Get the current key tree ID (changes when keys are reloaded)
    pub fn key_tree_id(&self) -> &str {
        &self.key_tree_id
    }

    /// Get the search history
    pub fn search_history(&self) -> Vec<SharedString> {
        self.search_history.clone()
    }

    pub fn clear_search_history(&mut self, _cx: &mut Context<Self>) {
        self.search_history.clear();
    }

    /// Get whether the server is readonly
    pub fn readonly(&self) -> bool {
        matches!(self.access_mode, AccessMode::StrictReadOnly | AccessMode::SafeMode)
    }
    pub fn toggle_readonly(&mut self, cx: &mut Context<Self>) {
        if matches!(self.access_mode, AccessMode::StrictReadOnly) {
            self.add_error_message(
                "toggle_readonly".to_string(),
                "Strict read-only mode, cannot be toggled".to_string(),
                cx,
            );
            return;
        }
        if self.access_mode == AccessMode::ReadWrite {
            self.access_mode = AccessMode::SafeMode;
        } else {
            self.access_mode = AccessMode::ReadWrite;
        }
        cx.emit(ServerEvent::ServerInfoUpdated);
    }

    /// Set the query mode (All/Prefix/Exact)
    pub fn set_query_mode(&mut self, mode: QueryMode, _cx: &mut Context<Self>) {
        self.query_mode = mode;
    }
    /// Set whether to soft wrap the editor
    pub fn set_soft_wrap(&mut self, soft_wrap: bool, cx: &mut Context<Self>) {
        self.soft_wrap = soft_wrap;
        cx.emit(ServerEvent::SoftWrapToggled(self.soft_wrap));
    }
    /// Get the current query mode (All/Prefix/Exact)
    pub fn query_mode(&self) -> QueryMode {
        self.query_mode
    }

    /// Check if the current scan has completed
    pub fn scan_completed(&self) -> bool {
        self.scan_completed
    }

    /// Check if a scan is currently in progress
    pub fn scanning(&self) -> bool {
        self.scanning
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
    pub fn redis_info(&self) -> Option<&RedisInfo> {
        self.redis_info.as_ref()
    }

    /// Get cluster node counts (master, replica)
    pub fn nodes(&self) -> (usize, usize) {
        self.nodes
    }
    /// Get the description of the nodes
    pub fn nodes_description(&self) -> Arc<RedisClientDescription> {
        self.nodes_description.clone()
    }

    /// Get the Redis server version string
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get the currently selected server id
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    /// Get the currently selected database
    pub fn db(&self) -> usize {
        self.db
    }

    /// Get whether the server supports database selection
    pub fn supports_db_selection(&self) -> bool {
        self.supports_db_selection
    }

    /// Get whether to soft wrap the editor
    pub fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    /// Get the currently selected key name
    pub fn key(&self) -> Option<SharedString> {
        self.key.clone()
    }
    /// Get the map of all loaded keys and their types
    pub fn keys(&self) -> &AHashMap<SharedString, KeyType> {
        &self.keys
    }

    /// Get the value data for the currently selected key
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
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
    /// * `db` - Database to connect to
    /// * `cx` - Context for spawning async tasks and state updates
    pub fn select(&mut self, server_id: SharedString, db: usize, cx: &mut Context<Self>) {
        // Only proceed if selecting a different server
        if self.server_id != server_id || self.db != db {
            self.reset();
            self.server_id = server_id.clone();
            self.db = db;

            let (query_mode, soft_wrap) = get_session_option(&server_id)
                .map(|option| {
                    let mode = option
                        .query_mode
                        .as_deref()
                        .and_then(|s| QueryMode::from_str(s).ok())
                        .unwrap_or_default();

                    let wrap = option.soft_wrap.unwrap_or(true);

                    // 返回一个元组，包含所有需要更新的值
                    (mode, wrap)
                })
                .unwrap_or((QueryMode::All, true));
            self.query_mode = query_mode;
            self.soft_wrap = soft_wrap;

            debug!(server_id = self.server_id.as_str(), "Selecting server");
            if let Ok(history) = HistoryManager::records(server_id.as_str()) {
                self.search_history = history;
            }
            cx.emit(ServerEvent::ServerSelected(server_id));
            cx.notify();

            if self.server_id.is_empty() {
                return;
            }

            // Set loading state
            self.server_status = RedisServerStatus::Loading;
            self.scanning = true;
            cx.notify();

            let server_id_clone = self.server_id.clone();
            let counting_server_id = server_id_clone.clone();
            let db = self.db;

            self.spawn(
                ServerTask::SelectServer,
                move || async move {
                    let client = get_connection_manager().get_client(&server_id_clone, db).await?;

                    // Gather server metadata
                    let dbsize = client.dbsize().await?;
                    let version = client.version().to_string();
                    let nodes = client.nodes();
                    let nodes_description = client.nodes_description();
                    let supports_db_selection = client.supports_db_selection();
                    let access_mode = client.access_mode();
                    Ok((
                        dbsize,
                        nodes,
                        nodes_description,
                        version,
                        supports_db_selection,
                        access_mode,
                    ))
                },
                move |this, result, cx| {
                    // Ignore if user switched to a different server while loading
                    if this.server_id != counting_server_id {
                        return;
                    }

                    // Update metadata if successful
                    if let Ok((dbsize, nodes, nodes_description, version, supports_db_selection, access_mode)) = result
                    {
                        this.dbsize = Some(dbsize);
                        this.nodes = nodes;
                        this.nodes_description = Arc::new(nodes_description);
                        this.version = version.into();
                        this.supports_db_selection = supports_db_selection;
                        this.access_mode = access_mode;
                    };

                    let server_id = this.server_id.clone();
                    this.server_status = RedisServerStatus::Idle;
                    cx.emit(ServerEvent::ServerInfoUpdated);
                    cx.notify();

                    // Auto-scan keys if in All mode
                    if this.query_mode == QueryMode::All {
                        this.scan_keys(server_id, SharedString::default(), cx);
                    } else {
                        this.scanning = false;
                        cx.notify();
                    }
                },
                cx,
            );
        }
    }
}

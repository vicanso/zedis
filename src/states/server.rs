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

use crate::connection::{
    AccessMode, Capability, RedisClientDescription, SlowLogEntry, get_connection_manager, get_server, get_servers,
};
use crate::db::get_search_history_manager;
use crate::error::{ConnectionErrorKind, Error};
use crate::helpers::unix_ts;
use crate::states::server::event::{ServerEvent, ServerTask};
use crate::states::server::history::{ValueHistoryEntry, push_history};
use crate::states::server::stat::{RedisInfo, get_metrics_cache};
use crate::states::{QueryMode, ZedisGlobalStore, get_session_option, i18n_common};
use ahash::AHashMap;
use ahash::AHashSet;
use bytes::Bytes;
use gpui::SharedString;
use gpui::prelude::*;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info};
use uuid::Uuid;
use value::{KeyType, RedisValue, RedisValueData};

pub mod cluster;
pub mod event;
pub mod hash;
pub mod history;
pub mod json;
pub mod key;
pub mod list;
pub mod persistence;
pub mod sentinel;
pub mod set;
pub mod stat;
pub mod stream;
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

    /// The last connection / metadata load failed (e.g. network down).
    /// Tracked separately from `Idle` so a later re-select of the *same*
    /// server retries the load instead of being swallowed by the
    /// same-server guard in [`ZedisServerState::select`].
    Failed,
}

/// Health of the live connection, derived purely from the status-bar
/// heartbeat's `PING` outcome (see `note_ping_result`). Distinct from
/// [`RedisServerStatus`], which only tracks the *initial* connect/load and
/// drives retry-on-reselect; this reflects the *ongoing* link so the UI can
/// show an online / reconnecting / offline dot.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ConnectionHealth {
    /// No heartbeat result yet (just selected / still doing the first load).
    #[default]
    Unknown,
    /// Last heartbeat PING succeeded.
    Connected,
    /// Recent PING(s) failed but still inside the retry window.
    Reconnecting,
    /// PING has failed past the threshold â treat the link as down.
    Offline,
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
    last_slow_logs_checked_at: i64,
    last_slow_log_count: usize,
    slow_logs: Vec<SlowLogEntry>,

    /// Whether the terminal is open
    terminal: bool,

    /// Currently selected server id
    server_id: SharedString,

    /// Search history
    search_history: Vec<SharedString>,

    /// Total number of databases
    databases: usize,

    /// Currently selected database
    db: usize,

    /// Key-tree separator for this connection (from server config, else `:`).
    /// Set on `select`.
    key_separator: String,
    /// Resolved SCAN page size for this connection.
    key_scan_count: usize,
    /// Resolved max key-tree depth for this connection.
    max_key_tree_depth: usize,
    /// Resolved auto-expand threshold for this connection.
    auto_expand_threshold: usize,
    /// Resolved "show TTL in key tree" for this connection.
    show_key_tree_ttl: bool,

    /// Access mode
    access_mode: AccessMode,

    /// Whether the server supports ReJSON module
    supports_rejson: bool,

    /// Whether the server has the RediSearch module loaded. Gates the
    /// "Search" entry in the Tools dropdown so unrelated servers don't
    /// surface a useless menu item.
    supports_search: bool,

    /// Query mode (All/Prefix/Exact) for key filtering
    query_mode: QueryMode,
    /// Optional filter to show only keys of one native type (`SCAN ... TYPE`).
    type_filter: Option<KeyType>,

    /// Whether to soft wrap the editor
    soft_wrap: bool,

    /// Current server status
    server_status: RedisServerStatus,

    /// Live-connection health derived from the heartbeat PING (online /
    /// reconnecting / offline). Separate from `server_status`, which only
    /// covers the initial connect/load.
    connection_health: ConnectionHealth,

    /// Why the last heartbeat failed (auth / perms / timeout / network /
    /// tunnel), surfaced in the status-bar offline tooltip. Cleared on a
    /// healthy PING.
    last_connection_error: ConnectionErrorKind,

    /// Consecutive heartbeat PING failures; drives the reconnecting->offline
    /// transition in `note_ping_result`, reset to 0 on any success.
    ping_failures: u32,

    /// User pressed "disconnect" on the status-bar health dot: the live
    /// connection is dropped and the heartbeat paused (health stays `Offline`)
    /// until `reconnect` re-establishes it. Distinct from an involuntary drop,
    /// where the heartbeat keeps retrying on its own.
    manually_offline: bool,

    /// Unix seconds of the last "reconnect first" notice shown while
    /// `manually_offline`. Throttles it so a background refresh loop can't spam
    /// the notification each tick.
    last_offline_notice: i64,

    /// This state belongs to an inactive workspace tab: its status-bar
    /// heartbeat keeps ticking, but `refresh_redis_info` throttles itself to a
    /// relaxed cadence so background tabs don't poll Redis at full speed.
    background: bool,

    /// Unix seconds of the last refresh allowed through while `background`.
    last_background_refresh: i64,

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

    /// Key whose oversized value the user chose to load anyway ("Load
    /// anyway" on the too-large panel). While the selected key matches,
    /// `get_value` skips the size gate so a later refresh doesn't bounce
    /// back to the panel. Cleared on server switch.
    size_gate_bypassed: Option<SharedString>,

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

    /// Monotonic scan-session counter, bumped by [`Self::reset_scan`]. Every
    /// in-flight prefix scan captures the epoch at launch and bails on
    /// completion if it no longer matches — so a lazy folder scan / "Load
    /// more" still streaming when the user re-runs the search can't write its
    /// stale keys and `incomplete_prefixes`/`loaded_prefixes` entries back
    /// onto the freshly-reset tree. A keyword check alone can't catch this:
    /// an empty-keyword browse re-searched with an empty keyword keeps the
    /// same keyword, so only a generation bump distinguishes the sessions.
    scan_epoch: u64,

    /// Unique ID for current key tree (changes when keys are reloaded)
    key_tree_id: SharedString,

    /// Set of prefixes that have been scanned (for lazy loading folders)
    loaded_prefixes: AHashSet<SharedString>,

    /// Prefixes whose lazy folder-scan (`scan_prefix`) is currently in
    /// flight, stored in the same `"{folder_id}:"` form `scan_prefix`
    /// receives. Drives the inline spinner on the matching folder row;
    /// entries are removed as each prefix scan finishes and the whole set
    /// is cleared on reset / server switch.
    scanning_prefixes: AHashSet<SharedString>,

    /// Prefixes whose lazy scan stopped at the per-load page cap before
    /// completing, mapped to the SCAN cursors to resume from. Drives the
    /// inline "Load more" row under the folder; an entry is removed once
    /// its prefix finishes (or is refreshed) and the whole map is cleared
    /// on reset / server switch.
    incomplete_prefixes: AHashMap<SharedString, Vec<u64>>,

    /// Map of all loaded keys and their types
    keys: AHashMap<SharedString, KeyType>,

    /// Parallel map of per-key TTL in seconds, populated alongside `keys`
    /// during SCAN. `-1` = no expiry, `-2` = missing (key vanished between
    /// SCAN and the TTL pipeline). Kept separate from `keys` so older code
    /// paths that read `&AHashMap<SharedString, KeyType>` keep compiling.
    ///
    /// Behind an `Arc` so the key tree's background build snapshots it with
    /// an Arc clone instead of a structural copy (with 500k keys that copy
    /// was ~20MB resident, twice). Writers go through `Arc::make_mut`: the
    /// map is copied at most once per build window (only while a snapshot
    /// is actually held), which is what the per-rebuild clone used to cost.
    key_ttls: Arc<AHashMap<SharedString, i64>>,

    /// In-memory write history per string key. Each entry is the bytes
    /// that were just overwritten by a SET, newest first. Capped at
    /// [`history::VALUE_HISTORY_CAPACITY`] per key and cleared on key
    /// delete or server switch. Never persisted to disk.
    value_history: AHashMap<SharedString, VecDeque<ValueHistoryEntry>>,

    // ===== Error tracking =====
    /// Recent error messages (limited to MAX_ERROR_MESSAGES)
    error_messages: Arc<RwLock<Vec<ErrorMessage>>>,
}

impl ZedisServerState {
    /// Create a new server state instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark this state as belonging to an inactive (background) workspace tab
    /// — see the `background` field. Clearing the flag also resets the
    /// throttle window so a re-activated tab refreshes on its next heartbeat
    /// tick instead of waiting out the relaxed interval.
    pub fn set_background(&mut self, background: bool) {
        self.background = background;
        if !background {
            self.last_background_refresh = 0;
        }
    }

    /// Whether this state belongs to an inactive (background) workspace tab.
    /// View-owned poll loops (e.g. the command-stats sampler) read this to
    /// pause their own traffic while the tab is hidden.
    pub fn is_background(&self) -> bool {
        self.background
    }

    /// Reset all scan-related state (clears keys, cursors, etc.)
    ///
    /// Called when switching servers or starting a new scan
    pub fn reset_scan(&mut self, cx: &mut Context<Self>) {
        self.keyword = SharedString::default();
        self.cursors = None;
        self.keys.clear();
        // Fresh Arc instead of `make_mut(..).clear()` — when a build still
        // holds the old snapshot, `make_mut` would copy the whole map just
        // to empty it.
        self.key_ttls = Arc::new(AHashMap::new());
        self.key_tree_id = Uuid::now_v7().to_string().into();
        self.scanning = false;
        self.scan_completed = false;
        self.scan_times = 0;
        // New scan session: bump the epoch so any prefix scan still in flight
        // from the previous session (a folder expand / "Load more" that hadn't
        // finished) is recognised as stale and drops its result instead of
        // re-injecting keys and resurrecting `incomplete_prefixes` entries.
        self.scan_epoch = self.scan_epoch.wrapping_add(1);
        self.loaded_prefixes.clear();
        self.scanning_prefixes.clear();
        self.incomplete_prefixes.clear();
        cx.emit(ServerEvent::KeyScanReset);
        cx.emit(ServerEvent::KeyTreeUpdated);
    }

    /// If the currently-tracked server has been removed from the
    /// configured server list, drop our reference to it and clean up
    /// the cached client / metrics. Without this the heartbeat keeps
    /// firing against a phantom id and logs `Redis config not found`
    /// once per tick.
    pub fn clear_if_removed(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let still_exists = get_servers()
            .map(|servers| servers.iter().any(|s| s.id == self.server_id.as_ref()))
            .unwrap_or(true);
        if still_exists {
            return;
        }
        get_connection_manager().remove_client(&self.server_id, self.db);
        get_metrics_cache().remove_server(self.server_id.as_str());
        self.reset(cx);
    }

    /// Reset all state when switching to a different server
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.server_id = SharedString::default();
        self.version = SharedString::default();
        self.nodes = (0, 0);
        self.keys.clear();
        self.key_ttls = Arc::new(AHashMap::new());
        // History is scoped to the current server session; drop it when
        // we move to a different server to avoid restoring stale bytes
        // into the wrong target.
        self.value_history.clear();
        self.key_tree_id = SharedString::default();
        self.nodes_description = Arc::new(RedisClientDescription::default());
        self.dbsize = None;
        self.key = None;
        self.redis_info = None;
        self.connection_health = ConnectionHealth::Unknown;
        self.last_connection_error = ConnectionErrorKind::Unknown;
        self.ping_failures = 0;
        // A fresh select / reconnect always re-establishes the link, so clear
        // any manual-disconnect pause (reconnect routes through here too).
        self.manually_offline = false;
        self.value = None;
        self.size_gate_bypassed = None;
        // Cleared on server switch (but NOT in reset_scan, which a filter
        // change triggers and must preserve the just-set filter).
        self.type_filter = None;
        self.reset_scan(cx);
        self.terminal = false;
        self.last_slow_logs_checked_at = 0;
        self.last_slow_log_count = 0;
        self.slow_logs.clear();
    }

    /// Add new keys with their types to the key map (deduplicating automatically)
    ///
    /// If any new keys were added, generates a new tree ID to trigger UI refresh
    fn extend_keys(&mut self, keys: Vec<(SharedString, SharedString, i64)>) {
        self.keys.reserve(keys.len());
        let key_ttls = Arc::make_mut(&mut self.key_ttls);
        key_ttls.reserve(keys.len());
        let mut insert_count = 0;

        for (key, key_type, ttl_secs) in keys {
            let kt = KeyType::from(key_type.as_ref());
            self.keys
                .entry(key.clone())
                .and_modify(|existing| {
                    if *existing == KeyType::Unknown && kt != KeyType::Unknown {
                        *existing = kt;
                    }
                })
                .or_insert_with(|| {
                    insert_count += 1;
                    kt
                });
            // Always update the freshest TTL (it counts down) — replace
            // existing rather than insert_with.
            key_ttls.insert(key, ttl_secs);
        }

        // Update tree ID only if new keys were added
        if insert_count != 0 {
            self.key_tree_id = Uuid::now_v7().to_string().into();
        }
    }

    /// Add an error message to the history and emit error event
    ///
    /// Maintains a rolling window of MAX_ERROR_MESSAGES most recent errors.
    /// Includes server_id and db context for easier debugging.
    fn add_error_message(&mut self, category: String, message: String, cx: &mut Context<Self>) {
        let mut guard = self.error_messages.write();

        // Remove oldest error if at capacity
        if guard.len() >= MAX_ERROR_MESSAGES {
            guard.remove(0);
        }

        let server_name = get_server(&self.server_id).map(|s| s.name).unwrap_or_default();
        let context_message: SharedString = if server_name.is_empty() {
            format!("[{category}] {message}").into()
        } else {
            format!("[{category}] [{server_name}:{}] {message}", self.db).into()
        };

        let info = ErrorMessage {
            category: category.into(),
            message: context_message,
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
        self.spawn_with_arg(name, SharedString::default(), task, callback, cx);
    }

    /// Like [`Self::spawn`] but records `arg` — the command's main argument
    /// (key name, scan pattern, channel, …) — on the task's
    /// completion/failure log lines so background tasks running against the
    /// same server can be told apart in the logs.
    fn spawn_with_arg<T, Fut>(
        &mut self,
        name: ServerTask,
        arg: impl Into<SharedString>,
        task: impl FnOnce() -> Fut + Send + 'static,
        callback: impl FnOnce(&mut Self, Result<T>, &mut Context<Self>) + Send + 'static,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        // Manually disconnected: run no Redis query. Every op here calls
        // get_client, which silently re-establishes the link the user just
        // dropped (hence "offline but data still loads"). reconnect clears the
        // flag first (via reset), so the reload after reconnect still runs.
        if self.manually_offline {
            // Tell the user why nothing loaded — but throttle it so a
            // background refresh loop can't spam a notice every tick.
            let now = unix_ts();
            if now.saturating_sub(self.last_offline_notice) >= 3 {
                self.last_offline_notice = now;
                self.emit_warning_notification(i18n_common(cx, "reconnect_first"), cx);
            }
            // If `select` already flipped us to Loading before calling spawn,
            // leave Failed so the UI is not stuck on the busy skeleton with
            // no in-flight task to clear it.
            if matches!(self.server_status, RedisServerStatus::Loading) {
                self.server_status = RedisServerStatus::Failed;
                self.scanning = false;
                cx.emit(ServerEvent::ServerInfoUpdated);
                cx.notify();
            }
            return;
        }
        let arg = arg.into();
        cx.emit(ServerEvent::TaskStarted(name.clone()));
        debug!(name = name.as_str(), arg = arg.as_str(), "Spawning background task");
        let server_id = self.server_id.clone();
        let db = self.db;
        let start = Instant::now();

        cx.spawn(async move |handle, cx| {
            // Run task in background executor (thread pool)
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;

            // Update state with result on main thread
            handle.update(cx, move |this, cx| {
                // Stale-result guard: the user may have switched server/db
                // while this task was in flight. Every task here is scoped to
                // the server+db that was current at launch, so applying its
                // result now would inject the previous target's data into the
                // freshly-reset state. The connection-level work already
                // happened — we only skip the state-mutating callback.
                let stale = this.server_id != server_id || this.db != db;
                if let Err(e) = &result {
                    error!(
                        task = name.as_str(),
                        arg = arg.as_str(),
                        server_id = server_id.as_str(),
                        error = %e,
                        "Task failed"
                    );
                    // Surface a toast only for the still-active target, and
                    // never for background info refreshes.
                    if !stale && name != ServerTask::RefreshRedisInfo {
                        this.add_error_message(name.as_str().to_string(), e.to_string(), cx);
                    }
                }
                if stale {
                    return;
                }
                callback(this, result, cx);
                let latency = start.elapsed();
                if name != ServerTask::RefreshRedisInfo {
                    info!(
                        task = name.as_str(),
                        arg = arg.as_str(),
                        server_id = server_id.as_str(),
                        latency_ms = latency.as_millis(),
                        "Task completed"
                    );
                }
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

    /// Check if the server is currently busy with an operation.
    ///
    /// Only `Loading` counts as busy — `Failed` must render the normal
    /// (empty) editor so the user can re-select and retry, otherwise the
    /// loading skeleton would spin forever after a connection failure.
    pub fn is_busy(&self) -> bool {
        matches!(self.server_status, RedisServerStatus::Loading)
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

    /// Get whether the server supports ReJSON module
    pub fn supports_rejson(&self) -> bool {
        self.supports_rejson
    }

    /// Whether the server has the RediSearch module loaded.
    pub fn supports_search(&self) -> bool {
        self.supports_search
    }

    /// Get whether the server is readonly
    pub fn readonly(&self) -> bool {
        matches!(self.access_mode, AccessMode::StrictReadOnly | AccessMode::SafeMode)
    }

    /// Whether `cap` is allowed under the current access mode.
    ///
    /// Prefer this over raw `!self.readonly()` so pure reads (folder
    /// refresh, export, multi-select, local tags) stay available when
    /// the connection is locked. See [`Capability`].
    pub fn can(&self, cap: Capability) -> bool {
        cap.allowed(self.readonly())
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
    /// The active key-type filter, if any.
    pub fn type_filter(&self) -> Option<KeyType> {
        self.type_filter
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

    /// The keyword the current key scan was started with (empty = full scan).
    pub fn keyword(&self) -> SharedString {
        self.keyword.clone()
    }

    /// Check if the current scan has completed
    pub fn scan_completed(&self) -> bool {
        self.scan_completed
    }

    /// Check if a scan is currently in progress
    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// Prefixes whose lazy folder-scan is currently in flight (form
    /// `"{folder_id}:"`). The key tree reads this to show an inline
    /// spinner on the matching folder row.
    pub fn scanning_prefixes(&self) -> &AHashSet<SharedString> {
        &self.scanning_prefixes
    }

    /// Prefixes whose folder scan stopped at the page cap with more keys
    /// still on the server (form `"{folder_id}:"`). The key tree shows an
    /// inline "Load more" row for each so the user can resume the scan.
    pub fn incomplete_prefix_set(&self) -> AHashSet<SharedString> {
        self.incomplete_prefixes.keys().cloned().collect()
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

    /// Current live-connection health (online / reconnecting / offline),
    /// updated each heartbeat tick by `note_ping_result`.
    pub fn connection_health(&self) -> ConnectionHealth {
        self.connection_health
    }

    /// Why the last heartbeat failed, for the offline tooltip. `Unknown` when
    /// healthy or when the driver didn't give us enough to classify.
    pub fn last_connection_error(&self) -> ConnectionErrorKind {
        self.last_connection_error
    }

    /// Get the slow logs
    pub fn slow_logs(&self) -> &Vec<SlowLogEntry> {
        &self.slow_logs
    }
    /// Get the last slow log count
    pub fn last_slow_log_count(&self) -> usize {
        self.last_slow_log_count
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

    /// Returns true when the connected Redis server supports per-field hash TTL
    /// commands (HEXPIRE, HTTL, HPERSIST), introduced in Redis 7.4.
    pub fn supports_hash_field_ttl(&self) -> bool {
        use semver::Version;
        Version::parse(self.version.as_ref())
            .map(|v| v >= Version::new(7, 4, 0))
            .unwrap_or(false)
    }

    /// Returns true when the server is at least Redis 6.0, where ACL
    /// (the `ACL USERS / GETUSER / SETUSER / WHOAMI` family) was
    /// introduced. Drives Tools-menu visibility so users on older
    /// servers don't see a non-functional entry.
    pub fn supports_acl(&self) -> bool {
        use semver::Version;
        Version::parse(self.version.as_ref())
            .map(|v| v >= Version::new(6, 0, 0))
            .unwrap_or(false)
    }

    /// Returns true when the server is at least Redis 7.0, where
    /// Functions (the `FUNCTION LIST / LOAD / DELETE / FCALL` family)
    /// were introduced as the successor to EVAL scripts.
    pub fn supports_functions(&self) -> bool {
        use semver::Version;
        Version::parse(self.version.as_ref())
            .map(|v| v >= Version::new(7, 0, 0))
            .unwrap_or(false)
    }

    /// Whether the Topology panel is meaningful: true for multi-node
    /// deployments (Cluster / Sentinel), false for Standalone where the panel
    /// would only show placeholder text. Compares the `ServerType` Debug repr
    /// surfaced on the description (the enum itself is crate-private to the
    /// connection layer).
    pub fn supports_topology(&self) -> bool {
        let server_type = &self.nodes_description().server_type;
        server_type.as_str() == "Cluster" || server_type.as_str() == "Sentinel"
    }

    /// Get the currently selected server id
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    /// Get the currently selected database
    pub fn db(&self) -> usize {
        self.db
    }

    /// Key-tree / prefix separator for the active connection.
    /// Never empty — defaults to `":"`.
    pub fn key_separator(&self) -> &str {
        if self.key_separator.is_empty() {
            ":"
        } else {
            &self.key_separator
        }
    }

    /// SCAN page size for the active connection (never zero).
    pub fn key_scan_count(&self) -> usize {
        self.key_scan_count.max(1)
    }

    /// Max key-tree nesting depth for the active connection.
    pub fn max_key_tree_depth(&self) -> usize {
        self.max_key_tree_depth.max(1)
    }

    /// Auto-expand threshold for the active connection.
    pub fn auto_expand_threshold(&self) -> usize {
        self.auto_expand_threshold
    }

    /// Whether TTL chips are shown in the key tree for this connection.
    pub fn show_key_tree_ttl(&self) -> bool {
        self.show_key_tree_ttl
    }

    /// Get the total number of databases
    pub fn databases(&self) -> usize {
        self.databases
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
    pub fn key_ttls(&self) -> &AHashMap<SharedString, i64> {
        self.key_ttls.as_ref()
    }

    /// Shared handle to the TTL map for background snapshots (the key-tree
    /// build) — an Arc clone instead of copying the whole map.
    pub fn key_ttls_arc(&self) -> Arc<AHashMap<SharedString, i64>> {
        Arc::clone(&self.key_ttls)
    }
    pub fn keys(&self) -> &AHashMap<SharedString, KeyType> {
        &self.keys
    }

    /// Get the value data for the currently selected key
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
    }

    /// Get the in-memory value history for `key` (newest first), if any.
    /// Returns `None` rather than an empty slice so callers can easily
    /// distinguish "never written" from "written and rolled back to 0".
    pub fn value_history_for(&self, key: &SharedString) -> Option<&VecDeque<ValueHistoryEntry>> {
        self.value_history.get(key).filter(|d| !d.is_empty())
    }

    /// Append the bytes about to be overwritten to the history ring buffer.
    /// Called from the SET paths in value.rs right before mutating state.
    /// Empty `bytes` are still recorded — "was empty" is itself meaningful
    /// history to roll back to.
    pub(super) fn push_value_history(&mut self, key: SharedString, bytes: Bytes) {
        let buffer = self.value_history.entry(key).or_default();
        push_history(buffer, ValueHistoryEntry { bytes, at: unix_ts() });
    }

    /// Drop all history entries for `key`. Called when the key is deleted
    /// so we don't pile up dangling versions of a name the server no
    /// longer knows about.
    pub(super) fn clear_value_history_for(&mut self, key: &SharedString) {
        self.value_history.remove(key);
    }

    pub fn set_search_history(&mut self, history: Vec<SharedString>) {
        self.search_history = history.into_iter().collect();
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
        // Reload when switching to a different server, OR when re-selecting
        // the *same* server whose previous load failed. The latter matters
        // because going Home clears only the global selection (the empty
        // `ServerSelected` is ignored by content.rs), so `self.server_id`
        // stays stale; without the `Failed` check, re-clicking the server
        // after a network error would be a silent no-op and never retry.
        let same_target = self.server_id == server_id && self.db == db;
        let retry_failed = same_target && matches!(self.server_status, RedisServerStatus::Failed);
        if !same_target || retry_failed {
            get_metrics_cache().remove_server(self.server_id.as_str());
            self.reset(cx);
            self.server_id = server_id.clone();
            self.db = db;
            // Resolve key-tree / scan prefs: per-server override, else Settings.
            let global = cx.global::<ZedisGlobalStore>().read(cx);
            let g_scan = global.key_scan_count();
            let g_depth = global.max_key_tree_depth();
            let g_expand = global.auto_expand_threshold();
            let g_ttl = global.show_key_tree_ttl();
            if let Ok(server) = get_server(server_id.as_str()) {
                self.key_separator = server.resolve_key_separator();
                self.key_scan_count = server.resolve_key_scan_count(g_scan);
                self.max_key_tree_depth = server.resolve_max_key_tree_depth(g_depth);
                self.auto_expand_threshold = server.resolve_auto_expand_threshold(g_expand);
                self.show_key_tree_ttl = server.resolve_show_key_tree_ttl(g_ttl);
            } else {
                self.key_separator = ":".to_string();
                self.key_scan_count = g_scan.max(1);
                self.max_key_tree_depth = g_depth.max(1);
                self.auto_expand_threshold = g_expand;
                self.show_key_tree_ttl = g_ttl;
            }

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
            let search_history_manager = get_search_history_manager();
            if let Ok(history) = search_history_manager.records(server_id.as_str()) {
                self.search_history = history.into_iter().map(Into::into).collect();
            }
            cx.emit(ServerEvent::ServerSelected(server_id));

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
                    let databases = client.databases();
                    let access_mode = client.access_mode();
                    let supports_rejson = client.supports_rejson();
                    let supports_search = client.supports_search();
                    Ok((
                        dbsize,
                        nodes,
                        nodes_description,
                        version,
                        databases,
                        access_mode,
                        supports_rejson,
                        supports_search,
                    ))
                },
                move |this, result, cx| {
                    // Ignore if user switched to a different server while loading
                    if this.server_id != counting_server_id {
                        return;
                    }

                    match result {
                        Ok((
                            dbsize,
                            nodes,
                            nodes_description,
                            version,
                            databases,
                            access_mode,
                            supports_rejson,
                            supports_search,
                        )) => {
                            this.dbsize = Some(dbsize);
                            this.nodes = nodes;
                            this.nodes_description = Arc::new(nodes_description);
                            this.version = version.into();
                            this.databases = databases;
                            this.access_mode = access_mode;
                            this.supports_rejson = supports_rejson;
                            this.supports_search = supports_search;

                            let server_id = this.server_id.clone();
                            this.server_status = RedisServerStatus::Idle;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            let is_exact_mode = this.query_mode == QueryMode::Exact;
                            if is_exact_mode {
                                this.scanning = false;
                            }
                            cx.notify();
                            // Auto-scan keys if not exact mode
                            if !is_exact_mode {
                                this.scan_keys(server_id, SharedString::default(), cx);
                            }
                        }
                        Err(_) => {
                            // Connection / metadata load failed (e.g. network
                            // down). Mark Failed so a later re-select retries
                            // instead of being swallowed by select()'s
                            // same-server guard. The error toast was already
                            // emitted in spawn_with_arg; skip the key scan
                            // since it would just fail again with a 2nd toast.
                            this.server_status = RedisServerStatus::Failed;
                            this.scanning = false;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            cx.notify();
                        }
                    }
                },
                cx,
            );
        }
    }

    /// Force a reconnect of the currently-selected server/db.
    ///
    /// The heartbeat marks a live connection `Offline` after repeated PING
    /// failures but leaves `server_status` as `Idle` (the initial load
    /// succeeded), so [`select`](Self::select)'s same-target guard would treat
    /// a re-select as a silent no-op. Flipping the status to `Failed` first
    /// makes the existing `retry_failed` path fire, reusing the full reload
    /// logic (reset, metadata fetch, key scan) instead of duplicating it.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        self.server_status = RedisServerStatus::Failed;
        self.select(server_id, db, cx);
    }
}

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

#[cfg(not(target_family = "wasm"))]
use crate::connection::SentinelMaster;
use crate::connection::error::Error as ConnectionError;
use crate::connection::floors::{self, Floor};
use crate::connection::{
    AccessMode, Capability, CommandStatus, DEFAULT_KEY_SCAN_COUNT, MAX_KEY_SCAN_COUNT, MIN_KEY_SCAN_COUNT, PauseMode,
    RedisClientDescription, ServerCommand, ServerDb, ServerFeatures, ServerSummary, SlowLogEntry, WRITE_UNLOCK_SECS,
    client_unpause, forget_client, get_server, get_server_features, get_servers, invalidate_server_features,
    lock_writes, note_server_command_error, probe_server_features, server_summary, unlock_writes,
};
use crate::db::get_search_history_manager;
use crate::error::{ConnectionErrorKind, Error};
use crate::helpers::{pacing, unix_ts};
use crate::states::server::event::{ServerEvent, ServerTask};
use crate::states::server::history::{KeyHistories, ValueHistoryEntry};
use crate::states::server::report::ReportFinding;
use crate::states::server::stat::{ClientPause, RedisInfo, client_pause_now, get_metrics_cache};
use crate::states::{
    HINT_FIRST_CONNECT, QueryMode, ServerView, ZedisGlobalStore, ask_for_fresh_page_data, command_unavailable_message,
    first_connect_hint, get_session_option, i18n_common, i18n_status_bar, save_session_option,
    update_app_state_and_save_quiet,
};
use ahash::AHashMap;
use ahash::AHashSet;
use bytes::Bytes;
use facts::ServerFacts;
use gpui::prelude::*;
use gpui::{SharedString, Task};
use imbl::OrdMap;
use link::Link;
use open_key::OpenKey;
use parking_lot::RwLock;
use prefs::ConnectionPrefs;
use readings::Readings;
use scan::KeyScan;
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};
use uuid::Uuid;
use value::{KeyType, RedisValue, RedisValueData};
use web_time::Instant;
use write_lock::WriteLock;
use zedis_core::change_log::ChangeEntry;
use zedis_core::report::Section;

#[cfg(not(target_family = "wasm"))]
pub mod cluster;
pub mod element;
pub mod event;
mod facts;
pub mod hash;
pub mod history;
pub mod json;
pub mod key;
mod link;
pub mod list;
mod open_key;
pub mod persistence;
mod prefs;
mod readings;
pub mod replication;
pub mod report;
mod scan;
pub mod script_kill;
#[cfg(not(target_family = "wasm"))]
pub mod sentinel;
pub mod set;
pub mod stat;
pub mod stream;
pub mod string;
pub mod value;
mod write_lock;
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

/// A frozen `(section, field, value)` capture of the Server Info panel's
/// parsed `INFO` rows, kept for before/after comparison. Stored on the
/// server state so it survives the panel being dropped on navigation;
/// scoped to the connection session (cleared by [`ZedisServerState::reset`]).
#[derive(Debug, Clone, Default)]
pub struct InfoSnapshot {
    /// `(section, field, value)` rows exactly as the panel displayed them
    /// (section carries the node address on clusters).
    pub rows: Vec<(SharedString, SharedString, SharedString)>,
    /// Wall-clock `HH:MM:SS` the capture was taken, for the banner.
    pub taken_at: SharedString,
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

/// The loaded key set: every key the scans brought in, with its type, in
/// key order.
///
/// A persistent B-tree rather than a hash map, for the key tree. It rebuilds
/// about every 100ms while a scan runs, and each rebuild used to copy every
/// loaded key out of this map on the UI thread — 11ms at a million keys —
/// because the build runs on another thread and the map belongs to an
/// entity; the build then sorted the copy, another 76ms of a worker. A clone
/// of this map is a pointer copy, a write while a build still holds one
/// copies only the path it touches (a 1,000-key page: 0.16ms), and it
/// iterates in order, so the build sorts nothing. `Arc<AHashMap>` would not
/// do: a scan page landing while a build holds the snapshot would copy the
/// whole map right there, on the UI thread.
pub type LoadedKeys = OrdMap<SharedString, KeyType>;

/// One connection's state: which server and database it is, and what is
/// known about it, a group a sub-struct of its own module, each with the
/// methods that read and change only it. What announces a change, spawns a
/// task or reaches across groups stays here, on the state.
#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    /// Currently selected server id
    server_id: SharedString,

    /// Currently selected database
    db: usize,

    /// What a connect learned about the server (`facts.rs`).
    facts: ServerFacts,

    /// The link as the heartbeat and the loads have known it (`link.rs`).
    link: Link,

    /// What this connection may do, and the write lock's window (`write_lock.rs`).
    lock: WriteLock,

    /// How this connection's tree and editor are drawn (`prefs.rs`).
    prefs: ConnectionPrefs,

    /// What the heartbeat and `DBSIZE` read (`readings.rs`).
    readings: Readings,

    /// The keys the tree shows, and the scan that loads them (`scan.rs`).
    scan: KeyScan,

    /// The open key, its value and its history (`open_key.rs`).
    open: OpenKey,

    /// What this session's panels found, for a troubleshooting report
    /// (`report.rs`).
    report_findings: Vec<(ReportFinding, Section)>,

    /// `(processed, total)` slots of an in-flight cluster reshard;
    /// `None` when no reshard is running. Drives the Topology progress
    /// bar via [`ServerEvent::ClusterReshardProgress`].
    reshard_progress: Option<(u32, u32)>,

    /// Frozen `INFO` field set captured by the Server Info panel for
    /// before/after comparison. Lives here (not on the view) so it
    /// survives panel navigation — tool views are dropped on route
    /// change — and dies with the session on server switch (`reset`).
    info_snapshot: Option<InfoSnapshot>,

    /// Whether the terminal is open
    terminal: bool,

    /// Recent error messages (limited to MAX_ERROR_MESSAGES)
    error_messages: Arc<RwLock<Vec<ErrorMessage>>>,
}

/// Why a workspace has nothing to show: the link, not an empty database.
/// The editor draws this in place of its "no key selected" screen, whose
/// "New key" buttons promise what an unreachable server cannot do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkProblem {
    /// The user took the connection offline.
    Disconnected,
    /// It could not be reached, or it stopped answering.
    Failed {
        kind: ConnectionErrorKind,
        /// The error as the driver or the server worded it; empty when a
        /// beat failed without one being kept.
        detail: SharedString,
        /// Another attempt is running now.
        retrying: bool,
    },
}

/// The decision behind [`ZedisServerState::link_problem`], on plain values.
fn link_problem_for(
    status: &RedisServerStatus,
    health: ConnectionHealth,
    manual: bool,
    failure: Option<&(ConnectionErrorKind, SharedString)>,
) -> Option<LinkProblem> {
    if manual {
        return Some(LinkProblem::Disconnected);
    }
    let retrying = matches!(status, RedisServerStatus::Loading);
    let down = matches!(status, RedisServerStatus::Failed)
        || matches!(health, ConnectionHealth::Offline | ConnectionHealth::Reconnecting)
        // A retry of a connect that failed: still down until it lands.
        || (retrying && failure.is_some());
    if !down {
        return None;
    }
    let (kind, detail) = failure.cloned().unwrap_or_default();
    Some(LinkProblem::Failed { kind, detail, retrying })
}

/// What a write the user types — a terminal line — meets on a connection.
/// The editors ask [`ZedisServerState::can`] per button; a typed command has
/// no button to grey out, so it asks this before it is sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteGate {
    /// Writes go through.
    Open,
    /// Refused: read-only for the session or by the entry, a read-only ACL
    /// user, a read-only account.
    ReadOnly,
    /// A write-locked entry outside its window (ADR 14): the lock's question
    /// first, as the status bar's lock button asks it.
    Locked,
}

/// `SafeMode` is two things under one name: the window of a write-locked
/// entry, which the user opens, and plain read-only, which is refused.
fn write_gate_for(mode: AccessMode, write_locked: bool) -> WriteGate {
    match mode {
        AccessMode::ReadWrite => WriteGate::Open,
        AccessMode::StrictReadOnly => WriteGate::ReadOnly,
        AccessMode::SafeMode if write_locked => WriteGate::Locked,
        AccessMode::SafeMode => WriteGate::ReadOnly,
    }
}

/// How many times in a row the heartbeat runs a failed load again before it
/// is left for the user's Reconnect.
const MAX_LOAD_RETRIES: u8 = 3;

/// Whether a load that failed this way is worth running again by itself once
/// the server answers: the link was down, slow or tunnelled through something
/// that was, or the server was not ready — loading, busy, without its master.
/// A wrong password, a missing permission or a TLS mismatch is the same on the
/// next attempt.
fn heals_with_the_link(kind: ConnectionErrorKind) -> bool {
    use ConnectionErrorKind as K;
    matches!(
        kind,
        K::Network | K::Timeout | K::Tunnel | K::Loading | K::Busy | K::ClusterDown | K::MasterDown
    )
}

/// What a background task was launched against, for the stale-result guard
/// and its log lines: everything about a task that does not depend on its
/// types.
///
/// `spawn_with_arg` is compiled once per call site, because each passes its
/// own closures. `begin_task`, `settle_task` and `log_completed` take this
/// instead and are `#[inline(never)]`, so the offline check, the error
/// reporting and the log lines exist once rather than in every instance —
/// about 200 KiB of the release binary's code.
struct TaskRun {
    name: ServerTask,
    arg: SharedString,
    server_id: SharedString,
    db: usize,
    start: Instant,
}

impl TaskRun {
    #[inline(never)]
    fn log_completed(&self) {
        if !matches!(
            self.name,
            ServerTask::RefreshRedisInfo | ServerTask::RefreshDbsize | ServerTask::RefreshValueHeader
        ) {
            info!(
                task = self.name.as_str(),
                arg = self.arg.as_str(),
                server_id = self.server_id.as_str(),
                latency_ms = self.start.elapsed().as_millis(),
                "Task completed"
            );
        }
    }
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
        self.link.set_background(background);
    }

    /// Whether this state belongs to an inactive (background) workspace tab.
    /// View-owned poll loops (e.g. the command-stats sampler) read this to
    /// pause their own traffic while the tab is hidden.
    ///
    /// Also true while nobody is looking at the app at all
    /// (`pacing::unattended`): a hidden browser page, or a desktop window that
    /// has been inactive for a while.
    pub fn is_background(&self) -> bool {
        self.link.is_background()
    }

    /// Reset all scan-related state (clears keys, cursors, etc.)
    ///
    /// Called when switching servers or starting a new scan.
    pub fn reset_scan(&mut self, cx: &mut Context<Self>) {
        self.reset_scan_inner(false, cx);
    }

    /// Reset for a scan that *replaces* the tree rather than emptying it first.
    ///
    /// A refresh asks the tree the same question it is already answering, so
    /// its rows are still the best answer anyone has until the new ones land:
    /// clearing up front blanks the panel for a round trip and throws away
    /// the reader's place in it. Everything else a new scan needs is reset
    /// exactly as [`Self::reset_scan`] does — the cursors, the epoch that
    /// makes an in-flight page stale, the per-prefix bookkeeping — only the
    /// rows stay, and the first batch of the new scan takes them out (see
    /// `extend_keys`).
    ///
    /// Not for a *new* query: those rows answer the old question and have to
    /// go at once, or the tree shows results for a keyword nobody typed. And
    /// not for a scan that may never reach `extend_keys` (an exact lookup),
    /// which would leave the old rows standing for good.
    pub fn begin_refresh_scan(&mut self, cx: &mut Context<Self>) {
        self.reset_scan_inner(true, cx);
    }

    fn reset_scan_inner(&mut self, keep_rows: bool, cx: &mut Context<Self>) {
        self.scan.reset(keep_rows);
        if !keep_rows {
            // Both events describe rows that just went away: the tree
            // collapses and scrolls back to the top, then rebuilds empty. A
            // refresh has neither to report — the rows on screen are still
            // the ones it was built from — and announcing it anyway would
            // cost a full rebuild of the tree it already has.
            cx.emit(ServerEvent::KeyScanReset);
            cx.emit(ServerEvent::KeyTreeUpdated);
        }
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
        forget_client(&self.at());
        get_metrics_cache().remove_server(self.server_id.as_str());
        self.reset(cx);
    }

    /// Reset all state when switching to a different server
    fn reset(&mut self, cx: &mut Context<Self>) {
        // A window belongs to the server it was opened on.
        self.lock.forget_window();
        self.server_id = SharedString::default();
        self.facts.reset();
        self.scan.forget_server();
        self.open.forget_server();
        self.link.reset();
        self.reset_scan(cx);
        self.terminal = false;
        self.report_findings.clear();
        self.readings.reset();
        // A reshard task from the previous server can no longer report back
        // (its completion callback is stale-guarded) — drop its progress.
        self.reshard_progress = None;
        // An INFO snapshot from the previous connection would only produce
        // a meaningless all-changed diff on the next server.
        self.info_snapshot = None;
    }

    /// Add new keys with their types to the loaded ones ([`KeyScan::extend`]).
    fn extend_keys(&mut self, keys: Vec<(SharedString, SharedString, i64)>) {
        self.scan.extend(keys);
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

    /// Whether the user took this server offline, in which case nothing may
    /// reach it: says why (throttled, so a background refresh loop can't
    /// spam a notice every tick) and answers `true`. A write checks this
    /// *before* its optimistic change — [`Self::spawn`] drops the task, and
    /// the change it had already made was never taken back.
    pub(crate) fn refuse_while_offline(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.link.manually_offline {
            return false;
        }
        if self.link.offline_notice_due() {
            self.emit_warning_notification(i18n_common(cx, "reconnect_first"), cx);
        }
        true
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
        let Some(run) = self.begin_task(name, arg.into(), cx) else {
            return;
        };
        cx.spawn(async move |handle, cx| {
            // Run task in background executor (thread pool)
            let task = cx.background_spawn(async move { task().await });
            let result: Result<T> = task.await;

            // Update state with result on main thread
            handle.update(cx, move |this, cx| {
                if !this.settle_task(&run, result.as_ref().err(), cx) {
                    return;
                }
                callback(this, result, cx);
                run.log_completed();
            })
        })
        .detach();
    }

    /// What [`Self::spawn_with_arg`] does before the task runs, or `None`
    /// when it must not run. Out of line — see [`TaskRun`].
    #[inline(never)]
    fn begin_task(&mut self, name: ServerTask, arg: SharedString, cx: &mut Context<Self>) -> Option<TaskRun> {
        // Manually disconnected: run no Redis query. Every op here calls
        // get_client, which silently re-establishes the link the user just
        // dropped (hence "offline but data still loads"). reconnect clears the
        // flag first (via reset), so the reload after reconnect still runs.
        if self.refuse_while_offline(cx) {
            // If `select` already flipped us to Loading before calling spawn,
            // leave Failed so the UI is not stuck on the busy skeleton with
            // no in-flight task to clear it.
            if matches!(self.link.server_status, RedisServerStatus::Loading) {
                self.link.server_status = RedisServerStatus::Failed;
                self.scan.scanning = false;
                cx.emit(ServerEvent::ServerInfoUpdated);
                cx.notify();
            }
            return None;
        }
        cx.emit(ServerEvent::TaskStarted(name.clone()));
        debug!(name = name.as_str(), arg = arg.as_str(), "Spawning background task");
        Some(TaskRun {
            name,
            arg,
            server_id: self.server_id.clone(),
            db: self.db,
            start: Instant::now(),
        })
    }

    /// Whether the task's callback should run: reports a failure and answers
    /// `false` for a result that belongs to a target the user has left. Out
    /// of line — see [`TaskRun`].
    #[inline(never)]
    fn settle_task(&mut self, run: &TaskRun, failure: Option<&Error>, cx: &mut Context<Self>) -> bool {
        // Stale-result guard: the user may have switched server/db
        // while this task was in flight. Every task here is scoped to
        // the server+db that was current at launch, so applying its
        // result now would inject the previous target's data into the
        // freshly-reset state. The connection-level work already
        // happened — we only skip the state-mutating callback.
        let stale = self.server_id != run.server_id || self.db != run.db;
        if let Some(e) = failure {
            error!(
                task = run.name.as_str(),
                arg = run.arg.as_str(),
                server_id = run.server_id.as_str(),
                error = %e,
                "Task failed"
            );
            // Surface a toast only for the still-active target, and
            // never for background info refreshes or the feature
            // probe itself. A `NOPERM` / `unknown command` reply goes
            // to the feature matrix first: the first one becomes a
            // "not available on this server" notice and degrades the
            // UI; repeats stay quiet.
            if !stale
                && !run.name.is_background_refresh()
                && run.name != ServerTask::ProbeFeatures
                && !self.note_command_error(e, cx)
                && !self.note_link_error(e, cx)
            {
                self.add_error_message(run.name.as_str().to_string(), e.to_string(), cx);
            }
        }
        if stale {
            return false;
        }
        if failure.is_none() && run.name != ServerTask::RefreshRedisInfo {
            self.link.reached();
        }
        true
    }

    /// After a write to the loaded value failed: the optimistic change never
    /// reached the server, so read back what the server holds rather than
    /// keep showing a write that did not happen — or guess at undoing it,
    /// which for an `RPUSH` onto a partly loaded list popped a real element.
    fn reload_after_failed_write(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.open.key.clone().filter(|key| !key.is_empty()) {
            self.reload_value(key, cx);
        }
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
        self.link.is_busy()
    }

    /// Get the current key tree ID (changes when keys are reloaded)
    pub fn key_tree_id(&self) -> &str {
        &self.scan.key_tree_id
    }

    /// Get the search history
    pub fn search_history(&self) -> Vec<SharedString> {
        self.scan.search_history.clone()
    }

    pub fn clear_search_history(&mut self, _cx: &mut Context<Self>) {
        self.scan.search_history.clear();
    }

    /// Get whether the server supports ReJSON module
    pub fn supports_rejson(&self) -> bool {
        self.facts.supports_rejson()
    }

    /// Whether the server has the RediSearch module loaded.
    pub fn supports_search(&self) -> bool {
        self.facts.supports_search()
    }

    /// Whether writes are refused by something the user cannot switch: a
    /// read-only ACL user, or a read-only bridge account.
    pub fn strict_readonly(&self) -> bool {
        self.lock.strict_readonly()
    }

    /// Get whether the server is readonly
    pub fn readonly(&self) -> bool {
        self.lock.readonly()
    }

    /// What a typed write meets here — see [`WriteGate`].
    pub fn write_gate(&self) -> WriteGate {
        // The config is only read where it decides something.
        let mode = self.lock.access_mode();
        let write_locked = matches!(mode, AccessMode::SafeMode) && self.write_locked();
        write_gate_for(mode, write_locked)
    }

    /// Whether `cap` is allowed under the current access mode *and* every
    /// server command it needs is usable here (see [`Capability`] and
    /// [`ServerFeatures`]).
    ///
    /// Prefer this over raw `!self.readonly()` so pure reads (folder
    /// refresh, export, multi-select, local tags) stay available when
    /// the connection is locked.
    pub fn can(&self, cap: Capability) -> bool {
        cap.available(self.readonly(), self.facts.feature_matrix())
    }

    /// Why `cap` is blocked on the server side, if it is — the command the
    /// probe found missing or denied. `None` when only the access mode (or
    /// nothing) stands in the way.
    pub fn blocked_by(&self, cap: Capability) -> Option<(ServerCommand, CommandStatus)> {
        self.facts.blocked_by(cap)
    }

    /// The probed feature matrix for this server (optimistic until probed).
    pub fn features(&self) -> Arc<ServerFeatures> {
        self.facts.features()
    }

    /// The first command `view` cannot function without that this server
    /// lacks — `Some` means the route shows a placeholder, not the panel.
    pub fn panel_block(&self, view: ServerView) -> Option<(ServerCommand, CommandStatus)> {
        self.facts.panel_block(view)
    }

    /// Why `command` is unusable here, if it is.
    pub fn command_block(&self, command: ServerCommand) -> Option<CommandStatus> {
        self.facts.command_block(command)
    }

    /// Runs the command probe in the background. Skipped when a probed
    /// matrix is already cached for this server (a db switch, a reconnect);
    /// `force` discards the cache and probes again.
    pub fn probe_features(&mut self, force: bool, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        if server_id.is_empty() {
            return;
        }
        if force {
            invalidate_server_features(server_id.as_str());
            self.facts.set_features(Arc::default());
            cx.emit(ServerEvent::FeaturesProbed);
        } else if self.facts.feature_matrix().probed {
            return;
        }
        let db = self.db;
        let id = server_id.clone();
        self.spawn(
            ServerTask::ProbeFeatures,
            move || async move { Ok(probe_server_features(id.as_str(), db).await?) },
            move |this, result, cx| {
                if let Ok(features) = result {
                    this.facts.set_features(features);
                    cx.emit(ServerEvent::FeaturesProbed);
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// Discards the cached matrix and probes again — the "Re-probe" button.
    pub fn reprobe_features(&mut self, cx: &mut Context<Self>) {
        self.probe_features(true, cx);
    }

    /// Handles the errors that mean "the link, not the command": a dropped
    /// connection (laptop woke up, VPN flipped, server restarted), a server
    /// that is loading / busy / cluster-down, or a topology change (`READONLY`
    /// / `MASTERDOWN` after a Sentinel or Cluster failover, where the cached
    /// client still points at the demoted master). Drops the stale client so
    /// the next call — or the 2s heartbeat — rebuilds it, and replaces the
    /// raw error with one throttled, localized notice. Returns `true` when
    /// the error was taken care of here.
    pub fn note_link_error(&mut self, error: &Error, cx: &mut Context<Self>) -> bool {
        use ConnectionErrorKind as K;
        let kind = error.connection_kind();
        let message = match kind {
            K::ReadOnly | K::MasterDown => {
                // A standalone replica really is read-only — that's not a
                // failover, let the raw error through.
                let server_type = self.nodes_description().server_type.clone();
                if server_type != "Sentinel" && server_type != "Cluster" {
                    return false;
                }
                forget_client(&self.at());
                i18n_status_bar(cx, "conn_master_changed")
            }
            K::Network => {
                forget_client(&self.at());
                match self.link.connection_health {
                    // The status bar already shows "reconnecting" — no toast
                    // per failed click on top of it.
                    ConnectionHealth::Reconnecting | ConnectionHealth::Offline => return true,
                    ConnectionHealth::Connected => i18n_status_bar(cx, "conn_lost"),
                    // The empty editor already draws `link_problem()` with
                    // host and error. A toast is window-global, so it would
                    // follow the user onto every other tab.
                    ConnectionHealth::Unknown => return true,
                }
            }
            K::Timeout | K::Loading | K::Busy | K::ClusterDown => i18n_status_bar(cx, kind.reason_key()),
            _ => return false,
        };
        if self.link.link_notice_due() {
            self.emit_warning_notification(message, cx);
        }
        true
    }

    /// Feeds a failed command's error into the feature matrix. Returns
    /// `true` when it was an unsupported/denied reply the matrix now accounts
    /// for — newly learned (a localized notice is shown and `FeaturesProbed`
    /// fires so panels degrade) or already known (silent) — so the caller can
    /// skip its own raw-error toast. Views that run their own tasks (monitor,
    /// pub/sub) call this too.
    pub fn note_command_error(&mut self, error: &Error, cx: &mut Context<Self>) -> bool {
        let Error::Connection { source } = error else {
            return false;
        };
        let (changed, features) = note_server_command_error(self.server_id.as_str(), source);
        if changed.is_empty() {
            return match source {
                ConnectionError::Redis { source } => self
                    .facts
                    .feature_matrix()
                    .already_explains(source.code(), source.detail().unwrap_or_default()),
                _ => false,
            };
        }
        self.facts.set_features(features);
        for (command, status) in changed {
            let message = command_unavailable_message(cx, command, status);
            self.emit_warning_notification(message, cx);
        }
        cx.emit(ServerEvent::FeaturesProbed);
        cx.notify();
        true
    }

    /// Whether this entry's writes are locked by default (ADR 14): its own
    /// setting, else its tag — production is.
    pub fn write_locked(&self) -> bool {
        get_server(&self.server_id).is_ok_and(|server| server.write_locked())
    }

    /// Remember the `CLIENT PAUSE` the clients panel just sent, so the
    /// status bar can show it on a server that will not say (Redis).
    pub fn note_client_pause(&mut self, mode: PauseMode, timeout_ms: u64, cx: &mut Context<Self>) {
        self.readings.note_client_pause(mode, timeout_ms);
        cx.notify();
    }

    /// The clients panel lifted the pause.
    pub fn note_client_unpause(&mut self, cx: &mut Context<Self>) {
        self.readings.clear_client_pause();
        cx.notify();
    }

    /// Whether clients are paused right now, for how long and by whose
    /// word — the server's (Valkey 8.1+), else this app's own record.
    pub fn client_pause(&self) -> Option<ClientPause> {
        self.readings.client_pause()
    }

    /// `CLIENT UNPAUSE` on every master — the status bar chip's click.
    /// Idempotent and harmless where nothing is paused, so no question.
    pub fn unpause_clients(&mut self, cx: &mut Context<Self>) {
        if !self.can(Capability::KillClient) {
            self.emit_warning_notification("Read-only mode — client commands blocked".into(), cx);
            return;
        }
        self.readings.clear_client_pause();
        let at = self.at();
        self.spawn(
            ServerTask::ClientUnpause,
            move || async move { Ok(client_unpause(&at).await?) },
            |this, result, cx| {
                if result.is_ok() {
                    this.emit_success_notification("CLIENT UNPAUSE".into(), "CLIENT UNPAUSE".into(), cx);
                    this.refresh_redis_info(cx);
                }
            },
            cx,
        );
    }

    /// How much of the open write window is left, if one is open.
    pub fn unlock_remaining(&self) -> Option<Duration> {
        self.lock.remaining()
    }

    /// Open the write window: leave `SafeMode` for `WRITE_UNLOCK_SECS`, then
    /// go back by a timer. The caller has had the lock's question answered
    /// (`DangerKind::WriteLocked`, by name on production). In the browser
    /// the bridge is told too, and a bridge that refuses closes the window
    /// again here: a page that believes itself unlocked while the bridge
    /// refuses every write would be the worse state.
    pub fn unlock_writes(&mut self, cx: &mut Context<Self>) {
        if self.lock.strict_readonly() {
            return;
        }
        let window = Duration::from_secs(WRITE_UNLOCK_SECS);
        let until = Instant::now() + window;
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(window).await;
            let _ = this.update(cx, |this, cx| this.expire_unlock(cx));
        });
        self.lock.open(until, timer);
        let at = self.at();
        self.spawn(
            ServerTask::UnlockWrites,
            move || async move { Ok(unlock_writes(&at).await?) },
            move |this, result, cx| {
                if let Err(e) = result {
                    this.lock.refuse();
                    let message = i18n_status_bar(cx, "unlock_failed").replace("%{error}", &e.to_string());
                    this.emit_warning_notification(message.into(), cx);
                    cx.emit(ServerEvent::ServerInfoUpdated);
                }
            },
            cx,
        );
        cx.emit(ServerEvent::ServerInfoUpdated);
        cx.notify();
    }

    /// Close the window early. The bridge is told the same, so a script
    /// cannot keep using a window the page has closed.
    pub fn lock_writes(&mut self, cx: &mut Context<Self>) {
        if !self.lock.is_open() {
            return;
        }
        self.relock(cx);
        let at = self.at();
        self.spawn(
            ServerTask::LockWrites,
            move || async move { Ok(lock_writes(&at).await?) },
            |_this, _result, _cx| {},
            cx,
        );
    }

    /// The timer's end: locked again, and said so — the person who unlocked
    /// may be mid-thought, and the next write refusing without a word would
    /// read as a bug.
    fn expire_unlock(&mut self, cx: &mut Context<Self>) {
        if !self.lock.is_open() {
            return;
        }
        self.relock(cx);
        let server_name = get_server(&self.server_id).map(|s| s.name).unwrap_or_default();
        let message = i18n_status_bar(cx, "relocked_message").replace("%{server}", &server_name);
        self.emit_info_notification(message.into(), cx);
    }

    fn relock(&mut self, cx: &mut Context<Self>) {
        self.lock.close();
        cx.emit(ServerEvent::ServerInfoUpdated);
        cx.notify();
    }

    pub fn toggle_readonly(&mut self, cx: &mut Context<Self>) {
        if !self.lock.toggle() {
            self.add_error_message(
                "toggle_readonly".to_string(),
                "Strict read-only mode, cannot be toggled".to_string(),
                cx,
            );
            return;
        }
        cx.emit(ServerEvent::ServerInfoUpdated);
    }

    /// Set the query mode (All/Prefix/Exact)
    pub fn set_query_mode(&mut self, mode: QueryMode, _cx: &mut Context<Self>) {
        self.scan.query_mode = mode;
    }
    /// Whether the keyword is a regex, which the scan cannot narrow by.
    pub fn regex_keyword(&self) -> bool {
        self.scan.regex_keyword
    }
    pub fn set_regex_keyword(&mut self, on: bool, _cx: &mut Context<Self>) {
        self.scan.regex_keyword = on;
    }
    /// The active key-type filter, if any.
    pub fn type_filter(&self) -> Option<KeyType> {
        self.scan.type_filter
    }
    /// Set whether to soft wrap the editor
    pub fn set_soft_wrap(&mut self, soft_wrap: bool, cx: &mut Context<Self>) {
        self.prefs.set_soft_wrap(soft_wrap);
        cx.emit(ServerEvent::SoftWrapToggled(self.prefs.soft_wrap()));
    }
    /// Get the current query mode (All/Prefix/Exact)
    pub fn query_mode(&self) -> QueryMode {
        self.scan.query_mode
    }

    /// The keyword the current key scan was started with (empty = full scan).
    pub fn keyword(&self) -> SharedString {
        self.scan.keyword.clone()
    }

    /// Check if the current scan has completed
    pub fn scan_completed(&self) -> bool {
        self.scan.scan_completed
    }

    /// Check if a scan is currently in progress
    pub fn scanning(&self) -> bool {
        self.scan.scanning
    }

    /// Prefixes whose lazy folder-scan is currently in flight (form
    /// `"{folder_id}:"`). The key tree reads this to show an inline
    /// spinner on the matching folder row.
    pub fn scanning_prefixes(&self) -> &AHashSet<SharedString> {
        &self.scan.scanning_prefixes
    }

    /// Prefixes whose folder scan stopped at the page cap with more keys
    /// still on the server (form `"{folder_id}:"`). The key tree shows an
    /// inline "Load more" row for each so the user can resume the scan.
    pub fn incomplete_prefix_set(&self) -> AHashSet<SharedString> {
        self.scan.incomplete_prefixes.keys().cloned().collect()
    }

    /// Get the total database size (number of keys)
    pub fn dbsize(&self) -> Option<u64> {
        self.readings.dbsize()
    }

    /// How many keys the tree lists: the loaded ones, less what a filter of
    /// the tree's own hides. The first number of the status bar's
    /// "listed / in the database" — with a keyword it always was the keys
    /// that matched, and with a tag or a TTL filter it stayed at everything
    /// loaded, 25/25 beside a tree of one key (#189).
    pub fn listed_count(&self) -> usize {
        self.scan.listed_count()
    }

    /// The tree's report after a build: `listed` keys of the ones loaded
    /// when it started. `None` when it lists them all, so that the count
    /// follows a scan page by page instead of a build behind it.
    pub fn set_listed_keys(&mut self, listed: Option<usize>, cx: &mut Context<Self>) {
        if self.scan.listed_keys != listed {
            self.scan.listed_keys = listed;
            cx.emit(ServerEvent::KeysListed);
        }
    }

    /// Get the last measured latency to the server
    pub fn redis_info(&self) -> Option<&RedisInfo> {
        self.readings.redis_info()
    }

    /// The link problem to show in place of the workspace's content, if the
    /// link is why there is none — see [`LinkProblem`].
    pub fn link_problem(&self) -> Option<LinkProblem> {
        if self.server_id.is_empty() {
            return None;
        }
        self.link.problem()
    }

    /// Remember what the link failed with, for [`Self::link_problem`].
    /// TLS-aware, like the status bar's reason: a link dropped on a
    /// plaintext connection points at the TLS toggle.
    pub(crate) fn note_link_failure(&mut self, error: &Error) {
        let tls_enabled = get_server(&self.server_id)
            .map(|server| server.tls.unwrap_or(false))
            .unwrap_or(false);
        let kind = error.connection_kind_tls_aware(tls_enabled);
        self.link.note_failure(kind, error.to_string().into());
    }

    /// Current live-connection health (online / reconnecting / offline),
    /// updated each heartbeat tick by `note_ping_result`.
    pub fn connection_health(&self) -> ConnectionHealth {
        self.link.connection_health
    }

    /// Why the last heartbeat failed, for the offline tooltip. `Unknown` when
    /// healthy or when the driver didn't give us enough to classify.
    pub fn last_connection_error(&self) -> ConnectionErrorKind {
        self.link.last_connection_error
    }

    /// Get the slow logs
    pub fn slow_logs(&self) -> &Vec<SlowLogEntry> {
        self.readings.slow_logs()
    }
    /// Which version of [`Self::slow_logs`] this is — it changes exactly
    /// when the log does.
    pub fn slow_logs_generation(&self) -> u64 {
        self.readings.slow_logs_generation()
    }
    /// Drop the cached slow-log entries and notify listeners — called
    /// after a `SLOWLOG RESET` so the panel empties immediately instead
    /// of waiting for the next heartbeat refresh.
    pub fn clear_slow_logs(&mut self, cx: &mut Context<Self>) {
        self.readings.clear_slow_logs();
        cx.emit(ServerEvent::ServerRedisInfoUpdated);
    }

    /// `(processed, total)` slots of the in-flight reshard, `None` when idle.
    pub fn reshard_progress(&self) -> Option<(u32, u32)> {
        self.reshard_progress
    }

    /// The Server Info panel's frozen `INFO` capture, if one was taken
    /// this session.
    pub fn info_snapshot(&self) -> Option<&InfoSnapshot> {
        self.info_snapshot.as_ref()
    }
    /// Store or discard the Server Info panel's `INFO` capture. No event
    /// is emitted — the panel is the only consumer and repaints itself.
    pub fn set_info_snapshot(&mut self, snapshot: Option<InfoSnapshot>) {
        self.info_snapshot = snapshot;
    }
    /// Update the in-flight reshard progress and broadcast it so the
    /// Topology panel can re-render its progress bar.
    pub fn set_reshard_progress(&mut self, progress: Option<(u32, u32)>, cx: &mut Context<Self>) {
        self.reshard_progress = progress;
        cx.emit(ServerEvent::ClusterReshardProgress);
    }
    /// Get the last slow log count
    pub fn last_slow_log_count(&self) -> usize {
        self.readings.last_slow_log_count()
    }

    /// Get cluster node counts (master, replica)
    pub fn nodes(&self) -> (usize, usize) {
        self.facts.nodes()
    }
    /// Get the description of the nodes
    pub fn nodes_description(&self) -> Arc<RedisClientDescription> {
        self.facts.nodes_description()
    }

    /// Get the Redis server version string
    pub fn version(&self) -> &str {
        self.facts.version()
    }

    /// The one version gate: both flavors decided by a [`Floor`] from
    /// `floors.rs` — Valkey's version numbers run ahead of Redis's, so a
    /// bare floor written for Redis is wrong on Valkey.
    pub fn supports(&self, floor: Floor) -> bool {
        self.facts.supports(floor)
    }

    /// Per-field hash TTL (`HEXPIRE / HTTL / HPERSIST`) — Redis 7.4,
    /// Valkey 9.0; see [`floors::HASH_FIELD_TTL`].
    pub fn supports_hash_field_ttl(&self) -> bool {
        self.supports(floors::HASH_FIELD_TTL)
    }

    /// `XACKDEL` / `XDELEX` and the `XTRIM`/`XADD` reference-policy words
    /// — Redis 8.2, never Valkey; see [`floors::STREAM_REF_POLICIES`].
    pub fn supports_stream_ref_policies(&self) -> bool {
        self.supports(floors::STREAM_REF_POLICIES)
    }

    /// `XNACK` — release a pending entry without acking (Redis 8.8, never
    /// Valkey); see [`floors::STREAM_NACK`].
    pub fn supports_stream_nack(&self) -> bool {
        self.supports(floors::STREAM_NACK)
    }

    /// `XGROUP CREATECONSUMER` (Redis 6.2); see
    /// [`floors::STREAM_CREATE_CONSUMER`].
    pub fn supports_stream_create_consumer(&self) -> bool {
        self.supports(floors::STREAM_CREATE_CONSUMER)
    }

    /// The ACL family (`ACL USERS / GETUSER / SETUSER / WHOAMI`) — drives
    /// Tools-menu visibility so older servers don't see a dead entry; see
    /// [`floors::ACL`].
    pub fn supports_acl(&self) -> bool {
        self.supports(floors::ACL)
    }

    /// Functions (`FUNCTION LIST / LOAD / DELETE / FCALL`) — see
    /// [`floors::FUNCTIONS`].
    pub fn supports_functions(&self) -> bool {
        self.supports(floors::FUNCTIONS)
    }

    /// Sharded Pub/Sub (`SSUBSCRIBE` / `SPUBLISH`) — gates the Sharded
    /// toggle in the Pub/Sub panel; see [`floors::SHARDED_PUBSUB`].
    pub fn supports_sharded_pubsub(&self) -> bool {
        self.supports(floors::SHARDED_PUBSUB)
    }

    /// `EXPIRE … NX | XX | GT | LT` — gates the condition radios in the
    /// batch-TTL dialog; see [`floors::EXPIRE_CONDITIONS`].
    pub fn supports_expire_conditions(&self) -> bool {
        self.supports(floors::EXPIRE_CONDITIONS)
    }

    /// Whether the Topology panel has something to show: Cluster and
    /// Sentinel get their node views, Standalone its primary / replica pair
    /// (ADR 6) — only a server whose type is not known yet is left out.
    /// Compares the `ServerType` Debug repr surfaced on the description (the
    /// enum itself is crate-private to the connection layer).
    pub fn supports_topology(&self) -> bool {
        matches!(
            self.nodes_description().server_type.as_str(),
            "Cluster" | "Sentinel" | "Standalone"
        )
    }

    /// Whether the server is a Cluster: many nodes, so what needs one
    /// connection to one node (the Lua debugger) is not offered.
    pub fn is_cluster(&self) -> bool {
        self.nodes_description().server_type == "Cluster"
    }

    /// Get the currently selected server id
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    /// Where this state's operations run: the selected database of the
    /// selected server, as the connection layer takes it (ADR 10).
    /// [`Self::at`] carrying the confirmation a dialog just collected, for
    /// an operation that only runs behind one (`ServerDb::confirmed`).
    pub fn at_confirmed(&self) -> ServerDb {
        let name = get_server(&self.server_id).map(|s| s.name).unwrap_or_default();
        self.at().confirmed(name)
    }

    pub fn at(&self) -> ServerDb {
        ServerDb::new(self.server_id.as_str(), self.db)
    }
    /// Get the currently selected database
    pub fn db(&self) -> usize {
        self.db
    }

    /// Key-tree / prefix separator for the active connection.
    /// Never empty — defaults to `":"`.
    pub fn key_separator(&self) -> &str {
        self.prefs.key_separator()
    }

    /// SCAN page size for the active connection (never below the floor).
    pub fn key_scan_count(&self) -> usize {
        self.prefs.key_scan_count()
    }

    /// Max key-tree nesting depth for the active connection.
    pub fn max_key_tree_depth(&self) -> usize {
        self.prefs.max_key_tree_depth()
    }

    /// Auto-expand threshold for the active connection.
    pub fn auto_expand_threshold(&self) -> usize {
        self.prefs.auto_expand_threshold()
    }

    /// Whether TTL chips are shown in the key tree for this connection.
    pub fn show_key_tree_ttl(&self) -> bool {
        self.prefs.show_key_tree_ttl()
    }

    /// Get the total number of databases
    pub fn databases(&self) -> usize {
        self.facts.databases()
    }

    /// Get whether to soft wrap the editor
    pub fn soft_wrap(&self) -> bool {
        self.prefs.soft_wrap()
    }

    /// Get the currently selected key name
    pub fn key(&self) -> Option<SharedString> {
        self.open.key.clone()
    }
    /// Shared handle to the TTL map for background snapshots (the key-tree
    /// build) — an Arc clone instead of copying the whole map.
    pub fn key_ttls_arc(&self) -> Arc<AHashMap<SharedString, i64>> {
        Arc::clone(&self.scan.key_ttls)
    }
    /// The loaded keys. A clone is a pointer copy (see [`LoadedKeys`]),
    /// which is how a background build should take them.
    pub fn keys(&self) -> &LoadedKeys {
        &self.scan.keys
    }

    /// Get the value data for the currently selected key
    pub fn value(&self) -> Option<&RedisValue> {
        self.open.value.as_ref()
    }

    /// Put `value` on screen as if it had been loaded: a view test has no
    /// server to load one from.
    #[cfg(test)]
    pub(crate) fn show_value_for_test(&mut self, value: RedisValue) {
        self.open.value = Some(value);
    }

    /// Get the in-memory value history for `key` (newest first), if any.
    /// Returns `None` rather than an empty slice so callers can easily
    /// distinguish "never written" from "written and rolled back to 0".
    pub fn value_history_for(&self, key: &SharedString) -> Option<&VecDeque<ValueHistoryEntry>> {
        self.open.histories.value_history_for(key)
    }

    /// Append the bytes about to be overwritten to the history ring buffer.
    /// Called from the SET paths in value.rs right before mutating state.
    /// Empty `bytes` are still recorded — "was empty" is itself meaningful
    /// history to roll back to. A version over
    /// `VALUE_HISTORY_MAX_VERSION_BYTES` is not kept.
    pub(super) fn push_value_history(&mut self, key: SharedString, bytes: Bytes) {
        self.open.histories.push_value(key, bytes, unix_ts());
    }

    /// The session change log for `key` (oldest first), if anything was
    /// recorded.
    pub fn change_log_for(&self, key: &SharedString) -> Option<&VecDeque<ChangeEntry>> {
        self.open.histories.change_log_for(key)
    }

    /// Append `entries` to `key`'s change log. Called from each collection
    /// write's success callback — never before the server confirmed it, so
    /// a refused write never shows up as a change.
    pub(super) fn record_changes(&mut self, key: SharedString, entries: Vec<ChangeEntry>) {
        self.open.histories.record_changes(key, entries, unix_ts());
    }

    /// Drop the history of every key not used for a week
    /// (`HISTORY_IDLE_EXPIRY_SECS`). Driven by the root's periodic cleanup
    /// loop, so an idle Zedis gives the memory back without any write
    /// having to happen first.
    pub fn sweep_expired_histories(&mut self, cx: &mut Context<Self>) {
        // The toolbar gates its diff and change-log entries on these
        // histories, so a sweep that dropped one has to re-render it.
        if self.open.histories.sweep_expired(unix_ts()) {
            cx.notify();
        }
    }

    pub fn set_search_history(&mut self, history: Vec<SharedString>) {
        self.scan.search_history = history.into_iter().collect();
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
        let retry_failed = same_target && matches!(self.link.server_status, RedisServerStatus::Failed);
        if !same_target || retry_failed {
            self.link.start_load();
            // The metrics history is the server's, whichever database is
            // open: a database switch or a retry keeps it (it used to empty
            // the charts), another server drops the last one's.
            if self.server_id != server_id {
                get_metrics_cache().remove_server(self.server_id.as_str());
            }
            // A retry of the same target is a reconnect: an open write
            // window is still the user's. `reset` closes windows because one
            // belongs to the server it was opened on, which this still is —
            // closing it made every reconnect re-lock the entry.
            let open_window = retry_failed.then(|| self.lock.clone());
            // And what the link failed with stays on screen while it is
            // tried again.
            let link_failure = if retry_failed {
                self.link.link_failure.take()
            } else {
                None
            };
            // And what this session's panels found is still this server's.
            let findings = retry_failed.then(|| std::mem::take(&mut self.report_findings));
            self.reset(cx);
            if let Some(findings) = findings {
                self.report_findings = findings;
            }
            if let Some(earlier) = open_window {
                self.lock.keep_window_from(&earlier);
            }
            self.link.link_failure = link_failure;
            self.server_id = server_id.clone();
            self.db = db;
            // Cached matrix from an earlier connect to this server (or the
            // optimistic default); the background probe refines it below.
            self.facts.set_features(get_server_features(server_id.as_str()));
            // Resolve key-tree / scan prefs: per-server override, else Settings.
            let settings = cx.global::<ZedisGlobalStore>().read(cx);
            self.prefs
                .resolve_tree(get_server(server_id.as_str()).ok().as_ref(), settings);

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
            self.scan.query_mode = query_mode;
            self.prefs.set_soft_wrap(soft_wrap);

            debug!(server_id = self.server_id.as_str(), "Selecting server");
            // In the browser, the tags and notes other accounts set here
            // since the last round, before the tree is read (ADR 17).
            ask_for_fresh_page_data();
            let search_history_manager = get_search_history_manager();
            if let Ok(history) = search_history_manager.records(server_id.as_str()) {
                self.scan.search_history = history.into_iter().map(Into::into).collect();
            }
            cx.emit(ServerEvent::ServerSelected(server_id));

            // Set loading state
            self.link.server_status = RedisServerStatus::Loading;
            self.scan.scanning = true;
            cx.notify();

            let server_id_clone = self.server_id.clone();
            let counting_server_id = server_id_clone.clone();
            let db = self.db;

            self.spawn(
                ServerTask::SelectServer,
                move || async move {
                    // Connects (or reuses the pooled client) and gathers the
                    // server's metadata.
                    Ok(server_summary(&ServerDb::new(server_id_clone.as_str(), db)).await?)
                },
                move |this, result, cx| {
                    // Ignore if user switched to a different server while loading
                    if this.server_id != counting_server_id {
                        return;
                    }

                    match result {
                        Ok(ServerSummary {
                            dbsize,
                            nodes,
                            description: nodes_description,
                            version,
                            databases,
                            access_mode,
                            supports_rejson,
                            supports_search,
                        }) => {
                            this.link.load_retries = 0;
                            this.link.link_failure = None;
                            this.readings.set_dbsize(dbsize);
                            this.facts.set_topology(nodes, nodes_description);
                            #[cfg(not(target_family = "wasm"))]
                            this.note_sentinel_master_choice(cx);
                            this.facts
                                .note_connect(version.into(), databases, supports_rejson, supports_search);
                            // Stamp the connection for the server list's
                            // "last used" order. Once per successful connect,
                            // and the write itself is a background task.
                            let sid = this.server_id.to_string();
                            if let Ok(mut option) = get_session_option(&sid) {
                                option.last_connected_at = Some(unix_ts());
                                save_session_option(&sid, option, cx);
                            }
                            // A locked entry starts in SafeMode — unless this
                            // connect is a reconnect inside an open window.
                            let access_mode = if access_mode == AccessMode::ReadWrite
                                && this.write_locked()
                                && this.unlock_remaining().is_none()
                            {
                                AccessMode::SafeMode
                            } else {
                                access_mode
                            };
                            this.lock.set_access_mode(access_mode);
                            this.probe_features(false, cx);

                            let server_id = this.server_id.clone();
                            this.link.server_status = RedisServerStatus::Idle;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            let is_exact_mode = this.scan.query_mode == QueryMode::Exact;
                            if is_exact_mode {
                                this.scan.scanning = false;
                            }
                            cx.notify();
                            // One-time onboarding toast on the very first
                            // successful connection: point at the two global
                            // entry points (palette, cross-db search) a new
                            // user won't discover from the key tree alone.
                            if !cx
                                .global::<ZedisGlobalStore>()
                                .read(cx)
                                .hint_dismissed(HINT_FIRST_CONNECT)
                            {
                                update_app_state_and_save_quiet(cx, "dismiss_hint_first_connect", |state, _| {
                                    state.dismiss_hint(HINT_FIRST_CONNECT)
                                });
                                let message = first_connect_hint(cx);
                                this.emit_info_notification(message, cx);
                            }
                            // Auto-scan keys if not exact mode — and not on a
                            // server already known to lack SCAN (the key tree
                            // shows its open-by-name banner instead).
                            if !is_exact_mode && this.command_block(ServerCommand::Scan).is_none() {
                                this.scan_keys(server_id, SharedString::default(), cx);
                            }
                        }
                        Err(error) => {
                            this.note_link_failure(&error);
                            this.link.note_load_failure(error.connection_kind());
                            // Connection / metadata load failed (e.g. network
                            // down). Mark Failed so a later re-select retries
                            // instead of being swallowed by select()'s
                            // same-server guard. The error toast was already
                            // emitted in spawn_with_arg; skip the key scan
                            // since it would just fail again with a 2nd toast.
                            this.link.server_status = RedisServerStatus::Failed;
                            this.scan.scanning = false;
                            cx.emit(ServerEvent::ServerInfoUpdated);
                            cx.notify();
                        }
                    }
                },
                cx,
            );
        }
    }

    /// Run the load again that the link failed, now that the heartbeat has
    /// an answer from a server with its dataset in memory. Called on every
    /// such beat; does nothing unless a load is owed.
    pub(super) fn retry_failed_load(&mut self, cx: &mut Context<Self>) {
        if !self.link.take_load_retry() {
            return;
        }
        info!(
            server_id = self.server_id.as_str(),
            attempt = self.link.load_retries,
            "the server answers: loading what the failed connect could not"
        );
        let server_id = self.server_id.clone();
        let db = self.db;
        self.select(server_id, db, cx);
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
        self.link.server_status = RedisServerStatus::Failed;
        self.select(server_id, db, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{init_ui, record_events, take_events};
    use gpui::TestAppContext;

    fn batch(keys: &[(&str, &str)]) -> Vec<(SharedString, SharedString, i64)> {
        keys.iter()
            .map(|(k, t)| (SharedString::from(k.to_string()), SharedString::from(t.to_string()), -1))
            .collect()
    }

    fn key_names(state: &ZedisServerState) -> Vec<String> {
        let mut names: Vec<String> = state.scan.keys.keys().map(|k| k.to_string()).collect();
        names.sort();
        names
    }

    /// What the key tree relies on: the loaded keys come out in key order,
    /// and a snapshot a build took is untouched by the pages that land
    /// while it runs.
    #[gpui::test]
    fn a_snapshot_of_the_loaded_keys_is_sorted_and_stays_as_taken(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, _cx| {
            let names = |keys: &LoadedKeys| keys.keys().map(|k| k.to_string()).collect::<Vec<_>>();
            state.extend_keys(batch(&[("b", "string"), ("a", "hash")]));
            let snapshot = state.keys().clone();
            state.extend_keys(batch(&[("c", "set"), ("a0", "list"), ("b", "string")]));
            assert_eq!(names(&snapshot), ["a", "b"]);
            assert_eq!(names(state.keys()), ["a", "a0", "b", "c"]);
            assert_eq!(state.keys().get("a0"), Some(&KeyType::List));
        });
    }

    /// A later page may know a key only as `Unknown` (a type lookup that
    /// failed) or name another type for it: the known type stays. An
    /// `Unknown` is replaced as soon as a page knows better.
    #[gpui::test]
    fn a_known_type_is_kept_and_an_unknown_one_is_filled_in(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, _cx| {
            state.extend_keys(batch(&[("known", "hash"), ("unknown", "")]));
            let id = state.key_tree_id().to_string();
            state.extend_keys(batch(&[("known", ""), ("unknown", "set")]));
            state.extend_keys(batch(&[("known", "string")]));
            assert_eq!(state.keys().get("known"), Some(&KeyType::Hash));
            assert_eq!(state.keys().get("unknown"), Some(&KeyType::Set));
            assert_eq!(state.keys().len(), 2);
            assert_eq!(state.key_tree_id(), id, "no key was added, so the tree is the same");
        });
    }

    /// ⌘R must not blank the tree: the rows stay up for the whole round trip
    /// and the first batch of the re-scan is what replaces them — including
    /// dropping a key that has since been deleted on the server.
    #[gpui::test]
    fn a_refresh_keeps_the_rows_until_the_first_batch_replaces_them(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, _cx| {
            state.extend_keys(batch(&[("a", "string"), ("b", "hash")]));
        });

        state.update(cx, |state, cx| {
            state.begin_refresh_scan(cx);
            assert_eq!(key_names(state), ["a", "b"], "rows stay up while the scan runs");
            assert!(state.scan.keys_superseded_by_next_batch);

            // "b" was deleted on the server since the last scan.
            state.extend_keys(batch(&[("a", "string"), ("c", "list")]));
            assert_eq!(key_names(state), ["a", "c"]);
            assert!(
                !state.scan.keys_superseded_by_next_batch,
                "spent once; the scan's later pages append"
            );

            state.extend_keys(batch(&[("d", "set")]));
            assert_eq!(key_names(state), ["a", "c", "d"]);
        });
    }

    /// An empty first batch still takes the old rows out — a refresh of a
    /// database someone just flushed has to end up empty, not unchanged.
    #[gpui::test]
    fn a_refresh_that_finds_nothing_empties_the_tree(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.extend_keys(batch(&[("a", "string")]));
            let before = state.scan.key_tree_id.clone();
            state.begin_refresh_scan(cx);
            state.extend_keys(Vec::new());
            assert!(key_names(state).is_empty());
            assert_ne!(state.scan.key_tree_id, before, "the tree has to rebuild for that");
        });
    }

    /// When a filter narrows a multi-page refresh and an intermediate page
    /// matches zero keys while cursors remain, the old rows stay on screen
    /// until a matching batch arrives (or the scan finishes empty).
    #[gpui::test]
    fn a_refresh_waits_past_an_empty_intermediate_page_before_replacing_rows(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.extend_keys(batch(&[("a", "string"), ("b", "hash")]));
            let before = state.scan.key_tree_id.clone();
            state.begin_refresh_scan(cx);
            assert_eq!(state.scan.scanned_len(), 0);

            // Page 1 matched 0 keys, but the cursor is not back at 0 yet.
            state.scan.cursors = Some(vec![42]);
            state.extend_keys(Vec::new());
            assert_eq!(key_names(state), ["a", "b"], "rows stay up while paging continues");
            assert!(state.scan.keys_superseded_by_next_batch);
            assert_eq!(state.scan.scanned_len(), 0);
            assert_eq!(state.scan.key_tree_id, before);

            // Page 2 brings the first matches and supersedes the old rows.
            state.scan.cursors = None;
            state.extend_keys(batch(&[("c", "list")]));
            assert_eq!(key_names(state), ["c"]);
            assert!(!state.scan.keys_superseded_by_next_batch);
            assert_eq!(state.scan.scanned_len(), 1);
            assert_ne!(state.scan.key_tree_id, before);
        });
    }

    /// Lazy-loaded folder prefixes saved by a refresh are re-scanned only
    /// when the folder and all of its ancestors remain expanded on screen.
    #[gpui::test]
    fn a_refresh_reloads_only_lazy_prefixes_whose_folder_chain_stays_open(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.link.manually_offline = true;
            state.link.last_offline_notice = unix_ts();
            state.scan.loaded_prefixes.extend([
                SharedString::from("open:sub:"),
                SharedString::from("closed:"),
                SharedString::from("collapsed_parent:child:"),
            ]);
            state.begin_refresh_scan(cx);
            assert!(state.scan.loaded_prefixes.is_empty());
            assert_eq!(state.scan.refresh_prefixes.len(), 3);

            let expanded: AHashSet<SharedString> = [
                SharedString::from("open"),
                SharedString::from("open:sub"),
                SharedString::from("collapsed_parent:child"),
            ]
            .into_iter()
            .collect();
            state.reload_expanded_prefixes(&expanded, cx);
            assert_eq!(
                state.scan.scanning_prefixes,
                [SharedString::from("open:sub:")].into_iter().collect()
            );
        });
    }

    /// A new query is not a refresh: its rows answer the old question and go
    /// at once, with nothing left owing to a later batch.
    #[gpui::test]
    fn a_new_query_clears_the_rows_up_front(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.extend_keys(batch(&[("a", "string")]));
            state.reset_scan(cx);
            assert!(key_names(state).is_empty());
            assert!(!state.scan.keys_superseded_by_next_batch);
        });
    }

    /// Every query mode that scans has to leave `keyword` saying what the
    /// tree is showing. A prefix search used to leave it empty, and the three
    /// readers of it — the search box mirror, auto-refresh and "load more" —
    /// each took that to mean "no filter".
    #[gpui::test]
    fn a_prefix_query_records_what_the_tree_is_showing(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            // Nothing here wants the scan itself, only what it records
            // before handing off, so stop it at the offline guard rather than
            // let it dial a server that does not exist. The guard's one-per-
            // 3s notice needs a global store this test has no use for — a
            // fresh timestamp keeps it throttled.
            state.link.manually_offline = true;
            state.link.last_offline_notice = unix_ts();
            for mode in [QueryMode::Prefix, QueryMode::All] {
                state.set_query_mode(mode, cx);
                state.handle_filter("user:".into(), false, cx);
                assert_eq!(state.keyword(), "user:", "{mode:?}");
                assert!(state.scanning(), "{mode:?} shows the toolbar it is working");
            }
        });
    }
    /// Load more runs one round at a time. A press while a round is in flight
    /// adds nothing — it used to start a second paging chain over the same
    /// cursors — and a press the offline guard refuses leaves `scanning`
    /// The link panel replaces the empty state while the link is why there
    /// is nothing to show — a failed load, a dead heartbeat, a manual
    /// disconnect — and stays up through the retries of a failed connect.
    #[test]
    fn a_down_link_is_a_problem_to_show_and_a_working_one_is_not() {
        use ConnectionHealth as H;
        use RedisServerStatus as S;
        let refused = (ConnectionErrorKind::Network, SharedString::from("Connection refused"));
        let failed = |retrying| {
            Some(LinkProblem::Failed {
                kind: ConnectionErrorKind::Network,
                detail: "Connection refused".into(),
                retrying,
            })
        };
        // Connected, or still on the first connect: nothing to say.
        assert_eq!(link_problem_for(&S::Idle, H::Connected, false, None), None);
        assert_eq!(link_problem_for(&S::Loading, H::Unknown, false, None), None);
        // The connect failed; the heartbeat then keeps failing.
        assert_eq!(
            link_problem_for(&S::Failed, H::Unknown, false, Some(&refused)),
            failed(false)
        );
        assert_eq!(
            link_problem_for(&S::Failed, H::Offline, false, Some(&refused)),
            failed(false)
        );
        // A retry in flight keeps the panel, marked as trying.
        assert_eq!(
            link_problem_for(&S::Loading, H::Unknown, false, Some(&refused)),
            failed(true)
        );
        // A link that dropped after a good load, with no error kept.
        assert_eq!(
            link_problem_for(&S::Idle, H::Reconnecting, false, None),
            Some(LinkProblem::Failed {
                kind: ConnectionErrorKind::Unknown,
                detail: SharedString::default(),
                retrying: false,
            })
        );
        // The user's own disconnect is not a failure.
        assert_eq!(
            link_problem_for(&S::Idle, H::Offline, true, Some(&refused)),
            Some(LinkProblem::Disconnected)
        );
    }

    /// Read-only refuses a typed write, a write-locked entry asks first, and
    /// only a connection that may write lets it through.
    #[test]
    fn a_typed_write_meets_the_connection_s_access_mode() {
        assert_eq!(write_gate_for(AccessMode::ReadWrite, false), WriteGate::Open);
        // Inside an unlock window a locked entry is ReadWrite.
        assert_eq!(write_gate_for(AccessMode::ReadWrite, true), WriteGate::Open);
        assert_eq!(write_gate_for(AccessMode::SafeMode, true), WriteGate::Locked);
        assert_eq!(write_gate_for(AccessMode::SafeMode, false), WriteGate::ReadOnly);
        // No window to open for a user or an account that may not write.
        assert_eq!(write_gate_for(AccessMode::StrictReadOnly, true), WriteGate::ReadOnly);
        assert_eq!(write_gate_for(AccessMode::StrictReadOnly, false), WriteGate::ReadOnly);
    }

    /// A load the link failed is owed one retry per failure, only while it is
    /// still the failed load on screen, and not for ever.
    #[gpui::test]
    fn a_load_the_link_failed_is_retried_a_bounded_number_of_times(cx: &mut TestAppContext) {
        use ConnectionErrorKind as K;
        for kind in [
            K::Network,
            K::Timeout,
            K::Loading,
            K::Busy,
            K::ClusterDown,
            K::MasterDown,
            K::Tunnel,
        ] {
            assert!(heals_with_the_link(kind), "{kind:?} passes by itself");
        }
        // Nothing the server coming back would change.
        for kind in [K::Auth, K::Permission, K::Tls, K::ReadOnly, K::Unknown] {
            assert!(!heals_with_the_link(kind), "{kind:?} needs the user");
        }

        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, _| {
            assert!(!state.link.take_load_retry(), "nothing is owed");
            // Owed, but a load has succeeded since: not the failed one any more.
            state.link.load_retry_pending = true;
            state.link.server_status = RedisServerStatus::Idle;
            assert!(!state.link.take_load_retry());

            state.link.server_status = RedisServerStatus::Failed;
            assert!(state.link.take_load_retry());
            assert_eq!(state.link.load_retries, 1);
            assert!(state.link.load_retry_running, "the select that follows is a retry");
            assert!(!state.link.take_load_retry(), "one retry per failure");
        });
    }

    /// down, since no round would come back to lower it and the tree's search
    /// waits on it.
    #[gpui::test]
    fn load_more_starts_one_round_and_none_while_offline(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.link.manually_offline = true;
            state.link.last_offline_notice = unix_ts();
            state.scan_next(cx);
            assert!(!state.scanning(), "an offline press starts nothing");
            assert_eq!(state.scan.scan_times, 0);

            state.link.manually_offline = false;
            state.scan.scanning = true;
            state.scan_next(cx);
            assert_eq!(state.scan.scan_times, 0, "a press during a round adds no batch");
        });
    }

    /// A regex keyword is scanned, whichever query mode the box was left in
    /// (#167). The mode used to decide first: Prefix sent the pattern as a
    /// glob (`MATCH ^zedis:fn:session*`) and Exact looked it up as a key
    /// name, and both answered "No keys found" for keys that were there.
    /// Offline, so that the branch taken is what is under test, not a scan.
    #[gpui::test]
    fn a_regex_keyword_is_scanned_in_every_query_mode(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.link.manually_offline = true;
            state.link.last_offline_notice = unix_ts();
            let pattern = SharedString::from("^zedis:fn:session");

            state.scan.regex_keyword = true;
            for mode in [QueryMode::All, QueryMode::Prefix, QueryMode::Exact] {
                state.scan.query_mode = mode;
                state.handle_filter(pattern.clone(), false, cx);
                assert_eq!(state.keyword(), pattern, "{mode:?}: what the tree is showing");
                assert!(
                    state.scan.scanning_prefixes.is_empty(),
                    "{mode:?}: not sent as a prefix"
                );
                assert!(state.open.key.is_none(), "{mode:?}: not looked up as a key name");
            }

            // Without the toggle the mode decides, as it always did.
            state.scan.regex_keyword = false;
            state.scan.query_mode = QueryMode::Prefix;
            state.handle_filter(pattern.clone(), false, cx);
            assert!(state.scan.scanning_prefixes.contains(&pattern));
        });
    }

    /// The regex toggle belongs to the server it was switched on for: the
    /// key tree starts over with it off on a server switch, and a state that
    /// kept it on scanned the next keyword as a regex under a box that said
    /// substring.
    #[gpui::test]
    fn a_server_switch_turns_the_regex_keyword_off(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.scan.regex_keyword = true;
            state.reset(cx);
            assert!(!state.regex_keyword());
        });
    }

    /// A task's result belongs to the server and database it was started
    /// against. Once the user has moved on its callback must not run — it
    /// would write the previous target's data into the new one's state —
    /// and its failure is not theirs to be told about either. A success for
    /// the current target proves the link, so the heartbeat stops backing
    /// off.
    #[gpui::test]
    fn a_task_result_is_applied_only_to_the_target_it_was_started_against(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.server_id = "one".into();
            let run = state
                .begin_task(ServerTask::DeleteKey, "key".into(), cx)
                .expect("a connected state starts its tasks");

            state.link.heartbeat_retry_at = Some(Instant::now());
            assert!(state.settle_task(&run, None, cx));
            assert!(state.link.heartbeat_retry_at.is_none(), "a reply means the link is up");

            let failure = Error::Invalid {
                message: "gone".to_string(),
            };
            state.db = 1;
            assert!(!state.settle_task(&run, None, cx), "another database");
            state.db = 0;
            state.server_id = "two".into();
            assert!(!state.settle_task(&run, Some(&failure), cx), "another server");
            assert!(
                state.error_messages.read().is_empty(),
                "nor is its failure reported here"
            );
        });
    }

    /// A manually disconnected state starts nothing, and a load that had
    /// already shown its skeleton is marked failed rather than left spinning
    /// with no task to end it.
    #[gpui::test]
    fn no_task_starts_while_the_server_is_offline(cx: &mut TestAppContext) {
        let state = cx.new(|_| ZedisServerState::default());
        state.update(cx, |state, cx| {
            state.link.manually_offline = true;
            state.link.last_offline_notice = unix_ts();
            state.link.server_status = RedisServerStatus::Loading;
            state.scan.scanning = true;
            assert!(
                state
                    .begin_task(ServerTask::SelectServer, SharedString::default(), cx)
                    .is_none()
            );
            assert_eq!(state.link.server_status, RedisServerStatus::Failed);
            assert!(!state.scan.scanning);
        });
    }

    /// A new query empties the tree and says so — the rows went, then the
    /// tree changed; a refresh keeps its rows and says nothing until its
    /// first batch lands.
    #[gpui::test]
    fn a_new_query_announces_the_emptied_tree_and_a_refresh_does_not(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);
        state.update(cx, |state, cx| {
            state.extend_keys(batch(&[("a", "string")]));
            state.reset_scan(cx);
            assert!(state.keys().is_empty());
        });
        assert_eq!(take_events(&log), ["KeyScanReset", "KeyTreeUpdated"]);
        state.update(cx, |state, cx| {
            state.extend_keys(batch(&[("a", "string")]));
            let id = state.key_tree_id().to_string();
            state.begin_refresh_scan(cx);
            assert_eq!(state.key_tree_id(), id, "the rows stay");
            assert_eq!(state.keys().len(), 1);
        });
        assert!(take_events(&log).is_empty());
    }

    /// The write window (ADR 14), opened from SafeMode for its time and
    /// closed early — after which its timer says nothing — or by that timer,
    /// with a word to the user; toggled by hand; neither where writes are
    /// refused outright. Each change is announced as it happens.
    #[gpui::test]
    fn the_write_window_opens_closes_and_runs_out_in_order(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);
        let window = Duration::from_secs(WRITE_UNLOCK_SECS);

        state.update(cx, |state, cx| {
            state.toggle_readonly(cx);
            assert!(state.readonly() && !state.strict_readonly());
            assert_eq!(
                state.write_gate(),
                WriteGate::ReadOnly,
                "a SafeMode the entry did not lock"
            );
            state.unlock_writes(cx);
            assert!(!state.readonly());
            let left = state.unlock_remaining().expect("the window is open");
            assert!(left <= window && left + Duration::from_secs(1) > window);
        });
        cx.run_until_parked();
        assert_eq!(
            take_events(&log),
            ["ServerInfoUpdated", "TaskStarted(UnlockWrites)", "ServerInfoUpdated"]
        );

        state.update(cx, |state, cx| state.lock_writes(cx));
        cx.run_until_parked();
        assert_eq!(take_events(&log), ["ServerInfoUpdated", "TaskStarted(LockWrites)"]);
        state.read_with(cx, |state, _| {
            assert!(state.readonly());
            assert!(state.unlock_remaining().is_none());
        });
        cx.executor().advance_clock(window);
        cx.run_until_parked();
        state.update(cx, |state, cx| state.lock_writes(cx));
        cx.run_until_parked();
        assert!(
            take_events(&log).is_empty(),
            "a window closed by hand is not closed again by its timer, nor twice"
        );

        state.update(cx, |state, cx| state.unlock_writes(cx));
        cx.run_until_parked();
        take_events(&log);
        cx.executor().advance_clock(window);
        cx.run_until_parked();
        let relocked = cx.update(|cx| i18n_status_bar(cx, "relocked_message").replace("%{server}", ""));
        assert_eq!(
            take_events(&log),
            ["ServerInfoUpdated".to_string(), format!("notice Info: {relocked}")]
        );
        state.read_with(cx, |state, _| {
            assert!(state.readonly() && state.unlock_remaining().is_none())
        });

        state.update(cx, |state, cx| {
            state.toggle_readonly(cx);
            assert!(!state.readonly());
            state.lock.set_access_mode(AccessMode::StrictReadOnly);
            state.unlock_writes(cx);
            assert!(state.unlock_remaining().is_none());
            state.toggle_readonly(cx);
            assert!(state.strict_readonly());
            assert_eq!(state.write_gate(), WriteGate::ReadOnly);
        });
        cx.run_until_parked();
        let events = take_events(&log);
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(events[0], "ServerInfoUpdated");
        assert!(events[1].starts_with("notice Error: [toggle_readonly]"), "{events:?}");
        assert!(events[2].starts_with("ErrorOccurred("), "{events:?}");
    }

    /// What a connection's tree and editor go by is settled when it is
    /// selected: here from Settings, the entry not being in the list — the
    /// scan count as chosen, the defaults where nothing was — and the soft
    /// wrap from the session, on by default.
    #[gpui::test]
    fn a_selected_connection_takes_its_tree_settings_from_settings(cx: &mut TestAppContext) {
        init_ui(cx);
        let global = cx.update(|cx| cx.global::<ZedisGlobalStore>().state());
        let state = cx.new(|_| ZedisServerState::default());

        cx.update(|cx| {
            global.update(cx, |settings, _| {
                settings.set_key_scan_count(2_500);
                settings.set_max_key_tree_depth(7);
                settings.set_auto_expand_threshold(40);
                settings.set_show_key_tree_ttl(false);
            })
        });
        state.update(cx, |state, cx| state.select("prefstest-a".into(), 0, cx));
        state.read_with(cx, |state, _| {
            assert_eq!(state.key_separator(), ":");
            assert_eq!(state.key_scan_count(), 2_500);
            assert_eq!(state.max_key_tree_depth(), 7);
            assert_eq!(state.auto_expand_threshold(), 40);
            assert!(!state.show_key_tree_ttl());
            assert!(state.soft_wrap());
        });
        cx.run_until_parked();

        cx.update(|cx| {
            global.update(cx, |settings, _| {
                settings.set_key_scan_count(0);
                settings.set_max_key_tree_depth(0);
                settings.set_auto_expand_threshold(0);
                settings.set_show_key_tree_ttl(true);
            })
        });
        state.update(cx, |state, cx| state.select("prefstest-b".into(), 0, cx));
        state.read_with(cx, |state, _| {
            assert_eq!(state.key_scan_count(), DEFAULT_KEY_SCAN_COUNT);
            assert_eq!(state.max_key_tree_depth(), 5);
            assert_eq!(state.auto_expand_threshold(), 100);
            assert!(state.show_key_tree_ttl());
        });
        cx.run_until_parked();
    }

    /// A forced probe drops what was known and says so before it asks
    /// again; an unforced one asks only while nothing was ever probed; a
    /// state without a server asks nothing.
    #[gpui::test]
    fn a_forced_probe_forgets_the_matrix_before_it_asks(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);

        state.update(cx, |state, cx| state.probe_features(true, cx));
        cx.run_until_parked();
        assert!(take_events(&log).is_empty());

        state.update(cx, |state, cx| {
            state.server_id = "factstest".into();
            state.probe_features(true, cx);
        });
        cx.run_until_parked();
        assert_eq!(take_events(&log), ["FeaturesProbed", "TaskStarted(ProbeFeatures)"]);

        state.update(cx, |state, cx| state.probe_features(false, cx));
        cx.run_until_parked();
        assert_eq!(take_events(&log), ["TaskStarted(ProbeFeatures)"]);

        state.update(cx, |state, cx| {
            let mut probed = ServerFeatures::default();
            probed.probed = true;
            state.facts.set_features(Arc::new(probed));
            state.probe_features(false, cx);
        });
        cx.run_until_parked();
        assert!(take_events(&log).is_empty());
    }

    /// Emptying the slow log is a new generation of it, announced; a pause
    /// this app sent shows until it is lifted.
    #[gpui::test]
    fn a_cleared_slow_log_is_a_new_generation_and_a_pause_shows_until_lifted(cx: &mut TestAppContext) {
        init_ui(cx);
        let state = cx.new(|_| ZedisServerState::default());
        let log = record_events(cx, &state);
        state.update(cx, |state, cx| {
            let entry = SlowLogEntry {
                id: 0,
                timestamp: 1,
                duration: Duration::ZERO,
                amount: 0,
                args: Vec::new(),
                client_addr: None,
                client_name: None,
            };
            state.readings.note_slow_logs(vec![entry; 4], 0);
            assert_eq!(state.last_slow_log_count(), 4);
            let generation = state.slow_logs_generation();
            state.clear_slow_logs(cx);
            assert_eq!(state.slow_logs_generation(), generation + 1);
            assert_eq!(state.last_slow_log_count(), 0);
            assert!(state.slow_logs().is_empty());

            assert!(state.client_pause().is_none());
            state.note_client_pause(PauseMode::Write, 60_000, cx);
            let pause = state.client_pause().expect("the pause this app sent");
            assert_eq!(pause.actions, "write");
            assert!(!pause.from_server);
            state.note_client_unpause(cx);
            assert!(state.client_pause().is_none());
        });
        cx.run_until_parked();
        assert_eq!(take_events(&log), ["ServerRedisInfoUpdated"]);
    }
}

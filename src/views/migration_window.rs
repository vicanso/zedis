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

//! Modal-style window for export / import / server-to-server copy jobs.
//!
//! Supports explicit key lists and streaming `SCAN` by multi-pattern + type
//! filter, resumable job checkpoints in `zedis-db`, conflict strategy
//! selection (Skip / Overwrite / Abort), dry-run conflict preview, post-copy
//! verification with one-click jump to Compare, and best-effort live sync
//! follow mode via keyspace notifications.

use super::export::{CopyPreset, ExportSource, dirs_default_directory};
use crate::connection::{
    ChannelSubscription, ConflictMode, ConflictPreview, CopyVerificationReport, DangerKind, SERVER_TYPE_CLUSTER,
    ServerDb, SubscribeKind, config_set, copy_keys_batch, delete_keys, get_keyspace_events_config, get_server,
    get_servers, preview_import_conflicts, preview_key_conflicts, verify_copied_keys,
};
use crate::helpers::with_app_identity;
use crate::states::{
    ExportFormat, KeySource, LogStatus, MigrationCheckpoint, MigrationCheckpointJob, MigrationEvent, MigrationJob,
    MigrationPhase, MigrationState, MigrationThrottle, ZedisGlobalStore, delete_migration_checkpoint, i18n_copy,
    i18n_migration, load_migration_checkpoints,
};
use crate::views::{confirm_dangerous_command, open_compare_for_key};
use chrono::Utc;
use gpui::{
    App, Bounds, Entity, FocusHandle, Focusable, KeyDownEvent, SharedString, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Root, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    radio::RadioGroup,
    scroll::ScrollableElement,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::error;

mod sections;

use sections::JobLook;

const WINDOW_WIDTH: f32 = 760.0;
const WINDOW_HEIGHT: f32 = 680.0;
const PREVIEW_SAMPLE_LIMIT: usize = 40;
pub(super) const VERIFY_MISMATCH_DISPLAY_LIMIT: usize = 15;

/// Selectable Redis key types for pattern-based export / copy.
pub(super) const PATTERN_KEY_TYPES: [(&str, &str); 7] = [
    ("string", "string"),
    ("hash", "hash"),
    ("list", "list"),
    ("set", "set"),
    ("zset", "zset"),
    ("stream", "stream"),
    ("ReJSON-RL", "json"),
];

/// Where an export goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    File,
    /// Another server / db, straight through DUMP / RESTORE.
    Server,
}

/// Whether Export / Copy operates on the pre-loaded key list or streams via `SCAN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum KeySourceMode {
    #[default]
    LoadedKeys,
    ScanPatterns,
}

impl KeySourceMode {
    pub(super) fn index(self) -> usize {
        match self {
            Self::LoadedKeys => 0,
            Self::ScanPatterns => 1,
        }
    }

    pub(super) fn from_index(index: usize) -> Self {
        if index == 1 {
            Self::ScanPatterns
        } else {
            Self::LoadedKeys
        }
    }
}

/// The newest log lines the progress panel shows.
const LOG_LINES_SHOWN: usize = 200;

/// What kind of job the window was opened for.
#[derive(Clone)]
pub enum MigrationWindowMode {
    Export {
        server_id: SharedString,
        server_name: SharedString,
        db: usize,
        keys: Vec<SharedString>,
        source: ExportSource,
        preset: Option<CopyPreset>,
    },
    Import {
        server_id: SharedString,
        server_name: SharedString,
        db: usize,
    },
}

pub struct ZedisMigrationWindow {
    focus_handle: FocusHandle,
    mode: MigrationWindowMode,
    state: Entity<MigrationState>,
    /// Path the user picked in the save/open dialog. `None` until they choose.
    chosen_path: Option<PathBuf>,
    /// Import / Copy: what to do when the destination already has the key.
    conflict_mode: ConflictMode,
    /// Export only: binary DUMP bundle (re-importable) vs readable JSON/CSV.
    export_format: ExportFormat,
    /// Export / Copy: whether to use the handed-in key list or scan by patterns.
    pub(super) source_mode: KeySourceMode,
    /// Export only: optional key-prefix filter narrowing the handed-in key
    /// list before the job starts. Empty = export everything.
    prefix_input_state: Entity<InputState>,
    /// Pattern mode: comma- or whitespace-separated glob patterns.
    pub(super) patterns_input_state: Entity<InputState>,
    /// Pattern mode: selected Redis key types (empty = all types).
    pub(super) selected_types: Vec<SharedString>,
    /// Export only: a file, or another server / db.
    destination: Destination,
    /// Export to a server: `(id, name)` of every configured server.
    servers: Vec<(SharedString, SharedString)>,
    target_server_id: Option<SharedString>,
    target_db_input: Entity<InputState>,
    /// Import / Copy: last dry-run result (if any).
    preview: Option<ConflictPreview>,
    preview_running: bool,
    preview_error: Option<SharedString>,
    preview_cancel: Arc<AtomicBool>,
    /// Optional throughput rate cap (keys/s).
    throttle: MigrationThrottle,
    /// Copy only: post-copy verification result.
    verify_report: Option<CopyVerificationReport>,
    verify_running: bool,
    verify_error: Option<SharedString>,
    verify_cancel: Arc<AtomicBool>,
    /// Persisted unfinished migration checkpoints loaded from `JobManager`.
    pub(super) unfinished_jobs: Vec<MigrationCheckpoint>,
    /// Patterns of the last pattern-based copy job, enabling live follow mode.
    pub(super) last_copy_patterns: Option<Vec<SharedString>>,
    /// Best-effort live follow state after a pattern copy completes.
    pub(super) following: bool,
    pub(super) follow_synced_count: u64,
    pub(super) follow_last_key: Option<SharedString>,
    pub(super) follow_needs_enable: bool,
    pub(super) follow_disconnected: bool,
    follow_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl ZedisMigrationWindow {
    pub fn new(mode: MigrationWindowMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let state = cx.new(|_| MigrationState::new());
        let prefix_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_migration(cx, "prefix_filter_placeholder"))
        });
        let patterns_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .default_value("*")
                .placeholder(i18n_migration(cx, "patterns_placeholder"))
        });
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id.into(), s.name.into()))
            .collect();
        let (source_id, source_db, preset) = match &mode {
            MigrationWindowMode::Export {
                server_id, db, preset, ..
            } => (server_id.clone(), *db, preset.clone()),
            MigrationWindowMode::Import { server_id, db, .. } => (server_id.clone(), *db, None),
        };
        let target_server_id = preset.as_ref().map(|preset| preset.target_id.clone()).or_else(|| {
            servers
                .iter()
                .find(|(id, _)| *id == source_id)
                .or_else(|| servers.first())
                .map(|(id, _)| id.clone())
        });
        let is_cluster = get_server(source_id.as_str())
            .ok()
            .and_then(|s| s.server_type)
            .is_some_and(|t| t == SERVER_TYPE_CLUSTER);
        let default_target_db = if target_server_id.as_ref() == Some(&source_id) && !is_cluster {
            if source_db == 0 { 1 } else { 0 }
        } else {
            source_db
        };
        let target_db = preset
            .as_ref()
            .map(|preset| preset.target_db)
            .unwrap_or(default_target_db);
        let target_db_input = cx.new(|cx| InputState::new(window, cx).default_value(target_db.to_string()));
        let destination = if preset.is_some() {
            Destination::Server
        } else {
            Destination::File
        };
        let conflict_mode = preset.as_ref().map(|preset| preset.conflict).unwrap_or_default();
        let unfinished_jobs = load_migration_checkpoints();

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&state, |view, _state, event: &MigrationEvent, cx| {
            if matches!(event, MigrationEvent::PhaseChanged) {
                view.unfinished_jobs = load_migration_checkpoints();
            }
            cx.notify();
        }));
        subs.push(
            cx.subscribe_in(&target_db_input, window, |_view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe_in(&prefix_input_state, window, |_view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe_in(&patterns_input_state, window, |_view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
        );
        Self {
            focus_handle,
            mode,
            state,
            chosen_path: None,
            conflict_mode,
            export_format: ExportFormat::Binary,
            source_mode: KeySourceMode::LoadedKeys,
            prefix_input_state,
            patterns_input_state,
            selected_types: Vec::new(),
            destination,
            servers,
            target_server_id,
            target_db_input,
            preview: None,
            preview_running: false,
            preview_error: None,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            throttle: MigrationThrottle::Unlimited,
            verify_report: None,
            verify_running: false,
            verify_error: None,
            verify_cancel: Arc::new(AtomicBool::new(false)),
            unfinished_jobs,
            last_copy_patterns: None,
            following: false,
            follow_synced_count: 0,
            follow_last_key: None,
            follow_needs_enable: false,
            follow_disconnected: false,
            follow_task: None,
            _subs: subs,
        }
    }

    fn title_label(&self, cx: &App) -> SharedString {
        match self.mode {
            MigrationWindowMode::Export { .. } if self.destination == Destination::Server => {
                i18n_migration(cx, "copy_title")
            }
            MigrationWindowMode::Export { .. } => i18n_migration(cx, "export_title"),
            MigrationWindowMode::Import { .. } => i18n_migration(cx, "import_title"),
        }
    }

    fn is_copy(&self) -> bool {
        matches!(self.mode, MigrationWindowMode::Export { .. }) && self.destination == Destination::Server
    }

    fn target_db(&self, cx: &App) -> usize {
        self.target_db_input.read(cx).value().trim().parse().unwrap_or(0)
    }

    /// The picked target is the source itself — a copy would be a no-op
    /// at best and a self-overwrite at worst.
    fn target_is_source(&self, cx: &App) -> bool {
        let MigrationWindowMode::Export { server_id, db, .. } = &self.mode else {
            return false;
        };
        self.target_server_id.as_ref() == Some(server_id) && self.target_db(cx) == *db
    }

    /// "Destination: server / db" for a copy, once a target is picked.
    fn destination_summary(&self, cx: &App) -> Option<SharedString> {
        if !self.is_copy() {
            return None;
        }
        let target_id = self.target_server_id.as_ref()?;
        let name = get_server(target_id)
            .map(|s| s.name)
            .unwrap_or_else(|_| target_id.to_string());
        Some(
            i18n_migration(cx, "destination_summary")
                .replace("{server}", &name)
                .replace("{db}", &self.target_db(cx).to_string())
                .into(),
        )
    }

    pub(super) fn active_key_source(&self, cx: &App) -> KeySource {
        match self.source_mode {
            KeySourceMode::LoadedKeys => KeySource::Keys {
                keys: self.filtered_export_keys(cx),
            },
            KeySourceMode::ScanPatterns => {
                let raw = self.patterns_input_state.read(cx).value();
                let patterns = KeySource::parse_patterns_input(raw.as_ref());
                KeySource::Patterns {
                    patterns,
                    types: self.selected_types.clone(),
                }
            }
        }
    }

    pub(super) fn set_source_mode(&mut self, mode: KeySourceMode, cx: &mut Context<Self>) {
        if self.source_mode == mode {
            return;
        }
        self.source_mode = mode;
        self.drop_preview();
        cx.notify();
    }

    pub(super) fn toggle_pattern_type(&mut self, redis_type: &str, cx: &mut Context<Self>) {
        let ty = SharedString::from(redis_type.to_string());
        if let Some(pos) = self.selected_types.iter().position(|t| t == &ty) {
            self.selected_types.remove(pos);
        } else {
            self.selected_types.push(ty);
        }
        cx.notify();
    }

    pub(super) fn clear_pattern_types(&mut self, cx: &mut Context<Self>) {
        if !self.selected_types.is_empty() {
            self.selected_types.clear();
            cx.notify();
        }
    }

    /// Start the server-to-server copy of the active key source.
    fn start_copy(&mut self, cx: &mut Context<Self>) {
        let (server_id, db) = match &self.mode {
            MigrationWindowMode::Export { server_id, db, .. } => (server_id.clone(), *db),
            _ => return,
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.target_is_source(cx) {
            return;
        }
        let source = self.active_key_source(cx);
        if let KeySource::Keys { keys } = &source
            && keys.is_empty()
        {
            return;
        }
        self.stop_follow(cx);
        self.last_copy_patterns = match &source {
            KeySource::Patterns { patterns, .. } => Some(patterns.clone()),
            KeySource::Keys { .. } => None,
        };
        // Cancel any in-flight preview or verification so it doesn't fight the worker.
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.verify_cancel.store(true, Ordering::Release);
        self.verify_running = false;
        self.verify_report = None;
        self.verify_error = None;
        let job = MigrationJob::Copy {
            source_id: server_id,
            source_db: db,
            target_id,
            target_db: self.target_db(cx),
            source,
            conflict: self.conflict_mode,
            throttle: self.throttle,
            resume: None,
        };
        self.state.update(cx, |s, cx| s.start(job, cx));
    }

    /// Resume a persisted checkpoint from `unfinished_jobs`.
    pub(super) fn resume_checkpoint(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cp) = self.unfinished_jobs.get(index).cloned() else {
            return;
        };
        self.stop_follow(cx);
        self.drop_preview();
        match &cp.job {
            MigrationCheckpointJob::Copy {
                target_id,
                target_db,
                source,
                conflict,
                ..
            } => {
                self.destination = Destination::Server;
                window.set_window_title(&self.title_label(cx));
                self.target_server_id = Some(target_id.clone());
                self.conflict_mode = *conflict;
                self.target_db_input
                    .update(cx, |input, cx| input.set_value(target_db.to_string(), window, cx));
                if let KeySource::Patterns { patterns, types } = source {
                    self.source_mode = KeySourceMode::ScanPatterns;
                    self.selected_types = types.clone();
                    self.last_copy_patterns = Some(patterns.clone());
                } else {
                    self.source_mode = KeySourceMode::LoadedKeys;
                    self.last_copy_patterns = None;
                }
                if let Ok(target_server) = get_server(target_id.as_str()) {
                    if target_server.readonly.unwrap_or(false) {
                        return;
                    }
                    if target_server.is_high_risk_tag()
                        || target_server.require_confirm_writes.unwrap_or(false)
                        || target_server.write_locked()
                    {
                        let state = self.state.clone();
                        let summary = cp.summary.clone();
                        let target_db = *target_db;
                        let job = cp.into_migration_job();
                        confirm_dangerous_command(
                            &target_server,
                            target_db,
                            &DangerKind::GenericWrite,
                            Some(&summary),
                            window,
                            cx,
                            move |_window, cx| {
                                state.update(cx, |s, cx| s.start(job.clone(), cx));
                            },
                        );
                        return;
                    }
                }
                let job = cp.into_migration_job();
                self.state.update(cx, |s, cx| s.start(job, cx));
            }
            MigrationCheckpointJob::Export {
                output_path,
                format,
                source,
                ..
            } => {
                self.destination = Destination::File;
                window.set_window_title(&self.title_label(cx));
                self.chosen_path = Some(output_path.clone());
                self.export_format = *format;
                if let KeySource::Patterns { types, .. } = source {
                    self.source_mode = KeySourceMode::ScanPatterns;
                    self.selected_types = types.clone();
                } else {
                    self.source_mode = KeySourceMode::LoadedKeys;
                }
                let job = cp.into_migration_job();
                self.state.update(cx, |s, cx| s.start(job, cx));
            }
        }
    }

    /// Discard an unfinished job checkpoint from `JobManager`.
    pub(super) fn discard_checkpoint(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(cp) = self.unfinished_jobs.get(index) else {
            return;
        };
        let id = cp.id.clone();
        delete_migration_checkpoint(&id);
        self.unfinished_jobs.retain(|j| j.id != id);
        cx.notify();
    }

    /// Starts best-effort live sync follow mode for a completed pattern copy.
    pub(super) fn start_follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let MigrationWindowMode::Export { server_id, db, .. } = &self.mode else {
            return;
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        let Some(patterns) = self.last_copy_patterns.clone() else {
            return;
        };
        let source_id = server_id.clone();
        let source_db = *db;
        let target_db = self.target_db(cx);
        let source_at = ServerDb::new(source_id.as_str(), source_db);

        self.follow_needs_enable = false;
        self.follow_disconnected = false;
        cx.notify();

        let _ = window;
        cx.spawn(async move |this, cx| {
            let flags = cx
                .background_spawn({
                    let source_at = source_at.clone();
                    async move { get_keyspace_events_config(&source_at).await.unwrap_or_default() }
                })
                .await;
            if !flags.contains('K') {
                let _ = this.update(cx, |view, cx| {
                    view.follow_needs_enable = true;
                    cx.notify();
                });
                return;
            }
            let _ = this.update(cx, |view, cx| {
                view.spawn_follow_stream(source_id, source_db, target_id, target_db, patterns, cx);
            });
        })
        .detach();
    }

    /// Prompts to enable `CONFIG SET notify-keyspace-events KEA` on the source
    /// server (guarded by `confirm_dangerous_command`), then starts following.
    pub(super) fn enable_keyspace_on_source_and_follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let MigrationWindowMode::Export { server_id, db, .. } = &self.mode else {
            return;
        };
        let Ok(source_server) = get_server(server_id.as_str()) else {
            return;
        };
        if source_server.readonly.unwrap_or(false) {
            return;
        }
        let source_id = server_id.clone();
        let source_db = *db;
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        let target_db = self.target_db(cx);
        let Some(patterns) = self.last_copy_patterns.clone() else {
            return;
        };
        let weak = cx.entity().downgrade();
        confirm_dangerous_command(
            &source_server,
            source_db,
            &DangerKind::ConfigSet,
            Some("CONFIG SET notify-keyspace-events KEA"),
            window,
            cx,
            move |_window, cx| {
                let Some(entity) = weak.upgrade() else {
                    return;
                };
                let source_id = source_id.clone();
                let target_id = target_id.clone();
                let patterns = patterns.clone();
                entity.update(cx, |view, cx| {
                    view.follow_needs_enable = false;
                    cx.spawn(async move |this, cx| {
                        let at = ServerDb::new(source_id.as_str(), source_db);
                        let res = cx
                            .background_spawn(async move { config_set(&at, "notify-keyspace-events", "KEA").await })
                            .await;
                        if res.is_ok() {
                            let _ = this.update(cx, |view, cx| {
                                view.spawn_follow_stream(source_id, source_db, target_id, target_db, patterns, cx);
                            });
                        }
                    })
                    .detach();
                });
            },
        );
    }

    fn spawn_follow_stream(
        &mut self,
        source_id: SharedString,
        source_db: usize,
        target_id: SharedString,
        target_db: usize,
        patterns: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.following = true;
        self.follow_synced_count = 0;
        self.follow_last_key = None;
        self.follow_disconnected = false;
        cx.notify();

        let source_at = ServerDb::new(source_id.as_str(), source_db);
        let target_at = ServerDb::new(target_id.as_str(), target_db);
        let psub_patterns: Vec<String> = patterns
            .iter()
            .map(|p| format!("__keyspace@{source_db}__:{p}"))
            .collect();
        let prefix_to_strip = format!("__keyspace@{source_db}__:");

        self.follow_task = Some(cx.spawn(async move |this, cx| {
            let sub = cx
                .background_spawn({
                    let source_at = source_at.clone();
                    let psub_patterns = psub_patterns.clone();
                    async move {
                        let refs: Vec<&str> = psub_patterns.iter().map(String::as_str).collect();
                        ChannelSubscription::open(&source_at, SubscribeKind::KeyspaceEvents, &refs).await
                    }
                })
                .await;
            let Ok(mut pubsub) = sub else {
                let _ = this.update(cx, |view, cx| {
                    view.following = false;
                    view.follow_disconnected = true;
                    cx.notify();
                });
                return;
            };

            loop {
                let (next_msg, returned_pubsub) = cx
                    .background_spawn(async move {
                        let msg = pubsub.next_message().await;
                        (msg, pubsub)
                    })
                    .await;
                pubsub = returned_pubsub;
                let Some(msg) = next_msg else {
                    let _ = this.update(cx, |view, cx| {
                        view.following = false;
                        view.follow_disconnected = true;
                        cx.notify();
                    });
                    break;
                };
                let Some(key) = msg.channel.strip_prefix(&prefix_to_strip).map(str::to_string) else {
                    continue;
                };
                let event = String::from_utf8_lossy(&msg.payload).trim().to_string();
                let source_at_op = source_at.clone();
                let target_at_op = target_at.clone();
                let key_op = key.clone();
                let event_op = event.clone();
                let _ = cx
                    .background_spawn(async move {
                        if matches!(event_op.as_str(), "del" | "expired" | "evicted") {
                            let _ = delete_keys(&target_at_op, vec![key_op]).await;
                        } else {
                            let _ = copy_keys_batch(
                                &source_at_op,
                                &target_at_op,
                                std::slice::from_ref(&key_op),
                                ConflictMode::Overwrite,
                            )
                            .await;
                        }
                    })
                    .await;
                let alive = this.update(cx, |view, cx| {
                    view.follow_synced_count += 1;
                    view.follow_last_key = Some(format!("{key} ({event})").into());
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    pub(super) fn stop_follow(&mut self, cx: &mut Context<Self>) {
        self.follow_task = None;
        if self.following {
            self.following = false;
            cx.notify();
        }
    }

    /// Opens the Compare window pre-populated with `key` so the user can
    /// inspect a verification mismatch in one click.
    pub(super) fn open_key_in_compare(&self, key: SharedString, cx: &mut App) {
        let MigrationWindowMode::Export {
            server_id,
            server_name,
            db,
            ..
        } = &self.mode
        else {
            return;
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        open_compare_for_key(
            server_id.clone(),
            server_name.clone(),
            *db,
            target_id,
            self.target_db(cx),
            key,
            cx,
        );
    }

    /// Post-copy verification: checks that each copied key exists on the
    /// target with the same type and value.
    fn verify_copy(&mut self, cx: &mut Context<Self>) {
        let MigrationWindowMode::Export { server_id, db, .. } = &self.mode else {
            return;
        };
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.verify_running || self.target_is_source(cx) {
            return;
        }
        let keys: Vec<String> = match self.source_mode {
            KeySourceMode::LoadedKeys => self
                .filtered_export_keys(cx)
                .iter()
                .map(|key| key.to_string())
                .collect(),
            KeySourceMode::ScanPatterns => self
                .state
                .read(cx)
                .copied_keys()
                .iter()
                .map(|key| key.to_string())
                .collect(),
        };
        if keys.is_empty() {
            return;
        }
        let source_at = ServerDb::new(server_id.as_str(), *db);
        let target_at = ServerDb::new(target_id.as_str(), self.target_db(cx));
        self.verify_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.verify_cancel.clone();
        let superseded = self.verify_cancel.clone();
        self.verify_running = true;
        self.verify_error = None;
        self.verify_report = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { verify_copied_keys(&source_at, &target_at, &keys, &cancel).await })
                .await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) {
                    return;
                }
                view.verify_running = false;
                match result {
                    Ok(report) => {
                        view.verify_report = Some(report);
                        view.verify_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "copy verification failed");
                        view.verify_report = None;
                        view.verify_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Dry run of a copy: `EXISTS` for the filtered keys on the target.
    fn preview_copy(&mut self, cx: &mut Context<Self>) {
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.preview_running || self.target_is_source(cx) {
            return;
        }
        let keys: Vec<String> = self
            .filtered_export_keys(cx)
            .iter()
            .map(|key| key.to_string())
            .collect();
        let target_id = target_id.to_string();
        let target_db = self.target_db(cx);
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        let superseded = self.preview_cancel.clone();
        self.preview_running = true;
        self.preview_error = None;
        self.preview = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    preview_key_conflicts(&target_id, target_db, &keys, PREVIEW_SAMPLE_LIMIT, &cancel).await
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) {
                    return;
                }
                view.preview_running = false;
                match result {
                    Ok(preview) => {
                        view.preview = Some(preview);
                        view.preview_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "copy conflict preview failed");
                        view.preview = None;
                        view.preview_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_destination(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination == destination {
            return;
        }
        self.destination = destination;
        window.set_window_title(&self.title_label(cx));
        self.drop_preview();
        cx.notify();
    }

    fn select_target(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.target_server_id = Some(id);
        self.drop_preview();
        cx.notify();
    }

    fn drop_preview(&mut self) {
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.preview = None;
        self.preview_error = None;
        self.verify_cancel.store(true, Ordering::Release);
        self.verify_running = false;
        self.verify_report = None;
        self.verify_error = None;
    }

    fn source_summary(&self, cx: &App) -> SharedString {
        match &self.mode {
            MigrationWindowMode::Export {
                server_name,
                db,
                keys,
                source,
                ..
            } => {
                let template = i18n_migration(cx, "source_summary");
                let mut summary = template
                    .replace("{server}", server_name)
                    .replace("{db}", &db.to_string())
                    .replace("{count}", &keys.len().to_string());
                if *source == ExportSource::Loaded {
                    summary.push_str(" · ");
                    summary.push_str(&i18n_migration(cx, "loaded_subset_note"));
                }
                summary.into()
            }
            MigrationWindowMode::Import { server_name, db, .. } => {
                let template = i18n_migration(cx, "destination_summary");
                template
                    .replace("{server}", server_name)
                    .replace("{db}", &db.to_string())
                    .into()
            }
        }
    }

    fn suggested_filename(&self) -> String {
        let stamp = Utc::now().format("%Y%m%d-%H%M%S");
        match &self.mode {
            MigrationWindowMode::Export { server_name, db, .. } => {
                let safe_name: String = server_name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                format!("zedis-{safe_name}-db{db}-{stamp}.{}", self.export_format.extension())
            }
            MigrationWindowMode::Import { .. } => format!("zedis-{stamp}.zedis-dump"),
        }
    }

    /// The export key list after the prefix filter (export mode only;
    /// empty filter passes everything through).
    fn filtered_export_keys(&self, cx: &App) -> Vec<SharedString> {
        self.export_keys(cx).cloned().collect()
    }

    fn export_keys<'a>(&'a self, cx: &App) -> impl Iterator<Item = &'a SharedString> + use<'a> {
        let keys: &[SharedString] = match &self.mode {
            MigrationWindowMode::Export { keys, .. } => keys,
            _ => &[],
        };
        let prefix = self.prefix_input_state.read(cx).value().trim().to_string();
        keys.iter().filter(move |key| key.starts_with(prefix.as_str()))
    }

    fn handle_pick_and_start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match &self.mode {
            MigrationWindowMode::Export { .. } if self.destination == Destination::Server => {
                self.start_copy(cx);
            }
            MigrationWindowMode::Export { .. } => {
                let suggested = self.suggested_filename();
                let directory = dirs_default_directory();
                let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
                cx.spawn(async move |this, cx| {
                    let result = receiver.await;
                    let _ = this.update(cx, move |view, cx| {
                        if let Ok(Ok(Some(path))) = result {
                            view.chosen_path = Some(path.clone());
                            view.start_with_path(path, cx);
                        }
                    });
                })
                .detach();
            }
            MigrationWindowMode::Import { .. } => {
                let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: None,
                });
                cx.spawn(async move |this, cx| {
                    let result = receiver.await;
                    let _ = this.update(cx, move |view, cx| {
                        if let Ok(Ok(Some(paths))) = result
                            && let Some(path) = paths.into_iter().next()
                        {
                            view.chosen_path = Some(path);
                            view.preview = None;
                            view.preview_error = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    fn start_with_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let throttle = self.throttle;
        let source = self.active_key_source(cx);
        let job = match &self.mode {
            MigrationWindowMode::Export { server_id, db, .. } => MigrationJob::Export {
                server_id: server_id.clone(),
                db: *db,
                source,
                output_path: path,
                format: self.export_format,
                throttle,
                resume: None,
            },
            MigrationWindowMode::Import { server_id, db, .. } => MigrationJob::Import {
                server_id: server_id.clone(),
                db: *db,
                input_path: path,
                conflict: self.conflict_mode,
                throttle,
            },
        };
        self.state.update(cx, |s, cx| s.start(job, cx));
    }

    fn handle_start_import(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.chosen_path.clone() else {
            return;
        };
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.start_with_path(path, cx);
    }

    fn handle_preview(&mut self, cx: &mut Context<Self>) {
        if self.is_copy() {
            self.preview_copy(cx);
            return;
        }
        let MigrationWindowMode::Import { server_id, db, .. } = &self.mode else {
            return;
        };
        let Some(path) = self.chosen_path.clone() else {
            return;
        };
        if self.preview_running {
            return;
        }
        let server_id = server_id.to_string();
        let db = *db;
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        self.preview_running = true;
        self.preview_error = None;
        self.preview = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    preview_import_conflicts(&server_id, db, path, PREVIEW_SAMPLE_LIMIT, &cancel).await
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.preview_running = false;
                match result {
                    Ok(preview) => {
                        view.preview = Some(preview);
                        view.preview_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "import conflict preview failed");
                        view.preview = None;
                        view.preview_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_conflict_mode(&mut self, mode: ConflictMode, cx: &mut Context<Self>) {
        if self.conflict_mode == mode {
            return;
        }
        self.conflict_mode = mode;
        cx.notify();
    }

    fn handle_cancel(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.cancel(cx));
    }

    fn handle_toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_pause(cx));
    }

    fn handle_close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    fn handle_reveal(&self, cx: &mut App) {
        if let Some(path) = &self.chosen_path {
            cx.reveal_path(path);
        }
    }
}

impl Focusable for ZedisMigrationWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn phase_label(phase: &MigrationPhase, cx: &App) -> SharedString {
    match phase {
        MigrationPhase::Idle => i18n_migration(cx, "phase_idle"),
        MigrationPhase::Running => i18n_migration(cx, "phase_running"),
        MigrationPhase::Paused => i18n_migration(cx, "phase_paused"),
        MigrationPhase::Finished => i18n_migration(cx, "phase_finished"),
        MigrationPhase::Failed(msg) => format!("{}: {}", i18n_migration(cx, "phase_failed"), msg).into(),
        MigrationPhase::Cancelled => i18n_migration(cx, "phase_cancelled"),
    }
}

impl Render for ZedisMigrationWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(font_size) = cx.global::<ZedisGlobalStore>().read(cx).font_rem_px() {
            window.set_rem_size(font_size);
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let state = self.state.read(cx);
        let phase = state.phase().clone();
        let is_running = matches!(phase, MigrationPhase::Running | MigrationPhase::Paused);
        let is_paused = state.is_paused();
        let is_import = matches!(self.mode, MigrationWindowMode::Import { .. });
        let filtered_count = self.export_keys(cx).count();
        let total_count = match &self.mode {
            MigrationWindowMode::Export { keys, .. } => keys.len(),
            MigrationWindowMode::Import { .. } => 0,
        };
        let is_copy = self.is_copy();
        let target_is_source = self.target_is_source(cx);
        let look = JobLook {
            phase,
            is_running,
            is_paused,
            is_import,
            is_copy,
            target_is_source,
            filtered_count,
            total_count,
            muted,
        };

        let header = self.header(&look, cx);
        let unfinished_section = self.unfinished_jobs_section(&look, cx);
        let status_section = self.status_section(&look, cx);
        let format_section = (!is_import).then(|| self.export_section(&look, cx));
        let copy_section = is_copy.then(|| self.copy_section(&look, cx));
        let follow_section = self.follow_section(&look, cx);
        let conflict_section = is_import.then(|| self.import_section(&look, cx));
        let log_section = self.log_section(&look, cx);
        let footer = self.footer(&look, cx);

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|_this, event: &KeyDownEvent, window, _cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(header)
                    .children(unfinished_section)
                    .child(status_section)
                    .children(format_section)
                    .children(copy_section)
                    .children(follow_section)
                    .children(conflict_section)
                    .child(log_section)
                    .child(footer),
            )
    }
}

fn open_migration_window(mode: MigrationWindowMode, title: SharedString, cx: &mut App) {
    let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
    let options = with_app_identity(WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..Default::default()
        }),
        is_resizable: true,
        focus: true,
        ..Default::default()
    });
    let _ = cx.open_window(options, move |window, cx| {
        let view = cx.new(|cx| ZedisMigrationWindow::new(mode.clone(), window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
}

/// Opens an export window for the given selection. Each call creates a fresh window
/// — no de-duplication, since each invocation captures different selected keys.
pub fn open_migration_export_window(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    source: ExportSource,
    cx: &mut App,
) {
    let title = i18n_migration(cx, "export_title");
    open_migration_window(
        MigrationWindowMode::Export {
            server_id,
            server_name,
            db,
            keys,
            source,
            preset: None,
        },
        title,
        cx,
    );
}

/// Opens the export window already set to copy `keys` to `preset`'s
/// server / db — for a caller that knows the target, like the compare
/// window handing over what the target lacks.
pub fn open_migration_copy_window(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    preset: CopyPreset,
    cx: &mut App,
) {
    let title = i18n_migration(cx, "copy_title");
    open_migration_window(
        MigrationWindowMode::Export {
            server_id,
            server_name,
            db,
            keys,
            source: ExportSource::Selection,
            preset: Some(preset),
        },
        title,
        cx,
    );
}

/// Opens an import window targeting the given server / db.
pub fn open_migration_import_window(server_id: SharedString, server_name: SharedString, db: usize, cx: &mut App) {
    let title = i18n_migration(cx, "import_title");
    open_migration_window(
        MigrationWindowMode::Import {
            server_id,
            server_name,
            db,
        },
        title,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{DifferingKey, KeyDifference, RedisServer, set_servers_cache};
    use crate::states::{MigrationResumeCursor, save_migration_checkpoint};
    use crate::test_support::{hold_the_server_list, init_ui};
    use gpui::{AnyWindowHandle, ElementId, TestAppContext};
    use gpui_kit::test::{TestQueryExt, TestWindowExt};

    /// The process's server list as it was, put back when the test ends,
    /// whether it passes or not.
    struct Restore(Vec<RedisServer>);

    impl Drop for Restore {
        fn drop(&mut self) {
            set_servers_cache(std::mem::take(&mut self.0));
        }
    }

    fn own_servers() -> Restore {
        let restore = Restore(get_servers().unwrap_or_default());
        let server = |id: &str| RedisServer {
            id: id.into(),
            name: id.into(),
            host: "127.0.0.1".into(),
            port: 6379,
            ..Default::default()
        };
        set_servers_cache(vec![server("migtest-b"), server("migtest-a")]);
        restore
    }

    fn export_mode(preset: Option<CopyPreset>) -> MigrationWindowMode {
        MigrationWindowMode::Export {
            server_id: "migtest-a".into(),
            server_name: "migtest-a".into(),
            db: 0,
            keys: vec!["user:1".into(), "user:2".into(), "order:1".into()],
            source: ExportSource::Selection,
            preset,
        }
    }

    fn open(cx: &mut TestAppContext, mode: MigrationWindowMode) -> (AnyWindowHandle, Entity<ZedisMigrationWindow>) {
        let mut view = None;
        let handle = cx.open_window(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), |window, cx| {
            let migration = cx.new(|cx| ZedisMigrationWindow::new(mode, window, cx));
            view = Some(migration.clone());
            Root::new(migration, window, cx)
        });
        cx.run_until_parked();
        (handle.into(), view.expect("the window built the view"))
    }

    fn frame<T>(cx: &mut TestAppContext, handle: AnyWindowHandle, act: impl FnOnce(&mut Window, &mut App) -> T) -> T {
        let out = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                act(window, cx)
            })
            .expect("the window is open");
        cx.run_until_parked();
        out
    }

    fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, input: &Entity<InputState>, text: &str) {
        frame(cx, handle, |window, cx| {
            input.update(cx, |state, cx| state.set_value(text, window, cx));
        });
    }

    fn label(window: &Window, id: &'static str) -> Option<String> {
        window.find(id).label().map(str::to_string)
    }

    fn radios(window: &Window, group: &'static str) -> usize {
        let group = ElementId::from(group);
        window
            .elements()
            .iter()
            .filter(|element| element.path().contains(&group))
            .count()
    }

    #[gpui::test]
    fn an_export_goes_to_a_file_or_to_another_server(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let _restore = own_servers();
        let (handle, view) = open(cx, export_mode(None));

        frame(cx, handle, |window, cx| {
            assert_eq!(radios(window, "migration-destination"), 2);
            assert_eq!(radios(window, "migration-export-format"), 3);
            assert_eq!(radios(window, "migration-copy-conflict"), 0);
            assert_eq!(radios(window, "migration-conflict-mode"), 0);
            assert_eq!(
                window.within("migration-destination").find(0usize).checked(),
                Some(true)
            );
            assert_eq!(
                window.within("migration-export-format").find(0usize).checked(),
                Some(true)
            );
            for absent in [
                "migration-target-migtest-b",
                "migration-preview",
                "migration-pick",
                "migration-cancel",
                "migration-reveal",
            ] {
                assert!(window.try_find(absent).is_none(), "{absent}");
            }
            assert!(window.try_find("migration-close").is_some());
            assert_eq!(window.find("migration-start").disabled(), None);
            assert_eq!(
                label(window, "migration-start").as_deref(),
                Some(i18n_migration(cx, "save_and_export").as_ref())
            );
            window.within("migration-export-format").click(2usize, cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.export_format.index()), 2);

        let prefix = view.read_with(cx, |view, _| view.prefix_input_state.clone());
        type_into(cx, handle, &prefix, "zzz");
        frame(cx, handle, |window, _| {
            assert_eq!(window.find("migration-start").disabled(), Some(true), "no key passes");
        });
        type_into(cx, handle, &prefix, "order");
        frame(cx, handle, |window, cx| {
            assert_eq!(window.find("migration-start").disabled(), None, "one key passes");
            window.within("migration-destination").click(1usize, cx);
        });

        view.read_with(cx, |view, cx| {
            assert_eq!(view.title_label(cx), i18n_migration(cx, "copy_title"));
            assert_eq!(view.target_server_id.as_deref(), Some("migtest-a"));
            assert_eq!(view.target_db(cx), 1);
        });
        frame(cx, handle, |window, cx| {
            assert_eq!(radios(window, "migration-export-format"), 0, "a copy has no format");
            assert_eq!(radios(window, "migration-copy-conflict"), 3);
            assert_eq!(
                window.within("migration-copy-conflict").find(0usize).checked(),
                Some(true)
            );
            assert!(window.try_find("migration-target-migtest-a").is_some());
            assert!(window.try_find("migration-target-migtest-b").is_some());
            assert_eq!(window.find("migration-preview").disabled(), None);
            assert_eq!(window.find("migration-start").disabled(), None);
            assert_eq!(
                label(window, "migration-start").as_deref(),
                Some(i18n_migration(cx, "start_copy").as_ref())
            );
        });
        let target_db = view.read_with(cx, |view, _| view.target_db_input.clone());
        type_into(cx, handle, &target_db, "0");
        frame(cx, handle, |window, _| {
            assert_eq!(window.find("migration-preview").disabled(), Some(true), "onto itself");
            assert_eq!(window.find("migration-start").disabled(), Some(true), "onto itself");
        });
        type_into(cx, handle, &target_db, "1");
        frame(cx, handle, |window, cx| {
            assert_eq!(window.find("migration-preview").disabled(), None, "another database");
            assert_eq!(window.find("migration-start").disabled(), None, "another database");
            window.within("migration-copy-conflict").click(2usize, cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.conflict_mode.index()), 2);

        let cancel = Arc::new(AtomicBool::new(false));
        view.update(cx, |view, cx| {
            view.preview_cancel = cancel.clone();
            view.preview_running = true;
            cx.notify();
        });
        frame(cx, handle, |window, cx| {
            assert!(window.try_find("migration-preview").is_none());
            assert!(window.try_find("migration-start").is_none());
            window.click("migration-cancel", cx);
        });
        assert!(cancel.load(Ordering::Acquire), "Cancel stops the dry run");

        view.update(cx, |view, cx| {
            view.preview_running = false;
            cx.notify();
        });
        frame(cx, handle, |window, cx| {
            window.within("migration-destination").click(0usize, cx)
        });
        frame(cx, handle, |window, _| {
            assert_eq!(
                window.within("migration-export-format").find(2usize).checked(),
                Some(true)
            );
            assert_eq!(radios(window, "migration-copy-conflict"), 0);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title_label(cx), i18n_migration(cx, "export_title"));
        });
    }

    #[gpui::test]
    fn a_copy_handed_its_target_opens_on_it(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let _restore = own_servers();
        let preset = CopyPreset {
            target_id: "migtest-b".into(),
            target_db: 3,
            conflict: ConflictMode::from_index(1),
        };
        let (handle, view) = open(cx, export_mode(Some(preset)));
        frame(cx, handle, |window, _| {
            assert_eq!(
                window.within("migration-destination").find(1usize).checked(),
                Some(true)
            );
            assert_eq!(
                window.within("migration-copy-conflict").find(1usize).checked(),
                Some(true)
            );
            assert_eq!(radios(window, "migration-export-format"), 0);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.target_server_id.as_deref(), Some("migtest-b"));
            assert_eq!(view.target_db(cx), 3);
        });
    }

    #[gpui::test]
    fn an_import_asks_for_its_file_before_it_previews_or_starts(cx: &mut TestAppContext) {
        init_ui(cx);
        let mode = MigrationWindowMode::Import {
            server_id: "migtest-a".into(),
            server_name: "migtest-a".into(),
            db: 0,
        };
        let (handle, view) = open(cx, mode);
        frame(cx, handle, |window, cx| {
            assert_eq!(radios(window, "migration-destination"), 0);
            assert_eq!(radios(window, "migration-export-format"), 0);
            assert_eq!(radios(window, "migration-copy-conflict"), 0);
            assert_eq!(radios(window, "migration-conflict-mode"), 3);
            for absent in ["migration-cancel", "migration-reveal"] {
                assert!(window.try_find(absent).is_none(), "{absent}");
            }
            assert_eq!(
                window.within("migration-conflict-mode").find(0usize).checked(),
                Some(true)
            );
            assert_eq!(window.find("migration-pick").disabled(), None);
            assert_eq!(
                label(window, "migration-pick").as_deref(),
                Some(i18n_migration(cx, "pick_file").as_ref())
            );
            assert_eq!(window.find("migration-preview").disabled(), Some(true));
            assert_eq!(window.find("migration-start").disabled(), Some(true));
            window.within("migration-conflict-mode").click(1usize, cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.conflict_mode.index()), 1);

        view.update(cx, |view, cx| {
            view.chosen_path = Some(PathBuf::from("keys.zedis-dump"));
            cx.notify();
        });
        frame(cx, handle, |window, cx| {
            assert_eq!(
                label(window, "migration-pick").as_deref(),
                Some(i18n_migration(cx, "pick_file_again").as_ref())
            );
            assert_eq!(window.find("migration-preview").disabled(), None);
            assert_eq!(window.find("migration-start").disabled(), None);
            window.press("escape", cx);
        });
        assert!(cx.update_window(handle, |_, _, _| ()).is_err(), "Esc closes the window");
    }

    #[gpui::test]
    fn throttle_pause_and_verify_controls_follow_job_state(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let _restore = own_servers();
        let preset = CopyPreset {
            target_id: "migtest-b".into(),
            target_db: 1,
            conflict: ConflictMode::Skip,
        };
        let (handle, view) = open(cx, export_mode(Some(preset)));

        frame(cx, handle, |window, cx| {
            assert_eq!(radios(window, "migration-throttle"), 4);
            assert!(window.try_find("migration-pause").is_none());
            assert!(window.try_find("migration-verify").is_some());
            window.within("migration-throttle").click(2usize, cx);
        });
        assert_eq!(view.read_with(cx, |view, _| view.throttle), MigrationThrottle::Rate200);

        let mig_state = view.read_with(cx, |view, _| view.state.clone());
        mig_state.update(cx, |s, cx| s.set_phase_for_test(MigrationPhase::Running, cx));

        frame(cx, handle, |window, cx| {
            assert!(window.try_find("migration-verify").is_none());
            assert_eq!(
                label(window, "migration-pause").as_deref(),
                Some(i18n_migration(cx, "pause").as_ref())
            );
            window.click("migration-pause", cx);
        });
        assert!(mig_state.read_with(cx, |s, _| s.is_paused()));

        frame(cx, handle, |window, cx| {
            assert_eq!(
                label(window, "migration-pause").as_deref(),
                Some(i18n_migration(cx, "resume").as_ref())
            );
            window.click("migration-pause", cx);
        });
        assert!(!mig_state.read_with(cx, |s, _| s.is_paused()));

        mig_state.update(cx, |s, cx| s.set_phase_for_test(MigrationPhase::Finished, cx));
        frame(cx, handle, |window, _| {
            assert!(window.try_find("migration-pause").is_none());
            assert!(window.try_find("migration-verify").is_some());
        });
    }

    #[gpui::test]
    fn pattern_mode_unfinished_jobs_and_verify_compare_jump_work(cx: &mut TestAppContext) {
        init_ui(cx);
        let _list = hold_the_server_list();
        let _restore = own_servers();

        let cp = MigrationCheckpoint {
            id: "mig-ui-test-cp".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_010,
            summary: "Copy migtest-a/db0 → migtest-b/db1 (user:*)".to_string(),
            job: MigrationCheckpointJob::Copy {
                source_id: "migtest-a".into(),
                source_db: 0,
                target_id: "migtest-b".into(),
                target_db: 1,
                source: KeySource::Patterns {
                    patterns: vec!["user:*".into()],
                    types: vec!["hash".into()],
                },
                conflict: ConflictMode::Skip,
                throttle: MigrationThrottle::Unlimited,
            },
            cursor: MigrationResumeCursor {
                processed_count: 128,
                ..Default::default()
            },
        };
        save_migration_checkpoint(&cp);

        let preset = CopyPreset {
            target_id: "migtest-b".into(),
            target_db: 1,
            conflict: ConflictMode::Skip,
        };
        let (handle, view) = open(cx, export_mode(Some(preset)));

        frame(cx, handle, |window, cx| {
            assert_eq!(radios(window, "migration-source-mode"), 2);
            assert!(window.try_find(("migration-discard-job", 0usize)).is_some());
            window.within("migration-source-mode").click(1usize, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, _| view.source_mode),
            KeySourceMode::ScanPatterns
        );

        let patterns_input = view.read_with(cx, |view, _| view.patterns_input_state.clone());
        type_into(cx, handle, &patterns_input, "user:*, session:*");
        frame(cx, handle, |window, cx| {
            window.click("migration-type-hash", cx);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.active_key_source(cx),
                KeySource::Patterns {
                    patterns: vec!["user:*".into(), "session:*".into()],
                    types: vec!["hash".into()],
                }
            );
        });

        // Discard the unfinished job checkpoint and verify the banner disappears.
        frame(cx, handle, |window, cx| {
            window.click(("migration-discard-job", 0usize), cx);
        });
        assert!(view.read_with(cx, |view, _| view.unfinished_jobs.is_empty()));

        // Populate a verification report with mismatches and confirm the Open in Compare button renders.
        view.update(cx, |view, cx| {
            view.verify_report = Some(CopyVerificationReport {
                checked: 3,
                matched: 1,
                missing_on_target: vec!["user:1".to_string()],
                differing: vec![DifferingKey {
                    key: "user:2".to_string(),
                    key_type: "string".to_string(),
                    difference: KeyDifference::Value,
                }],
                missing_keys: vec!["user:1".to_string()],
                differing_keys: vec!["user:2".to_string()],
                cancelled: false,
            });
            cx.notify();
        });
        frame(cx, handle, |window, _| {
            assert!(window.try_find(("migration-open-compare", 0usize)).is_some());
            assert!(window.try_find(("migration-open-compare", 1usize)).is_some());
        });
    }
}

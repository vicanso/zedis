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

//! State for export / import / copy migration jobs.
//!
//! Drives a single background worker that runs a [`MigrationJob`], checkpoints
//! batch progress to [`JobManager`] (`jobs` redb table) so interrupted runs
//! can be resumed, and posts incremental progress back to the foreground entity.

use crate::connection::{
    ConflictMode, DumpEntry, DumpHeader, DumpReader, DumpWriter, ImportFormat, ReadLimits, ReadableEntry,
    ReadableWriteStatus, RestoreStatus, ServerDb, csv_header, detect_import_format, dump_keys_chunk, entry_to_csv,
    entry_to_json, get_server, is_foreign_payload, parse_readable_entries, read_readable_chunk, restore_keys_chunk,
    restore_or_recreate_chunk, scan_pattern_batch, server_summary, ttl_left, write_readable_chunk,
};
use crate::db::{JobManager, StoredJobRecord};
use crate::error::Error;
use ahash::AHashSet;
use chrono::Utc;
use gpui::SharedString;
use gpui::prelude::*;
use gpui::{EventEmitter, Task};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};
use web_time::Instant;

const DUMP_BATCH_SIZE: usize = 64;
const RESTORE_BATCH_SIZE: usize = 64;
const LOG_RING_CAPACITY: usize = 500;
const COPIED_KEYS_CAP: usize = 10_000;
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub const MIGRATION_JOB_KIND: &str = "migration";

type Result<T, E = Error> = std::result::Result<T, E>;

/// Phase of a migration job. UI renders different chrome based on this.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum MigrationPhase {
    #[default]
    Idle,
    Running,
    Paused,
    Finished,
    Failed(SharedString),
    Cancelled,
}

/// Optional throughput cap (keys/s) so a bulk export, import, or copy does
/// not saturate a shared or production server.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationThrottle {
    #[default]
    Unlimited,
    Rate1000,
    Rate200,
    Rate50,
}

impl MigrationThrottle {
    pub const ALL: [MigrationThrottle; 4] = [
        MigrationThrottle::Unlimited,
        MigrationThrottle::Rate1000,
        MigrationThrottle::Rate200,
        MigrationThrottle::Rate50,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(MigrationThrottle::Unlimited)
    }

    pub fn keys_per_sec(self) -> u32 {
        match self {
            Self::Unlimited => 0,
            Self::Rate1000 => 1000,
            Self::Rate200 => 200,
            Self::Rate50 => 50,
        }
    }
}

/// Source of keys for an Export or Copy job: an explicit key list (selection /
/// loaded tree) or a streaming `SCAN` over one or more glob patterns and
/// optional Redis types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeySource {
    Keys {
        keys: Vec<SharedString>,
    },
    Patterns {
        patterns: Vec<SharedString>,
        /// Optional Redis key types (e.g. `"string"`, `"hash"`, `"list"`,
        /// `"set"`, `"zset"`, `"stream"`, `"ReJSON-RL"`). Empty means all types.
        #[serde(default)]
        types: Vec<SharedString>,
    },
}

impl Default for KeySource {
    fn default() -> Self {
        Self::Keys { keys: Vec::new() }
    }
}

impl KeySource {
    pub fn keys(keys: Vec<SharedString>) -> Self {
        Self::Keys { keys }
    }

    pub fn patterns(patterns: Vec<SharedString>, types: Vec<SharedString>) -> Self {
        Self::Patterns { patterns, types }
    }

    pub fn is_patterns(&self) -> bool {
        matches!(self, Self::Patterns { .. })
    }

    /// Parses comma- or whitespace-separated glob patterns from a text input.
    /// Defaults to `["*"]` when the input is blank.
    pub fn parse_patterns_input(input: &str) -> Vec<SharedString> {
        let parsed: Vec<SharedString> = input
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| SharedString::from(s.to_string()))
            .collect();
        if parsed.is_empty() {
            vec![SharedString::from("*")]
        } else {
            parsed
        }
    }
}

/// Persisted batch cursor for resuming an interrupted Export or Copy job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MigrationResumeCursor {
    /// For `KeySource::Keys`: offset into `keys` already processed.
    pub completed_offset: usize,
    /// For `KeySource::Patterns`: active pattern index in `patterns`.
    pub pattern_index: usize,
    /// For `KeySource::Patterns`: active type index in `types`.
    pub type_index: usize,
    /// For `KeySource::Patterns`: per-master SCAN cursors (`empty` means start
    /// from cursor 0 on `(pattern_index, type_index)`).
    pub node_cursors: Vec<u64>,
    /// Total keys processed across completed batches.
    pub processed_count: usize,
}

/// Serializable job specification stored inside a [`MigrationCheckpoint`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "job_type", rename_all = "snake_case")]
pub enum MigrationCheckpointJob {
    Export {
        server_id: SharedString,
        db: usize,
        source: KeySource,
        output_path: PathBuf,
        format: ExportFormat,
        throttle: MigrationThrottle,
    },
    Copy {
        source_id: SharedString,
        source_db: usize,
        target_id: SharedString,
        target_db: usize,
        source: KeySource,
        conflict: ConflictMode,
        throttle: MigrationThrottle,
    },
}

impl Default for MigrationCheckpointJob {
    fn default() -> Self {
        Self::Export {
            server_id: SharedString::default(),
            db: 0,
            source: KeySource::default(),
            output_path: PathBuf::new(),
            format: ExportFormat::Binary,
            throttle: MigrationThrottle::Unlimited,
        }
    }
}

/// Complete persisted checkpoint for a migration job in `JOBS_TABLE`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MigrationCheckpoint {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub summary: String,
    pub job: MigrationCheckpointJob,
    pub cursor: MigrationResumeCursor,
}

impl MigrationCheckpoint {
    pub fn into_migration_job(self) -> MigrationJob {
        let resume = Some((self.id, self.cursor));
        match self.job {
            MigrationCheckpointJob::Export {
                server_id,
                db,
                source,
                output_path,
                format,
                throttle,
            } => MigrationJob::Export {
                server_id,
                db,
                source,
                output_path,
                format,
                throttle,
                resume,
            },
            MigrationCheckpointJob::Copy {
                source_id,
                source_db,
                target_id,
                target_db,
                source,
                conflict,
                throttle,
            } => MigrationJob::Copy {
                source_id,
                source_db,
                target_id,
                target_db,
                source,
                conflict,
                throttle,
                resume,
            },
        }
    }
}

pub fn load_migration_checkpoints() -> Vec<MigrationCheckpoint> {
    let records = JobManager::list_by_kind(MIGRATION_JOB_KIND);
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        match serde_json::from_str::<MigrationCheckpoint>(&record.payload_json) {
            Ok(mut cp) => {
                cp.id = record.id;
                cp.created_at = record.created_at;
                cp.updated_at = record.updated_at;
                if cp.summary.is_empty() {
                    cp.summary = record.summary;
                }
                out.push(cp);
            }
            Err(e) => {
                warn!(id = %record.id, error = %e, "unreadable migration checkpoint payload, skipped");
            }
        }
    }
    out
}

pub fn save_migration_checkpoint(cp: &MigrationCheckpoint) {
    let Ok(payload_json) = serde_json::to_string(cp) else {
        return;
    };
    let record = StoredJobRecord {
        id: cp.id.clone(),
        kind: MIGRATION_JOB_KIND.to_string(),
        created_at: cp.created_at,
        updated_at: cp.updated_at,
        summary: cp.summary.clone(),
        payload_json,
    };
    let _ = JobManager::upsert(&record);
}

pub fn delete_migration_checkpoint(id: &str) {
    let _ = JobManager::delete(id);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogStatus {
    Ok,
    Skipped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub key: SharedString,
    pub bytes: u64,
    pub status: LogStatus,
    pub message: Option<SharedString>,
}

#[derive(Debug, Clone, Default)]
pub struct MigrationProgress {
    pub keys_total: u64,
    pub keys_done: u64,
    pub keys_skipped: u64,
    pub keys_failed: u64,
    pub bytes_done: u64,
}

/// Events the view subscribes to. The view re-renders on any of them, but
/// distinct variants make it easy to add finer-grained subscribers later.
#[derive(Debug, Clone)]
pub enum MigrationEvent {
    Progress,
    PhaseChanged,
    LogAppended,
}

/// Output format for export jobs. `Binary` is the framed DUMP/RESTORE
/// bundle (machine migration, re-importable); `Json` / `Csv` are the
/// human-readable value exports for eyes and downstream tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    #[default]
    Binary,
    Json,
    Csv,
}

impl ExportFormat {
    /// Position in the format radio group.
    pub fn index(self) -> usize {
        match self {
            ExportFormat::Binary => 0,
            ExportFormat::Json => 1,
            ExportFormat::Csv => 2,
        }
    }
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => ExportFormat::Json,
            2 => ExportFormat::Csv,
            _ => ExportFormat::Binary,
        }
    }
    /// Suggested file extension.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Binary => "zedis-dump",
            ExportFormat::Json => "json",
            ExportFormat::Csv => "csv",
        }
    }
}

/// Job specification handed to `start`.
#[derive(Debug, Clone)]
pub enum MigrationJob {
    Export {
        server_id: SharedString,
        db: usize,
        source: KeySource,
        output_path: PathBuf,
        format: ExportFormat,
        throttle: MigrationThrottle,
        resume: Option<(String, MigrationResumeCursor)>,
    },
    Import {
        server_id: SharedString,
        db: usize,
        input_path: PathBuf,
        conflict: ConflictMode,
        throttle: MigrationThrottle,
    },
    /// Straight from one server / db to another: `DUMP` on the source,
    /// `RESTORE` on the target, no file in between.
    Copy {
        source_id: SharedString,
        source_db: usize,
        target_id: SharedString,
        target_db: usize,
        source: KeySource,
        conflict: ConflictMode,
        throttle: MigrationThrottle,
        resume: Option<(String, MigrationResumeCursor)>,
    },
}

#[derive(Default)]
pub struct MigrationState {
    phase: MigrationPhase,
    progress: MigrationProgress,
    log: VecDeque<LogLine>,
    copied_keys: Vec<SharedString>,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    worker: Option<Task<()>>,
}

impl MigrationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(&self) -> &MigrationPhase {
        &self.phase
    }

    pub fn progress(&self) -> &MigrationProgress {
        &self.progress
    }

    pub fn log(&self) -> &VecDeque<LogLine> {
        &self.log
    }

    pub fn copied_keys(&self) -> &[SharedString] {
        &self.copied_keys
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.phase, MigrationPhase::Paused)
    }

    /// Toggles pause/resume while a job is active.
    pub fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            MigrationPhase::Running => {
                self.paused.store(true, Ordering::Release);
                self.set_phase(MigrationPhase::Paused, cx);
            }
            MigrationPhase::Paused => {
                self.paused.store(false, Ordering::Release);
                self.set_phase(MigrationPhase::Running, cx);
            }
            _ => {}
        }
    }

    /// Cancels the current job. The worker checks the flag between batches.
    pub fn cancel(&mut self, _cx: &mut Context<Self>) {
        if !matches!(self.phase, MigrationPhase::Running | MigrationPhase::Paused) {
            return;
        }
        self.paused.store(false, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
    }

    /// Starts a new job. If one is already running, it is cancelled first.
    pub fn start(&mut self, job: MigrationJob, cx: &mut Context<Self>) {
        // Drop any previous worker handle so the old task is not retained.
        self.worker = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.paused = Arc::new(AtomicBool::new(false));
        self.progress = MigrationProgress::default();
        self.log.clear();
        self.copied_keys.clear();
        self.set_phase(MigrationPhase::Running, cx);

        let cancel = self.cancel.clone();
        let paused = self.paused.clone();

        let worker = cx.spawn(async move |handle, cx| match job {
            MigrationJob::Export {
                server_id,
                db,
                source,
                output_path,
                format,
                throttle,
                resume,
            } => {
                let spec = ExportSpec {
                    server_id,
                    db,
                    source,
                    output_path,
                    format,
                    throttle,
                    paused,
                    resume,
                };
                run_export(handle, cx, spec, cancel).await;
            }
            MigrationJob::Import {
                server_id,
                db,
                input_path,
                conflict,
                throttle,
            } => {
                let ctl = WorkerControl {
                    throttle,
                    paused,
                    cancel,
                };
                run_import(handle, cx, server_id, db, input_path, conflict, ctl).await;
            }
            MigrationJob::Copy {
                source_id,
                source_db,
                target_id,
                target_db,
                source,
                conflict,
                throttle,
                resume,
            } => {
                let spec = CopySpec {
                    source_id,
                    source_db,
                    target_id,
                    target_db,
                    source,
                    conflict,
                    throttle,
                    paused,
                    resume,
                };
                run_copy(handle, cx, spec, cancel).await;
            }
        });
        self.worker = Some(worker);
    }

    fn set_phase(&mut self, phase: MigrationPhase, cx: &mut Context<Self>) {
        self.phase = phase;
        cx.emit(MigrationEvent::PhaseChanged);
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_phase_for_test(&mut self, phase: MigrationPhase, cx: &mut Context<Self>) {
        self.set_phase(phase, cx);
    }

    /// Pushes a batch of log lines and notifies the view exactly once.
    ///
    /// Workers post one batch per chunk (~64 keys); calling `cx.notify()` per line
    /// would queue dozens of redraws per chunk and starve the UI thread.
    fn extend_log<I: IntoIterator<Item = LogLine>>(&mut self, lines: I, cx: &mut Context<Self>) {
        let before = self.log.len();
        for line in lines {
            if self.log.len() >= LOG_RING_CAPACITY {
                self.log.pop_front();
            }
            self.log.push_back(line);
        }
        if self.log.len() != before {
            cx.emit(MigrationEvent::LogAppended);
            cx.notify();
        }
    }

    fn record_copied_keys(&mut self, keys: &[String]) {
        for key in keys {
            if self.copied_keys.len() >= COPIED_KEYS_CAP {
                break;
            }
            self.copied_keys.push(SharedString::from(key.clone()));
        }
    }
}

impl EventEmitter<MigrationEvent> for MigrationState {}

// ---------------------------------------------------------------------------
// Worker bodies & Checkpointing
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct WorkerControl {
    throttle: MigrationThrottle,
    paused: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

fn new_checkpoint(job: MigrationCheckpointJob, resume: Option<(String, MigrationResumeCursor)>) -> MigrationCheckpoint {
    let now = Utc::now().timestamp();
    let summary = match &job {
        MigrationCheckpointJob::Export {
            server_id,
            db,
            source,
            output_path,
            ..
        } => {
            let src_name = get_server(server_id.as_str())
                .map(|s| s.name)
                .unwrap_or_else(|_| server_id.to_string());
            let scope = match source {
                KeySource::Keys { keys } => format!("{} keys", keys.len()),
                KeySource::Patterns { patterns, .. } => {
                    patterns.iter().map(|p| p.as_ref()).collect::<Vec<_>>().join(", ")
                }
            };
            format!("Export {src_name}/db{db} ({scope}) → {}", output_path.display())
        }
        MigrationCheckpointJob::Copy {
            source_id,
            source_db,
            target_id,
            target_db,
            source,
            ..
        } => {
            let src_name = get_server(source_id.as_str())
                .map(|s| s.name)
                .unwrap_or_else(|_| source_id.to_string());
            let dst_name = get_server(target_id.as_str())
                .map(|s| s.name)
                .unwrap_or_else(|_| target_id.to_string());
            let scope = match source {
                KeySource::Keys { keys } => format!("{} keys", keys.len()),
                KeySource::Patterns { patterns, .. } => {
                    patterns.iter().map(|p| p.as_ref()).collect::<Vec<_>>().join(", ")
                }
            };
            format!("Copy {src_name}/db{source_db} → {dst_name}/db{target_db} ({scope})")
        }
    };
    let (id, cursor) = match resume {
        Some((id, cursor)) => (id, cursor),
        None => (
            format!("mig-{}", Utc::now().timestamp_nanos_opt().unwrap_or(now * 1_000_000)),
            MigrationResumeCursor::default(),
        ),
    };
    MigrationCheckpoint {
        id,
        created_at: now,
        updated_at: now,
        summary,
        job,
        cursor,
    }
}

fn update_checkpoint(cp: &mut MigrationCheckpoint, cursor: MigrationResumeCursor) {
    cp.updated_at = Utc::now().timestamp();
    cp.cursor = cursor;
    save_migration_checkpoint(cp);
}

/// Waits while `paused` is set (waking promptly if `cancel` is triggered),
/// and enforces the optional per-batch rate limit (`throttle`).
async fn pace_and_wait_if_paused(
    paused: &AtomicBool,
    cancel: &AtomicBool,
    throttle: MigrationThrottle,
    batch_keys: usize,
    batch_started_at: Instant,
    cx: &gpui::AsyncApp,
) {
    let rate = throttle.keys_per_sec();
    if rate > 0 && batch_keys > 0 && !cancel.load(Ordering::Acquire) {
        let target_dur = Duration::from_secs_f64(batch_keys as f64 / rate as f64);
        let elapsed = batch_started_at.elapsed();
        if let Some(rem) = target_dur.checked_sub(elapsed) {
            cx.background_executor().timer(rem).await;
        }
    }
    while paused.load(Ordering::Acquire) && !cancel.load(Ordering::Acquire) {
        cx.background_executor().timer(PAUSE_POLL_INTERVAL).await;
    }
}

/// Drives batches from a [`KeySource`] starting at `initial_cursor`, invoking
/// `process_batch(keys, next_cursor)` for each non-empty batch of keys.
async fn for_each_source_batch<F, Fut>(
    at: &ServerDb,
    source: &KeySource,
    initial_cursor: MigrationResumeCursor,
    cancel: &AtomicBool,
    mut on_checkpoint: impl FnMut(MigrationResumeCursor),
    mut process_batch: F,
) -> Result<()>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    match source {
        KeySource::Keys { keys } => {
            let mut offset = initial_cursor.completed_offset.min(keys.len());
            let mut processed = initial_cursor.processed_count;
            for chunk in keys[offset..].chunks(DUMP_BATCH_SIZE) {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let batch: Vec<String> = chunk.iter().map(|k| k.to_string()).collect();
                let batch_len = batch.len();
                process_batch(batch).await?;
                offset += batch_len;
                processed += batch_len;
                on_checkpoint(MigrationResumeCursor {
                    completed_offset: offset,
                    processed_count: processed,
                    ..Default::default()
                });
            }
        }
        KeySource::Patterns { patterns, types } => {
            let pat_list: Vec<SharedString> = if patterns.is_empty() {
                vec![SharedString::from("*")]
            } else {
                patterns.clone()
            };
            let type_list: Vec<Option<SharedString>> = if types.is_empty() {
                vec![None]
            } else {
                types.iter().cloned().map(Some).collect()
            };
            let mut processed = initial_cursor.processed_count;
            let mut seen: AHashSet<String> = AHashSet::new();
            let start_pat = initial_cursor.pattern_index.min(pat_list.len());

            for (pat_ix, pattern) in pat_list.iter().enumerate().skip(start_pat) {
                let start_ty = if pat_ix == initial_cursor.pattern_index {
                    initial_cursor.type_index.min(type_list.len())
                } else {
                    0
                };
                for (ty_ix, key_type) in type_list.iter().enumerate().skip(start_ty) {
                    let mut cursors: Option<Vec<u64>> = if pat_ix == initial_cursor.pattern_index
                        && ty_ix == initial_cursor.type_index
                        && !initial_cursor.node_cursors.is_empty()
                    {
                        Some(initial_cursor.node_cursors.clone())
                    } else {
                        None
                    };
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let (scanned, next_cursors, done) = scan_pattern_batch(
                            at,
                            pattern.as_ref(),
                            key_type.as_ref().map(|t| t.as_ref()),
                            cursors,
                            DUMP_BATCH_SIZE,
                        )
                        .await?;
                        let batch: Vec<String> = scanned.into_iter().filter(|k| seen.insert(k.clone())).collect();
                        if !batch.is_empty() {
                            let batch_len = batch.len();
                            process_batch(batch).await?;
                            processed += batch_len;
                        }
                        on_checkpoint(MigrationResumeCursor {
                            completed_offset: 0,
                            pattern_index: pat_ix,
                            type_index: ty_ix,
                            node_cursors: if done { Vec::new() } else { next_cursors.clone() },
                            processed_count: processed,
                        });
                        if done {
                            break;
                        }
                        cursors = Some(next_cursors);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Export parameters, grouped so the worker signatures stay under
/// clippy's argument limit.
struct ExportSpec {
    server_id: SharedString,
    db: usize,
    source: KeySource,
    output_path: PathBuf,
    format: ExportFormat,
    throttle: MigrationThrottle,
    paused: Arc<AtomicBool>,
    resume: Option<(String, MigrationResumeCursor)>,
}

async fn run_export(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: ExportSpec,
    cancel: Arc<AtomicBool>,
) {
    let total = match &spec.source {
        KeySource::Keys { keys } => keys.len() as u64,
        KeySource::Patterns { .. } => 0,
    };
    let initial_done = spec.resume.as_ref().map(|(_, c)| c.processed_count as u64).unwrap_or(0);
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = total;
        s.progress.keys_done = initial_done;
        cx.notify();
    });

    let cp_job = MigrationCheckpointJob::Export {
        server_id: spec.server_id.clone(),
        db: spec.db,
        source: spec.source.clone(),
        output_path: spec.output_path.clone(),
        format: spec.format,
        throttle: spec.throttle,
    };
    let mut checkpoint = new_checkpoint(cp_job, spec.resume.clone());
    save_migration_checkpoint(&checkpoint);
    let job_id = checkpoint.id.clone();

    let result = match spec.format {
        ExportFormat::Binary => export_worker(handle.clone(), cx, spec, &mut checkpoint, cancel.clone()).await,
        ExportFormat::Json | ExportFormat::Csv => {
            readable_export_worker(handle.clone(), cx, spec, &mut checkpoint, cancel.clone()).await
        }
    };
    match result {
        Ok(()) => {
            let cancelled = cancel.load(Ordering::Acquire);
            if !cancelled {
                delete_migration_checkpoint(&job_id);
            }
            let phase = if cancelled {
                MigrationPhase::Cancelled
            } else {
                MigrationPhase::Finished
            };
            let _ = handle.update(cx, |s, cx| s.set_phase(phase, cx));
        }
        Err(e) => {
            error!(error = %e, "export job failed");
            let _ = handle.update(cx, |s, cx| {
                s.set_phase(MigrationPhase::Failed(e.to_string().into()), cx)
            });
        }
    }
}

async fn export_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: ExportSpec,
    checkpoint: &mut MigrationCheckpoint,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let ExportSpec {
        server_id,
        db,
        source,
        output_path,
        throttle,
        paused,
        ..
    } = spec;
    let server_name = get_server(server_id.as_str())
        .map(|s| s.name)
        .unwrap_or_else(|_| server_id.to_string());

    let at = ServerDb::new(server_id.as_str(), db);
    let redis_version = server_summary(&at).await?.version;
    let key_count = match &source {
        KeySource::Keys { keys } => keys.len() as u64,
        KeySource::Patterns { .. } => 0,
    };

    let header = DumpHeader {
        format_version: 1,
        source_server_name: server_name,
        source_redis_version: redis_version,
        source_db: db as u32,
        key_count,
        created_at_ms: Utc::now().timestamp_millis(),
    };

    let path_for_open = output_path.clone();
    let header_for_open = header.clone();
    let writer_cell = Arc::new(parking_lot::Mutex::new(Some(
        smol::unblock(move || -> Result<DumpWriter<BufWriter<File>>> {
            let file = File::create(&path_for_open)?;
            Ok(DumpWriter::new(BufWriter::new(file), &header_for_open)?)
        })
        .await?,
    )));

    let initial_cursor = checkpoint.cursor.clone();
    let writer_for_loop = writer_cell.clone();
    let cx_ptr: *mut gpui::AsyncApp = cx;

    for_each_source_batch(
        &at,
        &source,
        initial_cursor,
        &cancel,
        |next_cursor| update_checkpoint(checkpoint, next_cursor),
        |chunk| {
            let writer_for_loop = writer_for_loop.clone();
            let handle = handle.clone();
            let paused = paused.clone();
            let cancel = cancel.clone();
            let at = at.clone();
            // SAFETY: `for_each_source_batch` awaits each `process_batch` future sequentially
            // within the same task before starting the next batch.
            let cx = unsafe { &mut *cx_ptr };
            async move {
                let batch_started_at = Instant::now();
                let entries = dump_keys_chunk(&at, &chunk).await?;
                let chunk_total = chunk.len();
                let dumped_count = entries.len();
                let bytes_in_chunk: u64 = entries.iter().map(|e| e.payload.len() as u64).sum();

                let entries_for_write = entries.clone();
                let mut writer = writer_for_loop.lock().take().ok_or_else(|| Error::Invalid {
                    message: "export writer missing".to_string(),
                })?;
                writer = smol::unblock(move || -> Result<DumpWriter<BufWriter<File>>> {
                    for entry in &entries_for_write {
                        writer.write_entry(entry)?;
                    }
                    Ok(writer)
                })
                .await?;
                *writer_for_loop.lock() = Some(writer);

                let mut log_lines: Vec<LogLine> = entries
                    .iter()
                    .map(|entry| LogLine {
                        key: String::from_utf8_lossy(&entry.key).into_owned().into(),
                        bytes: entry.payload.len() as u64,
                        status: LogStatus::Ok,
                        message: None,
                    })
                    .collect();

                let skipped = chunk_total - dumped_count;
                if skipped > 0 {
                    let dumped_set: AHashSet<&[u8]> = entries.iter().map(|e| e.key.as_slice()).collect();
                    for key in chunk.iter().filter(|k| !dumped_set.contains(k.as_str().as_bytes())) {
                        log_lines.push(LogLine {
                            key: key.clone().into(),
                            bytes: 0,
                            status: LogStatus::Skipped,
                            message: Some("missing".into()),
                        });
                    }
                }

                handle
                    .update(cx, |s, cx| {
                        s.progress.keys_done += dumped_count as u64;
                        s.progress.keys_skipped += skipped as u64;
                        s.progress.bytes_done += bytes_in_chunk;
                        cx.emit(MigrationEvent::Progress);
                        s.extend_log(log_lines, cx);
                    })
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                pace_and_wait_if_paused(&paused, &cancel, throttle, chunk_total, batch_started_at, cx).await;
                Ok(())
            }
        },
    )
    .await?;

    let writer_opt = writer_cell.lock().take();
    if let Some(writer) = writer_opt {
        smol::unblock(move || writer.finish().map(|_| ())).await?;
    }
    debug!(path = %output_path.display(), "export finished");
    info!(server = %server_id, db, "export finished");
    Ok(())
}

/// Human-readable export: full values as one JSON array (or CSV rows)
/// instead of DUMP payloads — for eyes and downstream tools, not
/// re-import. Values are lossy UTF-8 of the raw bytes; module-typed keys
/// are recorded with a null value and logged as skipped.
async fn readable_export_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: ExportSpec,
    checkpoint: &mut MigrationCheckpoint,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let ExportSpec {
        server_id,
        db,
        source,
        output_path,
        format,
        throttle,
        paused,
        ..
    } = spec;
    let at = ServerDb::new(server_id.as_str(), db);

    let path_for_open = output_path.clone();
    let writer_cell = Arc::new(parking_lot::Mutex::new(Some(
        smol::unblock(move || -> Result<BufWriter<File>> {
            let mut writer = BufWriter::new(File::create(&path_for_open)?);
            match format {
                ExportFormat::Json => writer.write_all(b"[")?,
                ExportFormat::Csv => writer.write_all(csv_header().as_bytes())?,
                ExportFormat::Binary => unreachable!("binary goes through export_worker"),
            }
            Ok(writer)
        })
        .await?,
    )));

    let first_entry = Arc::new(AtomicBool::new(true));
    let initial_cursor = checkpoint.cursor.clone();
    let writer_for_loop = writer_cell.clone();
    let cx_ptr: *mut gpui::AsyncApp = cx;

    for_each_source_batch(
        &at,
        &source,
        initial_cursor,
        &cancel,
        |next_cursor| update_checkpoint(checkpoint, next_cursor),
        |chunk| {
            let writer_for_loop = writer_for_loop.clone();
            let first_entry = first_entry.clone();
            let handle = handle.clone();
            let paused = paused.clone();
            let cancel = cancel.clone();
            let at = at.clone();
            let cx = unsafe { &mut *cx_ptr };
            async move {
                let batch_started_at = Instant::now();
                let entries = read_readable_chunk(&at, &chunk, ReadLimits::default()).await?;

                let mut payload = String::new();
                let mut log_lines: Vec<LogLine> = Vec::with_capacity(chunk.len());
                for entry in &entries {
                    let before = payload.len();
                    match format {
                        ExportFormat::Json => {
                            let is_first = first_entry.swap(false, Ordering::AcqRel);
                            payload.push_str(if is_first { "\n" } else { ",\n" });
                            payload.push_str(&entry_to_json(entry).to_string());
                        }
                        ExportFormat::Csv => payload.push_str(&entry_to_csv(entry)),
                        ExportFormat::Binary => unreachable!(),
                    }
                    let (status, message) = if entry.value.is_some() {
                        let message = entry.truncated.then(|| SharedString::from("truncated"));
                        (LogStatus::Ok, message)
                    } else {
                        (LogStatus::Skipped, Some(SharedString::from(entry.key_type.clone())))
                    };
                    log_lines.push(LogLine {
                        key: entry.key.clone().into(),
                        bytes: (payload.len() - before) as u64,
                        status,
                        message,
                    });
                }
                let fetched: AHashSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
                for key in chunk.iter().filter(|k| !fetched.contains(k.as_str())) {
                    log_lines.push(LogLine {
                        key: key.clone().into(),
                        bytes: 0,
                        status: LogStatus::Skipped,
                        message: Some("missing".into()),
                    });
                }

                let done = entries.iter().filter(|entry| entry.value.is_some()).count();
                let skipped = chunk.len() - done;
                let bytes_in_chunk = payload.len() as u64;
                let mut writer = writer_for_loop.lock().take().ok_or_else(|| Error::Invalid {
                    message: "readable export writer missing".to_string(),
                })?;
                writer = smol::unblock(move || -> Result<BufWriter<File>> {
                    writer.write_all(payload.as_bytes())?;
                    Ok(writer)
                })
                .await?;
                *writer_for_loop.lock() = Some(writer);

                handle
                    .update(cx, |s, cx| {
                        s.progress.keys_done += done as u64;
                        s.progress.keys_skipped += skipped as u64;
                        s.progress.bytes_done += bytes_in_chunk;
                        cx.emit(MigrationEvent::Progress);
                        s.extend_log(log_lines, cx);
                    })
                    .map_err(|e| Error::Invalid { message: e.to_string() })?;
                pace_and_wait_if_paused(&paused, &cancel, throttle, chunk.len(), batch_started_at, cx).await;
                Ok(())
            }
        },
    )
    .await?;

    let writer_opt = writer_cell.lock().take();
    if let Some(mut writer) = writer_opt {
        smol::unblock(move || -> Result<()> {
            if format == ExportFormat::Json {
                writer.write_all(b"\n]\n")?;
            }
            writer.flush()?;
            Ok(())
        })
        .await?;
    }
    info!(server = %server_id, db, format = ?format, "readable export finished");
    Ok(())
}

async fn run_import(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    conflict: ConflictMode,
    ctl: WorkerControl,
) {
    let cancel = ctl.cancel.clone();
    match import_worker(handle.clone(), cx, server_id, db, input_path, conflict, ctl).await {
        Ok(()) => {
            let phase = if cancel.load(Ordering::Acquire) {
                MigrationPhase::Cancelled
            } else {
                MigrationPhase::Finished
            };
            let _ = handle.update(cx, |s, cx| s.set_phase(phase, cx));
        }
        Err(e) => {
            error!(error = %e, "import job failed");
            let _ = handle.update(cx, |s, cx| {
                s.set_phase(MigrationPhase::Failed(e.to_string().into()), cx)
            });
        }
    }
}

/// Dispatch on the sniffed file format: the framed binary bundle streams
/// through `DumpReader` + `RESTORE`; the readable JSON/CSV exports are
/// parsed up front (fail-fast on a hand-edit typo, before any write) and
/// written back with type-native commands.
async fn import_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    conflict: ConflictMode,
    ctl: WorkerControl,
) -> Result<()> {
    let path_for_sniff = input_path.clone();
    let format = smol::unblock(move || detect_import_format(&path_for_sniff)).await?;
    match format {
        ImportFormat::Binary => import_binary_worker(handle, cx, server_id, db, input_path, conflict, ctl).await,
        ImportFormat::Json | ImportFormat::Csv => {
            let spec = ReadableImportSpec {
                server_id,
                db,
                input_path,
                format,
                conflict,
                ctl,
            };
            import_readable_worker(handle, cx, spec).await
        }
    }
}

/// Readable-import parameters, grouped like [`ExportSpec`] so the worker
/// signature stays under clippy's argument limit.
struct ReadableImportSpec {
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    format: ImportFormat,
    conflict: ConflictMode,
    ctl: WorkerControl,
}

async fn import_binary_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    conflict: ConflictMode,
    ctl: WorkerControl,
) -> Result<()> {
    let WorkerControl {
        throttle,
        paused,
        cancel,
    } = ctl;
    let at = ServerDb::new(server_id.as_str(), db);

    let path_for_open = input_path.clone();
    let mut reader = smol::unblock(move || -> Result<DumpReader<BufReader<File>>> {
        let file = File::open(&path_for_open)?;
        Ok(DumpReader::open(BufReader::new(file))?)
    })
    .await?;
    let header_total = reader.header().key_count;
    let dumped_at_ms = reader.header().created_at_ms;
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = header_total;
        cx.notify();
    });

    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let batch_started_at = Instant::now();
        let (returned_reader, mut batch, eof) =
            smol::unblock(move || -> Result<(DumpReader<BufReader<File>>, Vec<DumpEntry>, bool)> {
                let mut batch = Vec::with_capacity(RESTORE_BATCH_SIZE);
                let mut eof = false;
                while batch.len() < RESTORE_BATCH_SIZE {
                    match reader.read_entry()? {
                        Some(entry) => batch.push(entry),
                        None => {
                            eof = true;
                            break;
                        }
                    }
                }
                Ok((reader, batch, eof))
            })
            .await?;
        reader = returned_reader;
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len();
        let now_ms = Utc::now().timestamp_millis();
        let mut expired = Vec::new();
        batch.retain_mut(|entry| match ttl_left(entry.pttl_ms, dumped_at_ms, now_ms) {
            Some(left) => {
                entry.pttl_ms = left;
                true
            }
            None => {
                expired.push(LogLine {
                    key: String::from_utf8_lossy(&entry.key).into_owned().into(),
                    bytes: 0,
                    status: LogStatus::Skipped,
                    message: Some("expired".into()),
                });
                false
            }
        });
        if !expired.is_empty() {
            let count = expired.len() as u64;
            handle
                .update(cx, |s, cx| {
                    s.progress.keys_skipped += count;
                    cx.emit(MigrationEvent::Progress);
                    s.extend_log(expired, cx);
                })
                .map_err(|e| Error::Invalid { message: e.to_string() })?;
        }
        flush_restore_batch(&handle, cx, None, &at, &mut batch, conflict).await?;
        pace_and_wait_if_paused(&paused, &cancel, throttle, batch_len, batch_started_at, cx).await;
        if eof {
            break;
        }
    }
    Ok(())
}

/// Import a readable JSON/CSV export: parse the whole file first (they
/// exist for hand-edited, human-scale data — a typo fails here, before
/// anything is written), then write in chunks with type-native commands.
async fn import_readable_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: ReadableImportSpec,
) -> Result<()> {
    let ReadableImportSpec {
        server_id,
        db,
        input_path,
        format,
        conflict,
        ctl: WorkerControl {
            throttle,
            paused,
            cancel,
        },
    } = spec;
    let entries = smol::unblock(move || -> Result<Vec<ReadableEntry>> {
        let text = fs::read_to_string(&input_path)?;
        Ok(parse_readable_entries(&text, format)?)
    })
    .await?;

    let total = entries.len() as u64;
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = total;
        cx.notify();
    });

    let at = ServerDb::new(server_id.as_str(), db);

    for chunk in entries.chunks(RESTORE_BATCH_SIZE) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let batch_started_at = Instant::now();
        let statuses = write_readable_chunk(&at, chunk, conflict).await?;
        let mut written = 0u64;
        let mut skipped = 0u64;
        let mut failed = 0u64;
        let mut bytes = 0u64;
        let mut log_lines: Vec<LogLine> = Vec::with_capacity(chunk.len());
        for (entry, status) in chunk.iter().zip(statuses) {
            let size = entry.approx_bytes();
            let (log_status, message) = match status {
                ReadableWriteStatus::Written => {
                    written += 1;
                    bytes += size;
                    (LogStatus::Ok, None)
                }
                ReadableWriteStatus::SkippedExists => {
                    skipped += 1;
                    (LogStatus::Skipped, Some(SharedString::from("already exists")))
                }
                ReadableWriteStatus::SkippedTruncated => {
                    skipped += 1;
                    (LogStatus::Skipped, Some(SharedString::from("truncated in the export")))
                }
                ReadableWriteStatus::SkippedUnsupported => {
                    skipped += 1;
                    (LogStatus::Skipped, Some(SharedString::from(entry.key_type.clone())))
                }
                ReadableWriteStatus::SkippedEmpty => {
                    skipped += 1;
                    (LogStatus::Skipped, Some(SharedString::from("empty")))
                }
                ReadableWriteStatus::Failed(msg) => {
                    failed += 1;
                    (LogStatus::Failed, Some(SharedString::from(msg)))
                }
            };
            log_lines.push(LogLine {
                key: entry.key.clone().into(),
                bytes: size,
                status: log_status,
                message,
            });
        }
        handle
            .update(cx, |s, cx| {
                s.progress.keys_done += written;
                s.progress.keys_skipped += skipped;
                s.progress.keys_failed += failed;
                s.progress.bytes_done += bytes;
                cx.emit(MigrationEvent::Progress);
                s.extend_log(log_lines, cx);
            })
            .map_err(|e| Error::Invalid { message: e.to_string() })?;
        pace_and_wait_if_paused(&paused, &cancel, throttle, chunk.len(), batch_started_at, cx).await;
    }
    Ok(())
}

/// Copy parameters, grouped like [`ExportSpec`].
struct CopySpec {
    source_id: SharedString,
    source_db: usize,
    target_id: SharedString,
    target_db: usize,
    source: KeySource,
    conflict: ConflictMode,
    throttle: MigrationThrottle,
    paused: Arc<AtomicBool>,
    resume: Option<(String, MigrationResumeCursor)>,
}

async fn run_copy(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: CopySpec,
    cancel: Arc<AtomicBool>,
) {
    let cp_job = MigrationCheckpointJob::Copy {
        source_id: spec.source_id.clone(),
        source_db: spec.source_db,
        target_id: spec.target_id.clone(),
        target_db: spec.target_db,
        source: spec.source.clone(),
        conflict: spec.conflict,
        throttle: spec.throttle,
    };
    let mut checkpoint = new_checkpoint(cp_job, spec.resume.clone());
    save_migration_checkpoint(&checkpoint);
    let job_id = checkpoint.id.clone();

    match copy_worker(handle.clone(), cx, spec, &mut checkpoint, cancel.clone()).await {
        Ok(()) => {
            let cancelled = cancel.load(Ordering::Acquire);
            if !cancelled {
                delete_migration_checkpoint(&job_id);
            }
            let phase = if cancelled {
                MigrationPhase::Cancelled
            } else {
                MigrationPhase::Finished
            };
            let _ = handle.update(cx, |s, cx| s.set_phase(phase, cx));
        }
        Err(e) => {
            error!(error = %e, "copy job failed");
            let _ = handle.update(cx, |s, cx| {
                s.set_phase(MigrationPhase::Failed(e.to_string().into()), cx)
            });
        }
    }
}

/// Server-to-server copy: `DUMP` a chunk on the source and `RESTORE` it on
/// the target, straight through. Both ends are Zedis's own connections, so
/// a tunnel, TLS or a password on either side is no different from any
/// other command — nothing asks the source to reach the target itself the
/// way `MIGRATE` would. TTLs travel with the dump.
async fn copy_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: CopySpec,
    checkpoint: &mut MigrationCheckpoint,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let CopySpec {
        source_id,
        source_db,
        target_id,
        target_db,
        source,
        conflict,
        throttle,
        paused,
        ..
    } = spec;
    let total = match &source {
        KeySource::Keys { keys } => keys.len() as u64,
        KeySource::Patterns { .. } => 0,
    };
    let initial_done = checkpoint.cursor.processed_count as u64;
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = total;
        s.progress.keys_done = initial_done;
        cx.notify();
    });
    let src = ServerDb::new(source_id.as_str(), source_db);
    let dst = ServerDb::new(target_id.as_str(), target_db);
    let initial_cursor = checkpoint.cursor.clone();
    let cx_ptr: *mut gpui::AsyncApp = cx;

    for_each_source_batch(
        &src,
        &source,
        initial_cursor,
        &cancel,
        |next_cursor| update_checkpoint(checkpoint, next_cursor),
        |chunk| {
            let handle = handle.clone();
            let paused = paused.clone();
            let cancel = cancel.clone();
            let src = src.clone();
            let dst = dst.clone();
            let cx = unsafe { &mut *cx_ptr };
            async move {
                let batch_started_at = Instant::now();
                let _ = handle.update(cx, |s, _cx| s.record_copied_keys(&chunk));
                let mut entries = dump_keys_chunk(&src, &chunk).await?;
                let dumped: AHashSet<&[u8]> = entries.iter().map(|e| e.key.as_slice()).collect();
                let missing: Vec<LogLine> = chunk
                    .iter()
                    .filter(|key| !dumped.contains(key.as_bytes()))
                    .map(|key| LogLine {
                        key: key.clone().into(),
                        bytes: 0,
                        status: LogStatus::Skipped,
                        message: Some("missing".into()),
                    })
                    .collect();
                flush_restore_batch(&handle, cx, Some(&src), &dst, &mut entries, conflict).await?;
                if !missing.is_empty() {
                    let count = missing.len() as u64;
                    handle
                        .update(cx, |s, cx| {
                            s.progress.keys_skipped += count;
                            cx.emit(MigrationEvent::Progress);
                            s.extend_log(missing, cx);
                        })
                        .map_err(|e| Error::Invalid { message: e.to_string() })?;
                }
                pace_and_wait_if_paused(&paused, &cancel, throttle, chunk.len(), batch_started_at, cx).await;
                Ok(())
            }
        },
    )
    .await?;
    info!(source = %source_id, source_db, target = %target_id, target_db, "copy finished");
    Ok(())
}

/// Restore a batch onto `at`. With a `src` — a server-to-server copy — a
/// payload the target cannot read (Redis and Valkey number their RDB
/// versions apart) is re-created by type from the source instead; from a
/// file there is no source to read, so the line says what to do.
async fn flush_restore_batch(
    handle: &gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    src: Option<&ServerDb>,
    at: &ServerDb,
    buffer: &mut Vec<DumpEntry>,
    conflict: ConflictMode,
) -> Result<()> {
    let entries = std::mem::take(buffer);
    let statuses = match src {
        Some(src) => restore_or_recreate_chunk(src, at, &entries, conflict).await?,
        None => restore_keys_chunk(at, &entries, conflict).await?,
    };
    let mut written = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    let mut bytes = 0u64;
    let mut log_lines: Vec<LogLine> = Vec::with_capacity(entries.len());
    for (entry, status) in entries.iter().zip(statuses.iter()) {
        let key: SharedString = String::from_utf8_lossy(&entry.key).into_owned().into();
        let size = entry.payload.len() as u64;
        match status {
            RestoreStatus::Written => {
                written += 1;
                bytes += size;
                log_lines.push(LogLine {
                    key,
                    bytes: size,
                    status: LogStatus::Ok,
                    message: None,
                });
            }
            RestoreStatus::Recreated => {
                written += 1;
                bytes += size;
                log_lines.push(LogLine {
                    key,
                    bytes: size,
                    status: LogStatus::Ok,
                    message: Some("re-created by type: the target does not read this server's DUMP payloads".into()),
                });
            }
            RestoreStatus::Skipped => {
                skipped += 1;
                log_lines.push(LogLine {
                    key,
                    bytes: size,
                    status: LogStatus::Skipped,
                    message: Some("already exists".into()),
                });
            }
            RestoreStatus::Failed(msg) => {
                failed += 1;
                let message = if src.is_none() && is_foreign_payload(msg) {
                    format!(
                        "{msg} — the file was written by a server whose DUMP payloads this one cannot read \
                         (Redis and Valkey number their RDB versions apart); export as JSON to move keys between them"
                    )
                } else {
                    msg.clone()
                };
                log_lines.push(LogLine {
                    key,
                    bytes: size,
                    status: LogStatus::Failed,
                    message: Some(message.into()),
                });
            }
        }
    }
    handle
        .update(cx, |s, cx| {
            s.progress.keys_done += written;
            s.progress.keys_skipped += skipped;
            s.progress.keys_failed += failed;
            s.progress.bytes_done += bytes;
            cx.emit(MigrationEvent::Progress);
            s.extend_log(log_lines, cx);
        })
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    Ok(())
}

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
    entry_to_json, entry_to_redis_commands, get_server, is_foreign_payload, parse_readable_entries,
    read_readable_chunk, restore_keys_chunk, restore_or_recreate_chunk, scan_pattern_batch, server_summary, ttl_left,
    write_readable_chunk,
};
use crate::db::{JobManager, StoredJobRecord};
use crate::error::Error;
use ahash::AHashSet;
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

mod workers;

use workers::{CopySpec, ExportSpec, WorkerControl, run_copy, run_export, run_import};

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
    CommandScript,
}

impl ExportFormat {
    /// Position in the format radio group.
    pub fn index(self) -> usize {
        match self {
            ExportFormat::Binary => 0,
            ExportFormat::Json => 1,
            ExportFormat::Csv => 2,
            ExportFormat::CommandScript => 3,
        }
    }
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => ExportFormat::Json,
            2 => ExportFormat::Csv,
            3 => ExportFormat::CommandScript,
            _ => ExportFormat::Binary,
        }
    }
    /// Suggested file extension.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Binary => "zedis-dump",
            ExportFormat::Json => "json",
            ExportFormat::Csv => "csv",
            ExportFormat::CommandScript => "redis",
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

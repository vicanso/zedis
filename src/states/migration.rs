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

//! State for export / import migration jobs.
//!
//! Drives a single background worker that runs a [`MigrationJob`] and
//! posts incremental progress back to the foreground entity.

use crate::connection::{
    ConflictMode, DumpEntry, DumpHeader, DumpReader, DumpWriter, ImportFormat, ReadLimits, ReadableEntry,
    ReadableWriteStatus, RedisAsyncConn, RestoreStatus, csv_header, detect_import_format, dump_keys_chunk,
    entry_to_csv, entry_to_json, get_connection_manager, get_server, parse_readable_entries, read_readable_chunk,
    restore_keys_chunk, write_readable_chunk,
};
use crate::error::Error;
use chrono::Utc;
use gpui::SharedString;
use gpui::prelude::*;
use gpui::{EventEmitter, Task};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error, info};

const DUMP_BATCH_SIZE: usize = 64;
const RESTORE_BATCH_SIZE: usize = 64;
const LOG_RING_CAPACITY: usize = 500;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Phase of a migration job. UI renders different chrome based on this.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum MigrationPhase {
    #[default]
    Idle,
    Running,
    Finished,
    Failed(SharedString),
    Cancelled,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
        keys: Vec<SharedString>,
        output_path: PathBuf,
        format: ExportFormat,
    },
    Import {
        server_id: SharedString,
        db: usize,
        input_path: PathBuf,
        conflict: ConflictMode,
    },
}

#[derive(Default)]
pub struct MigrationState {
    phase: MigrationPhase,
    progress: MigrationProgress,
    log: VecDeque<LogLine>,
    cancel: Arc<AtomicBool>,
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

    /// Cancels the current job. The worker checks the flag between batches.
    pub fn cancel(&mut self, _cx: &mut Context<Self>) {
        if !matches!(self.phase, MigrationPhase::Running) {
            return;
        }
        self.cancel.store(true, Ordering::Release);
    }

    /// Starts a new job. If one is already running, it is cancelled first.
    pub fn start(&mut self, job: MigrationJob, cx: &mut Context<Self>) {
        // Drop any previous worker handle so the old task is not retained.
        self.worker = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = MigrationProgress::default();
        self.log.clear();
        self.set_phase(MigrationPhase::Running, cx);

        let cancel = self.cancel.clone();

        let worker = cx.spawn(async move |handle, cx| match job {
            MigrationJob::Export {
                server_id,
                db,
                keys,
                output_path,
                format,
            } => {
                let spec = ExportSpec {
                    server_id,
                    db,
                    keys,
                    output_path,
                    format,
                };
                run_export(handle, cx, spec, cancel).await;
            }
            MigrationJob::Import {
                server_id,
                db,
                input_path,
                conflict,
            } => {
                run_import(handle, cx, server_id, db, input_path, conflict, cancel).await;
            }
        });
        self.worker = Some(worker);
    }

    fn set_phase(&mut self, phase: MigrationPhase, cx: &mut Context<Self>) {
        self.phase = phase;
        cx.emit(MigrationEvent::PhaseChanged);
        cx.notify();
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
}

impl EventEmitter<MigrationEvent> for MigrationState {}

// ---------------------------------------------------------------------------
// Worker bodies
// ---------------------------------------------------------------------------

/// Export parameters, grouped so the worker signatures stay under
/// clippy's argument limit.
struct ExportSpec {
    server_id: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    output_path: PathBuf,
    format: ExportFormat,
}

async fn run_export(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    spec: ExportSpec,
    cancel: Arc<AtomicBool>,
) {
    let total = spec.keys.len() as u64;
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = total;
        cx.notify();
    });

    let result = match spec.format {
        ExportFormat::Binary => export_worker(handle.clone(), cx, spec, cancel.clone()).await,
        ExportFormat::Json | ExportFormat::Csv => {
            readable_export_worker(handle.clone(), cx, spec, cancel.clone()).await
        }
    };
    match result {
        Ok(()) => {
            let phase = if cancel.load(Ordering::Acquire) {
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
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let ExportSpec {
        server_id,
        db,
        keys,
        output_path,
        ..
    } = spec;
    let server_name = get_server(server_id.as_str())
        .map(|s| s.name)
        .unwrap_or_else(|_| server_id.to_string());

    let client = get_connection_manager().get_client(server_id.as_str(), db).await?;
    let redis_version = client.version().to_string();
    let mut conn = client.connection();

    let header = DumpHeader {
        format_version: 1,
        source_server_name: server_name,
        source_redis_version: redis_version,
        source_db: db as u32,
        key_count: keys.len() as u64,
        created_at_ms: Utc::now().timestamp_millis(),
    };

    // Open the file and write the header on a blocking thread — the I/O is sync,
    // running it on the async executor would stall everything else (including UI).
    let path_for_open = output_path.clone();
    let header_for_open = header.clone();
    let mut writer = smol::unblock(move || -> Result<DumpWriter<BufWriter<File>>> {
        let file = File::create(&path_for_open)?;
        Ok(DumpWriter::new(BufWriter::new(file), &header_for_open)?)
    })
    .await?;

    for chunk in keys.chunks(DUMP_BATCH_SIZE) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let chunk: Vec<String> = chunk.iter().map(|k| k.to_string()).collect();
        let entries = dump_keys_chunk(&mut conn, &chunk).await?;
        let chunk_total = chunk.len();
        let dumped_count = entries.len();
        let bytes_in_chunk: u64 = entries.iter().map(|e| e.payload.len() as u64).sum();

        // Move writer + entries onto a blocking thread for the file writes;
        // hand the writer back so the next chunk can keep using it.
        let entries_for_write = entries.clone();
        writer = smol::unblock(move || -> Result<DumpWriter<BufWriter<File>>> {
            for entry in &entries_for_write {
                writer.write_entry(entry)?;
            }
            Ok(writer)
        })
        .await?;

        // Build the full set of log lines for this chunk before crossing into the
        // foreground update closure — we want exactly one update + one notify per chunk.
        let mut log_lines: Vec<LogLine> = entries
            .iter()
            .map(|entry| LogLine {
                key: String::from_utf8_lossy(&entry.key).into_owned().into(),
                bytes: entry.payload.len() as u64,
                status: LogStatus::Ok,
                message: None,
            })
            .collect();

        // Missing keys (PTTL == -2 / TYPE == none) silently disappear.
        let skipped = chunk_total - dumped_count;
        if skipped > 0 {
            let dumped_set: ahash::AHashSet<&[u8]> = entries.iter().map(|e| e.key.as_slice()).collect();
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
    }

    smol::unblock(move || writer.finish().map(|_| ())).await?;
    debug!(path = %output_path.display(), "export finished");
    info!(server = %server_id, db, total = keys.len(), "export finished");
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
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let ExportSpec {
        server_id,
        db,
        keys,
        output_path,
        format,
    } = spec;
    let client = get_connection_manager().get_client(server_id.as_str(), db).await?;
    let mut conn = client.connection();

    let path_for_open = output_path.clone();
    let mut writer = smol::unblock(move || -> Result<BufWriter<File>> {
        let mut writer = BufWriter::new(File::create(&path_for_open)?);
        match format {
            ExportFormat::Json => writer.write_all(b"[")?,
            ExportFormat::Csv => writer.write_all(csv_header().as_bytes())?,
            ExportFormat::Binary => unreachable!("binary goes through export_worker"),
        }
        Ok(writer)
    })
    .await?;

    let mut first_entry = true;
    for chunk in keys.chunks(DUMP_BATCH_SIZE) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let chunk: Vec<String> = chunk.iter().map(|k| k.to_string()).collect();
        let entries = read_readable_chunk(&mut conn, &chunk, ReadLimits::default()).await?;

        // Serialize on this side so log lines can carry the byte counts.
        let mut payload = String::new();
        let mut log_lines: Vec<LogLine> = Vec::with_capacity(chunk.len());
        for entry in &entries {
            let before = payload.len();
            match format {
                ExportFormat::Json => {
                    payload.push_str(if first_entry { "\n" } else { ",\n" });
                    first_entry = false;
                    payload.push_str(&entry_to_json(entry).to_string());
                }
                ExportFormat::Csv => payload.push_str(&entry_to_csv(entry)),
                ExportFormat::Binary => unreachable!(),
            }
            let (status, message) = if entry.value.is_some() {
                // A capped collection still exports (marked in the file
                // itself too) — surface the cut in the job log.
                let message = entry.truncated.then(|| SharedString::from("truncated"));
                (LogStatus::Ok, message)
            } else {
                // Module types have no generic readable form.
                (LogStatus::Skipped, Some(SharedString::from(entry.key_type.clone())))
            };
            log_lines.push(LogLine {
                key: entry.key.clone().into(),
                bytes: (payload.len() - before) as u64,
                status,
                message,
            });
        }
        // Keys that vanished between SCAN and the fetch.
        let fetched: ahash::AHashSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        for key in chunk.iter().filter(|k| !fetched.contains(k.as_str())) {
            log_lines.push(LogLine {
                key: key.clone().into(),
                bytes: 0,
                status: LogStatus::Skipped,
                message: Some("missing".into()),
            });
        }

        let done = entries.len();
        let skipped = chunk.len() - done;
        let bytes_in_chunk = payload.len() as u64;
        writer = smol::unblock(move || -> Result<BufWriter<File>> {
            writer.write_all(payload.as_bytes())?;
            Ok(writer)
        })
        .await?;

        handle
            .update(cx, |s, cx| {
                s.progress.keys_done += done as u64;
                s.progress.keys_skipped += skipped as u64;
                s.progress.bytes_done += bytes_in_chunk;
                cx.emit(MigrationEvent::Progress);
                s.extend_log(log_lines, cx);
            })
            .map_err(|e| Error::Invalid { message: e.to_string() })?;
    }

    smol::unblock(move || -> Result<()> {
        if format == ExportFormat::Json {
            writer.write_all(b"\n]\n")?;
        }
        writer.flush()?;
        Ok(())
    })
    .await?;
    info!(server = %server_id, db, total = keys.len(), format = ?format, "readable export finished");
    Ok(())
}

async fn run_import(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    conflict: ConflictMode,
    cancel: Arc<AtomicBool>,
) {
    match import_worker(handle.clone(), cx, server_id, db, input_path, conflict, cancel.clone()).await {
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
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let path_for_sniff = input_path.clone();
    let format = smol::unblock(move || detect_import_format(&path_for_sniff)).await?;
    match format {
        ImportFormat::Binary => import_binary_worker(handle, cx, server_id, db, input_path, conflict, cancel).await,
        ImportFormat::Json | ImportFormat::Csv => {
            let spec = ReadableImportSpec {
                server_id,
                db,
                input_path,
                format,
                conflict,
            };
            import_readable_worker(handle, cx, spec, cancel).await
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
}

async fn import_binary_worker(
    handle: gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    server_id: SharedString,
    db: usize,
    input_path: PathBuf,
    conflict: ConflictMode,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let client = get_connection_manager().get_client(server_id.as_str(), db).await?;
    let mut conn = client.connection();

    // Open + parse the header on a blocking thread (sync I/O).
    let path_for_open = input_path.clone();
    let mut reader = smol::unblock(move || -> Result<DumpReader<BufReader<File>>> {
        let file = File::open(&path_for_open)?;
        Ok(DumpReader::open(BufReader::new(file))?)
    })
    .await?;
    let header_total = reader.header().key_count;
    let _ = handle.update(cx, |s, cx| {
        s.progress.keys_total = header_total;
        cx.notify();
    });

    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        // Read up to RESTORE_BATCH_SIZE entries on a blocking thread, hand the reader back.
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
        flush_restore_batch(&handle, cx, &mut conn, &mut batch, conflict).await?;
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
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let ReadableImportSpec {
        server_id,
        db,
        input_path,
        format,
        conflict,
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

    let client = get_connection_manager().get_client(server_id.as_str(), db).await?;
    let mut conn = client.connection();

    for chunk in entries.chunks(RESTORE_BATCH_SIZE) {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let statuses = write_readable_chunk(&mut conn, chunk, conflict).await?;
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
    }
    Ok(())
}

async fn flush_restore_batch(
    handle: &gpui::WeakEntity<MigrationState>,
    cx: &mut gpui::AsyncApp,
    conn: &mut RedisAsyncConn,
    buffer: &mut Vec<DumpEntry>,
    conflict: ConflictMode,
) -> Result<()> {
    let entries = std::mem::take(buffer);
    let statuses = restore_keys_chunk(conn, &entries, conflict).await?;
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
                log_lines.push(LogLine {
                    key,
                    bytes: size,
                    status: LogStatus::Failed,
                    message: Some(msg.clone().into()),
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

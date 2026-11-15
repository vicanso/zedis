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

//! Built-in Redis Benchmark (`ServerView::Benchmark`) — interactive
//! throughput and latency profiler inside Zedis.
//!
//! Features:
//! - Presets: `SET`, `GET`, `INCR`, `LPUSH`, `HSET`, `ZADD` with configurable
//!   pipelining (`P=1..64`), concurrency (`1..64`), payload size (`1B..1MB`),
//!   keyspace size (`1..1M`), duration (`1..60s`), and target `db`.
//! - Safety: gated by [`Capability::CreateKey`] (refused on read-only / locked
//!   Prod connections unless the ADR 14 write window is open) and confirmed
//!   through [`confirm_dangerous_command`].
//! - Throwaway keys: writes exclusively under a random `zedis:bench:<id>:`
//!   prefix and cleans up all touched keys on completion or cancellation.
//! - Charts & comparison: latency histogram bar chart, throughput timeline
//!   chart, cross-run / cross-server baseline comparison with relative Δ%,
//!   and CSV export.

/// Render methods for the benchmark view (`render_header`..`render_history_table` + `Render` impl).
mod render;
use crate::connection::{
    BenchmarkRunResult, Capability, DangerKind, ServerDb, get_server, new_benchmark_prefix_for,
    run_benchmark_with_prefix,
};
use crate::helpers::{build_csv, group_thousands, now_clock};
use crate::states::{ServerEvent, ZedisServerState, i18n_benchmark};
use crate::views::{confirm_dangerous_command, export_to_file};
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext, Context, Entity, ScrollHandle, SharedString, Subscription, Task, Window};
use gpui_kit::component::input::InputState;
use gpui_kit::component::notification::Notification;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use zedis_core::benchmark::{
    BenchmarkConfig, BenchmarkPreset, BenchmarkProgress, BenchmarkSummary, DEFAULT_CONCURRENCY, DEFAULT_DURATION_SECS,
    DEFAULT_KEY_PREFIX, DEFAULT_KEYSPACE_SIZE, DEFAULT_PAYLOAD_SIZE,
};

pub(super) const RUN_BUTTON_ID: &str = "bench-run-btn";
pub(super) const STOP_BUTTON_ID: &str = "bench-stop-btn";
pub(super) const EXPORT_BUTTON_ID: &str = "bench-export-btn";
pub(super) const CLEAR_BUTTON_ID: &str = "bench-clear-btn";

/// One completed benchmark run kept in the session history so runs can be
/// compared across configurations or across different servers.
#[derive(Debug, Clone)]
pub struct BenchmarkHistoryEntry {
    pub id: u64,
    pub timestamp: SharedString,
    pub server_label: SharedString,
    pub config: BenchmarkConfig,
    pub prefix: SharedString,
    pub summary: BenchmarkSummary,
}

#[derive(Default)]
struct BenchmarkSession {
    next_id: u64,
    runs: Vec<BenchmarkHistoryEntry>,
    selected_id: Option<u64>,
    baseline_id: Option<u64>,
    last_config: BenchmarkConfig,
}

static SESSION_HISTORY: OnceLock<Mutex<BenchmarkSession>> = OnceLock::new();

fn session_store() -> &'static Mutex<BenchmarkSession> {
    SESSION_HISTORY.get_or_init(|| Mutex::new(BenchmarkSession::default()))
}

fn record_run_in_session(
    timestamp: SharedString,
    server_label: SharedString,
    result: BenchmarkRunResult,
) -> (Vec<BenchmarkHistoryEntry>, Option<u64>, Option<u64>) {
    let mut guard = session_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.next_id += 1;
    let id = guard.next_id;
    guard.last_config = result.config.clone();
    let entry = BenchmarkHistoryEntry {
        id,
        timestamp,
        server_label,
        config: result.config,
        prefix: result.prefix.into(),
        summary: result.summary,
    };
    if guard.baseline_id.is_none() && !guard.runs.is_empty() {
        guard.baseline_id = guard.runs.first().map(|r| r.id);
    }
    guard.runs.insert(0, entry);
    guard.selected_id = Some(id);
    (guard.runs.clone(), guard.selected_id, guard.baseline_id)
}

fn remember_session_config(config: &BenchmarkConfig) {
    let mut guard = session_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_config = config.clone();
}

fn snapshot_session() -> (Vec<BenchmarkHistoryEntry>, Option<u64>, Option<u64>, BenchmarkConfig) {
    let guard = session_store().lock().unwrap_or_else(|e| e.into_inner());
    (
        guard.runs.clone(),
        guard.selected_id,
        guard.baseline_id,
        guard.last_config.clone(),
    )
}

fn update_session_selection(
    selected_id: Option<u64>,
    baseline_id: Option<u64>,
) -> (Vec<BenchmarkHistoryEntry>, Option<u64>, Option<u64>) {
    let mut guard = session_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.selected_id = selected_id;
    guard.baseline_id = baseline_id;
    (guard.runs.clone(), guard.selected_id, guard.baseline_id)
}

fn clear_session_history() -> (Vec<BenchmarkHistoryEntry>, Option<u64>, Option<u64>) {
    let mut guard = session_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.runs.clear();
    guard.selected_id = None;
    guard.baseline_id = None;
    guard.last_config = BenchmarkConfig::default();
    (Vec::new(), None, None)
}

/// Ensures an in-flight benchmark worker loop exits and cleans up its keys if
/// the view is dropped while a run is active.
struct CancelGuard(Arc<AtomicBool>);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Build a CSV document containing both the session run comparison table and
/// the selected run's latency histogram buckets.
fn build_benchmark_csv(runs: &[BenchmarkHistoryEntry], selected: Option<&BenchmarkHistoryEntry>) -> String {
    let headers = [
        "run_id",
        "timestamp",
        "server",
        "preset",
        "concurrency",
        "pipeline",
        "payload_bytes",
        "keyspace",
        "duration_s",
        "total_ops",
        "ops_per_sec",
        "avg_us",
        "min_us",
        "p50_us",
        "p95_us",
        "p99_us",
        "max_us",
        "errors",
        "cleaned_keys",
        "cancelled",
    ];
    let mut rows: Vec<Vec<String>> = runs
        .iter()
        .map(|r| {
            vec![
                r.id.to_string(),
                r.timestamp.to_string(),
                r.server_label.to_string(),
                r.config.preset.label().to_string(),
                r.config.concurrency.to_string(),
                r.config.pipeline.to_string(),
                r.config.payload_size.to_string(),
                r.config.keyspace_size.to_string(),
                r.config.duration_secs.to_string(),
                r.summary.total_ops.to_string(),
                format!("{:.2}", r.summary.ops_per_sec),
                format!("{:.2}", r.summary.avg_us),
                r.summary.min_us.to_string(),
                r.summary.p50_us.to_string(),
                r.summary.p95_us.to_string(),
                r.summary.p99_us.to_string(),
                r.summary.max_us.to_string(),
                r.summary.errors.to_string(),
                r.summary.cleaned_keys.to_string(),
                r.summary.cancelled.to_string(),
            ]
        })
        .collect();

    if let Some(sel) = selected {
        rows.push(Vec::new());
        rows.push(vec![
            format!("# Histogram (Run #{})", sel.id),
            "bucket".to_string(),
            "count".to_string(),
            "pct".to_string(),
        ]);
        for b in &sel.summary.buckets {
            rows.push(vec![
                sel.id.to_string(),
                b.label.clone(),
                b.count.to_string(),
                format!("{:.2}%", b.pct),
            ]);
        }
    }

    build_csv(&headers, &rows)
}

pub(super) fn format_delta_pct(delta: f64) -> String {
    if delta >= 0.0 {
        format!("+{delta:.1}%")
    } else {
        format!("{delta:.1}%")
    }
}

/// The glob the cleanup hint names: the same normalized prefix a run will
/// write under, plus `*`. An empty field is the default `zedis:bench:`.
fn benchmark_cleanup_glob(prefix: &str) -> String {
    let cfg = BenchmarkConfig {
        key_prefix: prefix.to_string(),
        ..BenchmarkConfig::default()
    };
    format!("{}*", cfg.normalized_key_prefix())
}

pub struct ZedisBenchmark {
    pub(super) server_state: Entity<ZedisServerState>,
    pub(super) preset: BenchmarkPreset,
    pub(super) pipeline: usize,
    pub(super) db_input: Entity<InputState>,
    pub(super) concurrency_input: Entity<InputState>,
    pub(super) payload_input: Entity<InputState>,
    pub(super) keyspace_input: Entity<InputState>,
    pub(super) duration_input: Entity<InputState>,
    pub(super) prefix_input: Entity<InputState>,
    pub(super) keep_keys: bool,
    pub(super) running: bool,
    pub(super) active_prefix: Option<SharedString>,
    pub(super) active_target_label: Option<SharedString>,
    pub(super) live_progress: Option<BenchmarkProgress>,
    pub(super) error: Option<SharedString>,
    pub(super) runs: Vec<BenchmarkHistoryEntry>,
    pub(super) selected_id: Option<u64>,
    pub(super) baseline_id: Option<u64>,
    cancel: Arc<AtomicBool>,
    _cancel_guard: CancelGuard,
    task: Option<Task<()>>,
    pub(super) pending_notification: Option<Notification>,
    pub(super) scroll: ScrollHandle,
    _subscription: Subscription,
}

impl ZedisBenchmark {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (runs, selected_id, baseline_id, last_config) = snapshot_session();
        let initial_db = server_state.read(cx).db();
        let db_input = cx.new(|cx| InputState::new(window, cx).default_value(initial_db.to_string()));
        let concurrency_input =
            cx.new(|cx| InputState::new(window, cx).default_value(last_config.concurrency.to_string()));
        let payload_input =
            cx.new(|cx| InputState::new(window, cx).default_value(last_config.payload_size.to_string()));
        let keyspace_input =
            cx.new(|cx| InputState::new(window, cx).default_value(last_config.keyspace_size.to_string()));
        let duration_input =
            cx.new(|cx| InputState::new(window, cx).default_value(last_config.duration_secs.to_string()));
        let prefix_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(last_config.key_prefix.clone())
                .placeholder(DEFAULT_KEY_PREFIX)
        });
        let keep_keys = last_config.keep_keys;

        let subscription = cx.subscribe_in(&server_state, window, |this, state, event, window, cx| match event {
            ServerEvent::ServerSelected(_) => {
                let db = state.read(cx).db();
                this.db_input
                    .update(cx, |input, cx| input.set_value(db.to_string(), window, cx));
                if this.running && !state.read(cx).can(Capability::CreateKey) {
                    this.stop_benchmark(cx);
                }
                cx.notify();
            }
            ServerEvent::ServerInfoUpdated => {
                if this.running && !state.read(cx).can(Capability::CreateKey) {
                    this.stop_benchmark(cx);
                }
                cx.notify();
            }
            _ => {}
        });

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_guard = CancelGuard(cancel.clone());

        Self {
            server_state,
            preset: last_config.preset,
            pipeline: last_config.pipeline,
            db_input,
            concurrency_input,
            payload_input,
            keyspace_input,
            duration_input,
            prefix_input,
            keep_keys,
            running: false,
            active_prefix: None,
            active_target_label: None,
            live_progress: None,
            error: None,
            runs,
            selected_id,
            baseline_id,
            cancel,
            _cancel_guard: cancel_guard,
            task: None,
            pending_notification: None,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    pub(super) fn sync_from_session(&mut self) {
        let (runs, selected_id, baseline_id, _) = snapshot_session();
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
    }

    fn parse_config_and_db(&self, cx: &App) -> (BenchmarkConfig, usize) {
        let state = self.server_state.read(cx);
        let default_db = state.db();
        let db_count = state.databases();
        let raw_db = self
            .db_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(default_db);
        let target_db = if db_count > 0 { raw_db.min(db_count - 1) } else { raw_db };
        let concurrency = self
            .concurrency_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(DEFAULT_CONCURRENCY);
        let payload_size = self
            .payload_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(DEFAULT_PAYLOAD_SIZE);
        let keyspace_size = self
            .keyspace_input
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(DEFAULT_KEYSPACE_SIZE);
        let duration_secs = self
            .duration_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .unwrap_or(DEFAULT_DURATION_SECS);
        let key_prefix = self.prefix_input.read(cx).value().to_string();

        let cfg = BenchmarkConfig {
            preset: self.preset,
            concurrency,
            payload_size,
            keyspace_size,
            duration_secs,
            pipeline: self.pipeline,
            key_prefix,
            keep_keys: self.keep_keys,
        }
        .sanitized();

        (cfg, target_db)
    }

    /// Hint under "Keep keys after run": the configured prefix, and whether
    /// those keys are deleted or left behind.
    pub(super) fn cleanup_note(&self, cx: &App) -> SharedString {
        let glob = benchmark_cleanup_glob(&self.prefix_input.read(cx).value());
        let key = if self.keep_keys { "cleanup_kept" } else { "cleanup_note" };
        i18n_benchmark(cx, key).replace("{prefix}", &glob).into()
    }

    pub(super) fn request_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || !self.server_state.read(cx).can(Capability::CreateKey) {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let (config, target_db) = self.parse_config_and_db(cx);
        let prefix = new_benchmark_prefix_for(&config);
        let preview = config.command_preview(&prefix);

        let Ok(server) = get_server(&server_id) else {
            return;
        };
        let confirmed_token = if server.name.trim().is_empty() {
            "confirm".to_string()
        } else {
            server.name.clone()
        };
        let entity = cx.entity().downgrade();
        confirm_dangerous_command(
            &server,
            target_db,
            &DangerKind::GenericWrite,
            Some(&preview),
            window,
            cx,
            move |_window, cx| {
                let Some(this) = entity.upgrade() else { return };
                this.update(cx, |this, cx| {
                    this.start_benchmark(
                        config.clone(),
                        target_db,
                        prefix.clone(),
                        Some(confirmed_token.clone()),
                        cx,
                    );
                });
            },
        );
    }

    fn start_benchmark(
        &mut self,
        config: BenchmarkConfig,
        target_db: usize,
        prefix: String,
        confirmation: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.running || !self.server_state.read(cx).can(Capability::CreateKey) {
            return;
        }
        remember_session_config(&config);
        let server_id = self.server_state.read(cx).server_id().to_string();
        let server_name = get_server(&server_id)
            .map(|s| {
                if s.name.is_empty() {
                    format!("{}:{}", s.host, s.port)
                } else {
                    s.name
                }
            })
            .unwrap_or_else(|_| server_id.clone());
        let server_label: SharedString = format!("{server_name} (db {target_db})").into();

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self._cancel_guard = CancelGuard(cancel.clone());
        self.running = true;
        self.error = None;
        self.active_prefix = Some(prefix.clone().into());
        self.active_target_label = Some(server_label.clone());
        self.live_progress = Some(BenchmarkProgress {
            elapsed_ms: 0,
            duration_ms: config.duration_secs.saturating_mul(1_000),
            total_ops: 0,
            current_ops_per_sec: 0.0,
            current_p99_us: 0,
        });
        cx.notify();

        let (progress_tx, mut progress_rx) = mpsc::unbounded::<BenchmarkProgress>();
        let (result_tx, result_rx) = oneshot::channel();

        // Detached background runner so cleanup always completes even if the
        // view is dropped mid-run.
        cx.background_spawn(async move {
            let base = ServerDb::new(&server_id, target_db);
            let at = match confirmation {
                Some(token) => base.confirmed(token),
                None => base,
            };
            let outcome = run_benchmark_with_prefix(&at, &config, prefix, &cancel, move |progress| {
                let _ = progress_tx.unbounded_send(progress);
            })
            .await;
            let _ = result_tx.send(outcome);
        })
        .detach();

        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(progress) = progress_rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    this.live_progress = Some(progress);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let Ok(outcome) = result_rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.running = false;
                this.live_progress = None;
                this.active_prefix = None;
                this.active_target_label = None;
                match outcome {
                    Ok(run_result) => {
                        let cleaned = run_result.summary.cleaned_keys;
                        let cancelled = run_result.summary.cancelled;
                        let kept = run_result.config.keep_keys;
                        let ops_sec = run_result.summary.ops_per_sec;
                        let (runs, selected_id, baseline_id) =
                            record_run_in_session(now_clock(false).into(), server_label, run_result);
                        this.runs = runs;
                        this.selected_id = selected_id;
                        this.baseline_id = baseline_id;
                        let suffix = if kept {
                            i18n_benchmark(cx, "status_kept").to_string()
                        } else {
                            format!("{}: {}", i18n_benchmark(cx, "cleaned_keys"), group_thousands(cleaned))
                        };
                        let msg = format!(
                            "{}: {} ops/s · {}",
                            if cancelled {
                                i18n_benchmark(cx, "stopped_toast")
                            } else {
                                i18n_benchmark(cx, "completed_toast")
                            },
                            group_thousands(ops_sec.round() as u64),
                            suffix,
                        );
                        this.pending_notification = Some(Notification::success(msg));
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        this.pending_notification = Some(Notification::error(e.to_string()));
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn stop_benchmark(&mut self, cx: &mut Context<Self>) {
        if !self.running {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        cx.notify();
    }

    pub(super) fn clear_history(&mut self, cx: &mut Context<Self>) {
        let (runs, selected_id, baseline_id) = clear_session_history();
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    pub(super) fn select_run(&mut self, id: u64, cx: &mut Context<Self>) {
        let (runs, selected_id, baseline_id) = update_session_selection(Some(id), self.baseline_id);
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    pub(super) fn toggle_baseline(&mut self, id: u64, cx: &mut Context<Self>) {
        let next_base = if self.baseline_id == Some(id) { None } else { Some(id) };
        let (runs, selected_id, baseline_id) = update_session_selection(self.selected_id, next_base);
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    pub(super) fn export_csv(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.runs.is_empty() {
            return;
        }
        let selected = self.selected_entry();
        let csv = build_benchmark_csv(&self.runs, selected);
        let success = i18n_benchmark(cx, "export_csv");
        let error = i18n_benchmark(cx, "error_prefix");
        export_to_file(
            cx,
            self.server_state.clone(),
            csv.into_bytes(),
            "benchmark.csv",
            success,
            error,
        );
    }

    pub(super) fn selected_entry(&self) -> Option<&BenchmarkHistoryEntry> {
        self.selected_id
            .and_then(|id| self.runs.iter().find(|r| r.id == id))
            .or_else(|| self.runs.first())
    }

    pub(super) fn baseline_entry(&self) -> Option<&BenchmarkHistoryEntry> {
        let selected = self.selected_entry()?;
        let base_id = self.baseline_id?;
        if base_id == selected.id {
            return None;
        }
        self.runs.iter().find(|r| r.id == base_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AppContext, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use zedis_core::benchmark::{BenchmarkRawOutcome, ThroughputPoint, summarize_benchmark};

    #[test]
    fn the_cleanup_hint_uses_the_configured_prefix() {
        assert_eq!(benchmark_cleanup_glob(""), "zedis:bench:*");
        assert_eq!(benchmark_cleanup_glob("zedis:fn:bench:"), "zedis:fn:bench:*");
        assert_eq!(benchmark_cleanup_glob("test"), "test:*");
    }

    #[test]
    fn csv_export_includes_run_metrics_and_histogram_buckets() {
        let summary = summarize_benchmark(BenchmarkRawOutcome {
            latencies_us: vec![90, 210, 480, 950],
            total_ops: 400,
            errors: 0,
            elapsed_ms: 1_000,
            timeline: vec![ThroughputPoint {
                elapsed_ms: 500,
                ops_per_sec: 400.0,
                p99_us: 950,
            }],
            cleaned_keys: 16,
            cancelled: false,
        });
        let entry = BenchmarkHistoryEntry {
            id: 1,
            timestamp: "12:00:00".into(),
            server_label: "local (db 0)".into(),
            config: BenchmarkConfig::default(),
            prefix: "zedis:bench:test1234:".into(),
            summary,
        };
        let csv = build_benchmark_csv(std::slice::from_ref(&entry), Some(&entry));
        assert!(csv.contains("run_id,timestamp,server,preset"));
        assert!(csv.contains("local (db 0)"));
        assert!(csv.contains("# Histogram (Run #1)"));
        assert!(csv.contains("≤0.1ms"));
    }

    #[gpui::test]
    fn the_cleanup_note_follows_the_prefix_and_whether_keys_are_kept(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        let mut panel = None;
        let handle = cx
            .open_window(size(px(1000.), px(700.)), |window, cx| {
                let view = cx.new(|cx| ZedisBenchmark::new(server_state.clone(), window, cx));
                panel = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let view = panel.expect("benchmark view");
        let prefix = view.read_with(cx, |bench, _| bench.prefix_input.clone());

        cx.update_window(handle, |_, window, cx| {
            prefix.update(cx, |input, cx| input.set_value("zedis:fn:bench:", window, cx));
            window.render_frame(cx);
            let note = view.read_with(cx, |bench, cx| bench.cleanup_note(cx).to_string());
            assert!(note.contains("zedis:fn:bench:*") && note.contains("deleted"), "{note}");
            window.click("bench-keep-keys", cx);
        })
        .expect("set prefix");
        cx.run_until_parked();
        let note = view.read_with(cx, |bench, cx| bench.cleanup_note(cx).to_string());
        assert!(note.contains("zedis:fn:bench:*") && note.contains("kept"), "{note}");
    }

    #[gpui::test]
    fn benchmark_view_gates_run_button_when_readonly_and_syncs_session(cx: &mut TestAppContext) {
        init_ui(cx);
        let _ = clear_session_history();

        let server_state_a = cx.new(|_| ZedisServerState::default());
        server_state_a.update(cx, |state, cx| {
            state.toggle_readonly(cx);
        });
        let server_state_b = cx.new(|_| ZedisServerState::default());

        let mut panel_a = None;
        let mut panel_b = None;
        cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view_a = cx.new(|cx| ZedisBenchmark::new(server_state_a.clone(), window, cx));
            let view_b = cx.new(|cx| ZedisBenchmark::new(server_state_b.clone(), window, cx));
            panel_a = Some(view_a.clone());
            panel_b = Some(view_b);
            Root::new(view_a, window, cx)
        });
        let view_a = panel_a.expect("the window built benchmark view A");
        let view_b = panel_b.expect("the window built benchmark view B");

        view_a.read_with(cx, |bench, cx| {
            assert!(!bench.server_state.read(cx).can(Capability::CreateKey));
            let (cfg, db) = bench.parse_config_and_db(cx);
            assert_eq!(cfg.preset, BenchmarkPreset::Set);
            assert_eq!(cfg.concurrency, DEFAULT_CONCURRENCY);
            assert_eq!(db, 0);
        });

        // Toggling read-only off emits ServerInfoUpdated and stops any active run if relocked.
        server_state_a.update(cx, |state, cx| {
            state.toggle_readonly(cx);
        });
        cx.run_until_parked();
        view_a.read_with(cx, |bench, cx| {
            assert!(bench.server_state.read(cx).can(Capability::CreateKey));
        });

        // Recording two runs in the shared session auto-pins the first as baseline
        // and sync_from_session() makes both runs visible in view_b as well.
        let custom_cfg = BenchmarkConfig {
            preset: BenchmarkPreset::Hset,
            concurrency: 16,
            payload_size: 512,
            keyspace_size: 2_000,
            duration_secs: 3,
            pipeline: 16,
            key_prefix: "test:bench:".to_string(),
            keep_keys: true,
        };
        let (_, sel1, base1) = record_run_in_session(
            "12:01:00".into(),
            "staging (db 0)".into(),
            BenchmarkRunResult {
                prefix: "zedis:bench:run1:".to_string(),
                config: custom_cfg.clone(),
                summary: summarize_benchmark(BenchmarkRawOutcome {
                    latencies_us: vec![100, 200, 300],
                    total_ops: 1_000,
                    errors: 0,
                    elapsed_ms: 1_000,
                    timeline: Vec::new(),
                    cleaned_keys: 50,
                    cancelled: false,
                }),
            },
        );
        let first_id = sel1.expect("first run id");
        assert_eq!(base1, None);

        let (_, sel2, base2) = record_run_in_session(
            "12:02:00".into(),
            "prod (db 0)".into(),
            BenchmarkRunResult {
                prefix: "zedis:bench:run2:".to_string(),
                config: custom_cfg.clone(),
                summary: summarize_benchmark(BenchmarkRawOutcome {
                    latencies_us: vec![50, 100, 150],
                    total_ops: 2_000,
                    errors: 0,
                    elapsed_ms: 1_000,
                    timeline: Vec::new(),
                    cleaned_keys: 50,
                    cancelled: false,
                }),
            },
        );
        let second_id = sel2.expect("second run id");
        assert_eq!(base2, Some(first_id));

        view_b.update(cx, |bench, _cx| {
            bench.sync_from_session();
            assert_eq!(bench.runs.len(), 2);
            assert_eq!(bench.selected_id, Some(second_id));
            assert_eq!(bench.baseline_id, Some(first_id));
            assert_eq!(bench.baseline_entry().map(|e| e.id), Some(first_id));
        });

        let _ = clear_session_history();
    }
}

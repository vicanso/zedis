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

use crate::assets::CustomIconName;
use crate::connection::{
    BenchmarkRunResult, Capability, DangerKind, ServerDb, get_server, new_benchmark_prefix, run_benchmark_with_prefix,
};
use crate::helpers::{build_csv, get_mono_font_family, group_thousands, now_clock, saves_files};
use crate::states::{
    ServerEvent, ServerView, ZedisGlobalStore, ZedisServerState, back_to_editor_tooltip, i18n_benchmark, i18n_common,
};
use crate::views::{
    ChartParams, ValueLabel, confirm_dangerous_command, export_to_file, make_bar_chart, make_line_chart,
};
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use gpui::{App, Context, Entity, ScrollHandle, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use zedis_core::benchmark::{
    BenchmarkComparison, BenchmarkConfig, BenchmarkPreset, BenchmarkProgress, BenchmarkSummary, DEFAULT_CONCURRENCY,
    DEFAULT_DURATION_SECS, DEFAULT_KEYSPACE_SIZE, DEFAULT_PAYLOAD_SIZE, PIPELINE_CHOICES, compare_summaries,
    format_latency_f64_us, format_latency_us,
};

const RUN_BUTTON_ID: &str = "bench-run-btn";
const STOP_BUTTON_ID: &str = "bench-stop-btn";
const EXPORT_BUTTON_ID: &str = "bench-export-btn";
const CLEAR_BUTTON_ID: &str = "bench-clear-btn";

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

fn format_delta_pct(delta: f64) -> String {
    if delta >= 0.0 {
        format!("+{delta:.1}%")
    } else {
        format!("{delta:.1}%")
    }
}

pub struct ZedisBenchmark {
    server_state: Entity<ZedisServerState>,
    preset: BenchmarkPreset,
    pipeline: usize,
    db_input: Entity<InputState>,
    concurrency_input: Entity<InputState>,
    payload_input: Entity<InputState>,
    keyspace_input: Entity<InputState>,
    duration_input: Entity<InputState>,
    running: bool,
    active_prefix: Option<SharedString>,
    active_target_label: Option<SharedString>,
    live_progress: Option<BenchmarkProgress>,
    error: Option<SharedString>,
    runs: Vec<BenchmarkHistoryEntry>,
    selected_id: Option<u64>,
    baseline_id: Option<u64>,
    cancel: Arc<AtomicBool>,
    _cancel_guard: CancelGuard,
    task: Option<Task<()>>,
    pending_notification: Option<Notification>,
    scroll: ScrollHandle,
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

    fn sync_from_session(&mut self) {
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

        let cfg = BenchmarkConfig {
            preset: self.preset,
            concurrency,
            payload_size,
            keyspace_size,
            duration_secs,
            pipeline: self.pipeline,
        }
        .sanitized();

        (cfg, target_db)
    }

    fn request_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || !self.server_state.read(cx).can(Capability::CreateKey) {
            return;
        }
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let (config, target_db) = self.parse_config_and_db(cx);
        let prefix = new_benchmark_prefix();
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
                        let ops_sec = run_result.summary.ops_per_sec;
                        let (runs, selected_id, baseline_id) =
                            record_run_in_session(now_clock(false).into(), server_label, run_result);
                        this.runs = runs;
                        this.selected_id = selected_id;
                        this.baseline_id = baseline_id;
                        let msg = format!(
                            "{}: {} ops/s · {}: {}",
                            if cancelled {
                                i18n_benchmark(cx, "stopped_toast")
                            } else {
                                i18n_benchmark(cx, "completed_toast")
                            },
                            group_thousands(ops_sec.round() as u64),
                            i18n_benchmark(cx, "cleaned_keys"),
                            group_thousands(cleaned),
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

    fn stop_benchmark(&mut self, cx: &mut Context<Self>) {
        if !self.running {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        cx.notify();
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        let (runs, selected_id, baseline_id) = clear_session_history();
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    fn select_run(&mut self, id: u64, cx: &mut Context<Self>) {
        let (runs, selected_id, baseline_id) = update_session_selection(Some(id), self.baseline_id);
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    fn toggle_baseline(&mut self, id: u64, cx: &mut Context<Self>) {
        let next_base = if self.baseline_id == Some(id) { None } else { Some(id) };
        let (runs, selected_id, baseline_id) = update_session_selection(self.selected_id, next_base);
        self.runs = runs;
        self.selected_id = selected_id;
        self.baseline_id = baseline_id;
        cx.notify();
    }

    fn export_csv(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
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

    fn selected_entry(&self) -> Option<&BenchmarkHistoryEntry> {
        self.selected_id
            .and_then(|id| self.runs.iter().find(|r| r.id == id))
            .or_else(|| self.runs.first())
    }

    fn baseline_entry(&self) -> Option<&BenchmarkHistoryEntry> {
        let selected = self.selected_entry()?;
        let base_id = self.baseline_id?;
        if base_id == selected.id {
            return None;
        }
        self.runs.iter().find(|r| r.id == base_id)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let has_runs = !self.runs.is_empty();
        h_flex()
            .w_full()
            .px_4()
            .py_2p5()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(CustomIconName::Zap).text_color(theme.primary))
                    .child(Label::new(i18n_benchmark(cx, "title")).text_base().font_semibold())
                    .child(
                        Label::new(i18n_benchmark(cx, "subtitle"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(saves_files(), |this| {
                        this.child(
                            Button::new(EXPORT_BUTTON_ID)
                                .small()
                                .outline()
                                .icon(Icon::new(CustomIconName::Download))
                                .label(i18n_benchmark(cx, "export_csv"))
                                .disabled(!has_runs || self.running)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.export_csv(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(CLEAR_BUTTON_ID)
                            .small()
                            .outline()
                            .icon(Icon::new(CustomIconName::Eraser))
                            .label(i18n_benchmark(cx, "clear_history"))
                            .disabled(!has_runs || self.running)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.clear_history(cx);
                            })),
                    )
                    .child(
                        Button::new("bench-back-btn")
                            .small()
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .tooltip(back_to_editor_tooltip(cx))
                            .on_click(|_, _window, cx| {
                                cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
                                    store.update(cx, |state, cx| state.go_to_view(ServerView::Editor, cx));
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let state = self.server_state.read(cx);
        let can_write = state.can(Capability::CreateKey);
        let single_db = state.databases() == 1;
        let uses_payload = self.preset.uses_payload();

        let mut preset_row = h_flex().gap_1().items_center().flex_wrap();
        preset_row = preset_row.child(
            Label::new(i18n_benchmark(cx, "preset"))
                .text_xs()
                .text_color(theme.muted_foreground)
                .mr_1(),
        );
        for preset in BenchmarkPreset::ALL {
            let active = self.preset == preset;
            preset_row = preset_row.child(
                Button::new(SharedString::from(format!("bench-preset-{}", preset.label())))
                    .xsmall()
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.outline())
                    .disabled(self.running)
                    .label(preset.label())
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.preset = preset;
                        cx.notify();
                    })),
            );
        }

        let mut pipe_row = h_flex().gap_1().items_center().flex_wrap();
        pipe_row = pipe_row.child(
            Label::new(i18n_benchmark(cx, "pipeline"))
                .text_xs()
                .text_color(theme.muted_foreground)
                .mr_1(),
        );
        for p in PIPELINE_CHOICES {
            let active = self.pipeline == p;
            pipe_row = pipe_row.child(
                Button::new(SharedString::from(format!("bench-pipe-{p}")))
                    .xsmall()
                    .when(active, |b| b.primary())
                    .when(!active, |b| b.outline())
                    .disabled(self.running)
                    .label(format!("P={p}"))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.pipeline = p;
                        cx.notify();
                    })),
            );
        }

        let action_btn = if self.running {
            Button::new(STOP_BUTTON_ID)
                .small()
                .danger()
                .icon(IconName::Square)
                .label(i18n_benchmark(cx, "stop"))
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.stop_benchmark(cx);
                }))
        } else {
            Button::new(RUN_BUTTON_ID)
                .small()
                .primary()
                .icon(Icon::new(CustomIconName::Zap))
                .label(i18n_benchmark(cx, "run"))
                .disabled(!can_write)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.request_run(window, cx);
                }))
        };

        let knob = |label: SharedString, input: &Entity<InputState>, width: f32, disabled: bool| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Label::new(label).text_xs().text_color(theme.muted_foreground))
                .child(div().w(px(width)).child(Input::new(input).xsmall().disabled(disabled)))
        };

        v_flex()
            .w_full()
            .px_4()
            .py_2p5()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .flex_wrap()
                            .child(preset_row)
                            .child(pipe_row),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(!can_write, |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(Icon::new(CustomIconName::Lock).text_xs().text_color(theme.warning))
                                        .child(
                                            Label::new(i18n_benchmark(cx, "readonly_hint"))
                                                .text_xs()
                                                .text_color(theme.warning),
                                        ),
                                )
                            })
                            .child(action_btn),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_center()
                    .flex_wrap()
                    .child(knob(
                        i18n_benchmark(cx, "target_db"),
                        &self.db_input,
                        64.0,
                        self.running || single_db,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "concurrency"),
                        &self.concurrency_input,
                        72.0,
                        self.running,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "payload_bytes"),
                        &self.payload_input,
                        88.0,
                        self.running || !uses_payload,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "keyspace"),
                        &self.keyspace_input,
                        96.0,
                        self.running,
                    ))
                    .child(knob(
                        i18n_benchmark(cx, "duration_secs"),
                        &self.duration_input,
                        68.0,
                        self.running,
                    ))
                    .child(
                        Label::new(i18n_benchmark(cx, "cleanup_note"))
                            .text_xs()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .into_any_element()
    }

    fn render_live_banner(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let Some(progress) = &self.live_progress else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let pct = if progress.duration_ms > 0 {
            ((progress.elapsed_ms as f64 / progress.duration_ms as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let prefix = self.active_prefix.clone().unwrap_or_else(|| "zedis:bench:".into());
        let target = self.active_target_label.clone().unwrap_or_default();

        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_1p5()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(i18n_benchmark(cx, "running_status"))
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.primary),
                            )
                            .child(Label::new(target).text_xs().text_color(theme.foreground))
                            .child(
                                Label::new(format!("{prefix}*"))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .child(
                                Label::new(format!(
                                    "{} ops/s",
                                    group_thousands(progress.current_ops_per_sec.round() as u64)
                                ))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(theme.success),
                            )
                            .child(
                                Label::new(format!("P99: {}", format_latency_us(progress.current_p99_us)))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.foreground),
                            )
                            .child(
                                Label::new(format!(
                                    "{:.1}s / {:.1}s ({pct:.0}%)",
                                    progress.elapsed_ms as f64 / 1000.0,
                                    progress.duration_ms as f64 / 1000.0
                                ))
                                .text_xs()
                                .font_family(mono)
                                .text_color(theme.muted_foreground),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_headline_cards(
        &self,
        entry: &BenchmarkHistoryEntry,
        baseline: Option<&BenchmarkHistoryEntry>,
        comparison: Option<&BenchmarkComparison>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let s = &entry.summary;

        let card = |title: SharedString, value: String, sub: Option<(String, gpui::Hsla)>| {
            v_flex()
                .min_w(px(145.))
                .flex_1()
                .p_3()
                .gap_1()
                .rounded(px(6.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(Label::new(title).text_xs().text_color(theme.muted_foreground))
                .child(
                    Label::new(value)
                        .text_base()
                        .font_family(mono.clone())
                        .text_color(theme.foreground),
                )
                .when_some(sub, |this, (text, color)| {
                    this.child(Label::new(text).text_xs().font_family(mono.clone()).text_color(color))
                })
        };

        let throughput_delta = comparison.map(|cmp| {
            let d = cmp.ops_per_sec_delta_pct;
            let color = if d >= 0.0 { theme.success } else { theme.warning };
            (
                format!("{} vs #{}", format_delta_pct(d), baseline.map_or(0, |b| b.id)),
                color,
            )
        });
        let latency_delta = |d: f64| {
            let color = if d <= 0.0 { theme.success } else { theme.warning };
            (
                format!("{} vs #{}", format_delta_pct(d), baseline.map_or(0, |b| b.id)),
                color,
            )
        };

        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(format!("#{} · {}", entry.id, entry.server_label))
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(theme.foreground),
                            )
                            .child(
                                Label::new(entry.config.summary_label())
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                Label::new(format!("{}*", entry.prefix))
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .when_some(baseline, |this, base| {
                        this.child(
                            Label::new(format!(
                                "{}: #{} ({} · {})",
                                i18n_benchmark(cx, "comparing_against"),
                                base.id,
                                base.server_label,
                                base.config.summary_label()
                            ))
                            .text_xs()
                            .text_color(theme.primary),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2p5()
                    .flex_wrap()
                    .child(card(
                        i18n_benchmark(cx, "ops_per_sec"),
                        format!("{}/s", group_thousands(s.ops_per_sec.round() as u64)),
                        throughput_delta,
                    ))
                    .child(card(
                        i18n_benchmark(cx, "avg_latency"),
                        format_latency_f64_us(s.avg_us),
                        comparison.map(|c| latency_delta(c.avg_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p50_latency"),
                        format_latency_us(s.p50_us),
                        comparison.map(|c| latency_delta(c.p50_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p95_latency"),
                        format_latency_us(s.p95_us),
                        comparison.map(|c| latency_delta(c.p95_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "p99_latency"),
                        format_latency_us(s.p99_us),
                        comparison.map(|c| latency_delta(c.p99_us_delta_pct)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "min_max_latency"),
                        format!("{} / {}", format_latency_us(s.min_us), format_latency_us(s.max_us)),
                        Some((format!("{:.2}s", s.elapsed_ms as f64 / 1000.0), theme.muted_foreground)),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "total_ops"),
                        group_thousands(s.total_ops),
                        Some((
                            format!(
                                "{} {}: {}",
                                group_thousands(s.total_batches),
                                i18n_benchmark(cx, "errors"),
                                s.errors
                            ),
                            if s.errors > 0 {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            },
                        )),
                    ))
                    .child(card(
                        i18n_benchmark(cx, "cleaned_keys"),
                        group_thousands(s.cleaned_keys),
                        Some((
                            i18n_benchmark(
                                cx,
                                if s.cancelled {
                                    "status_cancelled"
                                } else {
                                    "status_cleaned"
                                },
                            )
                            .to_string(),
                            theme.success,
                        )),
                    )),
            )
            .into_any_element()
    }

    fn render_charts(&self, entry: &BenchmarkHistoryEntry, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let mono = get_mono_font_family();
        let s = &entry.summary;

        // 1. Latency distribution bar chart
        let hist_labels: Vec<SharedString> = s.buckets.iter().map(|b| SharedString::from(b.label.clone())).collect();
        let hist_values: Vec<f64> = s.buckets.iter().map(|b| b.count as f64).collect();
        let hist_max = (hist_values.iter().copied().fold(0.0_f64, f64::max) * 1.1).max(1.0);
        let hist_params = ChartParams {
            id: "bench-latency-hist".into(),
            y_min: 0.0,
            dates: Arc::new(hist_labels),
            y_max: hist_max,
            y_label: ValueLabel::decimals(0),
            tick_margin: 1,
        };
        let hist_chart = make_bar_chart(hist_params, Arc::new(hist_values), theme.chart_1);

        let mut bucket_chips = h_flex().gap_2().flex_wrap();
        for b in &s.buckets {
            if b.count > 0 {
                bucket_chips = bucket_chips.child(
                    h_flex()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded(px(4.))
                        .bg(theme.secondary)
                        .child(Label::new(b.label.clone()).text_xs().text_color(theme.muted_foreground))
                        .child(
                            Label::new(format!("{} ({:.1}%)", group_thousands(b.count), b.pct))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(theme.foreground),
                        ),
                );
            }
        }

        // 2. Throughput timeline line chart
        let tl_dates: Vec<SharedString> = s
            .timeline
            .iter()
            .map(|pt| SharedString::from(format!("{:.1}s", pt.elapsed_ms as f64 / 1000.0)))
            .collect();
        let tl_values: Vec<f64> = s.timeline.iter().map(|pt| pt.ops_per_sec).collect();
        let tl_max = (tl_values.iter().copied().fold(0.01_f64, f64::max) * 1.1).max(1.0);
        let tl_tick_margin = (s.timeline.len() / 4).max(1);
        let tl_params = ChartParams {
            id: "bench-throughput-line".into(),
            y_min: 0.0,
            dates: Arc::new(tl_dates),
            y_max: tl_max,
            y_label: ValueLabel::decimals(0).unit("/s"),
            tick_margin: tl_tick_margin,
        };
        let tl_chart = make_line_chart(tl_params, Arc::new(tl_values), theme.chart_2);

        h_flex()
            .w_full()
            .gap_3()
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(360.))
                    .p_3()
                    .gap_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        Label::new(i18n_benchmark(cx, "latency_histogram"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(div().w_full().h(px(180.)).child(hist_chart))
                    .child(bucket_chips),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(360.))
                    .p_3()
                    .gap_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        Label::new(i18n_benchmark(cx, "throughput_timeline"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(div().w_full().h(px(180.)).child(tl_chart)),
            )
            .into_any_element()
    }

    fn render_history_table(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let fg = cx.theme().foreground;
        let muted_fg = cx.theme().muted_foreground;
        let secondary_bg = cx.theme().secondary;
        let hover_bg = cx.theme().list_hover;
        let border_col = cx.theme().border;
        let mono = get_mono_font_family();
        let selected_id = self.selected_entry().map(|e| e.id);
        let baseline = self.baseline_id.and_then(|id| self.runs.iter().find(|r| r.id == id));

        let header_cell = |label: SharedString, width: f32| {
            div()
                .w(px(width))
                .flex_none()
                .child(Label::new(label).text_xs().text_color(muted_fg))
        };

        let mut list = v_flex().w_full().gap_1();
        for run in &self.runs {
            let run_id = run.id;
            let is_selected = selected_id == Some(run_id);
            let is_baseline = self.baseline_id == Some(run_id);
            let cmp = baseline
                .filter(|b| b.id != run_id)
                .map(|b| compare_summaries(&b.summary, &run.summary));

            let ops_str = match &cmp {
                Some(c) => format!(
                    "{}/s ({})",
                    group_thousands(run.summary.ops_per_sec.round() as u64),
                    format_delta_pct(c.ops_per_sec_delta_pct)
                ),
                None => format!("{}/s", group_thousands(run.summary.ops_per_sec.round() as u64)),
            };
            let p99_str = match &cmp {
                Some(c) => format!(
                    "{} ({})",
                    format_latency_us(run.summary.p99_us),
                    format_delta_pct(c.p99_us_delta_pct)
                ),
                None => format_latency_us(run.summary.p99_us),
            };
            let btn_label = i18n_benchmark(cx, if is_baseline { "baseline_active" } else { "set_baseline" });

            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("bench-history-row-{run_id}")))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(secondary_bg))
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.select_run(run_id, cx);
                    }))
                    .child(
                        div().w(px(56.)).flex_none().child(
                            Label::new(format!("#{run_id} {}", run.timestamp))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted_fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(160.))
                            .flex_none()
                            .overflow_hidden()
                            .child(Label::new(run.server_label.clone()).text_xs().text_color(fg)),
                    )
                    .child(
                        div().flex_1().min_w(px(180.)).overflow_hidden().child(
                            Label::new(run.config.summary_label())
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .child(Label::new(ops_str).text_xs().font_family(mono.clone()).text_color(fg)),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_f64_us(run.summary.avg_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_us(run.summary.p50_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div().w(px(84.)).flex_none().child(
                            Label::new(format_latency_us(run.summary.p95_us))
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(fg),
                        ),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .child(Label::new(p99_str).text_xs().font_family(mono.clone()).text_color(fg)),
                    )
                    .child(
                        div().w(px(96.)).flex_none().child(
                            Button::new(SharedString::from(format!("bench-base-btn-{run_id}")))
                                .xsmall()
                                .when(is_baseline, |b| b.primary())
                                .when(!is_baseline, |b| b.outline())
                                .label(btn_label)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_baseline(run_id, cx);
                                })),
                        ),
                    ),
            );
        }

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(px(6.))
            .border_1()
            .border_color(border_col)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        Label::new(i18n_benchmark(cx, "history_title"))
                            .text_sm()
                            .font_semibold(),
                    )
                    .child(
                        Label::new(i18n_benchmark(cx, "history_hint"))
                            .text_xs()
                            .text_color(muted_fg),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(border_col)
                    .child(header_cell("#".into(), 56.0))
                    .child(header_cell(i18n_benchmark(cx, "col_server"), 160.0))
                    .child(
                        div().flex_1().min_w(px(180.)).child(
                            Label::new(i18n_benchmark(cx, "col_config"))
                                .text_xs()
                                .text_color(muted_fg),
                        ),
                    )
                    .child(header_cell(i18n_benchmark(cx, "ops_per_sec"), 140.0))
                    .child(header_cell(i18n_benchmark(cx, "avg_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p50_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p95_latency"), 84.0))
                    .child(header_cell(i18n_benchmark(cx, "p99_latency"), 130.0))
                    .child(header_cell(i18n_benchmark(cx, "col_baseline"), 96.0)),
            )
            .child(list)
            .into_any_element()
    }
}

impl Render for ZedisBenchmark {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_session();
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let muted_fg = cx.theme().muted_foreground;
        let border_col = cx.theme().border;
        let danger_bg = cx.theme().danger;
        let danger_fg = cx.theme().danger_foreground;

        let selected = self.selected_entry().cloned();
        let baseline = self.baseline_entry().cloned();
        let comparison = selected
            .as_ref()
            .zip(baseline.as_ref())
            .map(|(sel, base)| compare_summaries(&base.summary, &sel.summary));

        let body = if let Some(entry) = selected.as_ref() {
            let cards = self.render_headline_cards(entry, baseline.as_ref(), comparison.as_ref(), cx);
            let charts = self.render_charts(entry, cx);
            let table = self.render_history_table(cx);
            v_flex()
                .w_full()
                .p_4()
                .gap_4()
                .child(cards)
                .child(charts)
                .child(table)
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .p_8()
                .gap_2()
                .child(Icon::new(CustomIconName::Zap).size(px(32.)).text_color(muted_fg))
                .child(
                    Label::new(i18n_benchmark(cx, "empty_title"))
                        .text_sm()
                        .font_semibold()
                        .text_color(fg),
                )
                .child(
                    Label::new(i18n_benchmark(cx, "empty_description"))
                        .text_xs()
                        .text_color(muted_fg),
                )
                .into_any_element()
        };

        let header = self.render_header(cx);
        let controls = self.render_controls(cx);
        let live_banner = self.render_live_banner(cx);

        v_flex()
            .size_full()
            .bg(bg)
            .child(header)
            .child(controls)
            .child(live_banner)
            .when_some(self.error.clone(), move |this, err| {
                this.child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(danger_bg)
                        .child(Label::new(err).text_xs().text_color(danger_fg)),
                )
            })
            .child(
                div().flex_1().w_full().relative().child(
                    div()
                        .id("benchmark-scroll")
                        .absolute()
                        .inset_0()
                        .size_full()
                        .track_scroll(&self.scroll)
                        .overflow_y_scrollbar()
                        .child(body),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_1p5()
                    .border_t_1()
                    .border_color(border_col)
                    .justify_between()
                    .items_center()
                    .child(
                        Label::new(i18n_common(cx, "back_to_editor"))
                            .text_xs()
                            .text_color(muted_fg),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_ui;
    use gpui::{AppContext, TestAppContext, size};
    use gpui_kit::component::Root;
    use zedis_core::benchmark::{BenchmarkRawOutcome, ThroughputPoint, summarize_benchmark};

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

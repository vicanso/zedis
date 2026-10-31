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

//! A page's key export and server-to-server copy: the keys' values read
//! through the bridge and either written into one JSON or CSV document in the
//! tab and downloaded, or copied straight to another server / db via `DUMP`
//! and `RESTORE` (with type-aware re-creation when the target does not read
//! the source's RDB payload version).
//!
//! The desktop exports through the migration window (`migration_window.rs`).
//! What the file destination downloads here is the same `ReadableDocument` the
//! desktop's JSON / CSV export writes, held in memory, so it stops at
//! [`MAX_EXPORT_BYTES`] rather than at whatever the tab can take.

use super::export::{CopyPreset, ExportSource, export_filename_with, export_to_file_global, open_file};
use crate::assets::CustomIconName;
use crate::connection::{
    ConflictMode, ConflictPreview, ImportFormat, ReadLimits, ReadableDocument, ReadableEntry, ReadableWriteStatus,
    RestoreStatus, ServerDb, dump_keys_chunk, get_server, get_servers, parse_readable_entries, preview_key_conflicts,
    read_readable_chunk, restore_or_recreate_chunk, sniff_import_format, write_readable_chunk,
};
use crate::helpers::{PickFailure, PickedFile};
use crate::states::{ZedisGlobalStore, i18n_common, i18n_copy, i18n_migration};
use gpui::{AnyElement, App, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    progress::Progress,
    radio::RadioGroup,
    scroll::ScrollableElement,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use rust_i18n::t;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use zedis_ui::ZedisDialog;

/// Keys read or copied per round trip to the bridge.
const CHUNK_KEYS: usize = 64;
/// The most a download is built up to: the tab holds it whole, and twice
/// over while the browser takes it.
const MAX_EXPORT_BYTES: usize = 64_000_000;
/// Maximum sample conflicting keys shown in the dry-run preview.
const PREVIEW_SAMPLE_LIMIT: usize = 40;

/// Where the keys go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    File,
    Server,
}

/// What a page exports to when downloading a file: the readable forms.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Csv,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Csv => "csv",
        }
    }
}

enum Phase {
    Idle,
    RunningFile {
        format: Format,
        done: usize,
        total: usize,
    },
    FinishedFile {
        exported: usize,
        bytes: usize,
        /// The document reached [`MAX_EXPORT_BYTES`] before the last key.
        stopped_early: bool,
    },
    RunningCopy {
        done: usize,
        total: usize,
        written: u64,
        skipped: u64,
        failed: u64,
        bytes: u64,
    },
    FinishedCopy {
        written: u64,
        skipped: u64,
        failed: u64,
        bytes: u64,
        cancelled: bool,
    },
    Failed(SharedString),
}

pub struct ZedisKeyExport {
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    source: ExportSource,
    destination: Destination,
    prefix_input_state: Entity<InputState>,
    servers: Vec<(SharedString, SharedString)>,
    target_server_id: Option<SharedString>,
    target_db_input: Entity<InputState>,
    conflict_mode: ConflictMode,
    preview: Option<ConflictPreview>,
    preview_running: bool,
    preview_error: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    phase: Phase,
    /// The export or copy under way; dropped with the dialog, which stops it.
    _task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl ZedisKeyExport {
    fn new(
        server_id: SharedString,
        server_name: SharedString,
        db: usize,
        keys: Vec<SharedString>,
        source: ExportSource,
        preset: Option<CopyPreset>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefix_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_migration(cx, "prefix_filter_placeholder"))
        });
        let servers: Vec<(SharedString, SharedString)> = get_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id.into(), s.name.into()))
            .collect();
        let target_server_id = preset.as_ref().map(|preset| preset.target_id.clone()).or_else(|| {
            servers
                .iter()
                .find(|(id, _)| *id != server_id)
                .or_else(|| servers.first())
                .map(|(id, _)| id.clone())
        });
        let target_db = preset.as_ref().map(|preset| preset.target_db).unwrap_or(db);
        let target_db_input = cx.new(|cx| InputState::new(window, cx).default_value(target_db.to_string()));
        let destination = if preset.is_some() {
            Destination::Server
        } else {
            Destination::File
        };
        let conflict_mode = preset.as_ref().map(|preset| preset.conflict).unwrap_or_default();

        let mut subs = Vec::new();
        subs.push(
            cx.subscribe_in(&prefix_input_state, window, |view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    view.drop_preview();
                    cx.notify();
                }
            }),
        );
        subs.push(
            cx.subscribe_in(&target_db_input, window, |view, _state, event, _window, cx| {
                if let InputEvent::Change = event {
                    view.drop_preview();
                    cx.notify();
                }
            }),
        );

        Self {
            server_id,
            server_name,
            db,
            keys,
            source,
            destination,
            prefix_input_state,
            servers,
            target_server_id,
            target_db_input,
            conflict_mode,
            preview: None,
            preview_running: false,
            preview_error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            phase: Phase::Idle,
            _task: None,
            _subs: subs,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.phase, Phase::RunningFile { .. } | Phase::RunningCopy { .. })
    }

    fn target_db(&self, cx: &App) -> usize {
        self.target_db_input.read(cx).value().trim().parse().unwrap_or(0)
    }

    fn target_is_source(&self, cx: &App) -> bool {
        self.target_server_id.as_ref() == Some(&self.server_id) && self.target_db(cx) == self.db
    }

    fn export_keys<'a>(&'a self, cx: &App) -> impl Iterator<Item = &'a SharedString> + use<'a> {
        let prefix = self.prefix_input_state.read(cx).value().trim().to_string();
        self.keys.iter().filter(move |key| key.starts_with(prefix.as_str()))
    }

    fn filtered_keys(&self, cx: &App) -> Vec<String> {
        self.export_keys(cx).map(ToString::to_string).collect()
    }

    fn drop_preview(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.preview = None;
        self.preview_error = None;
    }

    fn set_destination(&mut self, destination: Destination, cx: &mut Context<Self>) {
        if self.destination == destination || self.is_running() {
            return;
        }
        self.destination = destination;
        self.drop_preview();
        self.phase = Phase::Idle;
        cx.notify();
    }

    fn select_target(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        self.target_server_id = Some(id);
        self.drop_preview();
        cx.notify();
    }

    fn start_file_export(&mut self, format: Format, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let keys = self.filtered_keys(cx);
        if keys.is_empty() {
            return;
        }
        let total = keys.len();
        let at = ServerDb::new(self.server_id.as_str(), self.db);
        let file_name = export_filename_with(&format!("{}-db{}", self.server_name, self.db), format.extension());
        let success = i18n_migration(cx, "export_title");
        let failure = i18n_migration(cx, "phase_failed");
        self.phase = Phase::RunningFile { format, done: 0, total };
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let mut document = match format {
                Format::Json => ReadableDocument::json(),
                Format::Csv => ReadableDocument::csv(),
            };
            let mut stopped_early = false;
            let mut done = 0;
            'chunks: for chunk in keys.chunks(CHUNK_KEYS) {
                let entries = match read_readable_chunk(&at, chunk, ReadLimits::default()).await {
                    Ok(entries) => entries,
                    Err(e) => {
                        let _ = this.update(cx, |view, cx| {
                            view.phase = Phase::Failed(e.to_string().into());
                            cx.notify();
                        });
                        return;
                    }
                };
                for entry in &entries {
                    if !document.push(entry, MAX_EXPORT_BYTES) {
                        stopped_early = true;
                        break 'chunks;
                    }
                }
                done += chunk.len();
                let progressed = this.update(cx, |view, cx| {
                    view.phase = Phase::RunningFile { format, done, total };
                    cx.notify();
                });
                if progressed.is_err() {
                    return;
                }
            }
            let exported = document.entries();
            let text = document.finish();
            let bytes = text.len();
            let _ = this.update(cx, |view, cx| {
                export_to_file_global(cx, text.into_bytes(), &file_name, success, failure);
                view.phase = Phase::FinishedFile {
                    exported,
                    bytes,
                    stopped_early,
                };
                cx.notify();
            });
        }));
    }

    fn preview_copy(&mut self, cx: &mut Context<Self>) {
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.is_running() || self.preview_running || self.target_is_source(cx) {
            return;
        }
        let keys = self.filtered_keys(cx);
        if keys.is_empty() {
            return;
        }
        let target_id = target_id.to_string();
        let target_db = self.target_db(cx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let superseded = cancel.clone();
        self.preview_running = true;
        self.preview_error = None;
        self.preview = None;
        cx.notify();

        self._task = Some(cx.spawn(async move |this, cx| {
            let result = preview_key_conflicts(&target_id, target_db, &keys, PREVIEW_SAMPLE_LIMIT, &cancel).await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) && !result.as_ref().is_ok_and(|p| p.cancelled) {
                    return;
                }
                view.preview_running = false;
                match result {
                    Ok(preview) => {
                        view.preview = Some(preview);
                        view.preview_error = None;
                    }
                    Err(e) => {
                        view.preview = None;
                        view.preview_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn start_copy(&mut self, cx: &mut Context<Self>) {
        let Some(target_id) = self.target_server_id.clone() else {
            return;
        };
        if self.is_running() || self.target_is_source(cx) {
            return;
        }
        let keys = self.filtered_keys(cx);
        if keys.is_empty() {
            return;
        }
        self.drop_preview();
        let total = keys.len();
        let src = ServerDb::new(self.server_id.as_str(), self.db);
        let dst = ServerDb::new(target_id.as_str(), self.target_db(cx));
        let conflict = self.conflict_mode;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.phase = Phase::RunningCopy {
            done: 0,
            total,
            written: 0,
            skipped: 0,
            failed: 0,
            bytes: 0,
        };
        cx.notify();

        self._task = Some(cx.spawn(async move |this, cx| {
            let mut done = 0usize;
            let mut written = 0u64;
            let mut skipped = 0u64;
            let mut failed = 0u64;
            let mut bytes = 0u64;

            for chunk in keys.chunks(CHUNK_KEYS) {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let entries = match dump_keys_chunk(&src, chunk).await {
                    Ok(entries) => entries,
                    Err(e) => {
                        let _ = this.update(cx, |view, cx| {
                            view.phase = Phase::Failed(e.to_string().into());
                            cx.notify();
                        });
                        return;
                    }
                };
                let missing = chunk.len().saturating_sub(entries.len()) as u64;
                skipped += missing;

                if !entries.is_empty() {
                    let statuses = match restore_or_recreate_chunk(&src, &dst, &entries, conflict).await {
                        Ok(statuses) => statuses,
                        Err(e) => {
                            let _ = this.update(cx, |view, cx| {
                                view.phase = Phase::Failed(e.to_string().into());
                                cx.notify();
                            });
                            return;
                        }
                    };
                    for (entry, status) in entries.iter().zip(statuses.iter()) {
                        let size = entry.payload.len() as u64;
                        match status {
                            RestoreStatus::Written | RestoreStatus::Recreated => {
                                written += 1;
                                bytes += size;
                            }
                            RestoreStatus::Skipped => {
                                skipped += 1;
                            }
                            RestoreStatus::Failed(_) => {
                                failed += 1;
                            }
                        }
                    }
                }

                done += chunk.len();
                let progressed = this.update(cx, |view, cx| {
                    view.phase = Phase::RunningCopy {
                        done,
                        total,
                        written,
                        skipped,
                        failed,
                        bytes,
                    };
                    cx.notify();
                });
                if progressed.is_err() {
                    return;
                }
            }

            let cancelled = cancel.load(Ordering::Acquire);
            let _ = this.update(cx, |view, cx| {
                view.phase = Phase::FinishedCopy {
                    written,
                    skipped,
                    failed,
                    bytes,
                    cancelled,
                };
                cx.notify();
            });
        }));
    }

    /// "Source: server / db / N keys", and whether that is the database or
    /// what the tree has scanned so far.
    fn summary(&self, cx: &App) -> SharedString {
        let mut summary = i18n_migration(cx, "source_summary")
            .replace("{server}", &self.server_name)
            .replace("{db}", &self.db.to_string())
            .replace("{count}", &self.keys.len().to_string());
        if self.source == ExportSource::Loaded {
            summary.push_str(" · ");
            summary.push_str(&i18n_migration(cx, "loaded_subset_note"));
        }
        summary.into()
    }

    fn destination_summary(&self, cx: &App) -> Option<SharedString> {
        if self.destination != Destination::Server {
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

    fn preview_summary(&self, cx: &App) -> SharedString {
        if self.preview_running {
            i18n_migration(cx, "preview_copy_running")
        } else if let Some(err) = &self.preview_error {
            format!("{}: {err}", i18n_migration(cx, "preview_failed")).into()
        } else if let Some(p) = &self.preview {
            if p.cancelled {
                i18n_migration(cx, "preview_cancelled")
            } else {
                i18n_migration(cx, "preview_copy_summary")
                    .replace("{total}", &p.total.to_string())
                    .replace("{conflicts}", &p.conflicting.to_string())
                    .replace("{free}", &p.free.to_string())
                    .into()
            }
        } else {
            i18n_migration(cx, "preview_copy_idle")
        }
    }

    fn preview_sample(&self) -> Vec<SharedString> {
        self.preview
            .as_ref()
            .map(|p| p.sample_keys.iter().map(|k| SharedString::from(k.clone())).collect())
            .unwrap_or_default()
    }

    fn export_button(&self, format: Format, filtered_count: usize, cx: &mut Context<Self>) -> Button {
        let (id, label) = match format {
            Format::Json => ("key-export-json", "export_json"),
            Format::Csv => ("key-export-csv", "export_csv"),
        };
        let running_this = matches!(self.phase, Phase::RunningFile { format: running, .. } if running == format);
        Button::new(id)
            .small()
            .outline()
            // The spinner takes the icon's place; without one, loading shows
            // nothing at all.
            .icon(Icon::new(CustomIconName::Download))
            .label(i18n_common(cx, label))
            .loading(running_this)
            .disabled(self.is_running() || filtered_count == 0)
            .on_click(cx.listener(move |this, _, _window, cx| this.start_file_export(format, cx)))
    }
}

impl Render for ZedisKeyExport {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let warning = theme.warning;
        let danger = theme.danger;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let limit = format_size(MAX_EXPORT_BYTES as u64, DECIMAL);
        let hint = t!("migration.web_export_hint", limit = limit, locale = &locale).to_string();
        let total_count = self.keys.len();
        let filtered_count = self.export_keys(cx).count();
        let is_running = self.is_running();
        let is_copy = self.destination == Destination::Server;
        let target_is_source = self.target_is_source(cx);

        let destination_labels = vec![
            i18n_migration(cx, "destination_file").to_string(),
            i18n_migration(cx, "destination_server").to_string(),
        ];
        let destination_index = match self.destination {
            Destination::File => 0,
            Destination::Server => 1,
        };

        let status = match &self.phase {
            Phase::Idle => None,
            Phase::RunningFile { done, total, .. } => {
                let text = t!(
                    "migration.web_export_progress",
                    done = done,
                    total = total,
                    locale = &locale
                );
                let share = if *total == 0 {
                    0.
                } else {
                    *done as f32 * 100. / *total as f32
                };
                Some(
                    v_flex()
                        .gap_1()
                        .child(Progress::new("key-export-progress").value(share))
                        .child(Label::new(text.to_string()).text_xs().text_color(muted))
                        .into_any_element(),
                )
            }
            Phase::FinishedFile {
                exported,
                bytes,
                stopped_early,
            } => {
                let size = format_size(*bytes as u64, DECIMAL);
                let done = t!(
                    "migration.web_export_done",
                    count = exported,
                    size = size,
                    locale = &locale
                );
                let stopped = stopped_early.then(|| {
                    t!(
                        "migration.web_export_stopped",
                        limit = &limit,
                        count = exported,
                        locale = &locale
                    )
                    .to_string()
                });
                Some(
                    v_flex()
                        .gap_1()
                        .child(Label::new(done.to_string()).text_sm())
                        .children(stopped.map(|text| Label::new(text).text_xs().text_color(warning)))
                        .into_any_element(),
                )
            }
            Phase::RunningCopy {
                done,
                total,
                written,
                skipped,
                failed,
                bytes,
            } => {
                let share = if *total == 0 {
                    0.
                } else {
                    *done as f32 * 100. / *total as f32
                };
                let text = format!(
                    "{}: {} / {} ({}) · written {written}, skipped {skipped}, failed {failed}",
                    i18n_migration(cx, "phase_running"),
                    done,
                    total,
                    format_size(*bytes, DECIMAL),
                );
                Some(
                    v_flex()
                        .gap_1()
                        .child(Progress::new("key-export-copy-progress").value(share))
                        .child(Label::new(text).text_xs().text_color(muted))
                        .into_any_element(),
                )
            }
            Phase::FinishedCopy {
                written,
                skipped,
                failed,
                bytes,
                cancelled,
            } => {
                let phase_name = if *cancelled {
                    i18n_migration(cx, "phase_cancelled")
                } else {
                    i18n_migration(cx, "phase_finished")
                };
                let summary = format!(
                    "{phase_name}: {written} ({}) · skipped {skipped} · failed {failed}",
                    format_size(*bytes, DECIMAL),
                );
                Some(Label::new(summary).text_sm().into_any_element())
            }
            Phase::Failed(error) => Some(
                Label::new(error.clone())
                    .text_xs()
                    .text_color(danger)
                    .into_any_element(),
            ),
        };

        let copy_controls = is_copy.then(|| {
            let selected = self.target_server_id.clone();
            let mut server_row = h_flex().gap_2().flex_wrap();
            for (id, name) in &self.servers {
                let is_selected = selected.as_ref() == Some(id);
                let id_click = id.clone();
                let button = Button::new(SharedString::from(format!("key-export-target-{id}")))
                    .small()
                    .label(name.clone())
                    .disabled(is_running);
                let button = if is_selected {
                    button.primary()
                } else {
                    button.outline()
                };
                server_row = server_row.child(
                    button.on_click(cx.listener(move |this, _, _window, cx| this.select_target(id_click.clone(), cx))),
                );
            }
            let conflict_labels = vec![
                i18n_migration(cx, "conflict_skip").to_string(),
                i18n_migration(cx, "conflict_overwrite").to_string(),
                i18n_migration(cx, "conflict_abort").to_string(),
            ];
            let sample_lines = self.preview_sample();
            v_flex()
                .gap_2()
                .child(Label::new(i18n_copy(cx, "target_server")).text_sm())
                .child(server_row)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_copy(cx, "target_db")).text_sm())
                        .child(
                            Input::new(&self.target_db_input)
                                .small()
                                .w(px(100.))
                                .disabled(is_running),
                        )
                        .when(target_is_source, |this| {
                            this.child(
                                Label::new(i18n_migration(cx, "same_target_note"))
                                    .text_xs()
                                    .text_color(theme.yellow),
                            )
                        }),
                )
                .child(Label::new(i18n_migration(cx, "conflict_label")).text_sm())
                .child(
                    RadioGroup::horizontal("key-export-conflict")
                        .children(conflict_labels)
                        .selected_index(Some(self.conflict_mode.index()))
                        .disabled(is_running)
                        .on_click(cx.listener(|this, index, _window, cx| {
                            this.conflict_mode = ConflictMode::from_index(*index);
                            cx.notify();
                        })),
                )
                .child(Label::new(self.preview_summary(cx)).text_xs().text_color(muted))
                .when(!sample_lines.is_empty(), |this| {
                    this.child(
                        Label::new(i18n_migration(cx, "preview_sample_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(sample_block(sample_lines, cx))
                })
                .child(
                    Label::new(i18n_copy(cx, "version_note"))
                        .text_xs()
                        .text_color(theme.yellow),
                )
        });

        let action_row = if is_copy {
            let can_copy = self.target_server_id.is_some() && !target_is_source && filtered_count > 0;
            let start_label = if matches!(self.phase, Phase::FinishedCopy { .. }) {
                "start_copy_again"
            } else {
                "start_copy"
            };
            let mut row = h_flex().w_full().gap_2().justify_end();
            if is_running || self.preview_running {
                row = row.child(
                    Button::new("key-export-cancel")
                        .small()
                        .danger()
                        .label(i18n_migration(cx, "cancel"))
                        .on_click(cx.listener(|this, _, _window, _cx| {
                            this.cancel.store(true, Ordering::Release);
                        })),
                );
            } else {
                row = row
                    .child(
                        Button::new("key-export-preview")
                            .small()
                            .outline()
                            .disabled(!can_copy)
                            .label(i18n_migration(cx, "preview_conflicts"))
                            .on_click(cx.listener(|this, _, _window, cx| this.preview_copy(cx))),
                    )
                    .child(
                        Button::new("key-export-start-copy")
                            .small()
                            .primary()
                            .disabled(!can_copy)
                            .label(i18n_migration(cx, start_label))
                            .on_click(cx.listener(|this, _, _window, cx| this.start_copy(cx))),
                    );
            }
            row
        } else {
            h_flex()
                .w_full()
                .gap_2()
                .justify_end()
                .child(self.export_button(Format::Csv, filtered_count, cx))
                .child(self.export_button(Format::Json, filtered_count, cx))
        };

        v_flex()
            .w_full()
            .gap_3()
            .child(Label::new(self.summary(cx)).text_sm())
            .when_some(self.destination_summary(cx), |this, text| {
                this.child(Label::new(text).text_xs().text_color(muted))
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(i18n_migration(cx, "destination_label")).text_sm())
                    .child(
                        RadioGroup::horizontal("key-export-destination")
                            .children(destination_labels)
                            .selected_index(Some(destination_index))
                            .disabled(is_running)
                            .on_click(cx.listener(|this, index, _window, cx| {
                                let destination = if *index == 1 {
                                    Destination::Server
                                } else {
                                    Destination::File
                                };
                                this.set_destination(destination, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Label::new(i18n_migration(cx, "prefix_filter")).text_sm())
                    .child(
                        Input::new(&self.prefix_input_state)
                            .small()
                            .w(px(220.))
                            .disabled(is_running),
                    )
                    .child(
                        Label::new(SharedString::from(format!("{filtered_count}/{total_count}")))
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .when(!is_copy, |this| {
                this.child(Label::new(hint).text_xs().text_color(muted))
            })
            .children(copy_controls)
            .children(status)
            .child(action_row)
    }
}

fn sample_block(sample_lines: Vec<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let long = sample_lines.len() > 5;
    let block = v_flex()
        .border_1()
        .border_color(theme.border)
        .rounded(px(4.))
        .px_2()
        .py_1()
        .children(
            sample_lines
                .into_iter()
                .map(|k| Label::new(k).text_xs().text_color(theme.danger_foreground)),
        );
    if long {
        block.h(px(100.)).overflow_y_scrollbar().into_any_element()
    } else {
        block.into_any_element()
    }
}

/// Open the export over the page. Closing it stops an export under way.
pub(crate) fn open_key_export_dialog(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    source: ExportSource,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ZedisKeyExport::new(server_id, server_name, db, keys, source, None, window, cx));
    ZedisDialog::new(i18n_migration(cx, "export_title"))
        .w(px(580.))
        .ok_text(i18n_common(cx, "close"))
        .child(move || view.clone())
        .open(window, cx);
}

/// Open the export dialog already set to copy `keys` to `preset`'s server / db.
pub(crate) fn open_key_copy_dialog(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    preset: CopyPreset,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| {
        ZedisKeyExport::new(
            server_id,
            server_name,
            db,
            keys,
            ExportSource::Selection,
            Some(preset),
            window,
            cx,
        )
    });
    ZedisDialog::new(i18n_migration(cx, "copy_title"))
        .w(px(580.))
        .ok_text(i18n_common(cx, "close"))
        .child(move || view.clone())
        .open(window, cx);
}

struct LoadedImportFile {
    name: SharedString,
    entries: Vec<ReadableEntry>,
}

enum ImportPhase {
    Idle,
    Running {
        done: usize,
        total: usize,
        written: u64,
        skipped: u64,
        failed: u64,
    },
    Finished {
        written: u64,
        skipped: u64,
        failed: u64,
        cancelled: bool,
    },
    Failed(SharedString),
}

pub struct ZedisKeyImport {
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    conflict_mode: ConflictMode,
    chosen_file: Option<LoadedImportFile>,
    preview: Option<ConflictPreview>,
    preview_running: bool,
    preview_error: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    phase: ImportPhase,
    _task: Option<Task<()>>,
}

impl ZedisKeyImport {
    fn new(server_id: SharedString, server_name: SharedString, db: usize) -> Self {
        Self {
            server_id,
            server_name,
            db,
            conflict_mode: ConflictMode::default(),
            chosen_file: None,
            preview: None,
            preview_running: false,
            preview_error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            phase: ImportPhase::Idle,
            _task: None,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.phase, ImportPhase::Running { .. })
    }

    fn destination_summary(&self, cx: &App) -> SharedString {
        i18n_migration(cx, "destination_summary")
            .replace("{server}", &self.server_name)
            .replace("{db}", &self.db.to_string())
            .into()
    }

    fn drop_preview(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.preview = None;
        self.preview_error = None;
    }

    fn pick_file(&mut self, cx: &mut Context<Self>) {
        if self.is_running() || self.preview_running {
            return;
        }
        let pick_task = open_file(cx, ".json,.csv", MAX_EXPORT_BYTES as u64);
        self._task = Some(cx.spawn(async move |this, cx| {
            let Some(picked) = pick_task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.drop_preview();
                match picked {
                    Err(PickFailure::TooLarge { size }) => {
                        view.chosen_file = None;
                        view.phase = ImportPhase::Failed(
                            format!(
                                "{} (max {})",
                                format_size(size, DECIMAL),
                                format_size(MAX_EXPORT_BYTES as u64, DECIMAL)
                            )
                            .into(),
                        );
                    }
                    Err(PickFailure::Unreadable(msg)) => {
                        view.chosen_file = None;
                        view.phase = ImportPhase::Failed(msg.into());
                    }
                    Ok(PickedFile { name, bytes }) => match parse_picked_readable(&bytes) {
                        Ok(entries) => {
                            view.chosen_file = Some(LoadedImportFile {
                                name: name.into(),
                                entries,
                            });
                            view.phase = ImportPhase::Idle;
                        }
                        Err(msg) => {
                            view.chosen_file = None;
                            view.phase = ImportPhase::Failed(msg.into());
                        }
                    },
                }
                cx.notify();
            });
        }));
    }

    fn preview_conflicts(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.chosen_file else {
            return;
        };
        if self.is_running() || self.preview_running || loaded.entries.is_empty() {
            return;
        }
        let keys: Vec<String> = loaded.entries.iter().map(|e| e.key.clone()).collect();
        let server_id = self.server_id.to_string();
        let db = self.db;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let superseded = cancel.clone();
        self.preview_running = true;
        self.preview_error = None;
        self.preview = None;
        cx.notify();

        self._task = Some(cx.spawn(async move |this, cx| {
            let result = preview_key_conflicts(&server_id, db, &keys, PREVIEW_SAMPLE_LIMIT, &cancel).await;
            let _ = this.update(cx, |view, cx| {
                if superseded.load(Ordering::Acquire) && !result.as_ref().is_ok_and(|p| p.cancelled) {
                    return;
                }
                view.preview_running = false;
                match result {
                    Ok(preview) => {
                        view.preview = Some(preview);
                        view.preview_error = None;
                    }
                    Err(e) => {
                        view.preview = None;
                        view.preview_error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn start_import(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.chosen_file else {
            return;
        };
        if self.is_running() || loaded.entries.is_empty() {
            return;
        }
        let entries = loaded.entries.clone();
        let total = entries.len();
        self.drop_preview();
        let at = ServerDb::new(self.server_id.as_str(), self.db);
        let conflict = self.conflict_mode;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.phase = ImportPhase::Running {
            done: 0,
            total,
            written: 0,
            skipped: 0,
            failed: 0,
        };
        cx.notify();

        self._task = Some(cx.spawn(async move |this, cx| {
            let mut done = 0usize;
            let mut written = 0u64;
            let mut skipped = 0u64;
            let mut failed = 0u64;

            for chunk in entries.chunks(CHUNK_KEYS) {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let statuses = match write_readable_chunk(&at, chunk, conflict).await {
                    Ok(statuses) => statuses,
                    Err(e) => {
                        let _ = this.update(cx, |view, cx| {
                            view.phase = ImportPhase::Failed(e.to_string().into());
                            cx.notify();
                        });
                        return;
                    }
                };
                for status in statuses {
                    match status {
                        ReadableWriteStatus::Written => written += 1,
                        ReadableWriteStatus::SkippedExists
                        | ReadableWriteStatus::SkippedTruncated
                        | ReadableWriteStatus::SkippedUnsupported
                        | ReadableWriteStatus::SkippedEmpty => skipped += 1,
                        ReadableWriteStatus::Failed(_) => failed += 1,
                    }
                }
                done += chunk.len();
                let progressed = this.update(cx, |view, cx| {
                    view.phase = ImportPhase::Running {
                        done,
                        total,
                        written,
                        skipped,
                        failed,
                    };
                    cx.notify();
                });
                if progressed.is_err() {
                    return;
                }
            }

            let cancelled = cancel.load(Ordering::Acquire);
            let _ = this.update(cx, |view, cx| {
                view.phase = ImportPhase::Finished {
                    written,
                    skipped,
                    failed,
                    cancelled,
                };
                cx.notify();
            });
        }));
    }

    fn preview_summary(&self, cx: &App) -> SharedString {
        if self.preview_running {
            i18n_migration(cx, "preview_running")
        } else if let Some(err) = &self.preview_error {
            format!("{}: {err}", i18n_migration(cx, "preview_failed")).into()
        } else if let Some(p) = &self.preview {
            if p.cancelled {
                i18n_migration(cx, "preview_cancelled")
            } else {
                i18n_migration(cx, "preview_summary")
                    .replace("{total}", &p.total.to_string())
                    .replace("{conflicts}", &p.conflicting.to_string())
                    .replace("{free}", &p.free.to_string())
                    .into()
            }
        } else {
            i18n_migration(cx, "preview_idle")
        }
    }

    fn preview_sample(&self) -> Vec<SharedString> {
        self.preview
            .as_ref()
            .map(|p| p.sample_keys.iter().map(|k| SharedString::from(k.clone())).collect())
            .unwrap_or_default()
    }
}

fn parse_picked_readable(bytes: &[u8]) -> Result<Vec<ReadableEntry>, String> {
    let format = sniff_import_format(bytes).map_err(|e| e.to_string())?;
    if format == ImportFormat::Binary {
        return Err(
            "binary .zedis-dump bundles are imported in the desktop app; choose a JSON or CSV export".to_string(),
        );
    }
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    parse_readable_entries(text, format).map_err(|e| e.to_string())
}

impl Render for ZedisKeyImport {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let is_running = self.is_running();
        let has_file = self.chosen_file.is_some();

        let conflict_labels = vec![
            i18n_migration(cx, "conflict_skip").to_string(),
            i18n_migration(cx, "conflict_overwrite").to_string(),
            i18n_migration(cx, "conflict_abort").to_string(),
        ];
        let file_label: SharedString = match &self.chosen_file {
            Some(loaded) => format!("{} ({})", loaded.name, loaded.entries.len()).into(),
            None => i18n_migration(cx, "no_file_chosen"),
        };
        let pick_label = if has_file {
            i18n_migration(cx, "pick_file_again")
        } else {
            i18n_migration(cx, "pick_file")
        };
        let sample_lines = self.preview_sample();

        let status = match &self.phase {
            ImportPhase::Idle => None,
            ImportPhase::Running {
                done,
                total,
                written,
                skipped,
                failed,
            } => {
                let share = if *total == 0 {
                    0.
                } else {
                    *done as f32 * 100. / *total as f32
                };
                let text = format!(
                    "{}: {done} / {total} · written {written}, skipped {skipped}, failed {failed}",
                    i18n_migration(cx, "phase_running"),
                );
                Some(
                    v_flex()
                        .gap_1()
                        .child(Progress::new("key-import-progress").value(share))
                        .child(Label::new(text).text_xs().text_color(muted))
                        .into_any_element(),
                )
            }
            ImportPhase::Finished {
                written,
                skipped,
                failed,
                cancelled,
            } => {
                let phase_name = if *cancelled {
                    i18n_migration(cx, "phase_cancelled")
                } else {
                    i18n_migration(cx, "phase_finished")
                };
                let text = format!("{phase_name}: {written} · skipped {skipped} · failed {failed}");
                Some(Label::new(text).text_sm().into_any_element())
            }
            ImportPhase::Failed(err) => Some(Label::new(err.clone()).text_xs().text_color(danger).into_any_element()),
        };

        let mut action_row = h_flex().w_full().gap_2().justify_end();
        if is_running || self.preview_running {
            action_row = action_row.child(
                Button::new("key-import-cancel")
                    .small()
                    .danger()
                    .label(i18n_migration(cx, "cancel"))
                    .on_click(cx.listener(|this, _, _window, _cx| {
                        this.cancel.store(true, Ordering::Release);
                    })),
            );
        } else {
            action_row = action_row
                .child(
                    Button::new("key-import-pick")
                        .small()
                        .outline()
                        .label(pick_label)
                        .on_click(cx.listener(|this, _, _window, cx| this.pick_file(cx))),
                )
                .child(
                    Button::new("key-import-preview")
                        .small()
                        .outline()
                        .disabled(!has_file)
                        .label(i18n_migration(cx, "preview_conflicts"))
                        .on_click(cx.listener(|this, _, _window, cx| this.preview_conflicts(cx))),
                )
                .child(
                    Button::new("key-import-start")
                        .small()
                        .primary()
                        .disabled(!has_file)
                        .label(i18n_migration(cx, "start_import"))
                        .on_click(cx.listener(|this, _, _window, cx| this.start_import(cx))),
                );
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(Label::new(self.destination_summary(cx)).text_sm())
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(i18n_migration(cx, "conflict_label")).text_sm())
                    .child(
                        Label::new(i18n_migration(cx, "conflict_hint"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        RadioGroup::horizontal("key-import-conflict")
                            .children(conflict_labels)
                            .selected_index(Some(self.conflict_mode.index()))
                            .disabled(is_running)
                            .on_click(cx.listener(|this, index, _window, cx| {
                                this.conflict_mode = ConflictMode::from_index(*index);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Label::new(i18n_migration(cx, "file_label")).text_sm())
                    .child(Label::new(file_label).text_xs().text_color(muted).text_ellipsis()),
            )
            .child(Label::new(self.preview_summary(cx)).text_xs().text_color(muted))
            .when(!sample_lines.is_empty(), |this| {
                this.child(
                    Label::new(i18n_migration(cx, "preview_sample_label"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(sample_block(sample_lines, cx))
            })
            .children(status)
            .child(action_row)
    }
}

/// Open the JSON/CSV key import dialog over the page.
pub(crate) fn open_key_import_dialog(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|_| ZedisKeyImport::new(server_id, server_name, db));
    ZedisDialog::new(i18n_migration(cx, "import_title"))
        .w(px(580.))
        .ok_text(i18n_common(cx, "close"))
        .child(move || view.clone())
        .open(window, cx);
}

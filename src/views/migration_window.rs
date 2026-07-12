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

//! Modal-style window for export / import jobs.
//!
//! Import supports conflict strategy selection (Skip / Overwrite / Abort)
//! and an optional dry-run preview (`EXISTS` on destination) before restore.

use crate::connection::{ConflictMode, ConflictPreview, preview_dump_conflicts};
use crate::helpers::{get_download_dir, get_home_dir, with_app_identity};
use crate::states::{
    LogStatus, MigrationEvent, MigrationJob, MigrationPhase, MigrationState, ZedisGlobalStore, i18n_migration,
};
use chrono::Utc;
use gpui::{
    App, Bounds, Entity, FocusHandle, Focusable, KeyDownEvent, SharedString, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme, Disableable, Root,
    button::{Button, ButtonVariants},
    h_flex,
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

const WINDOW_WIDTH: f32 = 720.0;
const WINDOW_HEIGHT: f32 = 620.0;
const PREVIEW_SAMPLE_LIMIT: usize = 40;

/// What kind of job the window was opened for.
#[derive(Clone)]
pub enum MigrationWindowMode {
    Export {
        server_id: SharedString,
        server_name: SharedString,
        db: usize,
        keys: Vec<SharedString>,
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
    /// Import only: what to do when the destination already has the key.
    conflict_mode: ConflictMode,
    /// Import only: last dry-run result (if any).
    preview: Option<ConflictPreview>,
    preview_running: bool,
    preview_error: Option<SharedString>,
    preview_cancel: Arc<AtomicBool>,
    _subs: Vec<Subscription>,
}

impl ZedisMigrationWindow {
    pub fn new(mode: MigrationWindowMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let state = cx.new(|_| MigrationState::new());
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&state, |_view, _state, _event: &MigrationEvent, cx| {
            cx.notify();
        }));
        Self {
            focus_handle,
            mode,
            state,
            chosen_path: None,
            conflict_mode: ConflictMode::Skip,
            preview: None,
            preview_running: false,
            preview_error: None,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            _subs: subs,
        }
    }

    fn title_label(&self, cx: &App) -> SharedString {
        match self.mode {
            MigrationWindowMode::Export { .. } => i18n_migration(cx, "export_title"),
            MigrationWindowMode::Import { .. } => i18n_migration(cx, "import_title"),
        }
    }

    fn source_summary(&self, cx: &App) -> SharedString {
        match &self.mode {
            MigrationWindowMode::Export {
                server_name, db, keys, ..
            } => {
                let template = i18n_migration(cx, "source_summary");
                template
                    .replace("{server}", server_name)
                    .replace("{db}", &db.to_string())
                    .replace("{count}", &keys.len().to_string())
                    .into()
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
                format!("zedis-{safe_name}-db{db}-{stamp}.zedis-dump")
            }
            MigrationWindowMode::Import { .. } => format!("zedis-{stamp}.zedis-dump"),
        }
    }

    fn handle_pick_and_start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match &self.mode {
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
                // Import: pick file only — user then previews / starts with strategy.
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
        let job = match &self.mode {
            MigrationWindowMode::Export {
                server_id, db, keys, ..
            } => MigrationJob::Export {
                server_id: server_id.clone(),
                db: *db,
                keys: keys.clone(),
                output_path: path,
            },
            MigrationWindowMode::Import { server_id, db, .. } => MigrationJob::Import {
                server_id: server_id.clone(),
                db: *db,
                input_path: path,
                conflict: self.conflict_mode,
            },
        };
        self.state.update(cx, |s, cx| s.start(job, cx));
    }

    fn handle_start_import(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.chosen_path.clone() else {
            return;
        };
        // Cancel any in-flight preview so it doesn't fight the import worker.
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_running = false;
        self.start_with_path(path, cx);
    }

    fn handle_preview(&mut self, cx: &mut Context<Self>) {
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
                    preview_dump_conflicts(&server_id, db, path, PREVIEW_SAMPLE_LIMIT, &cancel).await
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

pub(crate) fn dirs_default_directory() -> PathBuf {
    // Prefer the platform's real Downloads dir (UserDirs), falling back to the
    // home dir, then the current dir.
    get_download_dir()
        .or_else(get_home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn phase_label(phase: &MigrationPhase, cx: &App) -> SharedString {
    match phase {
        MigrationPhase::Idle => i18n_migration(cx, "phase_idle"),
        MigrationPhase::Running => i18n_migration(cx, "phase_running"),
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
        let progress = state.progress().clone();
        let log_lines: Vec<_> = state.log().iter().cloned().collect();
        let is_running = matches!(phase, MigrationPhase::Running);
        let is_finished = matches!(phase, MigrationPhase::Finished);
        let saved_path = self.chosen_path.clone();

        let progress_text: SharedString = if progress.keys_total > 0 {
            format!(
                "{} / {}   {}",
                progress.keys_done,
                progress.keys_total,
                format_size(progress.bytes_done, DECIMAL)
            )
            .into()
        } else {
            "—".into()
        };

        let header = div()
            .px_6()
            .pt_6()
            .child(
                Label::new(self.title_label(cx))
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD),
            )
            .child(
                div()
                    .pt_1()
                    .child(Label::new(self.source_summary(cx)).text_sm().text_color(muted)),
            );

        let status_section = div()
            .px_6()
            .pt_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(Label::new(i18n_migration(cx, "status_label")).text_sm())
                    .child(Label::new(phase_label(&phase, cx)).text_sm().text_color(muted)),
            )
            .child(
                h_flex()
                    .gap_4()
                    .pt_1()
                    .child(Label::new(i18n_migration(cx, "progress_label")).text_sm())
                    .child(Label::new(progress_text).text_sm().text_color(muted)),
            );

        let is_import = matches!(self.mode, MigrationWindowMode::Import { .. });
        let conflict_section = is_import.then(|| {
            let selected = self.conflict_mode.index();
            let labels = vec![
                i18n_migration(cx, "conflict_skip").to_string(),
                i18n_migration(cx, "conflict_overwrite").to_string(),
                i18n_migration(cx, "conflict_abort").to_string(),
            ];
            let path_label: SharedString = self
                .chosen_path
                .as_ref()
                .map(|p| p.display().to_string().into())
                .unwrap_or_else(|| i18n_migration(cx, "no_file_chosen"));
            let preview_summary: SharedString = if self.preview_running {
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
            };

            let sample_lines: Vec<SharedString> = self
                .preview
                .as_ref()
                .map(|p| p.sample_keys.iter().map(|k| SharedString::from(k.clone())).collect())
                .unwrap_or_default();

            v_flex()
                .px_6()
                .pt_3()
                .gap_2()
                .child(Label::new(i18n_migration(cx, "conflict_label")).text_sm())
                .child(
                    Label::new(i18n_migration(cx, "conflict_hint"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(
                    RadioGroup::horizontal("migration-conflict-mode")
                        .mt(px(4.))
                        .children(labels)
                        .selected_index(Some(selected))
                        .on_click(cx.listener(|this, index, _window, cx| {
                            this.set_conflict_mode(ConflictMode::from_index(*index), cx);
                        })),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_migration(cx, "file_label")).text_sm())
                        .child(Label::new(path_label).text_xs().text_color(muted).text_ellipsis()),
                )
                .child(Label::new(preview_summary).text_xs().text_color(muted))
                .when(!sample_lines.is_empty(), |this| {
                    this.child(
                        Label::new(i18n_migration(cx, "preview_sample_label"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child({
                        // Same Scrollable/`max_h` pitfall as the update dialog:
                        // a fixed height scrolls, `max_h` silently clips.
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
                    })
                })
        });

        let log_section = div()
            .px_6()
            .pt_4()
            .flex_1()
            .min_h_0()
            .child(Label::new(i18n_migration(cx, "log_label")).text_sm().text_color(muted))
            .child(
                gpui_component::v_flex()
                    .mt_1()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(4.))
                    .h(px(220.))
                    .overflow_y_scrollbar()
                    .px_2()
                    .py_1()
                    .children(log_lines.iter().rev().take(200).rev().map(|line| {
                        let color = match line.status {
                            LogStatus::Ok => theme.foreground,
                            LogStatus::Skipped => muted,
                            LogStatus::Failed => theme.danger_foreground,
                        };
                        let mut text = format!("{}    {}", format_size(line.bytes, DECIMAL), line.key);
                        if let Some(msg) = &line.message {
                            text.push_str("    — ");
                            text.push_str(msg);
                        }
                        Label::new(SharedString::from(text)).text_xs().text_color(color)
                    })),
            );

        let footer = {
            let mut row = gpui_component::h_flex().gap_2().justify_end().px_6().py_4();
            row = row.child(
                Button::new("migration-close")
                    .ghost()
                    .label(i18n_migration(cx, "close"))
                    .on_click(cx.listener(|this, _, window, cx| this.handle_close(window, cx))),
            );
            if is_finished && let Some(_path) = &saved_path {
                row = row.child(
                    Button::new("migration-reveal")
                        .outline()
                        .label(i18n_migration(cx, "reveal_in_finder"))
                        .on_click(cx.listener(|this, _, _window, cx| this.handle_reveal(cx))),
                );
            }
            if is_running || self.preview_running {
                row = row.child(
                    Button::new("migration-cancel")
                        .danger()
                        .label(i18n_migration(cx, "cancel"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.preview_running {
                                this.preview_cancel.store(true, Ordering::Release);
                            } else {
                                this.handle_cancel(cx);
                            }
                        })),
                );
            } else if is_import {
                let has_file = self.chosen_path.is_some();
                let pick_label = if has_file {
                    i18n_migration(cx, "pick_file_again")
                } else {
                    i18n_migration(cx, "pick_file")
                };
                row = row
                    .child(
                        Button::new("migration-pick")
                            .outline()
                            .label(pick_label)
                            .on_click(cx.listener(|this, _, window, cx| this.handle_pick_and_start(window, cx))),
                    )
                    .child(
                        Button::new("migration-preview")
                            .outline()
                            .disabled(!has_file)
                            .label(i18n_migration(cx, "preview_conflicts"))
                            .on_click(cx.listener(|this, _, _, cx| this.handle_preview(cx))),
                    )
                    .child(
                        Button::new("migration-start")
                            .primary()
                            .disabled(!has_file)
                            .label(i18n_migration(cx, "start_import"))
                            .on_click(cx.listener(|this, _, _, cx| this.handle_start_import(cx))),
                    );
            } else {
                let (idle_key, again_key, disabled) = match &self.mode {
                    MigrationWindowMode::Export { keys, .. } => {
                        ("save_and_export", "save_and_export_again", keys.is_empty())
                    }
                    MigrationWindowMode::Import { .. } => ("pick_file", "pick_file_again", false),
                };
                let label_key = if matches!(phase, MigrationPhase::Idle) {
                    idle_key
                } else {
                    again_key
                };
                row = row.child(
                    Button::new("migration-start")
                        .primary()
                        .disabled(disabled)
                        .label(i18n_migration(cx, label_key))
                        .on_click(cx.listener(|this, _, window, cx| this.handle_pick_and_start(window, cx))),
                );
            }
            row
        };

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
                    .child(status_section)
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
    cx: &mut App,
) {
    let title = i18n_migration(cx, "export_title");
    open_migration_window(
        MigrationWindowMode::Export {
            server_id,
            server_name,
            db,
            keys,
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

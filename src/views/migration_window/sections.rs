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

//! What the migration window draws, a section at a time: the job, its
//! status, the export's destination and format, the copy's target, the
//! import's file and conflict policy, the log, and the buttons.

use super::*;
use gpui::{AnyElement, Hsla};

/// What the window's sections are drawn with, read once a render.
pub(super) struct JobLook {
    pub(super) phase: MigrationPhase,
    pub(super) is_running: bool,
    pub(super) is_paused: bool,
    pub(super) is_import: bool,
    pub(super) is_copy: bool,
    pub(super) target_is_source: bool,
    pub(super) filtered_count: usize,
    pub(super) total_count: usize,
    pub(super) muted: Hsla,
}

impl ZedisMigrationWindow {
    /// The job's title, its source, and a copy's destination.
    pub(super) fn header(&self, look: &JobLook, cx: &App) -> impl IntoElement + use<> {
        let &JobLook { muted, .. } = look;
        div()
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
            )
            .when_some(self.destination_summary(cx), |this, text| {
                this.child(div().pt_1().child(Label::new(text).text_sm().text_color(muted)))
            })
    }

    /// Resumable checkpoints from `JobManager`: shown when the migration
    /// window has unfinished jobs from an earlier run.
    pub(super) fn unfinished_jobs_section(&self, look: &JobLook, cx: &Context<Self>) -> Option<AnyElement> {
        if self.unfinished_jobs.is_empty() || look.is_running {
            return None;
        }
        let theme = cx.theme();
        let muted = look.muted;
        let title =
            i18n_migration(cx, "unfinished_jobs_title").replace("{count}", &self.unfinished_jobs.len().to_string());
        let rows: Vec<AnyElement> = self
            .unfinished_jobs
            .iter()
            .enumerate()
            .map(|(ix, cp)| {
                let progress_text = i18n_migration(cx, "unfinished_job_progress")
                    .replace("{count}", &cp.cursor.processed_count.to_string());
                h_flex()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(SharedString::from(cp.summary.clone())).text_xs())
                            .child(
                                Label::new(SharedString::from(progress_text))
                                    .text_xs()
                                    .text_color(muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("migration-resume-job", ix))
                                    .xsmall()
                                    .primary()
                                    .label(i18n_migration(cx, "resume"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.resume_checkpoint(ix, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(("migration-discard-job", ix))
                                    .xsmall()
                                    .ghost()
                                    .label(i18n_migration(cx, "discard_job"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.discard_checkpoint(ix, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        Some(
            v_flex()
                .px_6()
                .pt_3()
                .child(
                    v_flex()
                        .gap_1()
                        .p_2()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(px(4.))
                        .child(
                            Label::new(SharedString::from(title))
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD),
                        )
                        .child(
                            Label::new(i18n_migration(cx, "scan_resume_note"))
                                .text_xs()
                                .text_color(theme.yellow),
                        )
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    /// Where the job is, and how far it got.
    pub(super) fn status_section(&self, look: &JobLook, cx: &Context<Self>) -> impl IntoElement + use<> {
        let &JobLook {
            ref phase,
            is_running,
            muted,
            ..
        } = look;
        let state = self.state.read(cx);
        let progress = state.progress().clone();

        let progress_text: SharedString = if progress.keys_total > 0 {
            format!(
                "{} / {}   {}",
                progress.keys_done,
                progress.keys_total,
                format_size(progress.bytes_done, DECIMAL)
            )
            .into()
        } else if progress.keys_done > 0 {
            format!("{}   {}", progress.keys_done, format_size(progress.bytes_done, DECIMAL)).into()
        } else {
            "—".into()
        };

        let throttle_labels = vec![
            i18n_migration(cx, "throttle_unlimited").to_string(),
            "1000 keys/s".to_string(),
            "200 keys/s".to_string(),
            "50 keys/s".to_string(),
        ];

        div()
            .px_6()
            .pt_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(Label::new(i18n_migration(cx, "status_label")).text_sm())
                    .child(Label::new(phase_label(phase, cx)).text_sm().text_color(muted)),
            )
            .child(
                h_flex()
                    .gap_4()
                    .pt_1()
                    .child(Label::new(i18n_migration(cx, "progress_label")).text_sm())
                    .child(Label::new(progress_text).text_sm().text_color(muted)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .pt_2()
                    .items_center()
                    .child(Label::new(i18n_migration(cx, "throttle_label")).text_sm())
                    .child(
                        RadioGroup::horizontal("migration-throttle")
                            .children(throttle_labels)
                            .selected_index(Some(self.throttle.index()))
                            .disabled(is_running)
                            .on_click(cx.listener(|this, index, _window, cx| {
                                this.throttle = MigrationThrottle::from_index(*index);
                                cx.notify();
                            })),
                    ),
            )
    }

    /// Export: format choice — must be picked before the file dialog,
    /// since choosing a file starts the job immediately. Locked while
    /// running so the worker's format can't be pulled out from under it.
    pub(super) fn export_section(&self, look: &JobLook, cx: &Context<Self>) -> impl IntoElement + use<> {
        let &JobLook {
            is_running,
            is_copy,
            filtered_count,
            total_count,
            muted,
            ..
        } = look;
        let format_labels = vec![
            i18n_migration(cx, "format_binary").to_string(),
            "JSON".to_string(),
            "CSV".to_string(),
        ];
        let destination_labels = vec![
            i18n_migration(cx, "destination_file").to_string(),
            i18n_migration(cx, "destination_server").to_string(),
        ];
        let destination_index = match self.destination {
            Destination::File => 0,
            Destination::Server => 1,
        };
        let source_mode_labels = vec![
            i18n_migration(cx, "source_mode_keys").to_string(),
            i18n_migration(cx, "source_mode_patterns").to_string(),
        ];
        let is_pattern_mode = self.source_mode == KeySourceMode::ScanPatterns;

        let mut type_chips = h_flex().gap_1().flex_wrap().items_center();
        let all_selected = self.selected_types.is_empty();
        let all_btn = Button::new("migration-type-all")
            .xsmall()
            .label(i18n_migration(cx, "type_all"))
            .disabled(is_running);
        let all_btn = if all_selected {
            all_btn.primary()
        } else {
            all_btn.outline()
        };
        type_chips = type_chips.child(all_btn.on_click(cx.listener(|this, _, _window, cx| {
            this.clear_pattern_types(cx);
        })));
        for (redis_ty, label) in PATTERN_KEY_TYPES {
            let selected = self.selected_types.iter().any(|t| t.as_ref() == redis_ty);
            let btn = Button::new(SharedString::from(format!("migration-type-{label}")))
                .xsmall()
                .label(label)
                .disabled(is_running);
            let btn = if selected { btn.primary() } else { btn.outline() };
            type_chips = type_chips.child(btn.on_click(cx.listener(move |this, _, _window, cx| {
                this.toggle_pattern_type(redis_ty, cx);
            })));
        }

        v_flex()
            .px_6()
            .pt_3()
            .gap_2()
            .child(Label::new(i18n_migration(cx, "destination_label")).text_sm())
            .child(
                RadioGroup::horizontal("migration-destination")
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
            )
            .when(!is_copy, |this| {
                this.child(Label::new(i18n_migration(cx, "format_label")).text_sm().mt_1())
                    .child(
                        Label::new(i18n_migration(cx, "format_hint"))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        RadioGroup::horizontal("migration-export-format")
                            .mt(px(4.))
                            .children(format_labels)
                            .selected_index(Some(self.export_format.index()))
                            .disabled(is_running)
                            .on_click(cx.listener(|this, index, _window, cx| {
                                this.export_format = ExportFormat::from_index(*index);
                                cx.notify();
                            })),
                    )
            })
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Label::new(i18n_migration(cx, "source_mode_label")).text_sm())
                    .child(
                        RadioGroup::horizontal("migration-source-mode")
                            .children(source_mode_labels)
                            .selected_index(Some(self.source_mode.index()))
                            .disabled(is_running)
                            .on_click(cx.listener(|this, index, _window, cx| {
                                this.set_source_mode(KeySourceMode::from_index(*index), cx);
                            })),
                    ),
            )
            .when(!is_pattern_mode, |this| {
                this.child(
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
            })
            .when(is_pattern_mode, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_migration(cx, "patterns_label")).text_sm())
                        .child(
                            Input::new(&self.patterns_input_state)
                                .small()
                                .w(px(300.))
                                .disabled(is_running),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Label::new(i18n_migration(cx, "types_label")).text_sm())
                        .child(type_chips),
                )
            })
    }

    /// Copy to a server: the target, the conflict policy and the dry run
    /// — the import section's shape, with the keys coming from the
    /// source instead of a file.
    pub(super) fn copy_section(&self, look: &JobLook, cx: &Context<Self>) -> impl IntoElement + use<> {
        let &JobLook {
            is_running,
            target_is_source,
            muted,
            ..
        } = look;
        let theme = cx.theme();
        let selected = self.target_server_id.clone();
        let mut server_row = h_flex().gap_2().flex_wrap();
        for (id, name) in &self.servers {
            let is_selected = selected.as_ref() == Some(id);
            let id_click = id.clone();
            let button = Button::new(SharedString::from(format!("migration-target-{id}")))
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
        let preview_summary =
            self.preview_summary("preview_copy_running", "preview_copy_summary", "preview_copy_idle", cx);
        let sample_lines = self.preview_sample();
        let mismatch_keys = self.verify_mismatch_keys();
        v_flex()
            .px_6()
            .pt_3()
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
            .child(Label::new(i18n_migration(cx, "conflict_label")).text_sm().mt_1())
            .child(
                RadioGroup::horizontal("migration-copy-conflict")
                    .children(conflict_labels)
                    .selected_index(Some(self.conflict_mode.index()))
                    .disabled(is_running)
                    .on_click(cx.listener(|this, index, _window, cx| {
                        this.set_conflict_mode(ConflictMode::from_index(*index), cx);
                    })),
            )
            .child(Label::new(preview_summary).text_xs().text_color(muted))
            .when_some(self.verify_summary(cx), |this, (text, ok)| {
                let color = if ok { theme.green } else { theme.yellow };
                this.child(Label::new(text).text_xs().text_color(color))
            })
            .when(!mismatch_keys.is_empty(), |this| {
                this.child(
                    Label::new(i18n_migration(cx, "verify_mismatches_label"))
                        .text_xs()
                        .text_color(muted),
                )
                .child(self.mismatch_block(mismatch_keys, cx))
            })
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
    }

    /// Best-effort live follow controls after a pattern-based copy finishes.
    pub(super) fn follow_section(&self, look: &JobLook, cx: &Context<Self>) -> Option<AnyElement> {
        if !look.is_copy || !matches!(look.phase, MigrationPhase::Finished) || self.last_copy_patterns.is_none() {
            return None;
        }
        let theme = cx.theme();
        let muted = look.muted;
        let mut row = h_flex().gap_2().items_center();
        if self.following {
            let last = self.follow_last_key.as_deref().unwrap_or("—");
            let status_text = i18n_migration(cx, "follow_status_active")
                .replace("{synced}", &self.follow_synced_count.to_string())
                .replace("{last}", last);
            row = row
                .child(
                    Label::new(SharedString::from(status_text))
                        .text_xs()
                        .text_color(theme.green),
                )
                .child(
                    Button::new("migration-follow-stop")
                        .xsmall()
                        .danger()
                        .label(i18n_migration(cx, "follow_stop"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.stop_follow(cx);
                        })),
                );
        } else {
            row = row.child(
                Button::new("migration-follow-start")
                    .xsmall()
                    .outline()
                    .label(i18n_migration(cx, "follow_start"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_follow(window, cx);
                    })),
            );
        }

        Some(
            v_flex()
                .px_6()
                .pt_2()
                .gap_1()
                .child(row)
                .when(self.follow_needs_enable, |this| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Label::new(i18n_migration(cx, "follow_keyspace_disabled_warning"))
                                    .text_xs()
                                    .text_color(theme.yellow),
                            )
                            .child(
                                Button::new("migration-follow-enable-source")
                                    .xsmall()
                                    .outline()
                                    .label(i18n_migration(cx, "follow_enable_on_source"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.enable_keyspace_on_source_and_follow(window, cx);
                                    })),
                            ),
                    )
                })
                .when(self.follow_disconnected, |this| {
                    this.child(
                        Label::new(i18n_migration(cx, "follow_disconnected_notice"))
                            .text_xs()
                            .text_color(muted),
                    )
                })
                .into_any_element(),
        )
    }

    /// An import's conflict policy, its file, and the dry run.
    pub(super) fn import_section(&self, look: &JobLook, cx: &Context<Self>) -> impl IntoElement + use<> {
        let &JobLook { muted, .. } = look;
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
        let preview_summary = self.preview_summary("preview_running", "preview_summary", "preview_idle", cx);

        let sample_lines = self.preview_sample();

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
                .child(sample_block(sample_lines, cx))
            })
    }

    /// v_flex, not div: a Block container would kill the child's flex
    /// sizing (see the CLAUDE.md gotcha), and the list box grows with
    /// the window via flex_1 instead of a fixed height.
    pub(super) fn log_section(&self, look: &JobLook, cx: &App) -> impl IntoElement + use<> {
        let &JobLook { muted, .. } = look;
        let theme = cx.theme();
        let state = self.state.read(cx);
        // Only the lines the panel shows: the log keeps more than that.
        let log = state.log();
        let log_lines: Vec<_> = log
            .range(log.len().saturating_sub(LOG_LINES_SHOWN)..)
            .cloned()
            .collect();

        gpui_kit::component::v_flex()
            .px_6()
            .pt_4()
            .flex_1()
            .min_h_0()
            .child(Label::new(i18n_migration(cx, "log_label")).text_sm().text_color(muted))
            .child(
                div()
                    .mt_1()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(4.))
                    .px_2()
                    .py_1()
                    .overflow_y_scrollbar()
                    .children(log_lines.iter().map(|line| {
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
            )
    }

    /// Close, and what the job can do next: Reveal a finished file, Cancel
    /// what is running, or pick, preview and start.
    pub(super) fn footer(&self, look: &JobLook, cx: &Context<Self>) -> impl IntoElement + use<> {
        let &JobLook {
            ref phase,
            is_running,
            is_paused,
            is_import,
            is_copy,
            target_is_source,
            filtered_count,
            ..
        } = look;
        let is_finished = matches!(phase, MigrationPhase::Finished);
        let saved_path = self.chosen_path.clone();
        let has_source = self.source_mode == KeySourceMode::ScanPatterns || filtered_count > 0;
        let mut row = gpui_kit::component::h_flex().gap_2().justify_end().px_6().py_4();
        row = row.child(
            Button::new("migration-close")
                .ghost()
                .label(i18n_migration(cx, "close"))
                .on_click(cx.listener(|this, _, window, cx| this.handle_close(window, cx))),
        );
        if is_finished
            && !is_copy
            && let Some(_path) = &saved_path
        {
            row = row.child(
                Button::new("migration-reveal")
                    .outline()
                    .label(i18n_migration(cx, "reveal_in_finder"))
                    .on_click(cx.listener(|this, _, _window, cx| this.handle_reveal(cx))),
            );
        }
        if is_running || self.preview_running || self.verify_running {
            if is_running && !self.preview_running && !self.verify_running {
                let pause_label = if is_paused {
                    i18n_migration(cx, "resume")
                } else {
                    i18n_migration(cx, "pause")
                };
                row = row.child(
                    Button::new("migration-pause")
                        .outline()
                        .label(pause_label)
                        .on_click(cx.listener(|this, _, _, cx| this.handle_toggle_pause(cx))),
                );
            }
            row = row.child(
                Button::new("migration-cancel")
                    .danger()
                    .label(i18n_migration(cx, "cancel"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.preview_running {
                            this.preview_cancel.store(true, Ordering::Release);
                        } else if this.verify_running {
                            this.verify_cancel.store(true, Ordering::Release);
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
        } else if is_copy {
            let can_copy = self.target_server_id.is_some() && !target_is_source && has_source;
            let label_key = if matches!(phase, MigrationPhase::Idle) {
                "start_copy"
            } else {
                "start_copy_again"
            };
            row = row
                .child(
                    Button::new("migration-preview")
                        .outline()
                        .disabled(!can_copy)
                        .label(i18n_migration(cx, "preview_conflicts"))
                        .on_click(cx.listener(|this, _, _, cx| this.handle_preview(cx))),
                )
                .child(
                    Button::new("migration-verify")
                        .outline()
                        .disabled(!can_copy)
                        .label(i18n_migration(cx, "verify_copy"))
                        .on_click(cx.listener(|this, _, _, cx| this.verify_copy(cx))),
                )
                .child(
                    Button::new("migration-start")
                        .primary()
                        .disabled(!can_copy)
                        .label(i18n_migration(cx, label_key))
                        .on_click(cx.listener(|this, _, _, cx| this.start_copy(cx))),
                );
        } else {
            let (idle_key, again_key, disabled) = match &self.mode {
                MigrationWindowMode::Export { .. } => ("save_and_export", "save_and_export_again", !has_source),
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
    }

    /// The dry run in a line: on its way, failed, cancelled, what it
    /// found, or not run yet. `running`, `summary` and `idle` are the
    /// copy's wording or the import's.
    fn preview_summary(&self, running: &str, summary: &str, idle: &str, cx: &App) -> SharedString {
        if self.preview_running {
            i18n_migration(cx, running)
        } else if let Some(err) = &self.preview_error {
            format!("{}: {err}", i18n_migration(cx, "preview_failed")).into()
        } else if let Some(p) = &self.preview {
            if p.cancelled {
                i18n_migration(cx, "preview_cancelled")
            } else {
                i18n_migration(cx, summary)
                    .replace("{total}", &p.total.to_string())
                    .replace("{conflicts}", &p.conflicting.to_string())
                    .replace("{free}", &p.free.to_string())
                    .into()
            }
        } else {
            i18n_migration(cx, idle)
        }
    }

    /// The keys the last dry run found taken, as many as it kept.
    fn preview_sample(&self) -> Vec<SharedString> {
        self.preview
            .as_ref()
            .map(|p| p.sample_keys.iter().map(|k| SharedString::from(k.clone())).collect())
            .unwrap_or_default()
    }

    /// Mismatched or missing keys from the last verification run, capped for display.
    fn verify_mismatch_keys(&self) -> Vec<SharedString> {
        let Some(r) = &self.verify_report else {
            return Vec::new();
        };
        r.missing_keys
            .iter()
            .chain(r.differing_keys.iter())
            .take(VERIFY_MISMATCH_DISPLAY_LIMIT)
            .map(|k| SharedString::from(k.clone()))
            .collect()
    }

    fn mismatch_block(&self, keys: Vec<SharedString>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let long = keys.len() > 4;
        let open_label = i18n_migration(cx, "open_in_compare");
        let rows: Vec<AnyElement> = keys
            .into_iter()
            .enumerate()
            .map(|(ix, key)| {
                let key_click = key.clone();
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(Label::new(key).text_xs().text_color(theme.yellow))
                    .child(
                        Button::new(("migration-open-compare", ix))
                            .xsmall()
                            .ghost()
                            .label(open_label.clone())
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.open_key_in_compare(key_click.clone(), cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let block = v_flex()
            .gap_1()
            .border_1()
            .border_color(theme.border)
            .rounded(px(4.))
            .px_2()
            .py_1()
            .children(rows);
        if long {
            block.h(px(96.)).overflow_y_scrollbar().into_any_element()
        } else {
            block.into_any_element()
        }
    }

    fn verify_summary(&self, cx: &App) -> Option<(SharedString, bool)> {
        if self.verify_running {
            Some((i18n_migration(cx, "verify_running"), false))
        } else if let Some(err) = &self.verify_error {
            Some((format!("{}: {err}", i18n_migration(cx, "verify_failed")).into(), false))
        } else if let Some(r) = &self.verify_report {
            if r.cancelled {
                Some((i18n_migration(cx, "preview_cancelled"), false))
            } else {
                let all_ok = r.missing_on_target.is_empty() && r.differing.is_empty();
                let text = i18n_migration(cx, "verify_summary")
                    .replace("{checked}", &r.checked.to_string())
                    .replace("{matched}", &r.matched.to_string())
                    .replace("{missing}", &r.missing_on_target.len().to_string())
                    .replace("{differing}", &r.differing.len().to_string());
                Some((text.into(), all_ok))
            }
        } else {
            None
        }
    }
}

/// The keys a dry run found taken on the destination, in a box of its
/// own that scrolls past five.
///
/// Same Scrollable/`max_h` pitfall as the update dialog:
/// a fixed height scrolls, `max_h` silently clips.
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

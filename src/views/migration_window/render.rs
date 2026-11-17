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

use super::*;

impl Focusable for ZedisMigrationWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub(super) fn phase_label(phase: &MigrationPhase, cx: &App) -> SharedString {
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

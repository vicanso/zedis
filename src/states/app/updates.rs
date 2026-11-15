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

//! Update-check state, onboarding hints, server group collapse toggles, DB/tab
//! resolution, and editor pane width preferences on [`ZedisAppState`].

use super::*;
use crate::helpers::unix_ts;

/// Minimum gap between two silent startup update checks, in seconds. Releases
/// are far rarer than launches, so a couple of days is plenty — a user who
/// wants one sooner has the manual check in the title-bar chip.
pub(super) const UPDATE_CHECK_INTERVAL: i64 = 2 * 24 * 60 * 60;

impl ZedisAppState {
    /// Whether a startup update check is due: never run, or longer than
    /// [`UPDATE_CHECK_INTERVAL`] ago.
    pub fn update_check_due(&self) -> bool {
        match self.last_update_check {
            Some(ts) => unix_ts().saturating_sub(ts) >= UPDATE_CHECK_INTERVAL,
            None => true,
        }
    }

    /// Record that an update check just ran, resetting the throttle.
    pub fn mark_update_checked(&mut self) {
        self.last_update_check = Some(unix_ts());
    }

    /// Whether a one-time onboarding hint has already been shown.
    pub fn hint_dismissed(&self, key: &str) -> bool {
        self.dismissed_hints
            .as_ref()
            .is_some_and(|list| list.iter().any(|k| k == key))
    }

    /// Mark a one-time onboarding hint as seen so it never fires again.
    pub fn dismiss_hint(&mut self, key: &str) {
        let list = self.dismissed_hints.get_or_insert_default();
        if !list.iter().any(|k| k == key) {
            list.push(key.to_string());
        }
    }

    /// Whether the user chose to skip this exact version.
    pub fn update_skipped(&self, version: &str) -> bool {
        self.skipped_version.as_deref() == Some(version)
    }

    pub fn set_skipped_version(&mut self, version: String) {
        self.skipped_version = if version.is_empty() { None } else { Some(version) };
    }

    /// The pending update in full (version, changelog, per-arch asset) — the
    /// check already fetched all of it, so the prompt can open straight from
    /// here instead of going back to the network.
    pub fn available_update(&self) -> Option<UpdateInfo> {
        self.available_update.clone()
    }

    /// Just the version string of the pending update — for the status-bar chip.
    pub fn available_update_version(&self) -> Option<SharedString> {
        self.available_update
            .as_ref()
            .map(|info| SharedString::from(info.version.clone()))
    }

    /// Set (or clear with `None`) the pending update and broadcast it so the
    /// status-bar chip lights up / clears.
    pub fn set_available_update(&mut self, info: Option<UpdateInfo>, cx: &mut Context<Self>) {
        self.available_update = info;
        cx.emit(GlobalEvent::UpdateAvailable);
    }

    /// Current installer download progress as `(downloaded, total)` bytes, or
    /// `None` when not downloading. `total` is always > 0 while downloading.
    pub fn download_progress(&self) -> Option<(u64, u64)> {
        self.download_progress
    }

    /// The same progress as a 0–100 percentage — what the title-bar chip shows.
    pub fn download_percent(&self) -> Option<u8> {
        self.download_progress
            .filter(|(_, total)| *total > 0)
            .map(|(done, total)| (done * 100 / total).min(100) as u8)
    }

    /// Set (or clear with `None`) the download progress and broadcast it so the
    /// update popover's progress bar and the title-bar chip both refresh.
    pub fn set_download_progress(&mut self, progress: Option<(u64, u64)>, cx: &mut Context<Self>) {
        self.download_progress = progress;
        cx.emit(GlobalEvent::UpdateDownloadProgress);
    }

    /// Whether the update chip's popover is open. A click on the chip does
    /// not open it: the click re-checks (the chip spins) and the check's
    /// answer opens it, so the notes it shows are the release's as of now.
    pub fn update_prompt_open(&self) -> bool {
        self.update_prompt_open
    }

    pub fn set_update_prompt_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.update_prompt_open = open;
        cx.emit(GlobalEvent::UpdateAvailable);
    }

    /// Whether an update check (network fetch) is currently running.
    pub fn update_checking(&self) -> bool {
        self.update_checking
    }

    /// Set the "checking for updates" flag and broadcast it so the update chip
    /// can show / clear its loading spinner.
    pub fn set_update_checking(&mut self, checking: bool, cx: &mut Context<Self>) {
        self.update_checking = checking;
        cx.emit(GlobalEvent::UpdateAvailable);
    }

    /// Whether the given server-page group section is collapsed.
    pub fn is_server_group_collapsed(&self, key: &str) -> bool {
        self.collapsed_server_groups
            .as_ref()
            .is_some_and(|groups| groups.iter().any(|g| g == key))
    }

    /// Flip the collapsed state of a server-page group section.
    pub fn toggle_server_group_collapsed(&mut self, key: &str) {
        let groups = self.collapsed_server_groups.get_or_insert_with(Vec::new);
        if let Some(pos) = groups.iter().position(|g| g == key) {
            groups.swap_remove(pos);
        } else {
            groups.push(key.to_string());
        }
    }

    /// The DB to open this server on when the caller has no DB of its own:
    /// the server's pinned `default_db` when configured, else the DB it was
    /// last viewed on (0 if never).
    ///
    /// Every "connect to server X" entry point (sidebar, server card, tray,
    /// palette, multi-search) resolves through here. A DB the user picked
    /// *explicitly* — the status-bar switcher, `--db`, a workspace tab
    /// already bound to one — bypasses it and stands.
    pub fn open_db_for(&self, server_id: &str) -> usize {
        self.open_db_from(server_id, self.last_db.get(server_id).copied().unwrap_or(0))
    }

    /// [`Self::open_db_for`] for a caller that already holds a remembered DB
    /// (the restored session snapshot / a restored tab): the pin still wins,
    /// since it means "always this DB", but an unpinned server keeps the DB
    /// it was left on.
    pub fn open_db_from(&self, server_id: &str, remembered: usize) -> usize {
        get_server(server_id)
            .map(|server| server.resolve_default_db(remembered))
            .unwrap_or(remembered)
    }

    pub fn kv_edit_panel_width(&self) -> Option<Pixels> {
        self.kv_edit_panel_width
    }

    pub fn set_kv_edit_panel_width(&mut self, width: Pixels) {
        self.kv_edit_panel_width = Some(width);
    }

    /// The persisted workspace-tab list — see the `open_tabs` field.
    pub fn open_tabs(&self) -> &[(String, usize)] {
        &self.open_tabs
    }

    pub fn set_open_tabs(&mut self, open_tabs: Vec<(String, usize)>) {
        self.open_tabs = open_tabs;
    }
}

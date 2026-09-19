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

//! What only the desktop app has on its root: the updater, the crash report
//! the previous run left behind, window placement and the multi-database
//! search palette. The browser build has none of it (ADR 9), and this module
//! is how it leaves them out — `mod desktop` is gated once in `root.rs`, and
//! the state lives in one struct so `Zedis` carries one gated field instead
//! of a gate per field, per initialiser and per method (CLAUDE.md, *Desktop
//! first*).

use super::*;
use gpui::Subscription;

/// The desktop-only state of the [`Zedis`] root.
pub(crate) struct DesktopOnly {
    pub(crate) multi_search: Entity<ZedisMultiSearch>,
    /// A newer release found by a check, awaiting its prompt. Consumed in
    /// `render` (which has the `Window` needed to open the dialog).
    pub(crate) pending_update: Option<UpdateInfo>,
    /// The in-flight update check, if any — guards against overlapping checks.
    pub(crate) update_task: Option<Task<()>>,
    /// The in-flight installer download, if any — guards against re-entry.
    pub(crate) download_task: Option<Task<()>>,
    /// The installer is open and this platform needs Zedis gone to finish the
    /// install — prompt to quit. Consumed in `render` (which has the `Window`).
    pub(crate) pending_install_quit: bool,
    /// The crash report the previous run left behind, if it ended in a panic.
    /// Consumed in `render` (which has the `Window` needed for the dialog).
    pub(crate) pending_crash: Option<CrashReport>,
    /// Counts down from the moment the main window stops being the active
    /// one; when it runs out the app is unattended (`pacing::unattended`) and
    /// its heartbeats relax. Dropped — cancelled — when the window comes back.
    idle_task: Option<Task<()>>,
    /// The main window's activation, which is what starts and stops it.
    _activation: Subscription,
}

impl DesktopOnly {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Zedis>) -> Self {
        let activation = cx.observe_window_activation(window, |zedis, window, cx| {
            zedis.on_window_activation(window.is_window_active(), cx);
        });
        Self {
            multi_search: cx.new(|cx| ZedisMultiSearch::new(window, cx)),
            pending_update: None,
            update_task: None,
            download_task: None,
            pending_install_quit: false,
            pending_crash: None,
            idle_task: None,
            _activation: activation,
        }
    }
}

impl Zedis {
    /// The main window became, or stopped being, the active one.
    ///
    /// A window nobody is looking at keeps sending its heartbeat — `INFO`
    /// every two seconds per connected tab, for as long as it stays open. It
    /// costs this machine nothing measurable; it costs a Redis billed per
    /// command tens of thousands of commands a day, and a laptop its idle
    /// network. So after [`pacing::WINDOW_IDLE_AFTER`] out of the front the
    /// app counts as unattended and beats like a background tab. The grace
    /// period is the point: switching to a document, a chat or Zedis's own
    /// Settings window and back changes nothing, and neither does a window
    /// parked on a second screen that is clicked now and then.
    ///
    /// Coming back is immediate: the flag clears and the active tab beats at
    /// once, so the status bar is never looked at stale.
    fn on_window_activation(&mut self, active: bool, cx: &mut Context<Self>) {
        if !active {
            self.desktop.idle_task = Some(cx.spawn(async move |_this, cx| {
                cx.background_executor().timer(pacing::WINDOW_IDLE_AFTER).await;
                pacing::set_unattended(true);
            }));
            return;
        }
        self.desktop.idle_task = None;
        if !pacing::unattended() {
            return;
        }
        pacing::set_unattended(false);
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let state = tab.content.read(cx).server_state();
            state.update(cx, |state, cx| state.resume_heartbeat(cx));
        }
    }

    /// Open whatever the desktop queued for the first frame that has a
    /// `Window`: the crash report, the install-quit prompt, the update prompt.
    pub(super) fn open_desktop_prompts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(report) = self.desktop.pending_crash.take() {
            // Deferred for the same focus reason as the welcome card.
            window.defer(cx, move |window, cx| open_crash_dialog(&report, window, cx));
        }
        // The installer is up and this platform needs Zedis closed to finish —
        // ask (the update dialog has already dismissed itself by now).
        if std::mem::take(&mut self.desktop.pending_install_quit) {
            open_install_quit_dialog(window, cx);
        }
        if let Some(info) = self.desktop.pending_update.take() {
            let weak = cx.entity().downgrade();
            open_update_dialog(info, weak, window, cx);
            // The prompt is on screen now — stop the chip's loading spinner so it
            // spins right up until the dialog appears (no stop-then-wait gap).
            cx.global::<ZedisGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_update_checking(false, cx));
        }
    }

    /// Kick off a background check for a newer release. A `manual` check always
    /// reports its outcome (up-to-date / failure toast) and ignores a skipped
    /// version; the silent startup check stays quiet unless it finds a fresh,
    /// non-skipped update.
    pub(crate) fn check_for_updates(&mut self, manual: bool, then_prompt: bool, cx: &mut Context<Self>) {
        // App Store builds are updated through the App Store; never self-check or
        // self-download (Apple forbids it). Guards every trigger at once.
        if is_app_store_build() {
            return;
        }
        if self.desktop.update_task.is_some() {
            return;
        }
        // Reset the throttle on every attempt so a transient failure doesn't
        // immediately retry on the next launch.
        update_app_state_and_save_quiet(cx, "mark_update_checked", |state, _| state.mark_update_checked());
        // Flag the check so the title-bar chip can show a loading spinner.
        cx.global::<ZedisGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.set_update_checking(true, cx));
        let include_prerelease = cx.global::<ZedisGlobalStore>().read(cx).update_prerelease();
        self.desktop.update_task = Some(cx.spawn(async move |handle, cx| {
            // `fetch_latest_release` is blocking (ureq) — keep it off the UI thread.
            let result = cx
                .background_spawn(async move { fetch_latest_release(include_prerelease) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.desktop.update_task = None;
                // For a chip click we keep the spinner running until the dialog
                // actually opens (cleared in `render` after `open_update_dialog`),
                // so there's no gap between "loading stops" and the prompt
                // appearing. Every other outcome clears it right here.
                let mut opened_prompt = false;
                match result {
                    Ok(Some(info)) => {
                        let skipped = cx.global::<ZedisGlobalStore>().read(cx).update_skipped(&info.version);
                        if manual || !skipped {
                            let version = info.version.clone();
                            // Light the persistent title-bar chip...
                            cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                state.set_available_update(Some(info.clone()), cx);
                            });
                            if then_prompt {
                                // Chip click: open the download/skip dialog with
                                // the freshly fetched info instead of toasting.
                                this.desktop.pending_update = Some(info);
                                opened_prompt = true;
                            } else {
                                // ...and fire a one-time toast so the user notices
                                // it, spelling out that updating is manual.
                                this.pending_notification = Some(Notification::info(format!(
                                    "{}: v{version}\n{}",
                                    i18n_update(cx, "found"),
                                    i18n_update(cx, "manual_hint")
                                )));
                            }
                        }
                    }
                    Ok(None) => {
                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                            state.set_available_update(None, cx);
                        });
                        if manual {
                            this.pending_notification = Some(Notification::success(i18n_update(cx, "up_to_date")));
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "update check failed");
                        if manual {
                            this.pending_notification = Some(Notification::error(i18n_update(cx, "check_failed")));
                        }
                    }
                }
                // When a prompt is opening, the spinner is cleared in `render`
                // (right after the dialog opens) to avoid a stop-then-wait gap.
                if !opened_prompt {
                    cx.global::<ZedisGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.set_update_checking(false, cx));
                }
                cx.notify();
            });
        }));
    }

    /// Act on the user's "Download" choice. With a verified manifest asset,
    /// download + checksum-verify it in the background and hand it to the OS
    /// installer; without one (API fallback / missing asset) just open the
    /// release page. A failed download falls back to the page too.
    pub(crate) fn start_download(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        let Some(asset) = info.asset.clone() else {
            // No verified asset for this os/arch (manifest missing → API
            // fallback, or no matching build): nothing to download in-app, so
            // hand off to the browser. Logged because it otherwise looks
            // identical to "the Download button did nothing".
            info!(version = %info.version, url = %info.page_url, "update: no asset for this platform, opening release page");
            cx.open_url(&info.page_url);
            return;
        };
        if self.desktop.download_task.is_some() {
            info!("update: download already in progress, ignoring");
            return;
        }
        let page_url = info.page_url.clone();
        let version = info.version.clone();
        info!(
            version = %version,
            asset = %asset.name,
            size = asset.size,
            "update: download started"
        );

        // Publish 0% *synchronously*, before any await: connecting (DNS, TLS,
        // the GitHub → CDN redirect) takes a second or two during which no byte
        // has arrived, and the first `on_progress` can only fire after that. If
        // the UI waited for it, the dialog would keep showing the Download
        // button and the chip its version — looking like the click did nothing,
        // which is exactly what made it get clicked twice. Publishing here
        // swaps both to the progress state on the click itself.
        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
            state.set_download_progress(Some((0, asset.size)), cx);
            // A fresh download voids any earlier "installed, restart?" state.
            state.set_update_installed(false, cx);
        });
        cx.notify();

        // Progress is produced on the background thread and ferried to the UI
        // through a channel as `(downloaded, total)` bytes; this foreground
        // drainer publishes it to the global store, which the update dialog
        // (progress bar) and the title-bar chip (percentage) both read.
        let (tx, rx) = channel::unbounded::<(u64, u64)>();
        cx.spawn(async move |_, cx| {
            while let Ok(progress) = rx.recv().await {
                cx.update(|cx| {
                    cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                        state.set_download_progress(Some(progress), cx);
                    });
                });
            }
            // The sender is dropped once the download settles, so the loop ends
            // with every queued tick already applied. Clearing *here* (rather
            // than in the completion handler, which races the drainer) means a
            // late tick can't land after the clear and freeze the chip at a
            // stale percent. This is also what dismisses the dialog.
            cx.update(|cx| {
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.set_download_progress(None, cx);
                });
            });
        })
        .detach();

        let log_name = asset.name.clone();
        self.desktop.download_task = Some(cx.spawn(async move |handle, cx| {
            // Networking + checksum are blocking — keep them off the UI thread.
            let result = cx
                .background_spawn(async move {
                    let mut last_pct = u8::MAX;
                    let mut last_logged_decile = u8::MAX;
                    let outcome = download_and_verify(&asset, |done, total| {
                        if total == 0 {
                            return;
                        }
                        // Throttle to integer-percent changes (≤101 updates).
                        let pct = ((done * 100 / total).min(100)) as u8;
                        if pct == last_pct {
                            return;
                        }
                        last_pct = pct;
                        // Log every 10% so a slow or stalled download is
                        // diagnosable from the log alone, without spamming it
                        // with a line per percent.
                        let decile = pct / 10;
                        if decile != last_logged_decile {
                            last_logged_decile = decile;
                            info!(asset = %log_name, pct, done, total, "update: download progress");
                        }
                        let _ = tx.try_send((done, total));
                    })
                    .and_then(|path| install_update(&path));
                    // Drop the sender so the drainer task ends.
                    drop(tx);
                    outcome
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.desktop.download_task = None;
                // The progress is cleared by the drainer once it has applied
                // every queued tick (see above) — clearing it here too would
                // race it and could leave the chip stuck at a stale percent.
                match result {
                    // macOS in-place install landed: the bundle on disk is
                    // already the new version. Flag it on the store — the
                    // update dialog (still open, showing the progress bar)
                    // swaps itself to the Restart / Later row instead of
                    // closing. A separate restart dialog is NOT opened
                    // here: the update dialog's deferred self-close targets
                    // the topmost dialog and would eat it.
                    #[cfg(target_os = "macos")]
                    Ok(Delivery::Replaced) => {
                        info!(version = %version, "update: installed in place, restart offered");
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "installed_done")));
                        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                            state.set_update_installed(true, cx);
                        });
                    }
                    Ok(Delivery::HandedToOs) => {
                        info!(version = %version, "update: download finished, installer handed to the OS");
                        this.pending_notification = Some(Notification::success(i18n_update(cx, "download_done")));
                        // macOS / Windows: the installer can't replace a running
                        // Zedis, so offer to quit.
                        if installer_requires_quit() {
                            this.desktop.pending_install_quit = true;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "update download failed");
                        this.pending_notification = Some(Notification::error(i18n_update(cx, "download_failed")));
                        // Fall back to the release page so the user can still get it.
                        cx.open_url(&page_url);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Debounce, then write the placement into `zedis.toml`.
    pub(super) fn save_window_placement(
        &mut self,
        new_bounds: Bounds<Pixels>,
        display: Option<(String, Point<Pixels>)>,
        maximized: bool,
        cx: &mut Context<Self>,
    ) {
        let store = cx.global::<ZedisGlobalStore>().clone();
        // Anchor the placement to the current display (origin relative to it) so
        // it survives monitor rearrangement; absolute `bounds` stays as fallback.
        let placement = display.map(|(display_uuid, screen_origin)| WindowPlacement {
            display_uuid,
            bounds: new_bounds - screen_origin,
            maximized,
        });
        let task = cx.spawn(async move |_, cx| {
            // wait 500ms
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;

            // Snapshot *after* the quiet window, never before: `save_app_state`
            // rewrites the whole TOML, so a clone taken up front would restore
            // every field to its pre-wait value. The first render always lands
            // here (`last_bounds` starts at zero), and its stale snapshot used
            // to revert the startup update-check stamp ~500ms after it was
            // written — freezing `last_update_check` and re-checking on every
            // launch. Same reason `apply_and_save` clones after its own wait.
            let value = store.update(cx, move |state, cx| {
                // The maximized rectangle is the display's, not the window's.
                if !maximized {
                    state.set_bounds(new_bounds);
                }
                if let Some(p) = placement {
                    state.upsert_window_placement(p);
                }
                cx.notify();
                state.clone()
            });

            cx.background_spawn(async move {
                if let Err(e) = save_app_state(&value) {
                    error!(error = %e, "save window bounds fail",);
                } else {
                    info!(bounds = ?new_bounds, "save window bounds success");
                }
            })
            .await;
        });
        self.save_task = Some(task);
    }

    /// Toggle the multi-database search palette (⌘⇧F). Global handler so
    /// it works regardless of focus, matching the command palette.
    pub fn toggle_multi_search(&mut self, cx: &mut Context<Self>) {
        self.desktop.multi_search.update(cx, |palette, cx| palette.toggle(cx));
    }
}

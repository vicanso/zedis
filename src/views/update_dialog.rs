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

//! Action area of the "update available" dialog: the Skip / Download buttons
//! before a download, the live progress bar during one — and, after a macOS
//! in-place install, the Restart / Later row. That last state lives *here*
//! rather than in a second dialog on purpose: this dialog dismisses itself
//! with a deferred `close_dialog`, which closes the **topmost** dialog — a
//! separate restart dialog opened in the same frame would be the topmost and
//! get eaten by that close (it happened).
//!
//! It is a view (not a plain closure) because the progress has to be *live*:
//! `ZedisDialog::child` / `footer_child` take a `Fn() -> impl IntoElement` with
//! no `cx`, so they cannot read the store. As an entity this subscribes to
//! `GlobalEvent::UpdateDownloadProgress` and repaints itself in place, so the
//! bar advances while the dialog stays up.
//!
//! It goes in the dialog **footer**, not at the end of the body: the body is the
//! dialog's scroll container, so a long changelog would push the buttons below
//! the fold (they were unreachable when this lived in the body). The footer is a
//! sibling of that container and always visible.

use crate::states::{GlobalEvent, ZedisGlobalStore, i18n_update};
use gpui::{App, Subscription, Window, prelude::*};
use gpui_component::{
    ActiveTheme, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    progress::Progress,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use std::rc::Rc;
use tracing::debug;

/// Action handed to the dialog by `main.rs` (start the download / skip the
/// version). Both need `&mut App` to reach the store and the root view.
pub type DialogCallback = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct ZedisUpdateDialog {
    /// Starts the download (`Zedis::start_download`); the dialog stays open and
    /// this row swaps itself for the progress bar.
    on_download: DialogCallback,
    /// Records the skipped version and clears the update chip.
    on_skip: DialogCallback,
    /// Latched once the download starts, so the render that sees progress go
    /// back to `None` knows the download *ended* (rather than never having
    /// begun) and can dismiss the dialog.
    was_downloading: bool,
    _subscriptions: Vec<Subscription>,
}

impl ZedisUpdateDialog {
    pub fn new(on_download: DialogCallback, on_skip: DialogCallback, cx: &mut Context<Self>) -> Self {
        // Repaint on every progress tick — this is the whole point of being a
        // view: the dialog's own render closure can't see the store.
        let global_state = cx.global::<ZedisGlobalStore>().state();
        let subscription = cx.subscribe(&global_state, |_this, _state, event, cx| {
            if matches!(event, GlobalEvent::UpdateDownloadProgress) {
                cx.notify();
            }
        });

        Self {
            on_download,
            on_skip,
            was_downloading: false,
            _subscriptions: vec![subscription],
        }
    }

    /// Before the download starts: Skip / Download.
    fn render_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on_download = self.on_download.clone();
        let on_skip = self.on_skip.clone();
        h_flex()
            .w_full()
            .justify_end()
            .gap_2()
            .child(
                Button::new("zedis-update-skip")
                    .outline()
                    .label(i18n_update(cx, "skip_version"))
                    .on_click(move |_, window, cx| {
                        on_skip(window, cx);
                        window.close_dialog(cx);
                    }),
            )
            .child(
                Button::new("zedis-update-download")
                    .primary()
                    .label(i18n_update(cx, "download"))
                    // Deliberately does NOT close the dialog: this row is
                    // replaced by the progress bar on the next render.
                    .on_click(move |_, window, cx| on_download(window, cx)),
            )
    }

    /// After a macOS in-place install: the bundle on disk is already the
    /// new version, the only step left is running it. Restart hands the
    /// relaunch to a detached shell that reopens the bundle once this
    /// process exits; Later is fine too — the next manual launch is the
    /// new version either way.
    fn render_restart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_2()
            .child(
                Label::new(i18n_update(cx, "restart_body"))
                    .text_sm()
                    .text_color(muted)
                    .whitespace_normal(),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("zedis-update-restart-later")
                            .outline()
                            .label(i18n_update(cx, "restart_later"))
                            .on_click(|_, window, cx| {
                                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                                    state.set_update_installed(false, cx);
                                });
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("zedis-update-restart-now")
                            .primary()
                            .label(i18n_update(cx, "restart_now"))
                            .on_click(|_, _window, cx| {
                                // App state is flushed by `flush_app_state_on_quit`,
                                // which gpui waits on during shutdown.
                                debug!("update: restarting into the freshly installed bundle");
                                #[cfg(target_os = "macos")]
                                crate::helpers::relaunch();
                                cx.quit();
                            }),
                    ),
            )
    }

    /// During the download: bar + percent + transferred bytes. Replaces the
    /// buttons, so there is nothing to double-click.
    fn render_progress(&self, done: u64, total: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_1p5()
            .child(Progress::new("zedis-update-progress").value(pct as f32))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new(format!("{} · {pct}%", i18n_update(cx, "downloading"))).text_sm())
                    .child(
                        Label::new(format!(
                            "{} / {}",
                            format_size(done, DECIMAL),
                            format_size(total, DECIMAL)
                        ))
                        .text_sm()
                        .text_color(muted),
                    ),
            )
    }
}

impl Render for ZedisUpdateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (progress, installed) = {
            let store = cx.global::<ZedisGlobalStore>().read(cx);
            (store.download_progress(), store.update_installed())
        };
        if progress.is_some() {
            self.was_downloading = true;
        } else if self.was_downloading && !installed {
            // Progress cleared after a download ran: the installer was handed
            // to the OS, or the download failed (the toast says so and the
            // release page opens). Either way the dialog has served its
            // purpose — dismiss it. Deferred: closing mutates the dialog
            // layer we are rendering into. An in-place install instead keeps
            // the dialog up and swaps in the restart row below.
            self.was_downloading = false;
            debug!("update dialog: download settled, closing");
            cx.defer_in(window, |_this, window, cx| window.close_dialog(cx));
        }

        match progress {
            Some((done, total)) => self.render_progress(done, total, cx).into_any_element(),
            None if installed => self.render_restart(cx).into_any_element(),
            None => self.render_actions(cx).into_any_element(),
        }
    }
}

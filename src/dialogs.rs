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

//! App-level dialogs opened from the root: crash report, first-run
//! welcome, SSH host-key confirmation, update / install prompts.

use crate::connection::{HostKeyPrompt, set_host_key_approver};
use crate::helpers::{
    ConfigRecovery, CrashReport, UpdateInfo, focus_installer_ui, get_mono_font_family, humanize_keystroke, logs_dir,
};
use crate::root::Zedis;
use crate::states::{
    ZedisGlobalStore, i18n_crash, i18n_hints, i18n_servers, i18n_update, update_app_state_and_save_quiet,
};
use crate::views::{DialogCallback, ZedisUpdateDialog};
use gpui::{App, SharedString, WeakEntity, Window, div, prelude::*, px, rems};
// Only the custom-drawn title bar path uses this (Linux/FreeBSD keep
// server-side decorations — see the cfg at the open_window call).
use gpui_component::{
    ActiveTheme, IconName,
    label::Label,
    scroll::ScrollableElement,
    text::{TextView, TextViewStyle},
    v_flex,
};
use rust_i18n::t;
use std::sync::Arc;
use std::{cell::Cell, rc::Rc, time::Duration};
use tracing::{error, info};
use zedis_ui::ZedisDialog;

pub(crate) fn release_notes_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, _base| match level {
            1 => px(18.),
            2 => px(16.),
            3 => px(15.),
            _ => px(14.),
        })
}

/// First-launch onboarding: a one-shot card walking through the three steps to
/// get productive. Only opened when no server is configured yet, and never
/// twice — `HINT_WELCOME` is dismissed the moment startup decides to show it.
/// Localized one-line account of a startup config recovery: which file, what
/// happened, and where the damaged copy was kept (the full path, so the user
/// can hand it over or inspect it).
pub(crate) fn config_recovery_message(recovery: &ConfigRecovery, cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let file = recovery
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let corrupt = recovery.corrupt_path().display().to_string();
    let key = match recovery {
        ConfigRecovery::RestoredFromBackup { .. } => "common.config_restored_from_backup",
        ConfigRecovery::Reset { .. } => "common.config_reset",
    };
    t!(key, file = file, corrupt = corrupt, locale = locale)
        .to_string()
        .into()
}

/// "Zedis closed unexpectedly last time": the panic message and where the full
/// report (with backtrace) was written, plus a one-click way to the folder so it
/// can be attached to an issue.
pub(crate) fn open_crash_dialog(report: &CrashReport, window: &mut Window, cx: &mut App) {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let body = i18n_crash(cx, "body");
    let summary: SharedString = report.summary.clone().into();
    let saved: SharedString = t!(
        "crash.report_saved",
        path = report.path.display().to_string(),
        locale = &locale
    )
    .to_string()
    .into();
    let muted = cx.theme().muted_foreground;
    let mono = get_mono_font_family();
    ZedisDialog::new(i18n_crash(cx, "title"))
        .icon(IconName::TriangleAlert)
        .child(move || {
            v_flex()
                .gap_2()
                .child(body.clone())
                .when(!summary.is_empty(), |this| {
                    this.child(div().font_family(mono.clone()).text_sm().child(summary.clone()))
                })
                .child(div().text_xs().text_color(muted).child(saved.clone()))
        })
        .ok_text(i18n_crash(cx, "open_logs"))
        .cancel_text(i18n_crash(cx, "dismiss"))
        .on_ok(|_, _window, cx| {
            match logs_dir() {
                Some(logs) => cx.open_with_system(&logs),
                None => error!("failed to resolve logs directory"),
            }
            true
        })
        .open(window, cx);
}

pub(crate) fn open_welcome_dialog(window: &mut Window, cx: &mut App) {
    let intro = i18n_hints(cx, "welcome_intro");
    let steps: [SharedString; 3] = [
        i18n_hints(cx, "welcome_step_connect"),
        i18n_hints(cx, "welcome_step_browse"),
        format!(
            "{} ({})",
            i18n_hints(cx, "welcome_step_palette"),
            humanize_keystroke("secondary-k")
        )
        .into(),
    ];
    ZedisDialog::new(i18n_hints(cx, "welcome_title"))
        .icon(IconName::Info)
        .child(move || v_flex().gap_2().child(intro.clone()).children(steps.iter().cloned()))
        .ok_text(i18n_hints(cx, "welcome_ok"))
        .open(window, cx);
}

/// Offered once the installer is open on a platform that can't install over a
/// running Zedis (macOS / Windows — see `installer_requires_quit`). Quitting is
/// the user's call: an editor may hold unsaved changes, and they may simply want
/// to install later.
/// SSH host keys seen for the first time are confirmed here rather than
/// trusted silently: the connection layer hands the fingerprint over a
/// channel, this foreground drainer opens a dialog on the active window,
/// and the answer travels back. No window to ask in, or no answer within
/// two minutes, declines — the connect fails with a message that says so
/// and can simply be retried.
pub(crate) fn install_host_key_prompt(cx: &mut App) {
    let (tx, rx) = smol::channel::unbounded::<(HostKeyPrompt, smol::channel::Sender<bool>)>();
    set_host_key_approver(Arc::new(move |prompt| {
        let tx = tx.clone();
        Box::pin(async move {
            let (answer_tx, answer_rx) = smol::channel::bounded::<bool>(1);
            if tx.send((prompt, answer_tx)).await.is_err() {
                return false;
            }
            let wait = async { answer_rx.recv().await.unwrap_or(false) };
            let give_up = async {
                smol::Timer::after(Duration::from_secs(120)).await;
                false
            };
            smol::future::or(wait, give_up).await
        })
    }));
    cx.spawn(async move |cx| {
        while let Ok((prompt, answer)) = rx.recv().await {
            let opened = cx.update(|cx| {
                let Some(window) = cx.active_window() else {
                    return false;
                };
                window
                    .update(cx, |_, window, cx| {
                        open_host_key_dialog(prompt, answer.clone(), window, cx)
                    })
                    .is_ok()
            });
            if !opened {
                // Nothing to ask in: decline rather than leave the connect
                // hanging until the timeout.
                let _ = answer.try_send(false);
            }
        }
    })
    .detach();
}

pub(crate) fn open_host_key_dialog(
    prompt: HostKeyPrompt,
    answer: smol::channel::Sender<bool>,
    window: &mut Window,
    cx: &mut App,
) {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let body = t!(
        "servers.ssh_hostkey_body",
        host = &prompt.host,
        port = prompt.port,
        algorithm = &prompt.algorithm,
        fingerprint = &prompt.fingerprint,
        locale = &locale
    )
    .to_string();
    let accept = answer.clone();
    ZedisDialog::new(i18n_servers(cx, "ssh_hostkey_title"))
        .icon(IconName::Info)
        .message(body)
        .ok_text(i18n_servers(cx, "ssh_hostkey_accept"))
        .cancel_text(i18n_servers(cx, "ssh_hostkey_reject"))
        .overlay_closable(false)
        .on_ok(move |_, _window, _cx| {
            let _ = accept.try_send(true);
            true
        })
        // Fires after OK too; the bounded(1) channel then already holds
        // `true`, so this `false` is dropped.
        .on_close(move |_, _window, _cx| {
            let _ = answer.try_send(false);
        })
        .open(window, cx);
}

pub(crate) fn open_install_quit_dialog(window: &mut Window, cx: &mut App) {
    ZedisDialog::new(i18n_update(cx, "quit_to_install_title"))
        .icon(IconName::Info)
        .message(i18n_update(cx, "quit_to_install_body"))
        .ok_text(i18n_update(cx, "quit_to_install_now"))
        .cancel_text(i18n_update(cx, "quit_to_install_later"))
        .on_ok(|_, _window, cx| {
            // The app state is flushed by `flush_app_state_on_quit`, which gpui
            // waits on during shutdown — nothing to do for it here.
            //
            // Quitting hands focus to whatever ran before Zedis, not to the
            // installer — pull its window forward first, or it ends up buried.
            focus_installer_ui();
            info!("update: quitting so the installer can replace the app");
            cx.quit();
            true
        })
        .open(window, cx);
}

pub(crate) fn open_update_dialog(info: UpdateInfo, zedis: WeakEntity<Zedis>, window: &mut Window, cx: &mut App) {
    // The notes area scrolls, so this cap only guards layout work against a
    // pathologically long release body.
    const MAX_NOTES: usize = 5000;
    let title = format!("{} {}", i18n_update(cx, "available_title"), info.version);
    let mut notes = info.notes.clone();
    if notes.chars().count() > MAX_NOTES {
        notes = notes.chars().take(MAX_NOTES).collect::<String>();
        notes.push('…');
    }
    let update_hint = i18n_update(cx, "update_body");
    let version_line = format!("{} → {}", info.current, info.version);
    let skip_version = info.version.clone();
    let download_info = info.clone();
    // Shared flag so the Download path suppresses the skip-on-close below (the
    // dialog's own × still records a skip when the user never started one).
    let downloaded = Rc::new(Cell::new(false));
    let on_download_flag = downloaded.clone();

    // Kick off the download and *leave the dialog open* — `ZedisUpdateDialog`
    // watches the progress in the store and swaps its buttons for the bar.
    let on_download: DialogCallback = Rc::new(move |_window, cx| {
        on_download_flag.set(true);
        // Download + verify + open the installer (or open the release page when
        // there's no verified asset) — see `Zedis::start_download`.
        if let Some(view) = zedis.upgrade() {
            view.update(cx, |this, cx| this.start_download(download_info.clone(), cx));
        }
    });
    let skip = skip_version.clone();
    let on_skip: DialogCallback = Rc::new(move |_window, cx| {
        info!(version = %skip, "update: version skipped by user");
        let version = skip.clone();
        update_app_state_and_save_quiet(cx, "skip_update_version", move |state, _| {
            state.set_skipped_version(version.clone());
        });
        cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
            state.set_available_update(None, cx);
        });
    });

    // The action row is a *view* in the dialog footer, not part of the body:
    // the body is the dialog's scroll container, so a long changelog would push
    // the buttons below the fold. As a footer view it stays put and can swap
    // itself for the live progress bar (see `ZedisUpdateDialog`).
    let actions = cx.new(|cx| ZedisUpdateDialog::new(on_download.clone(), on_skip.clone(), cx));
    ZedisDialog::new(title)
        .child(move || {
            let mut body = v_flex()
                .gap_2()
                .child(Label::new(update_hint.clone()))
                .child(Label::new(version_line.clone()));
            // Render the changelog as Markdown (it comes straight from the
            // GitHub release body) inside a capped, scrollable area.
            //
            // Not `max_h`: `Scrollable` copies the caller's size styles onto
            // its wrapper but the inner content keeps them too, and while its
            // forced `h_auto` overrides a fixed `h`, nothing resets `max_h` —
            // so the content itself gets clamped and there is never anything
            // to scroll. A definite `h` viewport scrolls correctly; short
            // bodies render inline so the dialog stays compact.
            if !notes.trim().is_empty() {
                let text = TextView::markdown("update-release-notes", notes.clone()).style(release_notes_style());
                let long_notes = notes.lines().count() > 12 || notes.chars().count() > 800;
                body = body.child(if long_notes {
                    div()
                        .w_full()
                        .h(px(280.))
                        .child(text)
                        .overflow_y_scrollbar()
                        .into_any_element()
                } else {
                    div().w_full().child(text).into_any_element()
                });
            }
            body
        })
        .footer_child(move || actions.clone().into_any_element())
        .w(px(520.))
        .overlay_closable(false)
        .on_close(move |_, _window, cx| {
            // Only dismissing without downloading (the × button) records a skip
            // and clears the chip. The Download path sets the flag above, and
            // the dialog then closes itself once the download settles.
            if !downloaded.get() {
                let version = skip_version.clone();
                update_app_state_and_save_quiet(cx, "skip_update_version", move |state, _| {
                    state.set_skipped_version(version.clone());
                });
                cx.global::<ZedisGlobalStore>().clone().update(cx, |state, cx| {
                    state.set_available_update(None, cx);
                });
            }
        })
        .open(window, cx);
}

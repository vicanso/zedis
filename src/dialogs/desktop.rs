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

//! The app-level dialogs the browser has no use for: the crash report, the
//! SSH host-key confirmation.
//!
//! One gated `mod` in `dialogs.rs` instead of a `#[cfg]` on each function and
//! on each import only some of them need — the shape `root/desktop.rs` and
//! `dump_restore/file.rs` already use. Every import below is desktop-only by
//! construction, so none of them needs a gate of its own.

use crate::connection::{HostKeyPrompt, set_host_key_approver};
use crate::helpers::channel;
use crate::helpers::{CrashReport, get_mono_font_family, logs_dir};
use crate::states::{ZedisGlobalStore, i18n_crash, i18n_servers};
use gpui::{App, SharedString, Window, div, prelude::*};
use gpui_kit::component::{ActiveTheme, IconName, v_flex};
use rust_i18n::t;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;
use zedis_ui::ZedisDialog;

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

/// SSH host keys seen for the first time are confirmed here rather than
/// trusted silently: the connection layer hands the fingerprint over a
/// channel, this foreground drainer opens a dialog on the active window,
/// and the answer travels back. No window to ask in, or no answer within
/// two minutes, declines — the connect fails with a message that says so
/// and can simply be retried.
pub(crate) fn install_host_key_prompt(cx: &mut App) {
    let (tx, rx) = channel::unbounded::<(HostKeyPrompt, channel::Sender<bool>)>();
    set_host_key_approver(Arc::new(move |prompt| {
        let tx = tx.clone();
        Box::pin(async move {
            let (answer_tx, answer_rx) = channel::bounded::<bool>(1);
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
    answer: channel::Sender<bool>,
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

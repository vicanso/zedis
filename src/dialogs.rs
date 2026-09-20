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

//! App-level dialogs opened from the root.
//!
//! The ones the browser also has are here; the desktop-only ones (crash
//! report, SSH host key, the updater's prompts) are one gated module away.

#[cfg(not(target_family = "wasm"))]
mod desktop;
#[cfg(not(target_family = "wasm"))]
pub(crate) use desktop::{install_host_key_prompt, open_crash_dialog, open_install_quit_dialog, open_update_dialog};

use crate::helpers::{ConfigRecovery, humanize_keystroke};
use crate::states::{ZedisGlobalStore, i18n_hints};
use gpui::{App, SharedString, Window, prelude::*, px, rems};
use gpui_kit::component::{IconName, text::TextViewStyle, v_flex};
use rust_i18n::t;
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

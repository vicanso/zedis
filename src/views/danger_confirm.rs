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

#[cfg(target_family = "wasm")]
use crate::connection::classify_guarded_script;
use crate::{
    connection::{ConfirmStrictness, DangerKind, RedisServer, WRITE_UNLOCK_SECS, confirm_strictness},
    states::{ZedisGlobalStore, dialog_button_props, i18n_common},
};
use gpui::{App, SharedString, Window};
use rust_i18n::t;
use std::rc::Rc;
use zedis_ui::ZedisDialog;

type ConfirmCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the bridge will ask about `cmd_name` beyond the desktop's rule, so
/// the page can ask first: a refusal from the bridge is an error, not a
/// dialog, and a caller that sent nothing to confirm with could only show
/// it. The desktop has no bridge and asks nothing more.
#[cfg(not(target_family = "wasm"))]
pub fn bridge_danger(_server: &RedisServer, _cmd_name: &str) -> Option<DangerKind> {
    None
}
/// In the browser: a script to a guarded entry (`classify_guarded_script`).
#[cfg(target_family = "wasm")]
pub fn bridge_danger(server: &RedisServer, cmd_name: &str) -> Option<DangerKind> {
    classify_guarded_script(server, cmd_name)
}

/// Open a confirm dialog before running a dangerous Redis command.
///
/// `on_ok` is invoked exactly once if the user confirms; nothing is invoked on
/// cancel/dismiss. The dialog wording is driven by `kind` and the server's
/// tag preset, so a tagged "PROD" connection gets stronger language than a
/// `DEV` one without coupling this helper to specific call-sites.
pub fn confirm_dangerous_command<F>(
    server: &RedisServer,
    kind: &DangerKind,
    line: Option<&str>,
    window: &mut Window,
    cx: &mut App,
    on_ok: F,
) where
    F: Fn(&mut Window, &mut App) + 'static,
{
    let strictness = confirm_strictness(server, kind);
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let server_name = server.name.clone();
    let tag = server.tag_label().unwrap_or_default().to_string();

    let title = title_for(kind, cx);
    let message = compose_message(kind, line, &server_name, &tag, strictness, &locale);

    let on_ok_rc: ConfirmCallback = Rc::new(on_ok);
    let confirm_label = i18n_common(cx, "confirm");
    let cancel_label = i18n_common(cx, "cancel");
    let dialog = ZedisDialog::new_alert(title, message);
    // What cannot be taken back is drawn as such, and Return declines it.
    let dialog = if kind.is_destructive() {
        dialog.danger().ok_text(confirm_label).cancel_text(cancel_label)
    } else {
        dialog.button_props(dialog_button_props(cx).ok_text(confirm_label).cancel_text(cancel_label))
    };
    dialog
        .on_ok(move |_, window, cx| {
            (on_ok_rc)(window, cx);
            true
        })
        .open(window, cx);
}

fn title_for(kind: &DangerKind, cx: &App) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
    let key = format!("{}_title", kind.i18n_key());
    let value = t!(&key, locale = &locale).to_string();
    if value == key {
        // Fallback if i18n is missing — better an English title than a raw key.
        return t!("danger.generic_title", locale = &locale).to_string().into();
    }
    value.into()
}

fn compose_message(
    kind: &DangerKind,
    line: Option<&str>,
    server_name: &str,
    tag: &str,
    strictness: ConfirmStrictness,
    locale: &str,
) -> SharedString {
    let target = if tag.is_empty() {
        server_name.to_string()
    } else {
        format!("{server_name} [{tag}]")
    };

    let body_key = format!("{}_body", kind.i18n_key());
    // Every placeholder any body uses goes in, each body taking its own:
    // `minutes` is the write lock's, `count` the batch delete's. The key
    // exists in every locale, so the fallback below never ran for a batch
    // delete, and without `count` here its dialog read "%{count} keys".
    let minutes = (WRITE_UNLOCK_SECS / 60).to_string();
    let count = match kind {
        DangerKind::BatchDelete { count } => count.to_string(),
        _ => String::new(),
    };
    let body_raw = t!(
        &body_key,
        target = &target,
        minutes = &minutes,
        count = &count,
        locale = locale
    )
    .to_string();
    let body = if body_raw == body_key {
        match kind {
            DangerKind::BatchDelete { count } => t!(
                "danger.batch_delete_body",
                target = &target,
                count = count,
                locale = locale
            )
            .to_string(),
            _ => t!("danger.generic_body", target = &target, locale = locale).to_string(),
        }
    } else {
        body_raw
    };

    let mut parts: Vec<String> = vec![body];
    if let Some(cmd) = line {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            parts.push(format!("> {trimmed}"));
        }
    }
    if let ConfirmStrictness::TypeName = strictness {
        // We do not yet require the user to retype the name, but we surface
        // the heightened severity in the body so the wording matches a
        // production-grade chip.
        parts.push(t!("danger.high_risk_warning", locale = locale).to_string());
    }
    parts.join("\n\n").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_of_a_body_is_filled() {
        for locale in ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"] {
            let body = compose_message(
                &DangerKind::BatchDelete { count: 60 },
                None,
                "prod",
                "",
                ConfirmStrictness::Click,
                locale,
            );
            assert!(!body.contains("%{"), "{locale}: {body}");
            assert!(body.contains("60"), "{locale}: {body}");
        }
    }

    /// The bridge's script question is the browser's alone: the desktop
    /// terminal and function editor run a script on production unasked, as
    /// they always have.
    #[test]
    fn the_desktop_asks_nothing_the_bridge_adds() {
        let production = RedisServer {
            name: "production".to_string(),
            tag_color: Some("red".to_string()),
            ..Default::default()
        };
        assert_eq!(bridge_danger(&production, "EVAL"), None);
        assert_eq!(bridge_danger(&production, "FCALL"), None);
    }

    #[test]
    fn a_script_question_has_its_own_words() {
        for locale in ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"] {
            let body = compose_message(&DangerKind::Script, None, "prod", "", ConfirmStrictness::Click, locale);
            assert!(!body.contains("%{") && body.contains("prod"), "{locale}: {body}");
            assert_ne!(
                body,
                compose_message(
                    &DangerKind::GenericWrite,
                    None,
                    "prod",
                    "",
                    ConfirmStrictness::Click,
                    locale
                ),
                "{locale}"
            );
        }
    }
}

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

use crate::{
    connection::{ConfirmStrictness, DangerKind, RedisServer, confirm_strictness},
    states::{ZedisGlobalStore, dialog_button_props, i18n_common},
};
use gpui::{App, SharedString, Window};
use rust_i18n::t;
use std::rc::Rc;
use zedis_ui::ZedisDialog;

type ConfirmCallback = Rc<dyn Fn(&mut Window, &mut App)>;

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
    let button_props = dialog_button_props(cx).ok_text(confirm_label).cancel_text(cancel_label);

    ZedisDialog::new_alert(title, message)
        .button_props(button_props)
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
    let body_raw = t!(&body_key, target = &target, locale = locale).to_string();
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

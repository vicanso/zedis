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
    states::{ZedisGlobalStore, dialog_button_props, escalate_dangerous_body, i18n_common},
};
use gpui::{App, Context, Entity, SharedString, Subscription, Window, div, prelude::*};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
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
/// cancel/dismiss. The title names what is acted on (the server, and for
/// `FLUSHDB` the database — `db`), the button names the action, and the
/// wording is driven by `kind` and the server's tag preset, so a tagged
/// "PROD" connection gets stronger language than a `DEV` one without
/// coupling this helper to specific call-sites. Where
/// [`confirm_strictness`] says `TypeName` — something destructive, or an
/// unlock, on production — the answer is the server's name typed out, not a
/// click.
pub fn confirm_dangerous_command<F>(
    server: &RedisServer,
    db: usize,
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

    let title = title_for(kind, &server_name, db, &locale);
    let message = compose_message(kind, line, &server_name, &tag, db, strictness, &locale);

    let on_ok_rc: ConfirmCallback = Rc::new(on_ok);
    let confirm_label = confirm_label_for(kind, &locale);
    let cancel_label = i18n_common(cx, "cancel");
    // A name that cannot be typed is not asked for.
    if strictness == ConfirmStrictness::TypeName && !server_name.trim().is_empty() {
        let prompt = t!("danger.type_name_prompt", server = &server_name, locale = &locale).to_string();
        let typed = TypedConfirmSpec {
            title,
            message,
            prompt: prompt.into(),
            expected: server_name.into(),
            confirm_label,
            cancel_label,
            destructive: kind.is_destructive(),
        };
        open_typed_confirm(typed, on_ok_rc, window, cx);
        return;
    }
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

/// Ask before deleting one key: the title names it, the body says whether
/// it can come back, the button says Delete — and Return answers Cancel.
/// `on_ok` runs once on Delete.
pub fn confirm_delete_key<F>(server_id: &str, key: &str, window: &mut Window, cx: &mut App, on_ok: F)
where
    F: Fn(&mut Window, &mut App) + 'static,
{
    let store = cx.global::<ZedisGlobalStore>().read(cx);
    let locale = store.locale().to_string();
    let recycled = store.soft_delete();
    let (title, body) = delete_key_wording(key, recycled, &locale);
    let body = escalate_dangerous_body(cx, server_id, body);
    ZedisDialog::new_alert(title, body)
        .danger()
        .ok_text(i18n_common(cx, "delete"))
        .cancel_text(i18n_common(cx, "cancel"))
        .on_ok(move |_, window, cx| {
            on_ok(window, cx);
            true
        })
        .open(window, cx);
}

/// Longest key a title quotes whole; a longer one is cut in the middle and
/// given in full in the body.
const TITLE_KEY_CHARS: usize = 48;

/// The delete-key dialog's title and body. `recycled` is whether the recycle
/// bin is on: with it the key can come back — unless it is over the bin's
/// size cap, which the body says rather than promise a restore that a large
/// key does not get.
fn delete_key_wording(key: &str, recycled: bool, locale: &str) -> (SharedString, String) {
    let shown = shorten_middle(key, TITLE_KEY_CHARS);
    let title = t!("danger.delete_key_title", key = &shown, locale = locale).to_string();
    let consequence = if recycled {
        t!("danger.delete_key_body_bin", locale = locale).to_string()
    } else {
        t!("danger.delete_key_body", locale = locale).to_string()
    };
    let body = if shown == key {
        consequence
    } else {
        format!("{key}\n\n{consequence}")
    };
    (title.into(), body)
}

/// `text` if it is at most `max` characters, else its start and end around
/// an ellipsis: a key's prefix says what it is and its tail which one.
fn shorten_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let tail = max / 3;
    let head = max - tail - 1;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

fn title_for(kind: &DangerKind, server_name: &str, db: usize, locale: &str) -> SharedString {
    let key = format!("{}_title", kind.i18n_key());
    let db = db.to_string();
    let value = t!(&key, server = server_name, db = &db, locale = locale).to_string();
    if value == key {
        // Fallback if i18n is missing — better an English title than a raw key.
        return t!("danger.generic_title", locale = locale).to_string().into();
    }
    value.into()
}

/// The button that goes ahead says what it does — "Flush all", "Unlock",
/// "Run" — rather than "Confirm".
fn confirm_label_for(kind: &DangerKind, locale: &str) -> SharedString {
    let key = format!("{}_confirm", kind.i18n_key());
    let value = t!(&key, locale = locale).to_string();
    if value == key {
        return t!("common.confirm", locale = locale).to_string().into();
    }
    value.into()
}

/// What a typed confirmation shows and expects.
struct TypedConfirmSpec {
    title: SharedString,
    message: SharedString,
    /// "Type "<name>" to confirm".
    prompt: SharedString,
    /// The server's name, as it has to be typed.
    expected: SharedString,
    confirm_label: SharedString,
    cancel_label: SharedString,
    /// Draw the confirm button in the danger variant.
    destructive: bool,
}

/// Body of a confirmation that is answered by typing the server's name.
///
/// A view, not elements built inline: a dialog body holding an `Input` has
/// to be one (see CLAUDE.md, *A dialog body that holds an `Input` must be a
/// view entity*).
struct TypedConfirm {
    message: SharedString,
    prompt: SharedString,
    expected: SharedString,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl TypedConfirm {
    fn new(spec: &TypedConfirmSpec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(spec.expected.clone()));
        // Every keystroke can change whether the name is the right one, and
        // the footer's button follows this view.
        let subscription = cx.observe(&input, |_, _, cx| cx.notify());
        Self {
            message: spec.message.clone(),
            prompt: spec.prompt.clone(),
            expected: spec.expected.clone(),
            input,
            _subscription: subscription,
        }
    }

    /// Whether what is typed is the server's name.
    fn matches(&self, cx: &App) -> bool {
        name_matches(self.input.read(cx).value().as_ref(), &self.expected)
    }
}

/// The typed text is the name: exact, except for space around either — a
/// pasted name often brings a trailing one.
fn name_matches(typed: &str, expected: &str) -> bool {
    let typed = typed.trim();
    !typed.is_empty() && typed == expected.trim()
}

impl Render for TypedConfirm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_3().child(div().child(self.message.clone())).child(
            v_flex()
                .gap_1()
                .child(
                    Label::new(self.prompt.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(Input::new(&self.input)),
        )
    }
}

/// Footer of a typed confirmation: Cancel, and a confirm button that stays
/// disabled until the name is typed. Its own view so the button follows the
/// input — a dialog's footer builder has no `cx` to read it with.
struct TypedConfirmFooter {
    confirm: Entity<TypedConfirm>,
    confirm_label: SharedString,
    cancel_label: SharedString,
    destructive: bool,
    on_ok: ConfirmCallback,
    _subscription: Subscription,
}

impl Render for TypedConfirmFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = self.confirm.read(cx).matches(cx);
        let on_ok = self.on_ok.clone();
        let ok = Button::new("danger-typed-ok").label(self.confirm_label.clone());
        let ok = if self.destructive { ok.danger() } else { ok.primary() };
        DialogFooter::new()
            .child(
                Button::new("danger-typed-cancel")
                    .label(self.cancel_label.clone())
                    .outline()
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(ok.disabled(!ready).on_click(move |_, window, cx| {
                on_ok(window, cx);
                window.close_dialog(cx);
            }))
    }
}

fn open_typed_confirm(spec: TypedConfirmSpec, on_ok: ConfirmCallback, window: &mut Window, cx: &mut App) {
    let confirm = cx.new(|cx| TypedConfirm::new(&spec, window, cx));
    let footer = cx.new(|cx| TypedConfirmFooter {
        confirm: confirm.clone(),
        confirm_label: spec.confirm_label.clone(),
        cancel_label: spec.cancel_label.clone(),
        destructive: spec.destructive,
        on_ok: on_ok.clone(),
        _subscription: cx.observe(&confirm, |_, _, cx| cx.notify()),
    });
    let body = confirm.clone();
    let typed = confirm.clone();
    ZedisDialog::new(spec.title)
        .alert()
        .icon(IconName::Info)
        .child(move || body.clone())
        .footer_child(move || footer.clone())
        // Return, from the input: goes ahead once the name is typed — typing
        // it is the deliberate act — and does nothing before.
        .on_ok(move |_, window, cx| {
            if !typed.read(cx).matches(cx) {
                return false;
            }
            on_ok(window, cx);
            true
        })
        .open(window, cx);
    let input = confirm.read(cx).input.clone();
    input.update(cx, |state, cx| state.focus(window, cx));
}

fn compose_message(
    kind: &DangerKind,
    line: Option<&str>,
    server_name: &str,
    tag: &str,
    db: usize,
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
    // `minutes` is the write lock's, `count` the batch delete's, `db` the
    // database flush's. The key
    // exists in every locale, so the fallback below never ran for a batch
    // delete, and without `count` here its dialog read "%{count} keys".
    let minutes = (WRITE_UNLOCK_SECS / 60).to_string();
    let count = match kind {
        DangerKind::BatchDelete { count } => count.to_string(),
        _ => String::new(),
    };
    let db = db.to_string();
    let body_raw = t!(
        &body_key,
        target = &target,
        minutes = &minutes,
        count = &count,
        db = &db,
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
        // The dialog also asks for the server's name; this line says why.
        parts.push(t!("danger.high_risk_warning", locale = locale).to_string());
    }
    parts.join("\n\n").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCALES: [&str; 8] = ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"];

    const KINDS: [DangerKind; 17] = [
        DangerKind::FlushAll,
        DangerKind::FlushDb,
        DangerKind::ConfigSet,
        DangerKind::ConfigResetStat,
        DangerKind::ConfigRewrite,
        DangerKind::Debug,
        DangerKind::Shutdown,
        DangerKind::ScriptFlush,
        DangerKind::FunctionDelete,
        DangerKind::SwapDb,
        DangerKind::ClusterReset,
        DangerKind::Replication,
        DangerKind::KeysGlob,
        DangerKind::BatchDelete { count: 2 },
        DangerKind::GenericWrite,
        DangerKind::WriteLocked,
        DangerKind::Script,
    ];

    #[test]
    fn a_title_names_what_is_acted_on() {
        for locale in LOCALES {
            for kind in [DangerKind::FlushAll, DangerKind::FlushDb, DangerKind::WriteLocked] {
                let title = title_for(&kind, "orders", 3, locale);
                assert!(title.contains("orders") && !title.contains("%{"), "{locale}: {title}");
            }
            let flush_db = title_for(&DangerKind::FlushDb, "orders", 3, locale);
            assert!(flush_db.contains('3'), "{locale}: {flush_db}");
            for kind in KINDS {
                let title = title_for(&kind, "orders", 3, locale);
                assert!(!title.contains("%{") && !title.contains("danger."), "{locale}: {title}");
            }
        }
    }

    #[test]
    fn the_button_that_goes_ahead_names_the_action() {
        for locale in LOCALES {
            let confirm = t!("common.confirm", locale = locale).to_string();
            for kind in KINDS {
                let label = confirm_label_for(&kind, locale);
                assert!(
                    !label.is_empty() && label.as_ref() != confirm && !label.contains("danger."),
                    "{locale} {kind:?}: {label}"
                );
            }
        }
    }

    #[test]
    fn a_delete_says_whether_the_key_can_come_back() {
        let (title, body) = delete_key_wording("user:1", false, "en");
        assert_eq!(title.as_ref(), "Delete \"user:1\"?");
        assert_eq!(body, "This cannot be undone.");
        for locale in LOCALES {
            let (title, kept) = delete_key_wording("user:1", true, locale);
            let (_, gone) = delete_key_wording("user:1", false, locale);
            assert!(title.contains("user:1") && !title.contains("%{"), "{locale}: {title}");
            // The bin's promise carries both of its limits: the day, and the megabyte.
            assert!(
                kept.contains("24") && kept.contains('1') && kept != gone,
                "{locale}: {kept}"
            );
        }
    }

    #[test]
    fn a_long_key_is_cut_in_the_title_and_whole_in_the_body() {
        let key = format!("{}:{}", "a".repeat(40), "b".repeat(60));
        let (title, body) = delete_key_wording(&key, false, "en");
        assert!(!title.contains(&key) && title.contains('…'), "{title}");
        assert!(title.chars().count() < key.chars().count());
        assert!(body.starts_with(&key), "{body}");
        // Whole characters, wherever the cut falls.
        assert_eq!(shorten_middle(&"键".repeat(60), 48).chars().count(), 48);
        assert_eq!(shorten_middle("short", 48), "short");
    }

    #[test]
    fn only_the_server_s_name_confirms() {
        assert!(name_matches("prod-orders", "prod-orders"));
        assert!(name_matches(" prod-orders ", "prod-orders"));
        assert!(!name_matches("prod-order", "prod-orders"));
        assert!(!name_matches("PROD-ORDERS", "prod-orders"));
        assert!(!name_matches("", "prod-orders"));
        assert!(!name_matches("", ""));
    }

    #[test]
    fn every_placeholder_of_a_body_is_filled() {
        for locale in ["en", "zh", "de", "es", "fr", "ja", "pt", "ru"] {
            let body = compose_message(
                &DangerKind::BatchDelete { count: 60 },
                None,
                "prod",
                "",
                0,
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
            let body = compose_message(
                &DangerKind::Script,
                None,
                "prod",
                "",
                0,
                ConfirmStrictness::Click,
                locale,
            );
            assert!(!body.contains("%{") && body.contains("prod"), "{locale}: {body}");
            assert_ne!(
                body,
                compose_message(
                    &DangerKind::GenericWrite,
                    None,
                    "prod",
                    "",
                    0,
                    ConfirmStrictness::Click,
                    locale
                ),
                "{locale}"
            );
        }
    }
}

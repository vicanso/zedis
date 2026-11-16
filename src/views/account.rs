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

//! The account page of the web version (ADR 17, amended 2026-10-18): one's
//! own password and MCP tokens, and for an administrator the accounts, the
//! invitations, the requests for an account where the bridge takes them and
//! the audit — a dialog over the page, like the browser's Settings, with its
//! sections down the left.
//!
//! Not `cfg`-gated: it compiles on the desktop as well, where nothing opens
//! it — *Account…* is drawn only while `signed_in_to_bridge()`, and
//! [`open_account`] needs the bridge's account requests, which only the
//! browser entry point installs. The view is handed them rather than reading
//! the global, so a test gives it a fake bridge.

mod admin;
mod audit;
mod handover;
mod invites;
mod online;
mod password;
mod signups;
mod tokens;

use crate::connection::{
    BridgeAccount, BridgeAccountError, BridgeAccountErrorKind, BridgeAccounts, BridgeOwnAccount, BridgeRole,
    BridgeToken, bridge_accounts,
};
use crate::helpers::{format_unix_secs, get_mono_font_family};
use crate::states::{ZedisGlobalStore, i18n_account, i18n_common};
use admin::AdminForm;
use audit::AuditForm;
use gpui::{AnyElement, App, Entity, FocusHandle, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    h_flex,
    label::Label,
    list::ListItem,
    scroll::ScrollableElement,
    v_flex,
};
use invites::InvitesForm;
use online::OnlineForm;
use password::PasswordForm;
use rust_i18n::t;
use signups::SignupsForm;
use std::sync::Arc;
use tokens::TokenForm;
use zedis_ui::ZedisDialog;

const ACCOUNT_DIALOG_WIDTH: f32 = 880.;
const ACCOUNT_DIALOG_MAX_HEIGHT: f32 = 600.;
const ACCOUNT_DIALOG_MIN_HEIGHT: f32 = 320.;
/// The dialog's title and margins, which the body's height leaves room for.
const ACCOUNT_DIALOG_CHROME: f32 = 160.;
const SECTION_NAV_WIDTH: f32 = 170.;

/// One section of the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AccountSection {
    Password,
    Tokens,
    Online,
    Accounts,
    Invites,
    Signups,
    Audit,
}

impl AccountSection {
    fn title_key(self) -> &'static str {
        match self {
            Self::Password => "section_password",
            Self::Tokens => "section_tokens",
            Self::Online => "section_online",
            Self::Accounts => "section_accounts",
            Self::Invites => "section_invites",
            Self::Signups => "section_signups",
            Self::Audit => "section_audit",
        }
    }

    /// What `me` has: tokens are a read-only account's (ADR 15); who is
    /// online, the accounts, the invitations and the audit an
    /// administrator's — the online list names other accounts and where they
    /// call from — and the requests for an account too, on a bridge that
    /// takes them.
    fn of(me: &BridgeOwnAccount) -> Vec<Self> {
        let mut sections = vec![Self::Password];
        match me.account.role {
            // A locked account's tokens are an administrator's to keep.
            BridgeRole::ReadOnly if !me.account.locked => sections.push(Self::Tokens),
            BridgeRole::ReadOnly => {}
            BridgeRole::Admin => {
                sections.extend([Self::Online, Self::Accounts, Self::Invites]);
                if me.signup {
                    sections.push(Self::Signups);
                }
                sections.push(Self::Audit);
            }
            BridgeRole::Full => {}
        }
        sections
    }
}

/// What the last request of a form came to, under it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    text: SharedString,
    good: bool,
}

impl Status {
    fn good(text: impl Into<SharedString>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            good: true,
        })
    }

    fn bad(text: impl Into<SharedString>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            good: false,
        })
    }
}

/// A token or a link: shown once, in the section that made it, with a way
/// to copy it. The bridge keeps only its hash.
#[derive(Clone, Debug)]
struct ShownOnce {
    section: AccountSection,
    text: SharedString,
    value: SharedString,
}

pub struct ZedisAccount {
    bridge: Arc<dyn BridgeAccounts>,
    /// Holds the focus inside the dialog, so Escape reaches it after the
    /// field that had the focus went with its section.
    focus_handle: FocusHandle,
    section: AccountSection,
    /// The signed-in account, once the bridge has said.
    me: Option<BridgeOwnAccount>,
    /// Why it could not.
    load_error: Option<SharedString>,
    shown: Option<ShownOnce>,
    password: PasswordForm,
    tokens: TokenForm,
    admin: AdminForm,
    invites: InvitesForm,
    signups: SignupsForm,
    audit: AuditForm,
    online: OnlineForm,
}

impl ZedisAccount {
    pub fn new(bridge: Arc<dyn BridgeAccounts>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            bridge,
            focus_handle: cx.focus_handle(),
            section: AccountSection::Password,
            me: None,
            load_error: None,
            shown: None,
            password: PasswordForm::new(window, cx),
            tokens: TokenForm::new(window, cx),
            admin: AdminForm::new(window, cx),
            invites: InvitesForm::new(window, cx),
            signups: SignupsForm::new(window, cx),
            audit: AuditForm::new(window, cx),
            online: OnlineForm::new(window, cx),
        };
        this.load_me(cx);
        this
    }

    /// Ask who is signed in, and for an administrator everything the other
    /// sections show.
    fn load_me(&mut self, cx: &mut Context<Self>) {
        let request = self.bridge.account();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(me) => {
                        let admin = me.account.role == BridgeRole::Admin && this.me.is_none();
                        let signup = me.signup;
                        this.me = Some(me);
                        this.load_error = None;
                        if admin {
                            this.load_accounts(cx);
                            this.load_invites(cx);
                            if signup {
                                this.load_signups(cx);
                            }
                        }
                    }
                    Err(e) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.load_error = Some(t!("account.load_failed", error = e.message, locale = locale).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_section(&mut self, section: AccountSection, window: &mut Window, cx: &mut Context<Self>) {
        self.section = section;
        // The audit is read when it is first looked at, not with the page;
        // the requests and who is online every time, since they change
        // while it is open.
        if section == AccountSection::Audit && !self.audit.loaded {
            self.load_audit(false, cx);
        }
        if section == AccountSection::Signups {
            self.load_signups(cx);
        }
        if section == AccountSection::Online {
            self.load_online(cx);
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn show_once(&mut self, text: SharedString, value: impl Into<SharedString>) {
        self.shown = Some(ShownOnce {
            section: self.section,
            text,
            value: value.into(),
        });
    }

    /// Revoke `token` — the signed-in account's own (`owner: None`), or an
    /// administrator revoking one of `owner`'s — and read again what lists it.
    fn revoke_token(&mut self, owner: Option<String>, token: String, cx: &mut Context<Self>) {
        let request = self.bridge.revoke_token(owner.clone(), token);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                let status = match result {
                    Ok(()) => None,
                    Err(e) => Status::bad(refusal(cx, &e, &[])),
                };
                match owner {
                    None => {
                        this.tokens.status = status;
                        this.load_me(cx);
                    }
                    Some(_) => {
                        this.admin.status = status;
                        this.load_accounts(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Its tokens, each with a way to revoke it.
    fn render_token_rows(&self, owner: Option<&str>, tokens: &[BridgeToken], cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if tokens.is_empty() {
            return Label::new(i18n_account(cx, "token_none"))
                .text_sm()
                .text_color(muted)
                .into_any_element();
        }
        let never = i18n_account(cx, "expires_never");
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        v_flex()
            .gap_1()
            .children(tokens.iter().map(|token| {
                let expires: SharedString = match token.expires_at {
                    Some(at) => t!("account.expires_on", date = when(at), locale = &locale).into(),
                    None => never.clone(),
                };
                let owner = owner.map(str::to_string);
                let name = token.name.clone();
                let id = SharedString::from(format!(
                    "account-revoke-{}-{}",
                    owner.as_deref().unwrap_or(""),
                    token.name
                ));
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Label::new(token.name.clone()).text_sm())
                    .child(Label::new(expires).text_xs().text_color(muted))
                    .child(
                        Button::new(id)
                            .ghost()
                            .xsmall()
                            .label(i18n_account(cx, "revoke"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.revoke_token(owner.clone(), name.clone(), cx);
                            })),
                    )
            }))
            .into_any_element()
    }

    /// The value shown once, if it was made in the section on screen.
    fn render_shown(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.shown.as_ref().filter(|shown| shown.section == self.section)?;
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(cx.theme().warning)
                .rounded(cx.theme().radius)
                .child(Label::new(shown.text.clone()).text_sm().whitespace_normal())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted)
                                .font_family(get_mono_font_family())
                                .text_sm()
                                .child(Label::new(shown.value.clone()).whitespace_normal()),
                        )
                        .child(
                            Clipboard::new("account-shown-copy")
                                .value(shown.value.clone())
                                .tooltip(i18n_account(cx, "copy")),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_nav(&self, me: &BridgeOwnAccount, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.section;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let who: SharedString = t!(
            "account.signed_in_as",
            name = me.account.name,
            role = role_name(cx, me.account.role),
            locale = &locale
        )
        .into();
        v_flex()
            .flex_none()
            .w(px(SECTION_NAV_WIDTH))
            .h_full()
            .p_2()
            .gap_0p5()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                Label::new(who)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .px_2()
                    .pb_2(),
            )
            .children(AccountSection::of(me).into_iter().map(|section| {
                ListItem::new(section.title_key())
                    .selected(section == active)
                    .child(Label::new(i18n_account(cx, section.title_key())).text_sm())
                    .on_click(cx.listener(move |this, _, window, cx| this.select_section(section, window, cx)))
            }))
    }
}

/// What a role is called.
fn role_name(cx: &App, role: BridgeRole) -> SharedString {
    i18n_account(
        cx,
        match role {
            BridgeRole::Admin => "role_admin",
            BridgeRole::Full => "role_full",
            BridgeRole::ReadOnly => "role_read_only",
        },
    )
}

/// The role names, in [`BridgeRole::ALL`]'s order, for a picker.
fn role_names(cx: &App) -> Vec<String> {
    BridgeRole::ALL
        .iter()
        .map(|role| role_name(cx, *role).to_string())
        .collect()
}

/// Unix seconds as the reader's date and time (the Settings time zone).
fn when(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(format_unix_secs)
        .unwrap_or_else(|| secs.to_string())
}

/// Server rules as typed: one a line (or comma-separated); none at all is
/// every shared entry.
fn rules_of(text: &str) -> Option<Vec<String>> {
    let rules: Vec<String> = text
        .split(['\n', ','])
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .map(str::to_string)
        .collect();
    (!rules.is_empty()).then_some(rules)
}

/// What a refused request is called on this page: `known`'s words where
/// the refusal is one of them, the bridge's own for the rest.
fn refusal(cx: &App, error: &BridgeAccountError, known: &[(BridgeAccountErrorKind, &'static str)]) -> SharedString {
    if let Some((_, key)) = known.iter().find(|(kind, _)| *kind == error.kind) {
        return i18n_account(cx, key);
    }
    match error.kind {
        BridgeAccountErrorKind::TooManyAttempts => i18n_account(cx, "too_many_attempts"),
        BridgeAccountErrorKind::Refused => i18n_account(cx, "admins_only"),
        _ => {
            let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
            t!("account.error_generic", error = error.message, locale = locale).into()
        }
    }
}

/// A form field: its label above it.
fn field(cx: &App, label: SharedString, control: impl IntoElement) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(Label::new(label).text_xs().text_color(cx.theme().muted_foreground))
        .child(control)
}

/// A section's heading.
fn heading(cx: &App, key: &str) -> impl IntoElement {
    Label::new(i18n_account(cx, key)).text_sm().font_semibold()
}

/// A note in the muted colour, wrapped.
fn note(cx: &App, text: SharedString) -> impl IntoElement {
    Label::new(text)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
}

/// What the last request came to, in the colour it deserves.
fn status_line(cx: &App, status: &Option<Status>) -> Option<AnyElement> {
    let status = status.as_ref()?;
    let color = if status.good {
        cx.theme().success
    } else {
        cx.theme().danger
    };
    Some(
        Label::new(status.text.clone())
            .text_sm()
            .text_color(color)
            .whitespace_normal()
            .into_any_element(),
    )
}

/// A form's submit button, which says it is busy and takes no second press.
fn submit(id: &'static str, label: SharedString, busy: bool) -> Button {
    Button::new(id)
        .primary()
        .icon(IconName::Check)
        .label(label)
        .loading(busy)
        .disabled(busy)
}

impl Render for ZedisAccount {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(me) = self.me.clone() else {
            let text = self.load_error.clone().unwrap_or_else(|| i18n_common(cx, "loading"));
            return h_flex()
                .track_focus(&self.focus_handle)
                .size_full()
                .items_center()
                .justify_center()
                .child(Label::new(text).text_sm().text_color(cx.theme().muted_foreground))
                .into_any_element();
        };
        // A section this account does not have (its role changed while the
        // page was open) is not drawn; the first one is.
        if !AccountSection::of(&me).contains(&self.section) {
            self.section = AccountSection::Password;
        }
        let body = match self.section {
            AccountSection::Password => self.render_password(&me, window, cx),
            AccountSection::Tokens => self.render_tokens(&me, window, cx),
            AccountSection::Online => self.render_online(window, cx),
            AccountSection::Accounts => self.render_accounts(window, cx),
            AccountSection::Invites => self.render_invites(&me, window, cx),
            AccountSection::Signups => self.render_signups(window, cx),
            AccountSection::Audit => self.render_audit(window, cx),
        };
        h_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .items_start()
            .child(self.render_nav(&me, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_6()
                    .py_4()
                    .gap_4()
                    .child(heading(cx, self.section.title_key()))
                    .child(body)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }
}

/// The account page, as a dialog over the page. Nothing on the desktop,
/// which has no bridge to ask.
pub fn open_account(window: &mut Window, cx: &mut App) {
    let Some(bridge) = bridge_accounts() else {
        return;
    };
    // One at a time: the menu still reaches here with a dialog up.
    if window.has_active_dialog(cx) {
        return;
    }
    let height = (window.viewport_size().height - px(ACCOUNT_DIALOG_CHROME))
        .min(px(ACCOUNT_DIALOG_MAX_HEIGHT))
        .max(px(ACCOUNT_DIALOG_MIN_HEIGHT));
    let view = cx.new(|cx| ZedisAccount::new(bridge, window, cx));
    ZedisDialog::new(i18n_account(cx, "title"))
        .w(px(ACCOUNT_DIALOG_WIDTH))
        .child(move || div().w_full().h(height).child(view.clone()))
        .open(window, cx);
}

#[cfg(test)]
mod tests;

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

//! An administrator's requests for an account (ADR 17, amended 2026-10-18),
//! where the bridge takes them: each waits here until it is approved — the
//! account made with the role and server rules picked below and the
//! password it was asked with — or rejected. A request for a name an account
//! already has cannot be approved, and says so.

use super::*;
use crate::connection::BridgeSignup;
use gpui::Subscription;
use gpui_kit::component::input::{Textarea, TextareaState};
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

pub(super) const SIGNUP_APPROVE_ID: &str = "account-signup-approve";
pub(super) const SIGNUP_REJECT_ID: &str = "account-signup-reject";

/// The row of the request `id`.
pub(super) fn signup_row_id(id: &str) -> SharedString {
    SharedString::from(format!("account-signup-{id}"))
}

pub(super) struct SignupsForm {
    pub(super) signups: Vec<BridgeSignup>,
    /// The request being answered, by id.
    selected: Option<String>,
    pub(super) role: Entity<ZedisSelect>,
    role_value: BridgeRole,
    pub(super) servers: Entity<TextareaState>,
    busy: bool,
    status: Option<Status>,
    clear: bool,
    _role_subscription: Subscription,
}

impl SignupsForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ZedisAccount>) -> Self {
        // Read-only unless the administrator says more, as an invitation is.
        let default_role = BridgeRole::ReadOnly;
        let ix = BridgeRole::ALL.iter().position(|role| *role == default_role);
        let role = cx.new(|cx| ZedisSelect::new(role_names(cx), ix, window, cx));
        let role_subscription = cx.subscribe(&role, |this, _, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(ix) = event;
            if let Some(role) = BridgeRole::ALL.get(*ix) {
                this.signups.role_value = *role;
                cx.notify();
            }
        });
        Self {
            signups: Vec::new(),
            selected: None,
            role,
            role_value: default_role,
            servers: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(2)
                    .placeholder(i18n_account(cx, "servers_all"))
            }),
            busy: false,
            status: None,
            clear: false,
            _role_subscription: role_subscription,
        }
    }

    fn picked(&self) -> Option<&BridgeSignup> {
        let id = self.selected.as_deref()?;
        self.signups.iter().find(|request| request.id == id)
    }
}

/// When a request was made, and from where when the bridge saw it.
fn asked(cx: &App, request: &BridgeSignup) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    let date = when(request.requested_at);
    match &request.peer {
        Some(peer) => t!("account.signup_asked_from", date = date, peer = peer, locale = locale).into(),
        None => t!("account.signup_asked", date = date, locale = locale).into(),
    }
}

impl ZedisAccount {
    pub(super) fn load_signups(&mut self, cx: &mut Context<Self>) {
        let request = self.bridge.signups();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(signups) => {
                        let form = &mut this.signups;
                        form.signups = signups;
                        // One answered elsewhere, or expired, is no longer
                        // there to answer.
                        if form.picked().is_none() {
                            form.selected = None;
                        }
                    }
                    Err(e) => this.signups.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_signup(&mut self, id: String, cx: &mut Context<Self>) {
        self.signups.selected = Some(id);
        self.signups.status = None;
        cx.notify();
    }

    pub(super) fn render_signups(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if std::mem::take(&mut self.signups.clear) {
            self.signups
                .servers
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        let form = &self.signups;
        let muted = cx.theme().muted_foreground;
        let list = if form.signups.is_empty() {
            note(cx, i18n_account(cx, "signup_none")).into_any_element()
        } else {
            v_flex()
                .gap_0p5()
                .children(form.signups.iter().map(|request| {
                    let id = request.id.clone();
                    let picked = form.selected.as_deref() == Some(request.id.as_str());
                    ListItem::new(signup_row_id(&request.id))
                        .selected(picked)
                        .child(
                            v_flex()
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Label::new(request.name.clone()).text_sm())
                                        .when(request.name_taken, |row| {
                                            row.child(
                                                Label::new(i18n_account(cx, "signup_name_taken"))
                                                    .text_xs()
                                                    .text_color(cx.theme().warning),
                                            )
                                        })
                                        .child(div().flex_1())
                                        .child(Label::new(asked(cx, request)).text_xs().text_color(muted)),
                                )
                                .when(!request.note.is_empty(), |rows| {
                                    rows.child(
                                        Label::new(request.note.clone())
                                            .text_xs()
                                            .text_color(muted)
                                            .whitespace_normal(),
                                    )
                                }),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.select_signup(id.clone(), cx)))
                }))
                .into_any_element()
        };
        let answer = match form.picked() {
            None => note(cx, i18n_account(cx, "signup_select")).into_any_element(),
            Some(request) => {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let title: SharedString = t!("account.signup_answer", name = request.name, locale = locale).into();
                let taken = request.name_taken;
                v_flex()
                    .gap_3()
                    .child(Label::new(title).text_sm().font_semibold())
                    .child(
                        div()
                            .w(px(200.))
                            .child(field(cx, i18n_account(cx, "col_role"), form.role.clone())),
                    )
                    .child(field(cx, i18n_account(cx, "col_servers"), Textarea::new(&form.servers)))
                    .child(note(cx, i18n_account(cx, "servers_hint")))
                    .children(taken.then(|| note(cx, i18n_account(cx, "signup_name_taken_hint"))))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                submit(SIGNUP_APPROVE_ID, i18n_account(cx, "signup_approve"), form.busy)
                                    .icon(gpui_kit::assets::IconName::Check)
                                    .disabled(form.busy || taken)
                                    .on_click(cx.listener(|this, _, _, cx| this.approve_signup(cx))),
                            )
                            .child(
                                Button::new(SIGNUP_REJECT_ID)
                                    .outline()
                                    .label(i18n_account(cx, "signup_reject"))
                                    .disabled(form.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.reject_signup(cx))),
                            ),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .gap_3()
            .child(note(cx, i18n_account(cx, "signups_intro")))
            .child(list)
            .children(status_line(cx, &form.status))
            // Nothing waiting is nothing to answer.
            .when(!form.signups.is_empty(), |page| {
                page.child(
                    v_flex()
                        .gap_3()
                        .pt_3()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(answer),
                )
            })
            .into_any_element()
    }

    /// The words for a refused answer: answered elsewhere, or a name taken
    /// since.
    fn signup_refusal(cx: &App, error: &BridgeAccountError) -> SharedString {
        let known = [
            (BridgeAccountErrorKind::NotFound, "signup_gone"),
            (BridgeAccountErrorKind::Conflict, "signup_taken"),
        ];
        refusal(cx, error, &known)
    }

    pub(super) fn approve_signup(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.signups.picked() else {
            return;
        };
        let (id, name) = (request.id.clone(), request.name.clone());
        let servers = rules_of(&self.signups.servers.read(cx).value());
        let role = self.signups.role_value;
        let request = self.bridge.approve_signup(id, role, servers);
        self.signups.busy = true;
        self.signups.status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.signups.busy = false;
                match result {
                    Ok(_) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.signups.status = Status::good(t!("account.signup_approved", name = name, locale = locale));
                        this.signups.selected = None;
                        this.signups.clear = true;
                        this.load_accounts(cx);
                    }
                    Err(e) => this.signups.status = Status::bad(Self::signup_refusal(cx, &e)),
                }
                this.load_signups(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn reject_signup(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.signups.picked() else {
            return;
        };
        let (id, name) = (request.id.clone(), request.name.clone());
        let request = self.bridge.reject_signup(id);
        self.signups.busy = true;
        self.signups.status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.signups.busy = false;
                match result {
                    Ok(()) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.signups.status = Status::good(t!("account.signup_rejected", name = name, locale = locale));
                        this.signups.selected = None;
                    }
                    Err(e) => this.signups.status = Status::bad(Self::signup_refusal(cx, &e)),
                }
                this.load_signups(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

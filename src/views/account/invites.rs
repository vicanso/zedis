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

//! An administrator's invitations: how every account after the first one is
//! made (ADR 17). An invitation registers one account, with the role and
//! the server rules it was made with, until it expires; the link carries
//! its code after `#`, and the bridge keeps only the code's hash.

use super::*;
use crate::connection::{BridgeInvite, BridgeInviteKind};
use gpui::Subscription;
use gpui_kit::component::{
    checkbox::Checkbox,
    input::{Input, InputState, Textarea, TextareaState},
};
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

/// The bridge's defaults and bounds for how long a link is valid.
const INVITE_DAYS: u64 = 7;
const MAX_INVITE_DAYS: u64 = 90;

pub(super) const CREATE_INVITE_ID: &str = "account-create-invite";
pub(super) const INVITE_DAYS_ID: &str = "account-invite-days";

pub(super) struct InvitesForm {
    invites: Vec<BridgeInvite>,
    role: Entity<ZedisSelect>,
    role_value: BridgeRole,
    servers: Entity<TextareaState>,
    days: Entity<InputState>,
    proxy: bool,
    busy: bool,
    status: Option<Status>,
    clear: bool,
    _role_subscription: Subscription,
}

impl InvitesForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ZedisAccount>) -> Self {
        // An invitation is read-only unless the administrator says more.
        let default_role = BridgeRole::ReadOnly;
        let ix = BridgeRole::ALL.iter().position(|role| *role == default_role);
        let role = cx.new(|cx| ZedisSelect::new(role_names(cx), ix, window, cx));
        let role_subscription = cx.subscribe(&role, |this, _, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(ix) = event;
            if let Some(role) = BridgeRole::ALL.get(*ix) {
                this.invites.role_value = *role;
                cx.notify();
            }
        });
        Self {
            invites: Vec::new(),
            role,
            role_value: default_role,
            servers: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(2)
                    .placeholder(i18n_account(cx, "servers_all"))
            }),
            days: cx.new(|cx| InputState::new(window, cx).default_value(INVITE_DAYS.to_string())),
            proxy: false,
            busy: false,
            status: None,
            clear: false,
            _role_subscription: role_subscription,
        }
    }
}

/// Days as typed, within what the bridge allows; anything unreadable is the
/// default.
fn days_of(text: &str) -> u64 {
    text.trim()
        .parse::<u64>()
        .map_or(INVITE_DAYS, |days| days.clamp(1, MAX_INVITE_DAYS))
}

/// What an invitation is for, as its row says it.
fn invite_kind(cx: &App, kind: &BridgeInviteKind) -> SharedString {
    let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
    match kind {
        BridgeInviteKind::Reset { account } => t!("account.invite_kind_reset", name = account, locale = locale).into(),
        BridgeInviteKind::Register { role, proxy, .. } => {
            let key = if *proxy {
                "account.invite_kind_proxy"
            } else {
                "account.invite_kind_register"
            };
            t!(key, role = role_name(cx, *role), locale = locale).into()
        }
    }
}

impl ZedisAccount {
    pub(super) fn load_invites(&mut self, cx: &mut Context<Self>) {
        let request = self.bridge.invites();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(invites) => this.invites.invites = invites,
                    Err(e) => this.invites.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_invites(
        &mut self,
        me: &BridgeOwnAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if std::mem::take(&mut self.invites.clear) {
            self.invites
                .servers
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        let form = &self.invites;
        let muted = cx.theme().muted_foreground;
        let list = if form.invites.is_empty() {
            note(cx, i18n_account(cx, "invite_none")).into_any_element()
        } else {
            v_flex()
                .gap_1()
                .children(form.invites.iter().enumerate().map(|(ix, invite)| {
                    let id = invite.id.clone();
                    let expires = when(invite.expires_at);
                    h_flex()
                        .id(("account-invite-row", ix))
                        .gap_3()
                        .items_center()
                        .child(Label::new(invite_kind(cx, &invite.kind)).text_sm().flex_1().min_w_0())
                        .child(Label::new(expires).text_xs().text_color(muted))
                        .child(Label::new(invite.created_by.clone()).text_xs().text_color(muted))
                        .child(
                            Button::new(SharedString::from(format!("account-revoke-invite-{}", invite.id)))
                                .ghost()
                                .xsmall()
                                .label(i18n_account(cx, "revoke"))
                                .on_click(cx.listener(move |this, _, _, cx| this.revoke_invite(id.clone(), cx))),
                        )
                }))
                .into_any_element()
        };
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_4()
                    .items_end()
                    .flex_wrap()
                    .child(
                        div()
                            .w(px(200.))
                            .child(field(cx, i18n_account(cx, "col_role"), form.role.clone())),
                    )
                    .child(div().w(px(120.)).child(field(
                        cx,
                        i18n_account(cx, "invite_days"),
                        Input::new(&form.days).id(INVITE_DAYS_ID),
                    )))
                    // Only where the bridge believes a sign-in proxy: an
                    // invitation for it registers the name the proxy asserts.
                    .when(me.proxy, |row| {
                        row.child(
                            Checkbox::new("account-invite-proxy")
                                .checked(form.proxy)
                                .label(i18n_account(cx, "invite_proxy"))
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.invites.proxy = *checked;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(field(cx, i18n_account(cx, "col_servers"), Textarea::new(&form.servers)))
            .child(note(cx, i18n_account(cx, "servers_hint")))
            .child(
                h_flex().child(
                    submit(CREATE_INVITE_ID, i18n_account(cx, "invite_create"), form.busy)
                        .icon(gpui_kit::assets::IconName::Plus)
                        .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                ),
            )
            .children(status_line(cx, &form.status))
            .children(self.render_shown(cx))
            .child(
                v_flex()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(heading(cx, "open_invites"))
                    .child(list),
            )
            .into_any_element()
    }

    pub(super) fn create_invite(&mut self, cx: &mut Context<Self>) {
        let form = &self.invites;
        let servers = rules_of(&form.servers.read(cx).value());
        let days = days_of(&form.days.read(cx).value());
        let proxy = form.proxy && self.me.as_ref().is_some_and(|me| me.proxy);
        let request = self.bridge.create_invite(form.role_value, servers, proxy, days);
        self.invites.busy = true;
        self.invites.status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.invites.busy = false;
                match result {
                    Ok(made) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let text = t!(
                            "account.invite_link_made",
                            date = when(made.invite.expires_at),
                            locale = locale
                        );
                        let link = this.bridge.invite_link(&made.code);
                        this.show_once(text.into(), link);
                        this.invites.clear = true;
                        this.load_invites(cx);
                    }
                    Err(e) => this.invites.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn revoke_invite(&mut self, id: String, cx: &mut Context<Self>) {
        let request = self.bridge.revoke_invite(id);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.invites.status = Status::bad(refusal(cx, &e, &[]));
                }
                this.load_invites(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::days_of;

    #[test]
    fn days_are_kept_within_what_the_bridge_allows() {
        assert_eq!(days_of("14"), 14);
        assert_eq!(days_of(" 0 "), 1);
        assert_eq!(days_of("365"), 90);
        assert_eq!(days_of("soon"), 7);
    }
}

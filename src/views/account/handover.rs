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

//! What becomes of the server entries and scripts an account kept to
//! itself (ADR 17): asked when the account is deleted, and of what an
//! account deleted before this existed left behind. Handed to an account —
//! the administrator who asks, unless they say otherwise — shared, or
//! deleted. Sharing is never the default: a private entry carries its
//! owner's Redis password.
//!
//! A view entity, because the dialog's body holds a picker (see CLAUDE.md).

use super::*;
use crate::connection::{BridgeDisposition, BridgeOwned, BridgeOwnedItem};
use gpui::Subscription;
use gpui_kit::component::radio::Radio;
use zedis_ui::{ZedisSelect, ZedisSelectEvent};

/// How many names of each kind the question lists before "and n more".
const NAMES_SHOWN: usize = 5;
const HANDOVER_DIALOG_WIDTH: f32 = 520.;

pub(super) const HAND_TO_ID: &str = "account-handover-hand-to";
pub(super) const SHARE_ID: &str = "account-handover-share";
pub(super) const DELETE_ID: &str = "account-handover-delete";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    HandTo,
    Share,
    Delete,
}

pub(super) struct HandoverDialog {
    owner: String,
    owned: BridgeOwned,
    kind: Handover,
    /// `None` until one is picked, where no account could be handed them.
    choice: Option<Choice>,
    /// Who may be handed them: an account that can write and sign in.
    recipients: Vec<String>,
    recipient: Option<usize>,
    picker: Entity<ZedisSelect>,
    _picker_subscription: Subscription,
}

impl HandoverDialog {
    /// `accounts` is the list as the page has it; `me` is the administrator
    /// asking, whom they go to unless another is picked.
    fn new(
        owner: String,
        owned: BridgeOwned,
        kind: Handover,
        accounts: &[BridgeAccount],
        me: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let recipients = recipients(accounts, &owner);
        let recipient = me
            .and_then(|me| recipients.iter().position(|name| name == me))
            .or((!recipients.is_empty()).then_some(0));
        let picker = cx.new(|cx| ZedisSelect::new(recipients.clone(), recipient, window, cx));
        let picker_subscription = cx.subscribe(&picker, |this, _, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(ix) = event;
            this.recipient = Some(*ix);
            cx.notify();
        });
        Self {
            owner,
            owned,
            kind,
            choice: recipient.map(|_| Choice::HandTo),
            recipients,
            recipient,
            picker,
            _picker_subscription: picker_subscription,
        }
    }

    /// What was chosen, once it is complete.
    pub(super) fn disposition(&self) -> Option<BridgeDisposition> {
        match self.choice? {
            Choice::HandTo => {
                let to = self.recipients.get(self.recipient?)?;
                Some(BridgeDisposition::HandTo(to.clone()))
            }
            Choice::Share => Some(BridgeDisposition::Share),
            Choice::Delete => Some(BridgeDisposition::Delete),
        }
    }

    /// One kind of thing it kept: a heading with the count, and the names.
    fn render_items(heading: String, items: &[BridgeOwnedItem], locale: &str, cx: &App) -> Option<AnyElement> {
        if items.is_empty() {
            return None;
        }
        let mut names = items
            .iter()
            .take(NAMES_SHOWN)
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if items.len() > NAMES_SHOWN {
            let more = t!(
                "account.handover_more",
                count = items.len() - NAMES_SHOWN,
                locale = locale
            );
            names = format!("{names} {more}");
        }
        let id = SharedString::from(format!("handover-items-{heading}"));
        Some(
            v_flex()
                .id(id)
                .gap_0p5()
                .child(Label::new(heading).text_sm())
                .child(note(cx, names.into()))
                .into_any_element(),
        )
    }
}

/// The accounts that may be handed what `owner` kept: not `owner`, and none
/// that could not change it (read-only) or not sign in to see it (disabled).
fn recipients(accounts: &[BridgeAccount], owner: &str) -> Vec<String> {
    accounts
        .iter()
        .filter(|account| account.name != owner && account.role != BridgeRole::ReadOnly && !account.disabled)
        .map(|account| account.name.clone())
        .collect()
}

impl Render for HandoverDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let intro: SharedString = match self.kind {
            Handover::Delete => t!("account.handover_intro", name = &self.owner, locale = &locale),
            Handover::Orphans => t!("account.settle_intro", name = &self.owner, locale = &locale),
        }
        .into();
        // What deleting the account does besides, when that is the question.
        let footnote =
            (self.kind == Handover::Delete).then(|| note(cx, i18n_account(cx, "delete_body")).into_any_element());
        let servers = t!(
            "account.handover_servers",
            count = self.owned.servers.len(),
            locale = &locale
        );
        let scripts = t!(
            "account.handover_scripts",
            count = self.owned.scripts.len(),
            locale = &locale
        );
        let no_recipient = self.recipients.is_empty();
        // Radios of their own rather than a group, so what goes with each —
        // the account to hand them to, what sharing or deleting does — sits
        // under it, not under the last.
        let radio = |id: &'static str, choice: Choice, label: &str, cx: &mut Context<Self>| {
            Radio::new(id)
                .label(i18n_account(cx, label))
                .checked(self.choice == Some(choice))
                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                    this.choice = Some(choice);
                    cx.notify();
                }))
        };
        let hand_to = radio(HAND_TO_ID, Choice::HandTo, "handover_hand_to", cx).disabled(no_recipient);
        let share = radio(SHARE_ID, Choice::Share, "handover_share", cx)
            .child(note(cx, i18n_account(cx, "handover_share_hint")));
        let delete = radio(DELETE_ID, Choice::Delete, "handover_delete", cx)
            .child(note(cx, i18n_account(cx, "handover_delete_hint")));
        let under_hand_to = if no_recipient {
            Some(note(cx, i18n_account(cx, "handover_no_recipient")).into_any_element())
        } else {
            (self.choice == Some(Choice::HandTo)).then(|| self.picker.clone().into_any_element())
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(Label::new(intro).text_sm().whitespace_normal())
            .children(Self::render_items(servers.into(), &self.owned.servers, &locale, cx))
            .children(Self::render_items(scripts.into(), &self.owned.scripts, &locale, cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(hand_to)
                    .children(under_hand_to.map(|under| div().pl_6().child(under)))
                    .child(share)
                    .child(delete),
            )
            .children(footnote)
    }
}

/// What the question is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Handover {
    /// The account is being deleted.
    Delete,
    /// An account deleted before handovers left this behind.
    Orphans,
}

impl ZedisAccount {
    /// Ask what becomes of what `owner` kept to itself, then delete the
    /// account or settle what it left. An account that kept nothing is
    /// asked about plainly.
    pub(super) fn ask_handover(
        &mut self,
        kind: Handover,
        owner: String,
        owned: BridgeOwned,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let entity = cx.entity().downgrade();
        let (title, ok) = match kind {
            Handover::Delete => (
                t!("account.delete_title", name = &owner, locale = &locale).to_string(),
                i18n_common(cx, "delete"),
            ),
            Handover::Orphans => (
                t!("account.settle_title", name = &owner, locale = &locale).to_string(),
                i18n_account(cx, "settle"),
            ),
        };
        if owned.is_empty() {
            if kind == Handover::Delete {
                ZedisDialog::new_alert(title, i18n_account(cx, "delete_body"))
                    .danger()
                    .ok_text(ok)
                    .cancel_text(i18n_common(cx, "cancel"))
                    .on_ok(move |_, _, cx| {
                        let owner = owner.clone();
                        let _ = entity.update(cx, |this, cx| {
                            this.delete_account(owner, BridgeDisposition::Delete, cx);
                        });
                        true
                    })
                    .open(window, cx);
            }
            return;
        }
        let me = self.me.as_ref().map(|me| me.account.name.clone());
        let accounts = self.admin.accounts().to_vec();
        let form = cx.new(|cx| HandoverDialog::new(owner.clone(), owned, kind, &accounts, me.as_deref(), window, cx));
        let body = form.clone();
        ZedisDialog::new(title)
            .alert()
            .w(px(HANDOVER_DIALOG_WIDTH))
            .child(move || body.clone())
            .danger()
            .ok_text(ok)
            .cancel_text(i18n_common(cx, "cancel"))
            .on_ok(move |_, _, cx| {
                let Some(disposition) = form.read(cx).disposition() else {
                    return false;
                };
                let owner = owner.clone();
                let _ = entity.update(cx, |this, cx| match kind {
                    Handover::Delete => this.delete_account(owner, disposition, cx),
                    Handover::Orphans => this.settle_orphans(owner, disposition, cx),
                });
                true
            })
            .open(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, role: BridgeRole, disabled: bool) -> BridgeAccount {
        BridgeAccount {
            name: name.to_string(),
            role,
            disabled,
            ..Default::default()
        }
    }

    #[test]
    fn only_an_account_that_could_use_them_is_handed_them() {
        let accounts = [
            named("alice", BridgeRole::Admin, false),
            named("bob", BridgeRole::Full, false),
            named("carol", BridgeRole::Full, true),
            named("ai", BridgeRole::ReadOnly, false),
        ];
        assert_eq!(recipients(&accounts, "bob"), vec!["alice".to_string()]);
        assert_eq!(
            recipients(&accounts, "ghost"),
            vec!["alice".to_string(), "bob".to_string()]
        );
    }
}

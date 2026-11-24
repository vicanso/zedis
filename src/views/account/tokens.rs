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

//! A read-only account's own MCP tokens (ADR 15): what an AI assistant signs
//! in at `/v1/mcp` with, and nowhere else.

use super::*;
use crate::helpers::{picker_before_matcher, to_picker_date};
use gpui_kit::component::{
    date_picker::{DatePicker, DatePickerState},
    input::{Input, InputState},
};
use jiff::{Span, Timestamp, civil::Date, tz::TimeZone};

pub(super) const TOKEN_NAME_ID: &str = "account-token-name";
pub(super) const ISSUE_TOKEN_ID: &str = "account-issue-token";

pub(super) struct TokenForm {
    name: Entity<InputState>,
    expires: Entity<DatePickerState>,
    busy: bool,
    pub(super) status: Option<Status>,
    clear: bool,
}

impl TokenForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ZedisAccount>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("claude-laptop")),
            expires: expiry_picker(None, window, cx),
            busy: false,
            status: None,
            clear: false,
        }
    }
}

/// How long a token an administrator issues for someone else lasts unless
/// they pick otherwise: such a token is the easiest kind to forget, so not
/// expiring is the choice that has to be made, by clearing the date.
pub(super) const ADMIN_TOKEN_DAYS: i64 = 90;

/// The first day a token may expire on. The bridge refuses a day already
/// begun (UTC midnight is when a token stops), so today is not offered.
pub(super) fn first_expiry_day() -> Date {
    Timestamp::now().to_zoned_datetime(TimeZone::UTC).date() + Span::new().days(1)
}

/// A picker for a token's expiry, which offers no day the bridge would
/// refuse, showing `days` from today when given.
pub(super) fn expiry_picker(
    days: Option<i64>,
    window: &mut Window,
    cx: &mut Context<ZedisAccount>,
) -> Entity<DatePickerState> {
    let first = first_expiry_day();
    cx.new(|cx| {
        let mut state = DatePickerState::new(window, cx).disabled_matcher(picker_before_matcher(first));
        if let Some(days) = days {
            let date = Timestamp::now().to_zoned_datetime(TimeZone::UTC).date() + Span::new().days(days);
            state.set_date(to_picker_date(date), window, cx);
        }
        state
    })
}

/// A picked day as the bridge takes it, `2027-01-01`; none, a token that
/// does not expire.
pub(super) fn picked_day(state: &DatePickerState) -> Option<String> {
    state.date_time().format("%Y-%m-%d").map(|day| day.to_string())
}

impl ZedisAccount {
    pub(super) fn render_tokens(
        &mut self,
        me: &BridgeOwnAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if std::mem::take(&mut self.tokens.clear) {
            self.tokens.name.update(cx, |state, cx| state.set_value("", window, cx));
        }
        let rows = self.render_token_rows(None, &me.account.tokens, cx);
        let form = &self.tokens;
        v_flex()
            .gap_3()
            .child(note(cx, i18n_account(cx, "tokens_intro")))
            .child(rows)
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .flex_wrap()
                    .child(div().w(px(240.)).child(field(
                        cx,
                        i18n_account(cx, "token_name"),
                        Input::new(&form.name).id(TOKEN_NAME_ID),
                    )))
                    .child(
                        div().w(px(200.)).child(field(
                            cx,
                            i18n_account(cx, "token_expires"),
                            DatePicker::new(&form.expires)
                                .placeholder(i18n_account(cx, "expires_never"))
                                .cleanable(true),
                        )),
                    )
                    .child(
                        submit(ISSUE_TOKEN_ID, i18n_account(cx, "token_issue"), form.busy)
                            .icon(gpui_kit::assets::IconName::Plus)
                            .on_click(cx.listener(|this, _, _, cx| this.issue_own_token(cx))),
                    ),
            )
            .children(status_line(cx, &form.status))
            .children(self.render_shown(cx))
            .into_any_element()
    }

    pub(super) fn issue_own_token(&mut self, cx: &mut Context<Self>) {
        let name = self.tokens.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let expires = picked_day(self.tokens.expires.read(cx));
        self.tokens.busy = true;
        self.tokens.status = None;
        cx.notify();
        let request = self.bridge.issue_token(None, name, expires);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.tokens.busy = false;
                match result {
                    Ok(token) => {
                        this.show_once(i18n_account(cx, "token_shown_once"), token);
                        this.tokens.clear = true;
                        this.load_me(cx);
                    }
                    Err(e) => {
                        let known = [(BridgeAccountErrorKind::Conflict, "token_name_taken")];
                        this.tokens.status = Status::bad(refusal(cx, &e, &known));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

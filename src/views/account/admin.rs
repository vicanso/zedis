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

//! An administrator's list of the accounts. A table to read and pick from,
//! and the picked account's form under it — role, server rules, disabled —
//! rather than editable cells: one account is changed at a time, and saved
//! as one change, which is one audit line (ADR 17).

use super::handover::Handover;
use super::tokens::{ADMIN_TOKEN_DAYS, expiry_picker, picked_day};
use super::*;
use crate::connection::{BridgeAccountChange, BridgeDisposition, BridgeOrphans};
use gpui::Subscription;
use gpui_kit::component::{
    checkbox::Checkbox,
    date_picker::{DatePicker, DatePickerState},
    input::{Input, InputState, Textarea, TextareaState},
    table::{DataTable, TableEvent, TableState},
};
use zedis_ui::{TextColumn, ZedisSelect, ZedisSelectEvent, ZedisTextTable, fitted_table};

const TABLE_HEIGHT: f32 = 200.;
const ROLE_COLUMN_WIDTH: f32 = 130.;
const SERVERS_COLUMN_WIDTH: f32 = 180.;
const TOKENS_COLUMN_WIDTH: f32 = 80.;
const STATE_COLUMN_WIDTH: f32 = 170.;
const NAME_COLUMN_WIDTH: f32 = 160.;

pub(super) const SAVE_ACCOUNT_ID: &str = "account-save";
pub(super) const LOCK_ACCOUNT_ID: &str = "account-locked";
pub(super) const ADMIN_TOKEN_NAME_ID: &str = "account-admin-token-name";
pub(super) const ADMIN_ISSUE_TOKEN_ID: &str = "account-admin-issue-token";
pub(super) const DELETE_ACCOUNT_ID: &str = "account-delete";
/// Followed by the owner's name.
pub(super) const SETTLE_ORPHANS_ID: &str = "account-settle-orphans";

pub(super) struct AdminForm {
    accounts: Vec<BridgeAccount>,
    /// What accounts deleted before handovers left behind, seen by nobody.
    orphans: Vec<BridgeOrphans>,
    pub(super) table: Entity<TableState<ZedisTextTable>>,
    /// The account whose form is under the table, by name.
    selected: Option<String>,
    role: Entity<ZedisSelect>,
    role_value: BridgeRole,
    servers: Entity<TextareaState>,
    pub(super) disabled: bool,
    pub(super) locked: bool,
    token_name: Entity<InputState>,
    token_expires: Entity<DatePickerState>,
    busy: bool,
    pub(super) status: Option<Status>,
    /// Put the selected account's settings back in the form at the next
    /// render: a refused change must not look like one that was kept.
    refill: bool,
    _table_subscription: Subscription,
    /// Follows `role`, and is replaced with it.
    _role_subscription: Subscription,
}

impl AdminForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ZedisAccount>) -> Self {
        let columns = vec![
            TextColumn::new("name", i18n_account(cx, "col_name"), NAME_COLUMN_WIDTH)
                .fills()
                .sortable(),
            TextColumn::new("role", i18n_account(cx, "col_role"), ROLE_COLUMN_WIDTH).sortable(),
            TextColumn::new("servers", i18n_account(cx, "col_servers"), SERVERS_COLUMN_WIDTH),
            TextColumn::new("tokens", i18n_account(cx, "col_tokens"), TOKENS_COLUMN_WIDTH).numeric(),
            TextColumn::new("state", i18n_account(cx, "col_state"), STATE_COLUMN_WIDTH),
        ];
        let table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
            .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"));
        let table = cx.new(|cx| TableState::new(table, window, cx));
        let role = cx.new(|cx| ZedisSelect::new(role_names(cx), None, window, cx));
        let table_subscription = cx.subscribe_in(&table, window, |this, table, event, window, cx| {
            if let TableEvent::SelectRow(row) = event {
                let name = table
                    .read(cx)
                    .delegate()
                    .visible_iter()
                    .nth(*row)
                    .and_then(|cells| cells.first())
                    .map(|name| name.to_string());
                if let Some(name) = name {
                    this.admin.status = None;
                    this.select_account(&name, window, cx);
                }
            }
        });
        let role_subscription = Self::follow_role(&role, cx);
        Self {
            accounts: Vec::new(),
            orphans: Vec::new(),
            table,
            selected: None,
            role,
            role_value: BridgeRole::default(),
            servers: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(3)
                    .placeholder(i18n_account(cx, "servers_all"))
            }),
            disabled: false,
            locked: false,
            token_name: cx.new(|cx| InputState::new(window, cx).placeholder("claude-laptop")),
            token_expires: expiry_picker(Some(ADMIN_TOKEN_DAYS), window, cx),
            busy: false,
            status: None,
            refill: false,
            _table_subscription: table_subscription,
            _role_subscription: role_subscription,
        }
    }

    fn follow_role(role: &Entity<ZedisSelect>, cx: &mut Context<ZedisAccount>) -> Subscription {
        cx.subscribe(role, |this, _, event: &ZedisSelectEvent, cx| {
            let ZedisSelectEvent::Change(ix) = event;
            if let Some(role) = BridgeRole::ALL.get(*ix) {
                this.admin.role_value = *role;
                cx.notify();
            }
        })
    }

    pub(super) fn accounts(&self) -> &[BridgeAccount] {
        &self.accounts
    }

    fn account(&self, name: &str) -> Option<&BridgeAccount> {
        self.accounts.iter().find(|account| account.name == name)
    }

    /// The selected account, while it is still in the list.
    fn selected_account(&self) -> Option<&BridgeAccount> {
        self.account(self.selected.as_deref()?)
    }

    /// The table's rows: name, role, rules, token count, state.
    fn rows(&self, cx: &App) -> Vec<Vec<SharedString>> {
        let every = i18n_account(cx, "servers_all");
        self.accounts
            .iter()
            .map(|account| {
                let servers: SharedString = match &account.servers {
                    Some(rules) => rules.join(", ").into(),
                    None => every.clone(),
                };
                vec![
                    account.name.clone().into(),
                    role_name(cx, account.role),
                    servers,
                    account.tokens.len().to_string().into(),
                    account_state(cx, account),
                ]
            })
            .collect()
    }
}

/// Whether an account can sign in, and how.
fn account_state(cx: &App, account: &BridgeAccount) -> SharedString {
    if account.disabled {
        i18n_account(cx, "disabled")
    } else if account.locked {
        i18n_account(cx, "locked_state")
    } else if !account.has_password {
        i18n_account(cx, "proxy_account")
    } else {
        i18n_account(cx, "active")
    }
}

impl ZedisAccount {
    pub(super) fn load_accounts(&mut self, cx: &mut Context<Self>) {
        let request = self.bridge.accounts();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(accounts) => {
                        this.admin.accounts = accounts;
                        // Someone else may have deleted it.
                        if this.admin.selected_account().is_none() {
                            this.admin.selected = None;
                        }
                        let rows = this.admin.rows(cx);
                        let none_selected = this.admin.selected.is_none();
                        this.admin.table.update(cx, |table, cx| {
                            table.delegate_mut().set_rows(rows);
                            // The highlight follows the form: a row left lit
                            // after its account went marks the next one.
                            if none_selected {
                                table.clear_selection(cx);
                            }
                        });
                    }
                    Err(e) => this.admin.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
        self.load_orphans(cx);
    }

    fn load_orphans(&mut self, cx: &mut Context<Self>) {
        let request = self.bridge.orphans();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(orphans) => this.admin.orphans = orphans,
                    Err(e) => this.admin.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Put `name`'s settings in the form under the table.
    pub(super) fn select_account(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.admin.account(name).cloned() else {
            return;
        };
        let ix = BridgeRole::ALL.iter().position(|role| *role == account.role);
        self.admin.role = cx.new(|cx| ZedisSelect::new(role_names(cx), ix, window, cx));
        self.admin._role_subscription = AdminForm::follow_role(&self.admin.role, cx);
        self.admin.role_value = account.role;
        let rules = account.servers.clone().unwrap_or_default().join("\n");
        self.admin
            .servers
            .update(cx, |state, cx| state.set_value(rules, window, cx));
        self.admin.disabled = account.disabled;
        self.admin.locked = account.locked;
        self.admin.selected = Some(account.name);
        cx.notify();
    }

    pub(super) fn render_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if std::mem::take(&mut self.admin.refill)
            && let Some(name) = self.admin.selected.clone()
        {
            self.select_account(&name, window, cx);
        }
        let table = fitted_table(
            &self.admin.table,
            DataTable::new(&self.admin.table)
                .stripe(true)
                .bordered(true)
                .scrollbar_visible(true, false),
        );
        v_flex()
            .gap_3()
            .children(self.render_orphans(cx))
            .child(div().w_full().h(px(TABLE_HEIGHT)).child(table))
            .children(status_line(cx, &self.admin.status))
            .child(match self.admin.selected_account().cloned() {
                Some(account) => self.render_account_form(&account, window, cx),
                None => note(cx, i18n_account(cx, "select_account")).into_any_element(),
            })
            .children(self.render_shown(cx))
            .into_any_element()
    }

    /// What deleted accounts left behind, each owner with a way to settle
    /// it — nothing at all while there is none.
    fn render_orphans(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.admin.orphans.is_empty() {
            return None;
        }
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let busy = self.admin.busy;
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(cx.theme().warning)
                .rounded(cx.theme().radius)
                .child(Label::new(i18n_account(cx, "orphans_title")).text_sm())
                .child(note(cx, i18n_account(cx, "orphans_intro")))
                .children(self.admin.orphans.iter().enumerate().map(|(ix, orphans)| {
                    let counts: SharedString = t!(
                        "account.orphans_counts",
                        servers = orphans.owned.servers.len(),
                        scripts = orphans.owned.scripts.len(),
                        locale = &locale
                    )
                    .into();
                    let owner = orphans.owner.clone();
                    let owned = orphans.owned.clone();
                    h_flex()
                        .id(("account-orphan-row", ix))
                        .gap_3()
                        .items_center()
                        .child(Label::new(orphans.owner.clone()).text_sm())
                        .child(note(cx, counts))
                        .child(
                            Button::new(SharedString::from(format!("{SETTLE_ORPHANS_ID}-{}", orphans.owner)))
                                .ghost()
                                .xsmall()
                                .label(i18n_account(cx, "orphans_settle"))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.ask_handover(Handover::Orphans, owner.clone(), owned.clone(), window, cx);
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    fn render_account_form(&mut self, account: &BridgeAccount, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let created: SharedString = t!("account.created", date = when(account.created_at), locale = &locale).into();
        let form = &self.admin;
        let tokens = (account.role == BridgeRole::ReadOnly).then(|| {
            let rows = self.render_token_rows(Some(&account.name), &account.tokens, cx);
            let form = &self.admin;
            v_flex()
                .gap_2()
                .child(Label::new(i18n_account(cx, "col_tokens")).text_sm())
                .child(rows)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .w(px(220.))
                                .child(Input::new(&form.token_name).id(ADMIN_TOKEN_NAME_ID)),
                        )
                        .child(
                            div().w(px(180.)).child(
                                DatePicker::new(&form.token_expires)
                                    .placeholder(i18n_account(cx, "expires_never"))
                                    .cleanable(true),
                            ),
                        )
                        .child(
                            Button::new(ADMIN_ISSUE_TOKEN_ID)
                                .outline()
                                .label(i18n_account(cx, "token_issue"))
                                .disabled(form.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.issue_token_for_selected(cx))),
                        ),
                )
        });
        v_flex()
            .gap_3()
            .pt_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(Label::new(account.name.clone()).text_sm().font_semibold())
                    .child(note(cx, created)),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_end()
                    .child(
                        div()
                            .w(px(200.))
                            .child(field(cx, i18n_account(cx, "col_role"), form.role.clone())),
                    )
                    .child(
                        Checkbox::new("account-disabled")
                            .checked(form.disabled)
                            .label(i18n_account(cx, "disabled"))
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.admin.disabled = *checked;
                                cx.notify();
                            })),
                    )
                    // Never an administrator's: it could not change its own
                    // password.
                    .when(form.role_value != BridgeRole::Admin, |row| {
                        row.child(
                            Checkbox::new(LOCK_ACCOUNT_ID)
                                .checked(form.locked)
                                .label(i18n_account(cx, "locked"))
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.admin.locked = *checked;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(form.role_value != BridgeRole::Admin, |page| {
                page.child(note(cx, i18n_account(cx, "locked_hint")))
            })
            .child(field(cx, i18n_account(cx, "col_servers"), Textarea::new(&form.servers)))
            .child(note(cx, i18n_account(cx, "servers_hint")))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        submit(SAVE_ACCOUNT_ID, i18n_common(cx, "save"), form.busy)
                            .icon(gpui_kit::assets::IconName::Check)
                            .on_click(cx.listener(|this, _, _, cx| this.save_selected(cx))),
                    )
                    .child(
                        Button::new("account-reset-link")
                            .outline()
                            .label(i18n_account(cx, "reset_password"))
                            .disabled(form.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.reset_link_for_selected(cx))),
                    )
                    .child(
                        Button::new(DELETE_ACCOUNT_ID)
                            .ghost()
                            .danger()
                            .label(i18n_account(cx, "delete"))
                            .disabled(form.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_delete_selected(window, cx))),
                    ),
            )
            .children(tokens)
            .into_any_element()
    }

    /// The form's change against the account as listed: only what differs.
    fn change_of_selected(&self, cx: &App) -> Option<(String, BridgeAccountChange)> {
        let account = self.admin.selected_account()?;
        let servers = rules_of(&self.admin.servers.read(cx).value());
        let change = BridgeAccountChange {
            role: (self.admin.role_value != account.role).then_some(self.admin.role_value),
            servers: (servers != account.servers).then_some(servers),
            disabled: (self.admin.disabled != account.disabled).then_some(self.admin.disabled),
            // Made an administrator, it is unlocked with it.
            locked: {
                let locked = self.admin.locked && self.admin.role_value != BridgeRole::Admin;
                (locked != account.locked).then_some(locked)
            },
        };
        Some((account.name.clone(), change))
    }

    pub(super) fn save_selected(&mut self, cx: &mut Context<Self>) {
        let Some((name, change)) = self.change_of_selected(cx) else {
            return;
        };
        if change == BridgeAccountChange::default() {
            return;
        }
        self.admin.busy = true;
        self.admin.status = None;
        cx.notify();
        let request = self.bridge.update_account(name.clone(), change);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.admin.busy = false;
                match result {
                    Ok(_) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.admin.status = Status::good(t!("account.saved", name = name, locale = locale));
                        this.load_accounts(cx);
                    }
                    Err(e) => {
                        let known = [(BridgeAccountErrorKind::Conflict, "last_admin")];
                        this.admin.status = Status::bad(refusal(cx, &e, &known));
                        this.admin.refill = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reset_link_for_selected(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.admin.selected.clone() else {
            return;
        };
        let request = self.bridge.reset_link(name.clone());
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(made) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let text = t!(
                            "account.reset_link_made",
                            name = name,
                            date = when(made.invite.expires_at),
                            locale = locale
                        );
                        let link = this.bridge.invite_link(&made.code);
                        this.show_once(text.into(), link);
                        this.load_invites(cx);
                    }
                    Err(e) => this.admin.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn issue_token_for_selected(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.admin.selected.clone() else {
            return;
        };
        let name = self.admin.token_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let expires = picked_day(self.admin.token_expires.read(cx));
        self.admin.busy = true;
        cx.notify();
        let request = self.bridge.issue_token(Some(account), name, expires);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.admin.busy = false;
                match result {
                    Ok(token) => {
                        this.show_once(i18n_account(cx, "token_shown_once"), token);
                        this.load_accounts(cx);
                    }
                    Err(e) => {
                        let known = [(BridgeAccountErrorKind::Conflict, "token_name_taken")];
                        this.admin.status = Status::bad(refusal(cx, &e, &known));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deleting ends the account's logins and tokens, and has to say what
    /// becomes of the server entries and scripts it kept to itself — so the
    /// bridge is asked what those are before the question is.
    fn confirm_delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.admin.selected.clone() else {
            return;
        };
        self.admin.busy = true;
        self.admin.status = None;
        cx.notify();
        let request = self.bridge.owned(name.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.admin.busy = false;
                match result {
                    Ok(owned) => this.ask_handover(Handover::Delete, name, owned, window, cx),
                    Err(e) => this.admin.status = Status::bad(refusal(cx, &e, &[])),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn delete_account(&mut self, name: String, private: BridgeDisposition, cx: &mut Context<Self>) {
        self.admin.busy = true;
        cx.notify();
        let request = self.bridge.delete_account(name.clone(), private);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.admin.busy = false;
                match result {
                    Ok(()) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.admin.status = Status::good(t!("account.deleted", name = name, locale = locale));
                        this.admin.selected = None;
                        this.load_accounts(cx);
                    }
                    Err(e) => {
                        let known = [(BridgeAccountErrorKind::Conflict, "last_admin")];
                        this.admin.status = Status::bad(refusal(cx, &e, &known));
                        this.admin.refill = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn settle_orphans(&mut self, owner: String, private: BridgeDisposition, cx: &mut Context<Self>) {
        self.admin.busy = true;
        cx.notify();
        let request = self.bridge.settle_orphans(owner.clone(), private);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.admin.busy = false;
                match result {
                    Ok(()) => {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        this.admin.status = Status::good(t!("account.settled", name = owner, locale = locale));
                    }
                    Err(e) => this.admin.status = Status::bad(refusal(cx, &e, &[])),
                }
                this.load_accounts(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

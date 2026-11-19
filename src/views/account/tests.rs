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

//! The account page against a bridge of its own: what it shows a role, what
//! it checks before it asks, and what it asks.

use super::admin::{
    ADMIN_ISSUE_TOKEN_ID, ADMIN_TOKEN_NAME_ID, DELETE_ACCOUNT_ID, LOCK_ACCOUNT_ID, SAVE_ACCOUNT_ID, SETTLE_ORPHANS_ID,
};
use super::audit::{AUDIT_ACCOUNT_ID, AUDIT_MORE_ID, AUDIT_SEARCH_ID};
use super::handover::SHARE_ID;
use super::online::ONLINE_REFRESH_ID;
use super::password::{CHANGE_PASSWORD_ID, CURRENT_PASSWORD_ID, NEW_PASSWORD_ID, REPEAT_PASSWORD_ID};
use super::signups::{SIGNUP_APPROVE_ID, SIGNUP_REJECT_ID, signup_row_id};
use super::tokens::{ADMIN_TOKEN_DAYS, ISSUE_TOKEN_ID, TOKEN_NAME_ID};
use super::*;
use crate::connection::{
    BridgeAccountChange, BridgeAccountResult, BridgeAuditPage, BridgeAuditQuery, BridgeClient, BridgeClients,
    BridgeDisposition, BridgeInvite, BridgeNewInvite, BridgeOrphans, BridgeOwned, BridgeOwnedItem, BridgeSignup,
};
use crate::test_support::init_ui;
use gpui::{AnyWindowHandle, TestAppContext, size};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use jiff::{Span, Timestamp, tz::TimeZone};
use std::sync::Mutex;
use zedis_ui::ZedisSelectEvent;

/// A bridge that answers at once, and writes down what it was asked.
#[derive(Default)]
struct FakeBridge {
    me: Mutex<BridgeOwnAccount>,
    accounts: Mutex<Vec<BridgeAccount>>,
    /// What the page asked, one line a request.
    asked: Mutex<Vec<String>>,
    updates: Mutex<Vec<(String, BridgeAccountChange)>>,
    /// The refusal the next password change gets.
    refuse_password: Mutex<Option<BridgeAccountError>>,
    /// The refusal the next account change gets.
    refuse_update: Mutex<Option<BridgeAccountError>>,
    /// What every account keeps to itself.
    owned: Mutex<BridgeOwned>,
    orphans: Mutex<Vec<BridgeOrphans>>,
    /// The page every audit question gets, and the questions.
    audit_page: Mutex<BridgeAuditPage>,
    audits: Mutex<Vec<BridgeAuditQuery>>,
    /// The requests waiting; an approval or a rejection takes one out.
    signups: Mutex<Vec<BridgeSignup>>,
    /// Who the bridge says is online.
    clients: Mutex<BridgeClients>,
}

impl FakeBridge {
    fn signed_in_as(name: &str, role: BridgeRole) -> Arc<Self> {
        let bridge = Self::default();
        *bridge.me.lock().expect("lock") = BridgeOwnAccount {
            account: account(name, role),
            proxy: false,
            signup: false,
        };
        Arc::new(bridge)
    }

    fn asked(&self, line: String) {
        self.asked.lock().expect("lock").push(line);
    }

    fn requests(&self) -> Vec<String> {
        self.asked.lock().expect("lock").clone()
    }
}

fn account(name: &str, role: BridgeRole) -> BridgeAccount {
    BridgeAccount {
        name: name.to_string(),
        role,
        has_password: true,
        created_at: 1_700_000_000,
        ..Default::default()
    }
}

fn answer<T: Send + 'static>(value: Result<T, BridgeAccountError>) -> BridgeAccountResult<T> {
    Box::pin(async move { value })
}

impl BridgeAccounts for FakeBridge {
    fn account(&self) -> BridgeAccountResult<BridgeOwnAccount> {
        answer(Ok(self.me.lock().expect("lock").clone()))
    }

    fn change_password(&self, current: String, new: String) -> BridgeAccountResult<()> {
        self.asked(format!("password {current} -> {new}"));
        match self.refuse_password.lock().expect("lock").take() {
            Some(refusal) => answer(Err(refusal)),
            None => answer(Ok(())),
        }
    }

    fn issue_token(
        &self,
        account: Option<String>,
        name: String,
        expires: Option<String>,
    ) -> BridgeAccountResult<String> {
        self.asked(format!("issue {account:?} {name} {expires:?}"));
        answer(Ok(format!("zdt_{name}")))
    }

    fn revoke_token(&self, account: Option<String>, name: String) -> BridgeAccountResult<()> {
        self.asked(format!("revoke {account:?} {name}"));
        answer(Ok(()))
    }

    fn accounts(&self) -> BridgeAccountResult<Vec<BridgeAccount>> {
        answer(Ok(self.accounts.lock().expect("lock").clone()))
    }

    fn update_account(&self, name: String, change: BridgeAccountChange) -> BridgeAccountResult<BridgeAccount> {
        self.updates.lock().expect("lock").push((name.clone(), change));
        match self.refuse_update.lock().expect("lock").take() {
            Some(refusal) => answer(Err(refusal)),
            None => answer(Ok(account(&name, BridgeRole::Full))),
        }
    }

    fn owned(&self, _name: String) -> BridgeAccountResult<BridgeOwned> {
        answer(Ok(self.owned.lock().expect("lock").clone()))
    }

    fn delete_account(&self, name: String, private: BridgeDisposition) -> BridgeAccountResult<()> {
        self.asked(format!("delete {name} {private:?}"));
        answer(Ok(()))
    }

    fn orphans(&self) -> BridgeAccountResult<Vec<BridgeOrphans>> {
        answer(Ok(self.orphans.lock().expect("lock").clone()))
    }

    fn audit(&self, query: BridgeAuditQuery) -> BridgeAccountResult<BridgeAuditPage> {
        self.audits.lock().expect("lock").push(query);
        answer(Ok(self.audit_page.lock().expect("lock").clone()))
    }

    fn clients(&self) -> BridgeAccountResult<BridgeClients> {
        self.asked("clients".to_string());
        answer(Ok(self.clients.lock().expect("lock").clone()))
    }

    fn settle_orphans(&self, owner: String, private: BridgeDisposition) -> BridgeAccountResult<()> {
        self.asked(format!("settle {owner} {private:?}"));
        answer(Ok(()))
    }

    fn reset_link(&self, name: String) -> BridgeAccountResult<BridgeNewInvite> {
        self.asked(format!("reset {name}"));
        answer(Err(BridgeAccountError::new(BridgeAccountErrorKind::Failed, "not here")))
    }

    fn invites(&self) -> BridgeAccountResult<Vec<BridgeInvite>> {
        answer(Ok(Vec::new()))
    }

    fn create_invite(
        &self,
        role: BridgeRole,
        _servers: Option<Vec<String>>,
        _proxy: bool,
        days: u64,
    ) -> BridgeAccountResult<BridgeNewInvite> {
        self.asked(format!("invite {} {days}", role.as_str()));
        answer(Err(BridgeAccountError::new(BridgeAccountErrorKind::Failed, "not here")))
    }

    fn revoke_invite(&self, id: String) -> BridgeAccountResult<()> {
        self.asked(format!("revoke invite {id}"));
        answer(Ok(()))
    }

    fn signups(&self) -> BridgeAccountResult<Vec<BridgeSignup>> {
        answer(Ok(self.signups.lock().expect("lock").clone()))
    }

    fn approve_signup(
        &self,
        id: String,
        role: BridgeRole,
        servers: Option<Vec<String>>,
    ) -> BridgeAccountResult<BridgeAccount> {
        self.asked(format!("approve {id} {} {servers:?}", role.as_str()));
        let mut waiting = self.signups.lock().expect("lock");
        let Some(at) = waiting.iter().position(|request| request.id == id) else {
            return answer(Err(BridgeAccountError::new(BridgeAccountErrorKind::NotFound, "gone")));
        };
        let request = waiting.remove(at);
        answer(Ok(account(&request.name, role)))
    }

    fn reject_signup(&self, id: String) -> BridgeAccountResult<()> {
        self.asked(format!("reject {id}"));
        self.signups.lock().expect("lock").retain(|request| request.id != id);
        answer(Ok(()))
    }

    fn invite_link(&self, code: &str) -> String {
        format!("https://bridge.test/#invite={code}")
    }
}

/// The page in a window of its own, with what the bridge first answered —
/// tall enough that a selected account's whole form is on screen.
fn open(cx: &mut TestAppContext, bridge: Arc<FakeBridge>) -> (Entity<ZedisAccount>, AnyWindowHandle) {
    init_ui(cx);
    let mut view = None;
    let handle = cx.open_window(size(px(900.), px(1200.)), |window, cx| {
        let account = cx.new(|cx| ZedisAccount::new(bridge.clone(), window, cx));
        view = Some(account.clone());
        Root::new(account, window, cx)
    });
    cx.run_until_parked();
    (view.expect("the window built the page"), handle.into())
}

/// A read-only account has its password and its MCP tokens, and nothing of
/// an administrator's; it is never asked for the accounts.
#[gpui::test]
fn a_read_only_account_has_its_tokens_and_no_administration(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("ai", BridgeRole::ReadOnly);
    let (_, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("section_password").is_some());
        assert!(window.try_find("section_tokens").is_some());
        assert!(window.try_find("section_accounts").is_none());
        assert!(window.try_find("section_invites").is_none());
        assert!(window.try_find("section_audit").is_none());
        assert!(
            window.try_find("section_online").is_none(),
            "the list names other accounts"
        );
    })
    .expect("the window is open");
}

/// A full account has its password and nothing of an administrator's — not
/// who is online either, which names the other accounts and where they call
/// from — and is never asked for them.
#[gpui::test]
fn a_full_account_has_its_password_and_no_administration(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Full);
    let (_, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("section_password").is_some());
        for section in ["section_online", "section_accounts", "section_invites", "section_audit"] {
            assert!(window.try_find(section).is_none(), "{section}");
        }
    })
    .expect("the window is open");
    assert!(
        !bridge.requests().contains(&"clients".to_string()),
        "never asked who is online"
    );
}

/// An administrator sees who is using the bridge, read each time the
/// section is opened: one row a client, with the servers it is on.
#[gpui::test]
fn an_administrator_sees_who_is_online_and_on_which_servers(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.clients.lock().expect("lock") = BridgeClients {
        process: "0123456789abcdef".to_string(),
        window_secs: 300,
        clients: vec![
            BridgeClient {
                account: "alice".to_string(),
                kind: "browser".to_string(),
                peer: Some("10.0.0.7".to_string()),
                seen_at: 1_700_000_000,
                servers: vec!["prod".to_string()],
                ..Default::default()
            },
            BridgeClient {
                account: "ai".to_string(),
                kind: "token".to_string(),
                token: Some("claude-laptop".to_string()),
                seen_at: 1_700_000_000,
                servers: vec!["prod".to_string(), "staging".to_string()],
                ..Default::default()
            },
        ],
    };
    let (view, window) = open(cx, bridge.clone());
    assert!(
        !bridge.requests().contains(&"clients".to_string()),
        "not before it is looked at"
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("section_online").is_some());
        window.click("section_online", cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
        let table = view.online.table.read(cx).delegate();
        assert_eq!(table.visible_len(), 2);
        let rows: Vec<Vec<SharedString>> = table.visible_iter().map(<[SharedString]>::to_vec).collect();
        assert_eq!(rows[1][1].as_ref(), "MCP token · claude-laptop");
        assert_eq!(rows[1][3].as_ref(), "prod, staging");
    });

    click(cx, window, ONLINE_REFRESH_ID);
    let asked = bridge.requests().iter().filter(|line| *line == "clients").count();
    assert_eq!(asked, 2, "asked again for a refresh");
}

/// A short or mistyped new password is caught on the page; a right one is
/// sent, said to be changed, and the fields are emptied.
#[gpui::test]
fn a_password_is_checked_before_it_is_sent(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Full);
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, text) in [
            (CURRENT_PASSWORD_ID, "old-password"),
            (NEW_PASSWORD_ID, "short"),
            (REPEAT_PASSWORD_ID, "short"),
        ] {
            window.click(id, cx);
            window.input(text, cx);
        }
        window.click(CHANGE_PASSWORD_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    let status = view.read_with(cx, |view, _| view.password.status.clone());
    assert_eq!(status, Status::bad("A password has at least 8 characters."));
    assert!(bridge.requests().is_empty(), "nothing is sent for a short one");

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for id in [NEW_PASSWORD_ID, REPEAT_PASSWORD_ID] {
            window.click(id, cx);
            window.input("-but-longer", cx);
        }
        window.click(CHANGE_PASSWORD_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(bridge.requests(), vec!["password old-password -> short-but-longer"]);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert!(view.password.status.as_ref().is_some_and(|status| status.good));
        assert_eq!(view.password.current.read(cx).value(), "");
    })
    .expect("the window is open");
}

/// The page has its own words for the refusals it knows.
#[gpui::test]
fn a_wrong_current_password_is_said_in_the_page_s_words(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Full);
    *bridge.refuse_password.lock().expect("lock") = Some(BridgeAccountError::new(
        BridgeAccountErrorKind::Refused,
        "the current password is not right",
    ));
    let (view, window) = open(cx, bridge);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        for (id, text) in [
            (CURRENT_PASSWORD_ID, "wrong"),
            (NEW_PASSWORD_ID, "a-new-password"),
            (REPEAT_PASSWORD_ID, "a-new-password"),
        ] {
            window.click(id, cx);
            window.input(text, cx);
        }
        window.click(CHANGE_PASSWORD_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    let status = view.read_with(cx, |view, _| view.password.status.clone());
    assert_eq!(status, Status::bad("The current password is not right."));
}

/// An administrator's save sends what the form changed and nothing else —
/// one setting, one line in the audit log.
#[gpui::test]
fn an_administrator_saves_only_what_changed(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") = vec![
        account("alice", BridgeRole::Admin),
        BridgeAccount {
            servers: Some(vec!["prod-*:ro".to_string()]),
            ..account("bob", BridgeRole::Full)
        },
    ];
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.admin.table.read(cx).delegate().visible_len(), 2);
            view.select_section(AccountSection::Accounts, window, cx);
            view.select_account("bob", window, cx);
        });
        window.render_frame(cx);
        window.click("account-disabled", cx);
        window.click(SAVE_ACCOUNT_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    let updates = bridge.updates.lock().expect("lock").clone();
    assert_eq!(
        updates,
        vec![(
            "bob".to_string(),
            BridgeAccountChange {
                role: None,
                servers: None,
                disabled: Some(true),
                locked: None,
            }
        )]
    );
}

/// An administrator locks an account from its form — never an
/// administrator, whose form has no such box — and a locked account's own
/// page offers no password to change and no tokens.
#[gpui::test]
fn a_locked_account_has_nothing_of_its_own_to_change(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") = vec![
        account("alice", BridgeRole::Admin),
        account("demo", BridgeRole::ReadOnly),
    ];
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_section(AccountSection::Accounts, window, cx);
            view.select_account("alice", window, cx);
        });
        window.render_frame(cx);
        assert!(
            window.try_find(LOCK_ACCOUNT_ID).is_none(),
            "an administrator is never locked"
        );
        view.update(cx, |view, cx| view.select_account("demo", window, cx));
        window.render_frame(cx);
        window.click(LOCK_ACCOUNT_ID, cx);
        window.click(SAVE_ACCOUNT_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    let updates = bridge.updates.lock().expect("lock").clone();
    assert_eq!(
        updates,
        vec![(
            "demo".to_string(),
            BridgeAccountChange {
                locked: Some(true),
                ..BridgeAccountChange::default()
            }
        )]
    );

    let demo = FakeBridge::signed_in_as("demo", BridgeRole::ReadOnly);
    demo.me.lock().expect("lock").account.locked = true;
    let (view, window) = open(cx, demo);
    view.read_with(cx, |view, _| {
        let me = view.me.as_ref().expect("signed in");
        assert_eq!(
            AccountSection::of(me),
            vec![AccountSection::Password],
            "no tokens of its own"
        );
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(CURRENT_PASSWORD_ID).is_none(), "no password to change");
    })
    .expect("the window is open");
}

/// A change the bridge refuses — the last administrator disabled — is said
/// in the page's words, and the form goes back to what the account is: a
/// ticked box left behind would read as a change that was kept.
#[gpui::test]
fn a_refused_change_puts_the_form_back(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") = vec![account("alice", BridgeRole::Admin)];
    *bridge.refuse_update.lock().expect("lock") = Some(BridgeAccountError::new(
        BridgeAccountErrorKind::Conflict,
        "the bridge would be left without an administrator",
    ));
    let (view, window) = open(cx, bridge);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_section(AccountSection::Accounts, window, cx);
            view.select_account("alice", window, cx);
        });
        window.render_frame(cx);
        window.click("account-disabled", cx);
        assert!(view.read(cx).admin.disabled);
        window.click(SAVE_ACCOUNT_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let view = view.read(cx);
        assert_eq!(
            view.admin.status,
            Status::bad("The last administrator cannot be demoted, disabled or deleted.")
        );
        assert!(!view.admin.disabled, "the form shows the account as it is");
    })
    .expect("the window is open");
}

/// A token is shown once, where it was issued, with its name sent and no
/// expiry when none was picked.
#[gpui::test]
fn an_issued_token_is_shown_once(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("ai", BridgeRole::ReadOnly);
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_section(AccountSection::Tokens, window, cx));
        window.render_frame(cx);
        window.click(TOKEN_NAME_ID, cx);
        window.input("laptop", cx);
        window.click(ISSUE_TOKEN_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(bridge.requests(), vec!["issue None laptop None"]);
    let shown = view.read_with(cx, |view, _| view.shown.clone()).expect("shown");
    assert_eq!(shown.value.as_ref(), "zdt_laptop");
    assert_eq!(shown.section, AccountSection::Tokens);
}

/// An administrator's token for an account expires in ninety days unless
/// another day is picked: one with no end is a choice, not what is left
/// when nothing was.
#[gpui::test]
fn an_administrator_s_token_expires_in_ninety_days_by_default(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") =
        vec![account("alice", BridgeRole::Admin), account("ai", BridgeRole::ReadOnly)];
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_section(AccountSection::Accounts, window, cx);
            view.select_account("ai", window, cx);
        });
        window.render_frame(cx);
        window.click(ADMIN_TOKEN_NAME_ID, cx);
        window.input("laptop", cx);
        window.click(ADMIN_ISSUE_TOKEN_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    let day = (Timestamp::now().to_zoned_datetime(TimeZone::UTC).date() + Span::new().days(ADMIN_TOKEN_DAYS))
        .strftime("%Y-%m-%d");
    assert_eq!(
        bridge.requests(),
        vec![format!("issue Some(\"ai\") laptop Some(\"{day}\")")]
    );
}

fn owning(servers: &[&str]) -> BridgeOwned {
    BridgeOwned {
        servers: servers
            .iter()
            .map(|name| BridgeOwnedItem {
                id: format!("id-{name}"),
                name: name.to_string(),
            })
            .collect(),
        scripts: Vec::new(),
    }
}

/// Click `id` in the window, then let what it started finish.
fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<SharedString>) {
    let id: SharedString = id.into();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
}

/// What a deleted account kept to itself goes to the administrator who
/// deletes it unless another answer is picked — and sharing it, which hands
/// every account its saved passwords, is never what is picked for them.
#[gpui::test]
fn a_deleted_account_s_own_go_to_the_administrator_unless_told_otherwise(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") =
        vec![account("alice", BridgeRole::Admin), account("bob", BridgeRole::Full)];
    *bridge.owned.lock().expect("lock") = owning(&["bob's cache"]);
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_section(AccountSection::Accounts, window, cx);
            view.select_account("bob", window, cx);
        });
    })
    .expect("the window is open");
    click(cx, window, DELETE_ACCOUNT_ID);
    click(cx, window, "zedis-dialog-ok");
    assert_eq!(bridge.requests(), vec![r#"delete bob HandTo("alice")"#]);

    // Asked again, and told to share.
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_account("bob", window, cx));
    })
    .expect("the window is open");
    click(cx, window, DELETE_ACCOUNT_ID);
    click(cx, window, SHARE_ID);
    click(cx, window, "zedis-dialog-ok");
    assert_eq!(bridge.requests().last().map(String::as_str), Some("delete bob Share"));
}

/// What an account deleted before handovers left behind is listed, and
/// settled the same way.
#[gpui::test]
fn what_a_deleted_account_left_is_listed_and_settled(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.accounts.lock().expect("lock") = vec![account("alice", BridgeRole::Admin)];
    *bridge.orphans.lock().expect("lock") = vec![BridgeOrphans {
        owner: "ghost".to_string(),
        owned: owning(&["ghost's cache"]),
    }];
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_section(AccountSection::Accounts, window, cx));
    })
    .expect("the window is open");
    click(cx, window, format!("{SETTLE_ORPHANS_ID}-ghost"));
    click(cx, window, "zedis-dialog-ok");
    assert_eq!(bridge.requests(), vec![r#"settle ghost HandTo("alice")"#]);
}

/// An administrator's audit is read when its section is first opened,
/// narrowed by what the fields hold, and continued from where a page ended.
#[gpui::test]
fn the_audit_is_read_narrowed_and_continued(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    let line = |event: &str, account: &str| {
        format!(r#"{{"ts":"2026-10-18T07:14:58.219Z","account":"{account}","event":"{event}"}}"#)
    };
    *bridge.audit_page.lock().expect("lock") = BridgeAuditPage {
        lines: vec![line("logout", "bob"), line("login", "bob")],
        next: Some("7".to_string()),
        stopped_early: false,
    };
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_section(AccountSection::Audit, window, cx));
    })
    .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(
        bridge.audits.lock().expect("lock").clone(),
        vec![BridgeAuditQuery::default()],
        "read when first opened, everything"
    );
    view.read_with(cx, |view, cx| {
        assert_eq!(view.audit.table.read(cx).delegate().visible_len(), 2);
    });

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(AUDIT_ACCOUNT_ID, cx);
        window.input("bob", cx);
        window.click(AUDIT_SEARCH_ID, cx);
    })
    .expect("the window is open");
    cx.run_until_parked();
    click(cx, window, AUDIT_MORE_ID);
    let bob = BridgeAuditQuery {
        account: Some("bob".to_string()),
        ..BridgeAuditQuery::default()
    };
    assert_eq!(
        bridge.audits.lock().expect("lock")[1..].to_vec(),
        vec![
            bob.clone(),
            BridgeAuditQuery {
                before: Some("7".to_string()),
                ..bob
            }
        ],
        "narrowed, then the next page of the same question"
    );
    view.read_with(cx, |view, cx| {
        assert_eq!(view.audit.lines.len(), 4, "the older page is added below");
        assert_eq!(view.audit.table.read(cx).delegate().visible_len(), 4);
    });
}

/// The audit narrows to a server and to how far back as picked: the
/// server by id, the period as where it starts.
#[gpui::test]
fn the_audit_narrows_to_a_server_and_a_period(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    let (view, window) = open(cx, bridge.clone());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.select_section(AccountSection::Audit, window, cx);
            view.audit.servers = vec!["srv-1".to_string(), "srv-2".to_string()];
            view.audit.server_index = 2;
            view.audit.period_index = 1;
        });
    })
    .expect("the window is open");
    cx.run_until_parked();
    let before = Timestamp::now().as_millisecond();
    click(cx, window, AUDIT_SEARCH_ID);
    let asked = bridge.audits.lock().expect("lock").last().cloned().expect("asked");
    assert_eq!(asked.server.as_deref(), Some("srv-2"));
    let since = asked.since_ms.expect("a period");
    let hour = 3_600_000;
    assert!(
        (before - hour - 5_000..=before - hour + 5_000).contains(&since),
        "{since} is an hour back from {before}"
    );
}

/// A new search starts with no line picked: a highlight left on the row that
/// was picked would mark a line of the new answer nobody chose, over a detail
/// that says to pick one.
#[gpui::test]
fn a_new_search_leaves_no_line_picked(cx: &mut TestAppContext) {
    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    *bridge.audit_page.lock().expect("lock") = BridgeAuditPage {
        lines: vec![r#"{"ts":"2026-10-18T07:14:58.219Z","account":"bob","event":"login"}"#.to_string()],
        next: None,
        stopped_early: false,
    };
    let (view, window) = open(cx, bridge);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_section(AccountSection::Audit, window, cx));
    })
    .expect("the window is open");
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        view.audit.table.update(cx, |table, cx| table.set_selected_row(0, cx));
    });
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
        assert_eq!(view.audit.table.read(cx).selected_row(), Some(0));
    });

    click(cx, window, AUDIT_SEARCH_ID);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.audit.table.read(cx).selected_row(), None, "nothing is marked");
    });
}

/// The requests for an account are an administrator's section only on a
/// bridge that takes them; a request picked is approved with the role chosen
/// and the rules typed, one for a taken name cannot be, and a rejection
/// takes one away.
#[gpui::test]
fn requests_are_approved_with_a_role_and_rejected(cx: &mut TestAppContext) {
    let shut = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    let (view, _) = open(cx, shut);
    view.read_with(cx, |view, _| {
        let me = view.me.as_ref().expect("signed in");
        assert!(
            !AccountSection::of(me).contains(&AccountSection::Signups),
            "a shut door has no section"
        );
    });

    let bridge = FakeBridge::signed_in_as("alice", BridgeRole::Admin);
    bridge.me.lock().expect("lock").signup = true;
    *bridge.signups.lock().expect("lock") = vec![
        BridgeSignup {
            id: "r1".to_string(),
            name: "dana".to_string(),
            note: "on call next week".to_string(),
            requested_at: 1_700_000_000,
            expires_at: 1_800_000_000,
            ..Default::default()
        },
        BridgeSignup {
            id: "r2".to_string(),
            name: "alice".to_string(),
            requested_at: 1_700_000_100,
            expires_at: 1_800_000_000,
            name_taken: true,
            ..Default::default()
        },
    ];
    let (view, window) = open(cx, bridge.clone());
    view.read_with(cx, |view, _| {
        let me = view.me.as_ref().expect("signed in");
        assert!(AccountSection::of(me).contains(&AccountSection::Signups));
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_section(AccountSection::Signups, window, cx));
    })
    .expect("the window is open");
    cx.run_until_parked();

    // The one for a taken name cannot be approved.
    click(cx, window, signup_row_id("r2"));
    click(cx, window, SIGNUP_APPROVE_ID);
    assert!(
        !bridge.requests().iter().any(|line| line.starts_with("approve")),
        "{:?}",
        bridge.requests()
    );

    click(cx, window, signup_row_id("r1"));
    // The role picker's answer and the rules typed, as the form hears them.
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.signups
                .role
                .update(cx, |_, cx| cx.emit(ZedisSelectEvent::Change(1)));
            view.signups
                .servers
                .update(cx, |state, cx| state.set_value("staging", window, cx));
        });
    })
    .expect("the window is open");
    cx.run_until_parked();
    click(cx, window, SIGNUP_APPROVE_ID);
    assert!(
        bridge
            .requests()
            .contains(&r#"approve r1 full Some(["staging"])"#.to_string()),
        "{:?}",
        bridge.requests()
    );
    view.read_with(cx, |view, _| {
        assert_eq!(view.signups.signups.len(), 1, "read again once answered");
    });

    click(cx, window, signup_row_id("r2"));
    click(cx, window, SIGNUP_REJECT_ID);
    assert!(bridge.requests().contains(&"reject r2".to_string()));
    view.read_with(cx, |view, _| assert!(view.signups.signups.is_empty()));
}

/// The desktop has no bridge to ask, so *Account…* — were anything to send
/// it — opens nothing.
#[gpui::test]
fn the_desktop_opens_no_account_page(cx: &mut TestAppContext) {
    let (_, window) = open(cx, FakeBridge::signed_in_as("alice", BridgeRole::Full));
    cx.update_window(window, |_, window, cx| open_account(window, cx))
        .expect("the window is open");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        assert!(!window.has_active_dialog(cx));
    })
    .expect("the window is open");
}

#[test]
fn rules_are_one_a_line_and_none_is_every_shared_entry() {
    assert_eq!(rules_of("  \n , "), None);
    assert_eq!(
        rules_of("prod-*:ro\nstaging, id:0199"),
        Some(vec![
            "prod-*:ro".to_string(),
            "staging".to_string(),
            "id:0199".to_string()
        ])
    );
}

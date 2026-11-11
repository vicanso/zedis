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

//! Requests for an account (ADR 17, amended 2026-10-18), where `--signup
//! approval` opens that door: each waits for an administrator to approve it,
//! with a role and rules, or reject it, and signs nobody in until then.

use super::*;

/// Whether the bridge takes requests for an account (`--signup`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Signup {
    /// Accounts come by invitation only.
    #[default]
    Off,
    /// Anyone may ask; an administrator approves or rejects.
    Approval,
}

impl Signup {
    /// What `--signup` or [`SIGNUP_ENV`] says; nothing is off.
    pub fn from_setting(setting: Option<&str>) -> Result<Self, String> {
        match setting.map(str::trim).unwrap_or_default() {
            "" | "off" => Ok(Signup::Off),
            "approval" => Ok(Signup::Approval),
            other => Err(format!("--signup takes approval or off, not {other}")),
        }
    }

    pub fn is_open(self) -> bool {
        self == Signup::Approval
    }
}

/// A request for an account as the administrators' list shows it — never
/// its password's hash.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SignupView {
    pub id: String,
    pub name: String,
    pub note: String,
    pub peer: Option<String>,
    pub requested_at: u64,
    pub expires_at: u64,
    /// An account has this name now: approving it is refused.
    pub name_taken: bool,
}

impl SignupView {
    fn of(request: &SignupRecord, name_taken: bool) -> Self {
        Self {
            id: request.id.clone(),
            name: request.name.clone(),
            note: request.note.clone(),
            peer: request.peer.clone(),
            requested_at: request.requested_at,
            expires_at: request.expires_at,
            name_taken,
        }
    }
}

impl Accounts {
    /// Every request waiting for an answer, oldest first: the order they
    /// came in, which is the order they are owed one.
    pub fn signups(&self) -> Vec<SignupView> {
        let now = now_secs();
        let state = self.read();
        let mut views: Vec<SignupView> = state
            .signups
            .values()
            .filter(|request| request.expires_at > now)
            .map(|request| SignupView::of(request, state.accounts.contains_key(&request.name)))
            .collect();
        views.sort_by(|a, b| a.requested_at.cmp(&b.requested_at).then_with(|| a.id.cmp(&b.id)));
        views
    }

    /// A request for `name`, waiting for an administrator. The name is
    /// checked for its form and never against the accounts — the answer
    /// would say which exist. Answers the request's id.
    pub async fn request_signup(
        &self,
        name: &str,
        password_hash: String,
        note: &str,
        peer: Option<String>,
    ) -> Result<String, AccountError> {
        check_new_name(name)?;
        let note: String = note.trim().chars().take(MAX_SIGNUP_NOTE_CHARS).collect();
        self.change(|state| {
            let now = now_secs();
            if state
                .signups
                .values()
                .filter(|request| request.expires_at > now)
                .count()
                >= MAX_SIGNUPS
            {
                return Err(AccountError::TooManySignups);
            }
            let request = SignupRecord {
                id: Uuid::now_v7().simple().to_string(),
                name: name.to_string(),
                password: password_hash.clone(),
                note: note.clone(),
                peer: peer.clone(),
                requested_at: now,
                expires_at: now + SIGNUP_DAYS * 86_400,
            };
            let id = request.id.clone();
            Ok((vec![AccountOp::PutSignup(request)], id))
        })
        .await
    }

    /// Make the account the request `id` asks for: its name and password,
    /// with `role` and `servers`. Refused when an account has the name by
    /// now. Answers the new account.
    pub async fn approve_signup(
        &self,
        id: &str,
        role: Role,
        servers: Option<Vec<String>>,
    ) -> Result<AccountView, AccountError> {
        check_rules(&servers)?;
        self.change(|state| {
            let request = state
                .signups
                .get(id)
                .filter(|request| request.expires_at > now_secs())
                .ok_or(AccountError::NotFound)?;
            check_new_name(&request.name)?;
            if state.accounts.contains_key(&request.name) {
                return Err(AccountError::NameTaken);
            }
            let record = AccountRecord {
                name: request.name.clone(),
                role,
                password: Some(request.password.clone()),
                servers: servers.clone(),
                version: 1,
                created_at: now_secs(),
                ..Default::default()
            };
            let view = AccountView::from(&record);
            Ok((
                vec![AccountOp::PutAccount(record), AccountOp::RemoveSignup(id.to_string())],
                view,
            ))
        })
        .await
    }

    /// Delete the request `id` unanswered. Answers the name it asked for.
    pub async fn reject_signup(&self, id: &str) -> Result<String, AccountError> {
        self.change(|state| {
            let request = state.signups.get(id).ok_or(AccountError::NotFound)?;
            Ok((vec![AccountOp::RemoveSignup(id.to_string())], request.name.clone()))
        })
        .await
    }

    /// Forget the requests nobody answered in time. Answers how many went.
    pub async fn purge_expired_signups(&self) -> Result<usize, AccountError> {
        let now = now_secs();
        self.change(|state| {
            let ops: Vec<AccountOp> = state
                .signups
                .values()
                .filter(|request| request.expires_at <= now)
                .map(|request| AccountOp::RemoveSignup(request.id.clone()))
                .collect();
            let count = ops.len();
            Ok((ops, count))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::tests::with_admin;

    /// A request is no account: it signs nobody in until an administrator
    /// approves it, and then the account is the name and password it asked
    /// with and the role and rules the administrator gave it.
    #[tokio::test]
    async fn a_request_is_no_account_until_an_administrator_approves_it() {
        let accounts = with_admin("alice@secret").await;
        let hash = hash_password("dana-password").expect("hash");
        let id = accounts
            .request_signup("dana", hash, "  on call next week  ", Some("10.0.0.9".to_string()))
            .await
            .expect("asked");
        assert!(!accounts.has("dana") && !accounts.exists("dana"));
        assert!(
            !accounts.accepts_password("dana", "dana-password"),
            "a request signs nobody in"
        );
        assert_eq!(
            accounts
                .check_basic_credentials("dana".to_string(), "dana-password".to_string())
                .await,
            Basic::Refused("dana".to_string())
        );
        let waiting = accounts.signups();
        assert_eq!(waiting.len(), 1);
        assert_eq!(
            (waiting[0].id.as_str(), waiting[0].name.as_str()),
            (id.as_str(), "dana")
        );
        assert_eq!(waiting[0].note, "on call next week");
        assert_eq!(waiting[0].peer.as_deref(), Some("10.0.0.9"));
        assert!(!waiting[0].name_taken);

        let made = accounts
            .approve_signup(&id, Role::Full, Some(vec!["staging".to_string()]))
            .await
            .expect("approved");
        assert_eq!((made.name.as_str(), made.role), ("dana", Role::Full));
        assert_eq!(made.servers, Some(vec!["staging".to_string()]));
        assert!(accounts.has("dana") && !accounts.is_read_only("dana"));
        assert!(
            accounts.accepts_password("dana", "dana-password"),
            "the password it asked with"
        );
        assert!(accounts.signups().is_empty(), "answered");
        assert!(matches!(
            accounts.approve_signup(&id, Role::Full, None).await,
            Err(AccountError::NotFound)
        ));
    }

    /// Asking for a name that is an account is answered like any request —
    /// what is taken is not told to whoever asks — and the administrator
    /// sees it marked, and cannot approve it.
    #[tokio::test]
    async fn a_request_for_a_taken_name_is_kept_and_refused_at_approval() {
        let accounts = with_admin("alice@secret").await;
        let id = accounts
            .request_signup("alice", hash_password("not-alices").expect("hash"), "", None)
            .await
            .expect("asked, whatever the name");
        assert!(accounts.signups()[0].name_taken);
        assert!(matches!(
            accounts.approve_signup(&id, Role::ReadOnly, None).await,
            Err(AccountError::NameTaken)
        ));
        assert!(accounts.accepts_password("alice", "secret"), "alice is untouched");
        assert_eq!(accounts.reject_signup(&id).await.expect("rejected"), "alice");
        assert!(accounts.signups().is_empty());
        assert!(matches!(
            accounts.request_signup("a:b", "hash".to_string(), "", None).await,
            Err(AccountError::BadName(_))
        ));
    }

    /// At most `MAX_SIGNUPS` wait; an expired one is neither listed, nor
    /// counted, nor approved, and the sweep forgets it.
    #[tokio::test]
    async fn requests_are_capped_and_expire() {
        let expired = SignupRecord {
            id: "old".to_string(),
            name: "erin".to_string(),
            password: hash_password("erin-password").expect("hash"),
            requested_at: 1,
            expires_at: 2,
            ..SignupRecord::default()
        };
        let at = AccountsAt {
            signups: vec![expired],
            ..AccountsAt::default()
        };
        let accounts = Accounts::with(Arc::new(MemoryAccounts(Mutex::new(at.clone()))), at);
        assert!(accounts.signups().is_empty(), "an expired request is not listed");
        assert!(matches!(
            accounts.approve_signup("old", Role::ReadOnly, None).await,
            Err(AccountError::NotFound)
        ));
        let long_note = "n".repeat(MAX_SIGNUP_NOTE_CHARS + 50);
        for n in 0..MAX_SIGNUPS {
            accounts
                .request_signup(&format!("user{n}"), "hash".to_string(), &long_note, None)
                .await
                .expect("within the cap");
        }
        assert!(matches!(
            accounts
                .request_signup("one-too-many", "hash".to_string(), "", None)
                .await,
            Err(AccountError::TooManySignups)
        ));
        let listed = accounts.signups();
        assert_eq!(listed.len(), MAX_SIGNUPS);
        assert_eq!(listed[0].note.chars().count(), MAX_SIGNUP_NOTE_CHARS, "a note is cut");
        assert_eq!(accounts.purge_expired_signups().await.expect("purged"), 1);
        assert_eq!(accounts.purge_expired_signups().await.expect("purged"), 0);
    }

    #[test]
    fn the_signup_setting_is_read_strictly() {
        assert_eq!(Signup::from_setting(None), Ok(Signup::Off));
        assert_eq!(Signup::from_setting(Some(" ")), Ok(Signup::Off));
        assert_eq!(Signup::from_setting(Some("off")), Ok(Signup::Off));
        assert_eq!(Signup::from_setting(Some(" approval ")), Ok(Signup::Approval));
        assert!(
            Signup::from_setting(Some("open")).is_err(),
            "an unknown word stops the bridge"
        );
    }
}

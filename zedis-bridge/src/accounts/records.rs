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

//! What the accounts are made of: the records a store keeps — an account, its
//! MCP tokens, an invitation, a request for an account — the views the pages
//! are shown, and the words a change is said in ([`AccountOp`],
//! [`AccountChange`], [`AccountError`]).

use super::*;

/// What an account may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A full account that also manages accounts and invitations.
    Admin,
    Full,
    /// May look at everything it can see and change none of it. The
    /// default of a record that does not say, being the safe answer.
    #[default]
    ReadOnly,
}

impl Role {
    pub const fn name(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Full => "full",
            Role::ReadOnly => "read_only",
        }
    }
}

/// One account as a store keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountRecord {
    pub name: String,
    pub role: Role,
    /// The password's Argon2id hash, in PHC form; `None` for an account only
    /// a reverse proxy signs in.
    pub password: Option<String>,
    /// The server rules of ADR 13 as written (`"prod-*:ro"`); `None` is
    /// every shared entry.
    pub servers: Option<Vec<String>>,
    /// Tokens for the MCP entry point (ADR 15); a read-only account's only.
    pub tokens: Vec<TokenRecord>,
    pub disabled: bool,
    /// Set by an administrator for an account many people sign in to — a
    /// demonstration's: it may not change its own password or tokens, and
    /// the bridge keeps no rows of its own for it (favorites, history,
    /// preferences), so no visitor changes what the next one finds. Never
    /// an administrator's.
    pub locked: bool,
    /// Bumped by every change of password, role, rules or state: a login
    /// carries the version it was opened under and lapses when they differ.
    pub version: u64,
    /// Unix seconds.
    pub created_at: u64,
}

/// One MCP token as a store keeps it: what it is called and the SHA-256 of
/// it. The token itself is shown once and never stored — it is 244 random
/// bits, so a plain hash keeps it as safe as a slow one would.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenRecord {
    /// What the audit log calls it: the machine or the client it was made
    /// for (`claude-laptop`).
    pub name: String,
    /// Lowercase hex.
    pub sha256: String,
    /// Unix seconds from which it is refused; `None` never expires.
    pub expires_at: Option<u64>,
    pub created_at: u64,
}

/// What an invitation does when its link is opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InviteKind {
    /// Registers a new account with this role and these rules. `proxy`: the
    /// account is the name a trusted reverse proxy asserts when the link is
    /// opened behind it, and has no password.
    Register {
        role: Role,
        #[serde(default)]
        servers: Option<Vec<String>>,
        #[serde(default)]
        proxy: bool,
    },
    /// Sets a new password on an existing account.
    Reset { account: String },
}

/// One open invitation as a store keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    /// What the administrators' list and the audit log call it.
    pub id: String,
    /// SHA-256 of the code its link carries, lowercase hex — the code
    /// itself is never kept.
    pub code_sha256: String,
    #[serde(flatten)]
    pub kind: InviteKind,
    /// Unix seconds from which it is refused.
    pub expires_at: u64,
    pub created_by: String,
    pub created_at: u64,
}

/// A request for an account, as a store keeps it until an administrator
/// answers it or it expires. It signs nobody in and reserves no name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SignupRecord {
    pub id: String,
    /// The name it asks for, checked against the accounts only at approval.
    pub name: String,
    /// The password's Argon2id hash, in PHC form: the approved account's.
    pub password: String,
    /// What the person wrote for the administrator.
    pub note: String,
    /// The address it came from, for the administrator to weigh.
    pub peer: Option<String>,
    /// Unix seconds.
    pub requested_at: u64,
    /// Unix seconds from which it is forgotten.
    pub expires_at: u64,
}

/// An account as the account page and the administrators' list show it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AccountView {
    pub name: String,
    pub role: Role,
    pub disabled: bool,
    pub locked: bool,
    pub has_password: bool,
    pub servers: Option<Vec<String>>,
    pub tokens: Vec<TokenView>,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TokenView {
    pub name: String,
    pub expires_at: Option<u64>,
    pub created_at: u64,
}

impl From<&AccountRecord> for AccountView {
    fn from(record: &AccountRecord) -> Self {
        Self {
            name: record.name.clone(),
            role: record.role,
            disabled: record.disabled,
            locked: record.locked,
            has_password: record.password.is_some(),
            servers: record.servers.clone(),
            tokens: record
                .tokens
                .iter()
                .map(|t| TokenView {
                    name: t.name.clone(),
                    expires_at: t.expires_at,
                    created_at: t.created_at,
                })
                .collect(),
            created_at: record.created_at,
        }
    }
}

/// What an administrator changes on an account; `None` leaves a setting as
/// it is.
#[derive(Clone, Debug, Default)]
pub struct AccountChange {
    pub role: Option<Role>,
    /// `Some(None)` clears the rules: every shared entry.
    pub servers: Option<Option<Vec<String>>>,
    pub disabled: Option<bool>,
    pub locked: Option<bool>,
}

/// Why a change was not made.
#[derive(Debug)]
pub enum AccountError {
    NotFound,
    NameTaken,
    BadName(String),
    BadPassword(String),
    /// Used, revoked, expired, or never was — not told apart, so a code
    /// cannot be probed for which.
    InviteInvalid,
    /// It would leave the bridge without a live administrator.
    LastAdmin,
    /// Tokens belong to read-only accounts (ADR 15).
    NotReadOnly,
    TokenNameTaken,
    BadRule(String),
    /// [`MAX_SIGNUPS`] requests are waiting already.
    TooManySignups,
    /// An administrator would be locked: it could not change its own
    /// password.
    LockedAdmin,
    Store(io::Error),
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountError::NotFound => write!(f, "there is no such account"),
            AccountError::NameTaken => write!(f, "that name is taken"),
            AccountError::BadName(m) | AccountError::BadPassword(m) | AccountError::BadRule(m) => write!(f, "{m}"),
            AccountError::InviteInvalid => {
                write!(f, "this invitation is not valid: used, revoked or expired")
            }
            AccountError::LastAdmin => write!(f, "the bridge would be left without an administrator"),
            AccountError::NotReadOnly => {
                write!(
                    f,
                    "tokens are for read-only accounts: the MCP entry point takes no other"
                )
            }
            AccountError::TokenNameTaken => write!(f, "this account already has a token by that name"),
            AccountError::LockedAdmin => {
                write!(
                    f,
                    "an administrator cannot be locked: it could not change its own password"
                )
            }
            AccountError::TooManySignups => {
                write!(f, "too many requests are waiting for an administrator; try again later")
            }
            AccountError::Store(e) => write!(f, "the accounts could not be saved: {e}"),
        }
    }
}

impl std::error::Error for AccountError {}

/// What an `Authorization: Basic …` credential pair comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Basic {
    Accepted(String),
    /// The name it tried, for the throttle and the log.
    Refused(String),
}

/// One change, as the store and the memory both apply it.
#[derive(Clone, Debug)]
pub enum AccountOp {
    PutAccount(AccountRecord),
    RemoveAccount(String),
    PutInvite(InviteRecord),
    RemoveInvite(String),
    PutSignup(SignupRecord),
    RemoveSignup(String),
}

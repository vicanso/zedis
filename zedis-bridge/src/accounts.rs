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

//! The bridge's accounts (ADR 17): registered, never configured.
//!
//! **The first account is the administrator**, and taking that seat needs
//! the setup token ([`Setup`]): while no administrator exists the bridge
//! mints one, prints it in its log and writes it beside its data, and the
//! page offers *Create the administrator* in place of signing in. Without
//! the token, "the first to register is the administrator" would hand a
//! freshly started bridge — and every production credential later added to
//! it — to whoever found its address first.
//!
//! **After that, registration is by invitation** ([`InviteKind::Register`]):
//! an administrator creates one (a role, the server rules, an expiry, one
//! use) and sends its link; the link carries a code the bridge keeps only
//! the hash of. A password an administrator resets is the same mechanism
//! ([`InviteKind::Reset`]), so an administrator never types anyone's
//! password.
//!
//! **Or by a request an administrator approves** ([`SignupRecord`]), where a
//! deployment opens that door ([`Signup::Approval`], off by default): a name,
//! a password's hash and a note, waiting for an administrator to make the
//! account with a role and rules — or to reject it. A request is not an
//! account and reserves no name: it is checked against the accounts only
//! when it is approved, so asking says nothing about who has one.
//!
//! **Roles** ([`Role`]): an administrator is a full account that also
//! manages accounts and invitations; full and read-only are what they were.
//! The server rules of ADR 13 live on the account. The last administrator
//! cannot be demoted, disabled or deleted.
//!
//! **Credentials**: passwords are Argon2id hashes. Every change of
//! password, role, rules or state bumps the account's `version`, which every
//! login carries (`auth::Logins`), so the change ends the logins the account
//! already has. HTTP Basic, which a script sends on every request, is
//! checked against Argon2 once and then against a keyed fingerprint for a
//! few minutes ([`VERIFIED_FOR`]), or each request would cost one.
//!
//! [`Accounts`] is the accounts in memory, in front of an
//! `store::AccountStore`: reads are synchronous — every route asks who may
//! do what — and changes are made one at a time, written to the store
//! first. [`LocalAccounts`] is that store in the local directory,
//! `bridge-accounts.json`.
//!
//! An upgraded deployment's `ZEDIS_BRIDGE_USERS` or users file is read once,
//! into an empty store ([`import_records`]), and never again.

use crate::auth::{ServerRule, bearer_token, hex, new_mcp_token, now_secs, parse_expiry, same};
use crate::store::{AccountStore, AccountsAt, SetupStore, StoreFuture, blocking};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};
use uuid::Uuid;
use zedis_connection::RedisServer;
use zedis_core::fs::write_file_atomic;

mod invitations;
mod local;
mod password;
mod records;
mod setup;
mod signups;
mod tokens;
mod users_file;

pub use invitations::InviteView;
pub use local::LocalAccounts;
pub use password::{check_new_name, check_password, check_rules, hash_password};
use password::{new_code, verify_password};
pub use records::{
    AccountChange, AccountError, AccountOp, AccountRecord, AccountView, Basic, InviteKind, InviteRecord, Role,
    SignupRecord, TokenRecord,
};
pub use setup::{LocalSetup, Setup};
pub use signups::{Signup, SignupView};
pub use users_file::import_records;

/// How many times a change is decided again when another process changed
/// the accounts under it, before it gives up.
const CHANGE_ATTEMPTS: usize = 5;

/// The accounts, inline, as they were configured before registration:
/// `ZEDIS_BRIDGE_USERS="alice@secret,bob:ro@hunter2"`. Imported once.
pub const USERS_ENV: &str = "ZEDIS_BRIDGE_USERS";
/// The same as a TOML file of `[[users]]` tables. Imported once.
pub const USERS_FILE_ENV: &str = "ZEDIS_BRIDGE_USERS_FILE";
/// A setup token an unattended install chooses in advance.
pub const SETUP_TOKEN_ENV: &str = "ZEDIS_BRIDGE_SETUP_TOKEN";

/// How long a Basic password, once checked against its hash, is accepted
/// by its fingerprint.
pub const VERIFIED_FOR: Duration = Duration::from_secs(10 * 60);
/// How long an invitation lasts unless the administrator says otherwise.
pub const INVITE_DAYS: u64 = 7;
/// The longest an invitation may last.
pub const MAX_INVITE_DAYS: u64 = 90;
/// The shortest password registration and a change accept.
pub const MIN_PASSWORD_CHARS: usize = 8;
/// The longest account name.
pub const MAX_NAME_CHARS: usize = 64;
/// Whether the bridge takes requests for an account: `approval` or `off`.
pub const SIGNUP_ENV: &str = "ZEDIS_BRIDGE_SIGNUP";
/// How long a request for an account waits for an answer.
pub const SIGNUP_DAYS: u64 = 7;
/// How many requests may wait at once: past it, the next is refused until
/// one is answered or expires.
pub const MAX_SIGNUPS: usize = 100;
/// The longest note a request carries for the administrator.
pub const MAX_SIGNUP_NOTE_CHARS: usize = 500;

/// An MCP token as the accounts check it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct McpToken {
    name: String,
    sha256: [u8; 32],
    expires_at: Option<u64>,
}

/// `"9f86…"` (64 hex digits) → the bytes.
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    let mut digest = [0u8; 32];
    // ASCII first: the digits are sliced by byte, and a stray non-ASCII
    // character would put a slice inside it.
    let parsed = text.len() == 64
        && text.is_ascii()
        && digest.iter_mut().enumerate().all(|(index, byte)| {
            u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map(|value| *byte = value)
                .is_ok()
        });
    parsed.then_some(digest)
}

/// An account as the checks read it: the record, its rules and tokens
/// parsed once.
#[derive(Clone, Debug)]
struct Entry {
    record: AccountRecord,
    rules: Option<Vec<ServerRule>>,
    tokens: Vec<McpToken>,
}

impl Entry {
    /// A record whose rules no longer parse sees no shared entry rather
    /// than every one, and a token whose hash does not parse signs nobody
    /// in: what was stored wrongly fails closed.
    fn new(record: AccountRecord) -> Self {
        let rules = record.servers.as_ref().map(|rules| {
            rules
                .iter()
                .map(|rule| ServerRule::parse(rule))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|e| {
                    tracing::warn!(account = record.name, error = %e, "unreadable server rules; it sees no shared entry");
                    Vec::new()
                })
        });
        let tokens = record
            .tokens
            .iter()
            .filter_map(|token| {
                Some(McpToken {
                    name: token.name.clone(),
                    sha256: parse_sha256(&token.sha256)?,
                    expires_at: token.expires_at,
                })
            })
            .collect();
        Self { record, rules, tokens }
    }

    fn live(&self) -> bool {
        !self.record.disabled
    }
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, Entry>,
    invites: HashMap<String, InviteRecord>,
    signups: HashMap<String, SignupRecord>,
    /// The store's version of what this holds: a change is applied against
    /// it, and refused when another process moved it (ADR 18).
    version: u64,
}

impl State {
    fn at(at: AccountsAt) -> Self {
        let mut state = Self {
            version: at.version,
            ..Self::default()
        };
        for record in at.accounts {
            state.apply(AccountOp::PutAccount(record));
        }
        for invite in at.invites {
            state.apply(AccountOp::PutInvite(invite));
        }
        for request in at.signups {
            state.apply(AccountOp::PutSignup(request));
        }
        state
    }

    fn live(&self, name: &str) -> Option<&Entry> {
        self.accounts.get(name).filter(|entry| entry.live())
    }

    fn live_admins(&self) -> usize {
        self.accounts
            .values()
            .filter(|entry| entry.live() && entry.record.role == Role::Admin)
            .count()
    }

    /// The open invitation whose code is `code`.
    fn invite_by_code(&self, code: &str, now: u64) -> Option<&InviteRecord> {
        let digest = Sha256::digest(code.trim().as_bytes());
        self.invites.values().find(|invite| {
            invite.expires_at > now && parse_sha256(&invite.code_sha256).is_some_and(|stored| same(&digest, &stored))
        })
    }

    fn apply(&mut self, op: AccountOp) {
        match op {
            AccountOp::PutAccount(record) => {
                self.accounts.insert(record.name.clone(), Entry::new(record));
            }
            AccountOp::RemoveAccount(name) => {
                self.accounts.remove(&name);
            }
            AccountOp::PutInvite(invite) => {
                self.invites.insert(invite.id.clone(), invite);
            }
            AccountOp::RemoveInvite(id) => {
                self.invites.remove(&id);
            }
            AccountOp::PutSignup(request) => {
                self.signups.insert(request.id.clone(), request);
            }
            AccountOp::RemoveSignup(id) => {
                self.signups.remove(&id);
            }
        }
    }
}

/// A Basic password this process has checked against its hash.
struct Verified {
    fingerprint: [u8; 32],
    version: u64,
    at: Instant,
}

/// The accounts, in memory in front of their store.
#[derive(Clone)]
pub struct Accounts(Arc<Inner>);

struct Inner {
    store: Arc<dyn AccountStore>,
    state: RwLock<State>,
    /// One change at a time, from its check to the end of its write.
    edit: tokio::sync::Mutex<()>,
    verified: Mutex<HashMap<String, Verified>>,
    /// The key of the Basic fingerprints: random per process, so a
    /// fingerprint is worth nothing outside it.
    pepper: [u8; 32],
}

impl Accounts {
    /// The accounts `store` holds.
    pub async fn load(store: Arc<dyn AccountStore>) -> io::Result<Self> {
        let at = store.load().await?;
        Ok(Self::with(store, at))
    }

    fn with(store: Arc<dyn AccountStore>, at: AccountsAt) -> Self {
        let state = State::at(at);
        let mut pepper = [0u8; 32];
        pepper[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        pepper[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self(Arc::new(Inner {
            store,
            state: RwLock::new(state),
            edit: tokio::sync::Mutex::new(()),
            verified: Mutex::new(HashMap::new()),
            pepper,
        }))
    }

    fn read(&self) -> RwLockReadGuard<'_, State> {
        self.0.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Make the changes `decide` names, one at a time: it reads the
    /// accounts as they are, and what it answers is written to the store
    /// and then to memory, with nothing else changing in between. When
    /// another process changed the accounts first, the store refuses the
    /// write: they are read again and `decide` asked again, on what they are
    /// now — so a rule like "not the last administrator" holds across
    /// processes (ADR 18).
    async fn change<T>(
        &self,
        decide: impl Fn(&State) -> Result<(Vec<AccountOp>, T), AccountError>,
    ) -> Result<T, AccountError> {
        let _edit = self.0.edit.lock().await;
        // Whether the accounts were read from the store for this change: a
        // refusal is made on what the store holds, not on what this process
        // last read — another one may have added the administrator that
        // makes a demotion fine.
        let mut fresh = false;
        for _ in 0..CHANGE_ATTEMPTS {
            let decided = {
                let state = self.read();
                decide(&state).map(|(ops, answer)| (state.version, ops, answer))
            };
            let (seen, ops, answer) = match decided {
                Ok(decided) => decided,
                Err(_) if !fresh => {
                    self.reload().await.map_err(AccountError::Store)?;
                    fresh = true;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if ops.is_empty() {
                return Ok(answer);
            }
            match self.0.store.apply(seen, ops.clone()).await {
                Ok(Some(version)) => {
                    let mut state = self.0.state.write().unwrap_or_else(PoisonError::into_inner);
                    for op in ops {
                        state.apply(op);
                    }
                    state.version = version;
                    return Ok(answer);
                }
                Ok(None) => {
                    self.reload().await.map_err(AccountError::Store)?;
                    fresh = true;
                }
                Err(e) => return Err(AccountError::Store(e)),
            }
        }
        Err(AccountError::Store(io::Error::other(
            "the accounts kept changing under this change; try again",
        )))
    }

    /// Read the accounts again, as the store has them now.
    async fn reload(&self) -> io::Result<()> {
        let at = self.0.store.load().await?;
        *self.0.state.write().unwrap_or_else(PoisonError::into_inner) = State::at(at);
        Ok(())
    }

    /// The store's version of the accounts this process holds — what the
    /// poll compares (ADR 18).
    pub fn version_held(&self) -> u64 {
        self.read().version
    }

    /// Read the accounts again because another process changed them: the
    /// poll saw their counter move. Waits for a change in progress here.
    pub async fn refresh(&self) -> io::Result<()> {
        let _edit = self.0.edit.lock().await;
        self.reload().await
    }

    // ---- who may do what -------------------------------------------------

    /// Whether `name` is a live account — what a proxy-asserted identity has
    /// to be to get in.
    pub fn has(&self, name: &str) -> bool {
        self.read().live(name).is_some()
    }

    /// Whether `name` is an account at all, live or disabled.
    pub fn exists(&self, name: &str) -> bool {
        self.read().accounts.contains_key(name)
    }

    pub fn is_admin(&self, name: &str) -> bool {
        self.read().live(name).is_some_and(|e| e.record.role == Role::Admin)
    }

    /// Whether `name` is locked by an administrator: it may not change its
    /// own password or tokens, and keeps no rows of its own on the bridge.
    pub fn is_locked(&self, name: &str) -> bool {
        self.read().accounts.get(name).is_some_and(|e| e.record.locked)
    }

    /// Whether `name` may change anything. An account that is not there,
    /// or is disabled, is read-only: a caller whose account went
    /// mid-session must not fall through to full rights on the way to being
    /// refused.
    pub fn is_read_only(&self, name: &str) -> bool {
        self.read().live(name).is_none_or(|e| e.record.role == Role::ReadOnly)
    }

    /// Whether `name` may see the *shared* entry `server`: an account
    /// without rules sees every one, one with rules what a rule names. The
    /// account's own entries are not asked here — they are its own.
    pub fn may_see_shared(&self, name: &str, server: &RedisServer) -> bool {
        self.read().live(name).is_some_and(|entry| {
            entry
                .rules
                .as_ref()
                .is_none_or(|rules| rules.iter().any(|rule| rule.matches(server)))
        })
    }

    /// Whether `name` may change nothing *on this entry*: a read-only
    /// account, everywhere; otherwise a shared entry every matching rule
    /// grants as `:ro` (write wins where rules disagree). An entry the
    /// account owns follows the account's role alone.
    pub fn is_read_only_on(&self, name: &str, server: &RedisServer) -> bool {
        let state = self.read();
        let Some(entry) = state.live(name) else {
            return true;
        };
        let read_only = entry.record.role == Role::ReadOnly;
        if read_only || server.owner.as_deref() == Some(name) {
            return read_only;
        }
        let Some(rules) = &entry.rules else {
            return false;
        };
        let mut matched = rules.iter().filter(|rule| rule.matches(server)).peekable();
        matched.peek().is_some() && matched.all(|rule| rule.is_read_only())
    }

    /// The credential version a login of `name` has to carry, or `None` if
    /// no login of it stands: the account is gone or disabled.
    pub fn version(&self, name: &str) -> Option<u64> {
        self.read().live(name).map(|e| e.record.version)
    }

    /// `name`'s credential version as this process holds it, live or not —
    /// `None` for an account it has not read.
    pub fn known_version(&self, name: &str) -> Option<u64> {
        self.read().accounts.get(name).map(|e| e.record.version)
    }

    /// The live accounts with no password — those only a reverse proxy can
    /// sign in. Named at start-up when there is no such proxy.
    pub fn passwordless(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .read()
            .accounts
            .values()
            .filter(|e| e.live() && e.record.password.is_none())
            .map(|e| e.record.name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn len(&self) -> usize {
        self.read().accounts.len()
    }

    pub fn admin_count(&self) -> usize {
        self.read().live_admins()
    }

    pub fn read_only_count(&self) -> usize {
        self.read()
            .accounts
            .values()
            .filter(|e| e.record.role == Role::ReadOnly)
            .count()
    }

    pub fn with_server_rules(&self) -> usize {
        self.read().accounts.values().filter(|e| e.rules.is_some()).count()
    }

    pub fn mcp_token_count(&self) -> usize {
        self.read().accounts.values().map(|e| e.tokens.len()).sum()
    }

    /// Keyed SHA-256 fingerprint of `(name, password)` under this process's
    /// random pepper, plus the stored `(hash, version)` if `name` is a live
    /// password-bearing account. Returns `Ok(Basic::Accepted)` immediately on
    /// a valid cache hit, `Ok(Basic::Refused)` if the account has no password
    /// or is disabled/missing, or `Err((hash, version, fingerprint))` when an
    /// Argon2 check is required.
    fn lookup_basic_cached(&self, name: &str, password: &str) -> Result<Basic, (String, u64, [u8; 32])> {
        let (hash, version) = {
            let state = self.read();
            match state.live(name) {
                Some(Entry {
                    record:
                        AccountRecord {
                            password: Some(hash),
                            version,
                            ..
                        },
                    ..
                }) => (hash.clone(), *version),
                _ => return Ok(Basic::Refused(name.to_string())),
            }
        };
        let mut fingerprint = Sha256::new();
        fingerprint.update(self.0.pepper);
        fingerprint.update(name.as_bytes());
        fingerprint.update([0]);
        fingerprint.update(password.as_bytes());
        let fingerprint: [u8; 32] = fingerprint.finalize().into();
        {
            let verified = self.0.verified.lock().unwrap_or_else(PoisonError::into_inner);
            if verified.get(name).is_some_and(|v| {
                v.version == version && v.at.elapsed() < VERIFIED_FOR && same(&v.fingerprint, &fingerprint)
            }) {
                return Ok(Basic::Accepted(name.to_string()));
            }
        }
        Err((hash, version, fingerprint))
    }

    fn record_basic_verified(&self, name: String, fingerprint: [u8; 32], version: u64) -> Basic {
        self.0.verified.lock().unwrap_or_else(PoisonError::into_inner).insert(
            name.clone(),
            Verified {
                fingerprint,
                version,
                at: Instant::now(),
            },
        );
        Basic::Accepted(name)
    }

    /// Check a decoded HTTP Basic `(name, password)` pair for route handlers:
    /// answers synchronously on an unknown/passwordless account or a warm
    /// [`VERIFIED_FOR`] cache hit, and offloads Argon2id to `spawn_blocking`
    /// on a cache miss so Tokio worker threads never block on hashing.
    pub async fn check_basic_credentials(&self, name: String, password: String) -> Basic {
        match self.lookup_basic_cached(&name, &password) {
            Ok(verdict) => verdict,
            Err((hash, version, fingerprint)) => {
                let valid = tokio::task::spawn_blocking(move || verify_password(&hash, &password))
                    .await
                    .unwrap_or(false);
                if !valid {
                    return Basic::Refused(name);
                }
                self.record_basic_verified(name, fingerprint, version)
            }
        }
    }

    /// Whether `password` is `name`'s, for the sign-in form and setup. Slow
    /// on purpose: call it off the async workers.
    pub fn accepts_password(&self, name: &str, password: &str) -> bool {
        let hash = self.read().live(name).and_then(|e| e.record.password.clone());
        hash.is_some_and(|hash| verify_password(&hash, password))
    }

    // ---- what the pages show ---------------------------------------------

    pub fn view(&self, name: &str) -> Option<AccountView> {
        self.read().accounts.get(name).map(|e| AccountView::from(&e.record))
    }

    /// Every account, by name.
    pub fn list(&self) -> Vec<AccountView> {
        let mut views: Vec<AccountView> = self
            .read()
            .accounts
            .values()
            .map(|e| AccountView::from(&e.record))
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        views
    }

    /// For a miss that may only mean another process changed the accounts
    /// within the last poll — an invitation it made, an account it
    /// registered: ask the store for their version, and read them again
    /// only if it moved. A name nobody registered costs that one question,
    /// never a read of every account.
    pub async fn catch_up(&self) {
        match self.0.store.version().await {
            Ok(version) if version == self.version_held() => {}
            Ok(_) => {
                if let Err(e) = self.refresh().await {
                    tracing::warn!(error = %e, "the accounts could not be read again");
                }
            }
            Err(e) => tracing::warn!(error = %e, "the accounts' version could not be read"),
        }
    }

    // ---- changes ---------------------------------------------------------

    /// Accounts read from a users file, into a store that has none.
    /// Answers how many went in; none if the store already had accounts.
    pub async fn import(&self, records: Vec<AccountRecord>) -> Result<usize, AccountError> {
        self.change(|state| {
            if !state.accounts.is_empty() {
                return Ok((Vec::new(), 0));
            }
            let count = records.len();
            Ok((records.iter().cloned().map(AccountOp::PutAccount).collect(), count))
        })
        .await
    }

    /// The setup token's work: `name` becomes the administrator — created
    /// with `password_hash` when there is no such account, or promoted when
    /// there is (the caller has checked its password). Answers whether it
    /// was a promotion.
    pub async fn make_admin(&self, name: &str, password_hash: Option<String>) -> Result<bool, AccountError> {
        self.change(|state| match (state.accounts.get(name), password_hash.clone()) {
            (Some(entry), _) => {
                let mut record = entry.record.clone();
                record.role = Role::Admin;
                record.disabled = false;
                // Seated from the host, with its own password: nothing an
                // administrator set on it before stands over that.
                record.locked = false;
                // A token is a read-only account's; an administrator's would
                // be a credential for a door that refuses it.
                record.tokens.clear();
                record.version += 1;
                Ok((vec![AccountOp::PutAccount(record)], true))
            }
            (None, Some(hash)) => {
                check_new_name(name)?;
                let record = AccountRecord {
                    name: name.to_string(),
                    role: Role::Admin,
                    password: Some(hash),
                    version: 1,
                    created_at: now_secs(),
                    ..Default::default()
                };
                Ok((vec![AccountOp::PutAccount(record)], false))
            }
            (None, None) => Err(AccountError::NotFound),
        })
        .await
    }

    /// `name`'s own new password.
    pub async fn set_password(&self, name: &str, password_hash: String) -> Result<(), AccountError> {
        self.change(|state| {
            let entry = state.live(name).ok_or(AccountError::NotFound)?;
            let mut record = entry.record.clone();
            record.password = Some(password_hash.clone());
            record.version += 1;
            Ok((vec![AccountOp::PutAccount(record)], ()))
        })
        .await
    }

    /// An administrator's change to `name`: any of its role, its rules, and
    /// whether it is disabled or locked. Answers what changed, setting →
    /// (from, to), for the audit line.
    pub async fn update(
        &self,
        name: &str,
        change: AccountChange,
    ) -> Result<Vec<(&'static str, String, String)>, AccountError> {
        let AccountChange {
            role,
            servers,
            disabled,
            locked,
        } = change;
        if let Some(rules) = &servers {
            check_rules(rules)?;
        }
        self.change(|state| {
            let entry = state.accounts.get(name).ok_or(AccountError::NotFound)?;
            let mut record = entry.record.clone();
            let mut changed = Vec::new();
            if let Some(role) = role.filter(|role| *role != record.role) {
                if role != Role::ReadOnly && !record.tokens.is_empty() {
                    return Err(AccountError::NotReadOnly);
                }
                changed.push(("role", record.role.name().to_string(), role.name().to_string()));
                record.role = role;
            }
            if let Some(rules) = servers.clone().filter(|rules| *rules != record.servers) {
                let show = |rules: &Option<Vec<String>>| rules.as_ref().map_or("*".to_string(), |r| r.join(", "));
                changed.push(("servers", show(&record.servers), show(&rules)));
                record.servers = rules;
            }
            if let Some(disabled) = disabled.filter(|d| *d != record.disabled) {
                changed.push(("disabled", record.disabled.to_string(), disabled.to_string()));
                record.disabled = disabled;
            }
            if let Some(locked) = locked.filter(|l| *l != record.locked) {
                changed.push(("locked", record.locked.to_string(), locked.to_string()));
                record.locked = locked;
            }
            if changed.is_empty() {
                return Ok((Vec::new(), changed));
            }
            if record.locked && record.role == Role::Admin {
                return Err(AccountError::LockedAdmin);
            }
            let was_admin = entry.live() && entry.record.role == Role::Admin;
            let still_admin = !record.disabled && record.role == Role::Admin;
            if was_admin && !still_admin && state.live_admins() <= 1 {
                return Err(AccountError::LastAdmin);
            }
            record.version += 1;
            Ok((vec![AccountOp::PutAccount(record)], changed))
        })
        .await
    }

    /// Delete `name`, its logins lapsing with it.
    pub async fn delete(&self, name: &str) -> Result<(), AccountError> {
        self.change(|state| {
            let entry = state.accounts.get(name).ok_or(AccountError::NotFound)?;
            if entry.live() && entry.record.role == Role::Admin && state.live_admins() <= 1 {
                return Err(AccountError::LastAdmin);
            }
            // Invitations to reset its password go with it.
            let mut ops: Vec<AccountOp> = state
                .invites
                .values()
                .filter(|i| matches!(&i.kind, InviteKind::Reset { account } if account == name))
                .map(|i| AccountOp::RemoveInvite(i.id.clone()))
                .collect();
            ops.push(AccountOp::RemoveAccount(name.to_string()));
            Ok((ops, ()))
        })
        .await
    }
}

/// Owner-only on unix: the file holds password hashes, or a token that
/// seats an administrator.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not restrict a file to its owner");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

// ---- for the tests of this crate -----------------------------------------------

/// An `AccountStore` that keeps nothing past the process: the tests'.
/// [`MemoryAccounts::changed_elsewhere`] plays another process.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryAccounts(Mutex<AccountsAt>);

#[cfg(test)]
impl MemoryAccounts {
    /// Another process's change: `op` applied, and the version moved.
    pub fn changed_elsewhere(&self, op: AccountOp) {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        apply_to(&mut held, op);
        held.version += 1;
    }
}

#[cfg(test)]
fn apply_to(at: &mut AccountsAt, op: AccountOp) {
    match op {
        AccountOp::PutAccount(account) => {
            at.accounts.retain(|a| a.name != account.name);
            at.accounts.push(account);
        }
        AccountOp::RemoveAccount(name) => at.accounts.retain(|a| a.name != name),
        AccountOp::PutInvite(invite) => {
            at.invites.retain(|i| i.id != invite.id);
            at.invites.push(invite);
        }
        AccountOp::RemoveInvite(id) => at.invites.retain(|i| i.id != id),
        AccountOp::PutSignup(request) => {
            at.signups.retain(|r| r.id != request.id);
            at.signups.push(request);
        }
        AccountOp::RemoveSignup(id) => at.signups.retain(|r| r.id != id),
    }
}

#[cfg(test)]
impl AccountStore for MemoryAccounts {
    fn load(&self) -> StoreFuture<AccountsAt> {
        let held = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(async move { Ok(held) })
    }

    fn version(&self) -> StoreFuture<u64> {
        let version = self.0.lock().unwrap_or_else(PoisonError::into_inner).version;
        Box::pin(async move { Ok(version) })
    }

    fn apply(&self, seen: u64, ops: Vec<AccountOp>) -> StoreFuture<Option<u64>> {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if held.version != seen {
            return Box::pin(async { Ok(None) });
        }
        for op in ops {
            apply_to(&mut held, op);
        }
        held.version += 1;
        let version = held.version;
        Box::pin(async move { Ok(Some(version)) })
    }
}

#[cfg(test)]
impl Accounts {
    /// `records`, in a memory store that holds them too.
    fn in_memory(records: Vec<AccountRecord>) -> Self {
        let at = AccountsAt {
            accounts: records,
            ..AccountsAt::default()
        };
        Self::with(Arc::new(MemoryAccounts(Mutex::new(at.clone()))), at)
    }

    /// No accounts, in memory: a fresh bridge.
    pub fn empty() -> Self {
        Self::with(Arc::new(MemoryAccounts::default()), AccountsAt::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    fn entry(id: &str, name: &str, owner: Option<&str>) -> RedisServer {
        RedisServer {
            id: id.to_string(),
            name: name.to_string(),
            owner: owner.map(str::to_string),
            ..Default::default()
        }
    }

    pub(super) fn basic(pair: &str) -> String {
        format!("Basic {}", B64.encode(pair))
    }

    /// The error of what is expected to fail. `expect_err` would want
    /// `Debug` on `Accounts`, which holds every password hash.
    pub(super) fn refused(text: &str) -> String {
        match Accounts::from_toml(text) {
            Ok(accounts) => panic!("expected an error, got {} accounts", accounts.len()),
            Err(e) => e,
        }
    }

    pub(super) async fn with_admin(spec: &str) -> Accounts {
        let accounts = Accounts::from_inline(spec).expect("accounts");
        accounts
            .make_admin("root", Some(hash_password("root-password").expect("hash")))
            .await
            .expect("admin");
        accounts
    }

    pub(super) async fn invite(accounts: &Accounts, kind: InviteKind) -> String {
        accounts.invite_new("root", kind, INVITE_DAYS).await.expect("invite").1
    }

    pub(super) fn register_full() -> InviteKind {
        InviteKind::Register {
            role: Role::Full,
            servers: None,
            proxy: false,
        }
    }

    #[test]
    fn the_servers_list_decides_what_is_seen_and_where_writes_are_allowed() {
        let accounts = Accounts::from_toml(
            r#"
[[users]]
name = "alice"
password = "a"

[[users]]
name = "bob"
password = "b"
servers = ["prod-*:ro", "prod-eu", "id:id-9"]

[[users]]
name = "carol"
password = "c"
read_only = true
servers = ["prod-*"]

[[users]]
name = "dave"
password = "d"
servers = []
"#,
        )
        .expect("file");
        assert_eq!(accounts.with_server_rules(), 3);
        let prod_us = entry("id-1", "prod-us", None);
        let prod_eu = entry("id-2", "prod-eu", None);
        let staging = entry("id-3", "staging", None);
        let by_id = entry("id-9", "whatever", None);
        let bobs_own = entry("id-4", "prod-mine", Some("bob"));

        for s in [&prod_us, &prod_eu, &staging, &by_id] {
            assert!(accounts.may_see_shared("alice", s));
            assert!(!accounts.is_read_only_on("alice", s));
        }
        assert!(accounts.may_see_shared("bob", &prod_us));
        assert!(accounts.may_see_shared("bob", &prod_eu));
        assert!(accounts.may_see_shared("bob", &by_id), "by id");
        assert!(!accounts.may_see_shared("bob", &staging));
        assert!(accounts.is_read_only_on("bob", &prod_us));
        assert!(!accounts.is_read_only_on("bob", &prod_eu), "write wins");
        assert!(!accounts.is_read_only_on("bob", &by_id));
        assert!(!accounts.is_read_only_on("bob", &bobs_own), "its own is its own");
        assert!(accounts.may_see_shared("carol", &prod_us) && accounts.is_read_only_on("carol", &prod_us));
        assert!(
            accounts.is_read_only_on("carol", &entry("id-5", "x", Some("carol"))),
            "even its own"
        );
        assert!(!accounts.may_see_shared("dave", &prod_us) && !accounts.may_see_shared("dave", &staging));
        assert!(!accounts.may_see_shared("nobody", &staging) && accounts.is_read_only_on("nobody", &staging));
        let err = refused("[[users]]\nname = \"e\"\npassword = \"p\"\nservers = [\":ro\"]\n");
        assert!(err.contains("\"e\"") && err.contains(":ro"), "{err}");
    }

    #[test]
    fn an_account_without_a_password_belongs_to_the_proxy() {
        let accounts = Accounts::from_toml(
            "[[users]]\nname = \"alice\"\n[[users]]\nname = \"bob\"\npassword = \"pw\"\nread_only = true\n",
        )
        .expect("file");
        assert_eq!(accounts.passwordless(), vec!["alice".to_string()]);
        assert!(
            !accounts.accepts_password("alice", ""),
            "no password is not an empty password"
        );
        assert!(!accounts.accepts_password("alice", "anything"));
        assert!(accounts.accepts_password("bob", "pw"));
        assert!(accounts.has("alice") && !accounts.has("carol"));
        assert!(!accounts.is_read_only("alice") && accounts.is_read_only("bob"));
        let err = refused("[[users]]\nname = \"alice\"\npassword = \"\"\n");
        assert!(err.contains("empty password"), "{err}");
    }

    #[tokio::test]
    async fn an_import_goes_into_an_empty_store_only() {
        let accounts = Accounts::from_inline("alice@secret").expect("accounts");
        let more = Accounts::from_inline("bob@hunter2").expect("accounts");
        let bob = more.read().accounts["bob"].record.clone();
        assert_eq!(accounts.import(vec![bob.clone()]).await.expect("import"), 0);
        let empty = Accounts::empty();
        assert_eq!(empty.import(vec![bob]).await.expect("import"), 1);
        assert!(
            empty.has("bob") && !empty.is_admin("bob"),
            "an import seats no administrator"
        );
    }

    #[tokio::test]
    async fn a_basic_header_signs_in_as_the_account_it_names() {
        use crate::auth::basic_credentials;
        let accounts = Accounts::from_inline("alice@se:cret,bob@hunter2").expect("accounts");
        let check = |header: Option<&str>| {
            let accounts = accounts.clone();
            let parsed = basic_credentials(header);
            async move {
                match parsed {
                    Some((name, password)) => Some(accounts.check_basic_credentials(name, password).await),
                    None => None,
                }
            }
        };
        let accepted = |name: &str| Some(Basic::Accepted(name.to_string()));
        assert_eq!(check(Some(&basic("alice:se:cret"))).await, accepted("alice"));
        assert_eq!(
            check(Some(&basic("alice:se:cret"))).await,
            accepted("alice"),
            "again, from the fingerprint"
        );
        assert_eq!(check(Some(&basic("bob:hunter2"))).await, accepted("bob"));
        let lowercase = format!("basic {}", B64.encode("alice:se:cret"));
        assert_eq!(check(Some(&lowercase)).await, accepted("alice"));
        assert_eq!(
            check(Some(&basic("alice:wrong"))).await,
            Some(Basic::Refused("alice".to_string()))
        );
        assert_eq!(
            check(Some(&basic("carol:se:cret"))).await,
            Some(Basic::Refused("carol".to_string()))
        );
        assert_eq!(check(Some(&basic("alice"))).await, None, "no password at all");
        assert_eq!(check(Some("Bearer se:cret")).await, None);
        assert_eq!(check(Some("Basic not-base64!")).await, None);
        assert_eq!(check(None).await, None);
    }

    /// The fingerprint a checked password is remembered by lapses with the
    /// account's version: the old password is refused at once after a
    /// change, not ten minutes later.
    #[tokio::test]
    async fn a_new_password_ends_the_old_one_at_once() {
        let accounts = Accounts::from_inline("alice@secret-one").expect("accounts");
        assert_eq!(
            accounts
                .check_basic_credentials("alice".to_string(), "secret-one".to_string())
                .await,
            Basic::Accepted("alice".to_string())
        );
        accounts
            .set_password("alice", hash_password("secret-two").expect("hash"))
            .await
            .expect("password");
        assert_eq!(
            accounts
                .check_basic_credentials("alice".to_string(), "secret-one".to_string())
                .await,
            Basic::Refused("alice".to_string())
        );
        assert_eq!(
            accounts
                .check_basic_credentials("alice".to_string(), "secret-two".to_string())
                .await,
            Basic::Accepted("alice".to_string())
        );
    }

    #[test]
    fn a_password_is_checked_against_its_own_account_only() {
        let accounts = Accounts::from_inline("alice@secret,bob@hunter2").expect("accounts");
        assert!(accounts.accepts_password("alice", "secret"));
        assert!(!accounts.accepts_password("alice", "Secret"));
        assert!(
            !accounts.accepts_password("alice", "hunter2"),
            "bob's password is not alice's"
        );
        assert!(!accounts.accepts_password("carol", "secret"));
    }

    /// The bridge is never left without a live administrator: the last one
    /// cannot be demoted, disabled or deleted, and one more makes each
    /// possible.
    #[tokio::test]
    async fn the_last_administrator_stays() {
        let accounts = with_admin("alice@secret").await;
        assert!(accounts.is_admin("root") && accounts.admin_count() == 1);
        assert!(matches!(
            accounts
                .update(
                    "root",
                    AccountChange {
                        role: Some(Role::Full),
                        ..AccountChange::default()
                    }
                )
                .await,
            Err(AccountError::LastAdmin)
        ));
        assert!(matches!(
            accounts
                .update(
                    "root",
                    AccountChange {
                        disabled: Some(true),
                        ..AccountChange::default()
                    }
                )
                .await,
            Err(AccountError::LastAdmin)
        ));
        assert!(matches!(accounts.delete("root").await, Err(AccountError::LastAdmin)));
        assert!(
            accounts.make_admin("alice", None).await.expect("promoted"),
            "a promotion"
        );
        let changed = accounts
            .update(
                "root",
                AccountChange {
                    role: Some(Role::Full),
                    ..AccountChange::default()
                },
            )
            .await
            .expect("demoted");
        assert_eq!(changed, vec![("role", "admin".to_string(), "full".to_string())]);
        assert!(
            accounts
                .update(
                    "root",
                    AccountChange {
                        role: Some(Role::Full),
                        ..AccountChange::default()
                    }
                )
                .await
                .expect("no change")
                .is_empty()
        );
        assert!(matches!(accounts.delete("alice").await, Err(AccountError::LastAdmin)));
    }

    #[tokio::test]
    async fn an_administrator_changes_rules_and_state_and_bad_rules_are_refused() {
        let accounts = with_admin("alice@secret").await;
        let changed = accounts
            .update(
                "alice",
                AccountChange {
                    servers: Some(Some(vec!["prod-*:ro".to_string()])),
                    disabled: Some(true),
                    ..AccountChange::default()
                },
            )
            .await
            .expect("changed");
        assert_eq!(changed.len(), 2);
        assert!(
            !accounts.has("alice") && accounts.exists("alice"),
            "disabled is not gone"
        );
        assert!(accounts.is_read_only("alice"), "a disabled account changes nothing");
        assert!(matches!(
            accounts
                .update(
                    "alice",
                    AccountChange {
                        servers: Some(Some(vec![":ro".to_string()])),
                        ..AccountChange::default()
                    }
                )
                .await,
            Err(AccountError::BadRule(_))
        ));
        accounts
            .update(
                "alice",
                AccountChange {
                    servers: Some(None),
                    disabled: Some(false),
                    ..AccountChange::default()
                },
            )
            .await
            .expect("cleared");
        assert!(accounts.may_see_shared("alice", &entry("x", "staging", None)));
    }

    /// Two processes on one store each demote one of the last two
    /// administrators: the one that writes second sees the first's change,
    /// decides again, and is refused — the bridge keeps an administrator.
    #[tokio::test]
    async fn a_change_another_process_made_first_is_decided_again() {
        let admin = |name: &str| AccountRecord {
            name: name.to_string(),
            role: Role::Admin,
            version: 1,
            ..Default::default()
        };
        let at = AccountsAt {
            accounts: vec![admin("alice"), admin("bob")],
            ..AccountsAt::default()
        };
        let store = Arc::new(MemoryAccounts(Mutex::new(at.clone())));
        let one = Accounts::with(store.clone(), at.clone());
        let two = Accounts::with(store.clone(), at);
        one.update(
            "alice",
            AccountChange {
                role: Some(Role::Full),
                ..AccountChange::default()
            },
        )
        .await
        .expect("demoted");
        assert!(matches!(
            two.update(
                "bob",
                AccountChange {
                    role: Some(Role::Full),
                    ..AccountChange::default()
                }
            )
            .await,
            Err(AccountError::LastAdmin)
        ));
        assert!(two.is_admin("bob") && !two.is_admin("alice"), "two read what one did");

        // A change nobody raced lands at once.
        store.changed_elsewhere(AccountOp::PutAccount(admin("carol")));
        two.update(
            "bob",
            AccountChange {
                role: Some(Role::Full),
                ..AccountChange::default()
            },
        )
        .await
        .expect("carol is an administrator now");
        assert_eq!(two.version_held(), 3);
    }

    /// A locked account is any role but an administrator's — locking one
    /// would leave it unable to change its own password — and the setup
    /// token, which seats one from the host, unlocks it.
    #[tokio::test]
    async fn a_locked_account_is_never_an_administrator() {
        let accounts = with_admin("demo:ro@demo-password,bob@hunter22").await;
        let lock = AccountChange {
            locked: Some(true),
            ..AccountChange::default()
        };
        let changed = accounts.update("demo", lock.clone()).await.expect("locked");
        assert_eq!(changed, vec![("locked", "false".to_string(), "true".to_string())]);
        assert!(accounts.is_locked("demo") && accounts.view("demo").is_some_and(|v| v.locked));
        assert!(accounts.version("demo").is_some(), "it still signs in");
        assert!(matches!(
            accounts.update("root", lock.clone()).await,
            Err(AccountError::LockedAdmin)
        ));
        assert!(matches!(
            accounts
                .update(
                    "demo",
                    AccountChange {
                        role: Some(Role::Admin),
                        ..AccountChange::default()
                    }
                )
                .await,
            Err(AccountError::LockedAdmin)
        ));
        assert!(!accounts.is_admin("demo"), "nothing was changed");
        accounts
            .update("bob", lock)
            .await
            .expect("a full account may be locked");
        assert!(accounts.make_admin("bob", None).await.expect("promoted"));
        assert!(
            accounts.is_admin("bob") && !accounts.is_locked("bob"),
            "seated, and unlocked"
        );
    }
}

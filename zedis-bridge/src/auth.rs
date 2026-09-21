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

//! Who may call the bridge: named accounts, and nothing else.
//!
//! `ZEDIS_BRIDGE_USERS="alice@secret,bob@hunter2"` is the short form, and
//! [`USERS_FILE_ENV`] / `--users-file` the long one — a TOML file of
//! `[[users]]` tables, for a deployment that would rather not put its
//! passwords in an environment variable every `docker inspect` prints, and
//! the only form that can be edited without restating the whole list.
//! Exactly one of the two is configured; both is an error, because a bridge
//! that silently preferred one would be the worst kind of auth
//! misconfiguration to have.
//!
//! An account may be **read-only** (`read_only = true`, or `alice:ro@secret`
//! in the short form). That is enforced where the commands are — see
//! `policy` and `zedis_connection::is_read_only_command` — not here; this
//! module only carries the flag, because "who" and "may they" are different
//! questions and only the routes can ask the second one.
//!
//! There used to be a second mode — one generated bearer token shared by
//! everyone — and it was removed when server entries became owned: a private
//! entry needs an owner, and a caller who is "whoever holds the token" cannot
//! be one (ADR 9). So the bridge does not start without accounts, rather than
//! start open or start with a credential nobody chose.
//!
//! Every check here answers *who*, not merely *whether*: the routes need the
//! name to decide which entries a caller sees.
//!
//! Browsers do not hold the password. They post it once to `/v1/login` and
//! get back a cookie carrying a [`Logins`] id instead: `HttpOnly`, so no
//! script on the page can read it, `SameSite=Strict`, so it does not ride
//! along with a cross-site request, and revocable and expiring, which a copy
//! of the password in `localStorage` would be neither. Scripts and the CLI
//! send HTTP Basic.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use zedis_core::fs::write_file_atomic;

/// The accounts, inline: `ZEDIS_BRIDGE_USERS="alice@secret,bob:ro@hunter2"`.
pub const USERS_ENV: &str = "ZEDIS_BRIDGE_USERS";

/// The accounts, as a file: `ZEDIS_BRIDGE_USERS_FILE=/data/users.toml`.
pub const USERS_FILE_ENV: &str = "ZEDIS_BRIDGE_USERS_FILE";

/// One account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub password: String,
    /// May look at everything it can see, and change none of it. Enforced by
    /// the routes, not here.
    pub read_only: bool,
}

/// The file `--users-file` names.
#[derive(Deserialize)]
struct UsersFile {
    #[serde(default)]
    users: Vec<UserEntry>,
}

/// One `[[users]]` table. `read_only` defaults to false, so an account that
/// does not mention it is a full one — the same answer the short form gives.
#[derive(Deserialize)]
struct UserEntry {
    name: String,
    password: String,
    #[serde(default)]
    read_only: bool,
}

/// The accounts that may sign in, name → account.
#[derive(Clone)]
pub struct Accounts(HashMap<String, Account>);

impl Accounts {
    /// From [`USERS_FILE_ENV`] / `--users-file`, or [`USERS_ENV`]. Missing,
    /// empty, malformed or *both at once* is an error that stops the bridge:
    /// there is no other way in to fall back to, and an auth setting that
    /// silently meant something else would be the worst kind of
    /// misconfiguration to have.
    pub fn load(file: Option<&Path>) -> Result<Self, String> {
        let inline = match env::var(USERS_ENV) {
            Ok(spec) => Some(spec),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => return Err(format!("{USERS_ENV} is not valid UTF-8")),
        };
        match (file, inline) {
            (Some(path), None) => Self::from_file(path),
            (None, Some(spec)) => parse_users(&spec).map(Self).map_err(|e| format!("{USERS_ENV}: {e}")),
            (Some(path), Some(_)) => Err(format!(
                "both {USERS_ENV} and a users file ({}) are set — pick one, \
                 so that what the bridge accepts is what you can read",
                path.display()
            )),
            (None, None) => Err(format!(
                "no accounts are configured. Either {USERS_ENV}=\"alice@secret,bob:ro@hunter2\" \
                 or {USERS_FILE_ENV}=/path/to/users.toml (--users-file)"
            )),
        }
    }

    /// The TOML form. The path is named in every error: a file that is
    /// missing or unreadable is the one misconfiguration an operator cannot
    /// diagnose from "unauthorized" alone.
    fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: UsersFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut users = HashMap::new();
        for (index, entry) in file.users.into_iter().enumerate() {
            let position = index + 1;
            let name = entry.name.trim().to_string();
            check_name(&name, position).map_err(|e| format!("{}: {e}", path.display()))?;
            if entry.password.is_empty() {
                return Err(format!(
                    "{}: user \"{name}\" (entry {position}) has an empty password",
                    path.display()
                ));
            }
            let account = Account {
                password: entry.password,
                read_only: entry.read_only,
            };
            if users.insert(name.clone(), account).is_some() {
                return Err(format!("{}: user \"{name}\" is listed twice", path.display()));
            }
        }
        if users.is_empty() {
            return Err(format!("{}: no [[users]] entries", path.display()));
        }
        Ok(Self(users))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// How many of them may change nothing — worth a line in the startup log,
    /// because a read-only account that was meant to be a full one shows up
    /// as a button that does nothing and no error anybody reads.
    pub fn read_only_count(&self) -> usize {
        self.0.values().filter(|a| a.read_only).count()
    }

    /// The account an `Authorization: Basic …` header signs in as, if its
    /// password is right.
    pub fn account_for_header(&self, header: Option<&str>) -> Option<String> {
        let (name, password) = basic_credentials(header)?;
        self.accepts_password(&name, &password).then_some(name)
    }

    /// A username and password, as the page posts them.
    pub fn accepts_password(&self, name: &str, password: &str) -> bool {
        self.0
            .get(name)
            .is_some_and(|account| same(password.as_bytes(), account.password.as_bytes()))
    }

    /// Whether `name` may change anything. An account that is not there is
    /// read-only: a caller whose account was deleted mid-session must not
    /// fall through to full rights on the way to being refused.
    pub fn is_read_only(&self, name: &str) -> bool {
        self.0.get(name).is_none_or(|account| account.read_only)
    }

    /// What ties a stored login to the password it was opened with: changing
    /// an account's password changes this, and every login saved under the
    /// old one stops being accepted — which is what changing a password is
    /// for. Salted per file, so the stored value is not a bare hash of it.
    ///
    /// The read-only flag is in it too, so *demoting* an account to read-only
    /// also ends its open sessions. Otherwise a browser signed in before the
    /// change would keep the rights it signed in with, for up to thirty days.
    fn credential_tag(&self, salt: &str, name: &str) -> Option<String> {
        let account = self.0.get(name)?;
        let role = if account.read_only { "ro" } else { "rw" };
        Some(hex(&Sha256::digest(format!(
            "{salt}:{name}:{role}:{}",
            account.password
        ))))
    }
}

/// The rules a name obeys whichever form it was written in.
fn check_name(name: &str, position: usize) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("entry {position} has an empty user name"));
    }
    if name.contains(':') {
        return Err(format!("user \"{name}\" (entry {position}): a name cannot contain ':'"));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `alice@secret,bob:ro@hunter2` → the accounts.
///
/// An entry splits at its *first* `@`, so a password may contain one and a
/// name may not; entries split at `,`, so a password may not contain that.
///
/// The role rides on the **name** side, `alice:ro`, and not as a suffix on
/// the password: a password may contain `:` (only `,` and the leading `@`
/// are barred), so `alice@secret:ro` cannot be told apart from the password
/// `secret:ro`. A name never could contain one — HTTP Basic splits
/// `name:password` at the first `:`, so such a name could not be presented
/// and has always been rejected here — which leaves it free to mean this,
/// and leaves every spelling that worked before working unchanged.
///
/// Empty entries (a trailing comma) are skipped; anything else wrong is an
/// error that names the entry by position, never by content, because the
/// content is a password.
pub fn parse_users(spec: &str) -> Result<HashMap<String, Account>, String> {
    let mut users = HashMap::new();
    for (index, entry) in spec.split(',').map(str::trim).enumerate() {
        if entry.is_empty() {
            continue;
        }
        let position = index + 1;
        let Some((name, password)) = entry.split_once('@') else {
            return Err(format!("entry {position} is not user@password"));
        };
        let (name, read_only) = match name.trim().split_once(':') {
            Some((name, "ro")) => (name.trim(), true),
            // A name with a `:` that is not the role is the old error, and
            // its wording still fits: the name is what cannot hold one.
            Some(_) => (name.trim(), false),
            None => (name.trim(), false),
        };
        check_name(name, position)?;
        if password.is_empty() {
            return Err(format!("user \"{name}\" (entry {position}) has an empty password"));
        }
        // `alice@secret:ro` is the spelling people reach for first, and it
        // parses — as a full account whose password is `secret:ro`. Both
        // halves of that are wrong and neither says so, which is the kind of
        // auth misconfiguration this module exists to refuse. It cannot be
        // an error, because a password really may end in `:ro`; so it is a
        // warning that names the account and the spelling that works.
        if !read_only && password.ends_with(":ro") {
            tracing::warn!(
                user = name,
                "password ends in \":ro\" and this account is NOT read-only — the role goes on the name: \
                 {name}:ro@<password>. Ignore this if the password really ends in \":ro\"."
            );
        }
        let account = Account {
            password: password.to_string(),
            read_only,
        };
        if users.insert(name.to_string(), account).is_some() {
            return Err(format!("user \"{name}\" is listed twice"));
        }
    }
    if users.is_empty() {
        return Err("no accounts (set it to user@password,…)".to_string());
    }
    Ok(users)
}

/// The `name:password` behind an `Authorization: Basic …` header.
fn basic_credentials(header: Option<&str>) -> Option<(String, String)> {
    let value = header?;
    let encoded = value.strip_prefix("Basic ").or_else(|| value.strip_prefix("basic "))?;
    let decoded = B64.decode(encoded.trim()).ok()?;
    let pair = String::from_utf8(decoded).ok()?;
    let (name, password) = pair.split_once(':')?;
    Some((name.to_string(), password.to_string()))
}

/// Equality that does not short-circuit on the first differing byte, so the
/// time it takes does not leak how much of a guess was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One browser's login: who it is, under which password, and when it was
/// last seen.
#[derive(Serialize, Deserialize)]
struct Login {
    account: String,
    /// `Accounts::credential_tag` at the time of signing in.
    tag: String,
    /// Unix seconds. Wall-clock, because it has to survive a restart.
    last_used: u64,
    /// "Keep me signed in": the long idle limit instead of the working day.
    remember: bool,
}

/// What is kept on disk. The key of `logins` is the SHA-256 of a cookie id,
/// never the id: the file then holds nothing a reader could present.
#[derive(Serialize, Deserialize, Default)]
struct LoginFile {
    salt: String,
    logins: HashMap<String, Login>,
}

struct LoginState {
    file: LoginFile,
    /// Where it is saved; `None` keeps it in memory (the tests).
    path: Option<PathBuf>,
    dirty: bool,
}

/// Browser logins: an opaque cookie id, and who is behind it.
///
/// Server-side because the point of the cookie is that the password never
/// stays in the browser. Dropping an entry logs that browser out.
///
/// **On disk**, because they used to live in memory only, and then every
/// restart of the bridge signed everybody out — the actual reason people were
/// typing their password over and over. Keeping the login alive is the fix
/// for that; keeping the *password* in the browser's `localStorage` would
/// have traded a revocable, expiring id for a secret any script or passer-by
/// with the devtools open can read (ADR 9).
#[derive(Clone)]
pub struct Logins(Arc<Mutex<LoginState>>);

/// How long a login may sit unused. A working day, so a browser tab left open
/// overnight asks for the password again in the morning.
pub const LOGIN_IDLE_TIMEOUT: Duration = Duration::from_secs(8 * 60 * 60);

/// The same for "keep me signed in on this device".
pub const REMEMBERED_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The cookie the browser gets. Named for the app so it cannot collide with
/// another service sharing a host.
pub const COOKIE_NAME: &str = "zedis_bridge";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long a login of this kind may sit unused — also the cookie's lifetime.
pub fn idle_timeout(remember: bool) -> Duration {
    if remember {
        REMEMBERED_IDLE_TIMEOUT
    } else {
        LOGIN_IDLE_TIMEOUT
    }
}

impl Login {
    fn live_at(&self, now: u64) -> bool {
        now.saturating_sub(self.last_used) < idle_timeout(self.remember).as_secs()
    }
}

impl Logins {
    /// In memory only.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with(LoginFile::default(), None)
    }

    fn with(mut file: LoginFile, path: Option<PathBuf>) -> Self {
        if file.salt.is_empty() {
            file.salt = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        }
        Self(Arc::new(Mutex::new(LoginState {
            file,
            path,
            dirty: true,
        })))
    }

    /// The logins saved at `path`, minus every one that no longer stands: an
    /// account that is gone, a password that has changed, an idle limit that
    /// has passed. A file that cannot be read is an empty one — the worst
    /// case is that people sign in again.
    pub fn load(path: PathBuf, accounts: &Accounts) -> Self {
        let mut file: LoginFile = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let (now, salt) = (now_secs(), file.salt.clone());
        file.logins.retain(|_, login| {
            login.live_at(now) && accounts.credential_tag(&salt, &login.account).as_deref() == Some(login.tag.as_str())
        });
        let logins = Self::with(file, Some(path));
        logins.flush();
        logins
    }

    pub fn len(&self) -> usize {
        self.0.lock().expect("logins").file.logins.len()
    }

    /// Mint an id for a browser that signed in as `account`, or `None` if
    /// there is no such account.
    pub fn open(&self, account: &str, accounts: &Accounts, remember: bool) -> Option<String> {
        let id = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        {
            let mut state = self.0.lock().expect("logins");
            let tag = accounts.credential_tag(&state.file.salt, account)?;
            let login = Login {
                account: account.to_string(),
                tag,
                last_used: now_secs(),
                remember,
            };
            state.file.logins.insert(hex(&Sha256::digest(&id)), login);
            state.dirty = true;
        }
        self.flush();
        Some(id)
    }

    /// Whose live login `id` is, refreshing its idle clock.
    pub fn account(&self, id: &str) -> Option<String> {
        let key = hex(&Sha256::digest(id));
        let mut state = self.0.lock().expect("logins");
        let now = now_secs();
        match state.file.logins.get_mut(&key) {
            Some(login) if login.live_at(now) => {
                // Saved by the sweep rather than on every request: a last-used
                // time that is a minute stale on disk costs nothing.
                if login.last_used != now {
                    login.last_used = now;
                    state.dirty = true;
                }
                Some(state.file.logins[&key].account.clone())
            }
            // Expired: drop it now rather than wait for the sweep.
            Some(_) => {
                state.file.logins.remove(&key);
                state.dirty = true;
                None
            }
            None => None,
        }
    }

    /// Log `id` out, returning whose login it was.
    pub fn close(&self, id: &str) -> Option<String> {
        let account = {
            let mut state = self.0.lock().expect("logins");
            let removed = state.file.logins.remove(&hex(&Sha256::digest(id)));
            state.dirty |= removed.is_some();
            removed.map(|login| login.account)
        };
        self.flush();
        account
    }

    /// Drop everything idle past its limit, returning how many.
    pub fn sweep(&self) -> usize {
        let mut state = self.0.lock().expect("logins");
        let (now, before) = (now_secs(), state.file.logins.len());
        state.file.logins.retain(|_, login| login.live_at(now));
        let dropped = before - state.file.logins.len();
        state.dirty |= dropped > 0;
        dropped
    }

    /// Write the file if anything changed since the last write.
    pub fn flush(&self) {
        let mut state = self.0.lock().expect("logins");
        let (true, Some(path)) = (state.dirty, state.path.clone()) else {
            return;
        };
        match serde_json::to_vec(&state.file) {
            Ok(bytes) => match write_file_atomic(&path, &bytes) {
                Ok(()) => {
                    restrict(&path);
                    state.dirty = false;
                }
                Err(e) => tracing::warn!(error = %e, path = %path.display(), "could not save the logins"),
            },
            Err(e) => tracing::warn!(error = %e, "could not serialise the logins"),
        }
    }
}

/// Owner-only on unix: the file names who is signed in. On Windows it
/// inherits the config directory's ACL, as `redis-servers.toml` does.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not restrict the logins file");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

/// The value of [`COOKIE_NAME`] in a `Cookie:` header.
///
/// Hand-rolled rather than adding a cookie crate: one name to find in a
/// `a=b; c=d` list, and the parsing rules that matter here are that a value
/// may contain `=` and that surrounding spaces are not part of it.
pub fn cookie_value(header: Option<&str>) -> Option<&str> {
    header?.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COOKIE_NAME).then(|| value.trim())
    })
}

/// The attributes every login cookie of this bridge carries — decided once,
/// at startup, so the cookie that is cleared is the cookie that was set (a
/// browser only drops the one whose `Path` matches).
#[derive(Clone, Debug)]
pub struct CookiePolicy {
    /// `Secure` unless the operator opted out for a plain-http local run: a
    /// deployment that forgets to enable TLS then sees a login that visibly
    /// does not stick, rather than a credential travelling in the clear.
    secure: bool,
    /// Where the bridge is mounted, with its trailing slash: `/`, or
    /// `/zedis/` under `--base-path /zedis`. A bridge that shares its host
    /// name with other applications must not hand them its login: with
    /// `Path=/` the browser sends the cookie to every one of them.
    path: String,
}

impl CookiePolicy {
    /// `base_path` as [`crate::api::normalize_base_path`] leaves it: empty
    /// for the root, else `/prefix` with no trailing slash.
    pub fn new(secure: bool, base_path: &str) -> Self {
        Self {
            secure,
            path: format!("{base_path}/"),
        }
    }

    /// The `Set-Cookie` value that installs `id`.
    pub fn set(&self, id: &str, lifetime: Duration) -> String {
        self.header(id, lifetime.as_secs())
    }

    /// The `Set-Cookie` value that removes it.
    pub fn clear(&self) -> String {
        self.header("", 0)
    }

    fn header(&self, id: &str, max_age: u64) -> String {
        let mut cookie = format!(
            "{COOKIE_NAME}={id}; HttpOnly; SameSite=Strict; Path={}; Max-Age={max_age}",
            self.path
        );
        if self.secure {
            cookie.push_str("; Secure");
        }
        cookie
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts(spec: &str) -> Accounts {
        Accounts(parse_users(spec).expect("a valid spec"))
    }

    #[test]
    fn a_cookie_is_found_among_its_neighbours() {
        assert_eq!(cookie_value(Some("zedis_bridge=abc")), Some("abc"));
        assert_eq!(cookie_value(Some("other=1; zedis_bridge=abc; more=2")), Some("abc"));
        assert_eq!(cookie_value(Some(" zedis_bridge = abc ")), Some("abc"));
        assert_eq!(
            cookie_value(Some("zedis_bridge=a=b")),
            Some("a=b"),
            "a value may hold ="
        );
        assert_eq!(
            cookie_value(Some("zedis_bridgex=abc")),
            None,
            "a prefix is a different cookie"
        );
        assert_eq!(cookie_value(Some("other=1")), None);
        assert_eq!(cookie_value(None), None);
    }

    #[test]
    fn the_cookie_is_scoped_to_where_the_bridge_is_mounted() {
        let lifetime = Duration::from_secs(60);
        let root = CookiePolicy::new(true, "");
        assert_eq!(
            root.set("abc", lifetime),
            "zedis_bridge=abc; HttpOnly; SameSite=Strict; Path=/; Max-Age=60; Secure"
        );
        // Under a prefix the neighbours on the same host never see it, and
        // the clearing cookie names the same path or it clears nothing.
        let nested = CookiePolicy::new(false, "/tools/zedis");
        assert_eq!(
            nested.set("abc", lifetime),
            "zedis_bridge=abc; HttpOnly; SameSite=Strict; Path=/tools/zedis/; Max-Age=60"
        );
        assert_eq!(
            nested.clear(),
            "zedis_bridge=; HttpOnly; SameSite=Strict; Path=/tools/zedis/; Max-Age=0"
        );
    }

    #[test]
    fn a_login_answers_with_its_account_until_it_is_closed() {
        let logins = Logins::new();
        let accounts = accounts("alice@secret");
        assert_eq!(
            logins.open("carol", &accounts, false),
            None,
            "no such account, no login"
        );
        let id = logins.open("alice", &accounts, false).expect("login");
        assert_eq!(logins.account(&id).as_deref(), Some("alice"));
        assert_eq!(logins.account("not-an-id"), None);
        assert_eq!(logins.close(&id).as_deref(), Some("alice"), "a logout says who left");
        assert_eq!(logins.account(&id), None, "a closed login must not work again");
        assert_eq!(logins.close(&id), None, "closing twice is not an error");
    }

    #[test]
    fn two_logins_are_independent() {
        let logins = Logins::new();
        let accounts = accounts("alice@secret");
        let a = logins.open("alice", &accounts, false).expect("login");
        let b = logins.open("alice", &accounts, true).expect("login");
        assert_ne!(a, b);
        logins.close(&a);
        assert_eq!(logins.account(&a), None);
        assert_eq!(
            logins.account(&b).as_deref(),
            Some("alice"),
            "closing one browser must not log the others out"
        );
    }

    #[test]
    fn the_cookie_is_not_readable_by_scripts_and_does_not_travel_cross_site() {
        let secure = CookiePolicy::new(true, "");
        let cookie = secure.set("abc", idle_timeout(false));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Max-Age=28800"), "a working day: {cookie}");
        assert!(
            secure.set("abc", idle_timeout(true)).contains("Max-Age=2592000"),
            "thirty days"
        );
        assert!(
            !CookiePolicy::new(false, "")
                .set("abc", idle_timeout(false))
                .contains("Secure")
        );
        assert!(secure.clear().contains("Max-Age=0"));
    }

    fn scratch_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zedis-logins-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.join("bridge-logins.json")
    }

    #[test]
    fn a_login_survives_a_restart_and_the_file_holds_nothing_presentable() {
        let path = scratch_file("restart");
        let accounts = accounts("alice@secret,bob@hunter2");
        let id = Logins::load(path.clone(), &accounts)
            .open("alice", &accounts, true)
            .expect("login");

        // "Restart": a new store, read from the same file.
        let restarted = Logins::load(path.clone(), &accounts);
        assert_eq!(restarted.account(&id).as_deref(), Some("alice"));
        let on_disk = std::fs::read_to_string(&path).expect("file");
        assert!(!on_disk.contains(&id), "the cookie id itself is never written");
        assert!(!on_disk.contains("secret"), "nor the password");

        // A logout is a logout after a restart too.
        restarted.close(&id);
        assert_eq!(Logins::load(path.clone(), &accounts).account(&id), None);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn changing_a_password_or_removing_an_account_ends_its_saved_logins() {
        let path = scratch_file("revoke");
        let before = accounts("alice@secret,bob@hunter2");
        let logins = Logins::load(path.clone(), &before);
        let alice = logins.open("alice", &before, true).expect("login");
        let bob = logins.open("bob", &before, true).expect("login");

        let after = accounts("alice@a-new-password");
        let restarted = Logins::load(path.clone(), &after);
        assert_eq!(restarted.account(&alice), None, "the password changed");
        assert_eq!(restarted.account(&bob), None, "the account is gone");
        assert_eq!(restarted.len(), 0);

        // Unchanged accounts keep theirs.
        let kept = Logins::load(path.clone(), &before);
        assert_eq!(kept.len(), 0, "and what was dropped stays dropped");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn a_login_lapses_after_its_own_idle_limit() {
        let login = |remember| Login {
            account: "alice".to_string(),
            tag: String::new(),
            last_used: 1_000_000,
            remember,
        };
        let (day, remembered) = (login(false), login(true));
        let nine_hours = 1_000_000 + 9 * 3600;
        assert!(!day.live_at(nine_hours), "a working day");
        assert!(remembered.live_at(nine_hours));
        assert!(remembered.live_at(1_000_000 + 29 * 86400));
        assert!(!remembered.live_at(1_000_000 + 31 * 86400), "thirty days");
    }

    /// The short form's role marker. It rides on the name because a password
    /// may contain `:` and a name may not — see `parse_users`.
    #[test]
    fn the_short_form_marks_a_read_only_account_on_the_name() {
        let users = parse_users("alice@secret,bob:ro@hunter2").expect("two accounts");
        assert!(!users["alice"].read_only);
        assert!(users["bob"].read_only);
        assert_eq!(users["bob"].password, "hunter2", "the role is not part of the password");

        let users = parse_users("carol:ro@p:ss").expect("a role and a colon in the password");
        assert!(users["carol"].read_only);
        assert_eq!(users["carol"].password, "p:ss");
    }

    /// The spelling people reach for first. It cannot be made to mean what
    /// they meant — a password may genuinely end in `:ro` — so it keeps
    /// parsing as a full account with that literal password, and the parser
    /// says so out loud instead of leaving it to be discovered.
    #[test]
    fn the_role_on_the_wrong_side_stays_part_of_the_password() {
        let users = parse_users("alice@secret:ro").expect("parses");
        assert_eq!(users["alice"].password, "secret:ro");
        assert!(!users["alice"].read_only, "the role is only ever on the name");

        let users = parse_users("alice:ro@secret").expect("parses");
        assert_eq!(users["alice"].password, "secret");
        assert!(users["alice"].read_only);
    }

    #[test]
    fn a_read_only_account_is_the_one_the_routes_ask_about() {
        let accounts = accounts("alice@secret,bob:ro@hunter2");
        assert!(!accounts.is_read_only("alice"));
        assert!(accounts.is_read_only("bob"));
        assert_eq!(accounts.read_only_count(), 1);
        assert!(
            accounts.is_read_only("nobody"),
            "an account that is not there cannot be a full one on the way to being refused"
        );
    }

    /// Demoting an account has to end the logins it already has, or a browser
    /// signed in before the change keeps writing for up to thirty days.
    #[test]
    fn demoting_an_account_to_read_only_ends_its_saved_logins() {
        let path = scratch_file("demote");
        let before = accounts("alice@secret");
        let logins = Logins::load(path.clone(), &before);
        let alice = logins.open("alice", &before, true).expect("login");
        assert_eq!(logins.account(&alice).as_deref(), Some("alice"));

        let after = accounts("alice:ro@secret");
        let restarted = Logins::load(path.clone(), &after);
        assert_eq!(restarted.account(&alice), None, "same password, different rights");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn accounts_are_read_from_a_toml_file() {
        let path = scratch_file("users").with_file_name("users.toml");
        std::fs::write(
            &path,
            "[[users]]\nname = \"alice\"\npassword = \"secret\"\n\n\
             [[users]]\nname = \"bob\"\npassword = \"hunter2\"\nread_only = true\n",
        )
        .expect("write users.toml");
        let accounts = Accounts::from_file(&path).expect("two accounts");
        assert_eq!(accounts.len(), 2);
        assert!(accounts.accepts_password("alice", "secret"));
        assert!(accounts.accepts_password("bob", "hunter2"));
        assert!(!accounts.is_read_only("alice"), "read_only defaults to false");
        assert!(accounts.is_read_only("bob"));
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    /// `expect_err` would want `Debug` on `Accounts`, and `Accounts` holds
    /// every password — one stray `{:?}` in a log line and they are in it.
    /// The error is what these tests want anyway.
    fn file_error(path: &Path) -> String {
        match Accounts::from_file(path) {
            Ok(accounts) => panic!("expected an error, got {} accounts", accounts.len()),
            Err(e) => e,
        }
    }

    #[test]
    fn a_users_file_that_is_wrong_says_which_file_and_not_which_password() {
        let path = scratch_file("bad").with_file_name("users.toml");
        let write = |text: &str| std::fs::write(&path, text).expect("write users.toml");

        write("[[users]]\nname = \"alice\"\npassword = \"\"\n");
        let err = file_error(&path);
        assert!(err.contains("users.toml") && err.contains("entry 1"), "{err}");

        write("users = []\n");
        let err = file_error(&path);
        assert!(err.contains("no [[users]] entries"), "{err}");

        write(
            "[[users]]\nname = \"alice\"\npassword = \"first-hunter2\"\n\n\
             [[users]]\nname = \"alice\"\npassword = \"second-hunter2\"\n",
        );
        let err = file_error(&path);
        assert!(err.contains("listed twice"), "{err}");
        assert!(!err.contains("hunter2"), "an error never echoes a password: {err}");

        write("this is not toml {{{\n");
        let err = file_error(&path);
        assert!(err.contains("users.toml"), "{err}");

        let missing = path.with_file_name("gone.toml");
        let err = file_error(&missing);
        assert!(err.contains("gone.toml"), "{err}");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn accounts_are_read_from_the_environment_shape() {
        let users = parse_users("alice@secret,bob@hunter2").expect("two accounts");
        assert_eq!(users.len(), 2);
        assert_eq!(users["alice"].password, "secret");
        assert_eq!(users["bob"].password, "hunter2");
        assert!(!users["alice"].read_only, "a plain entry is a full account");

        let users = parse_users(" alice@secret , bob@hunter2 , ").expect("spaces and a trailing comma");
        assert_eq!(users.len(), 2, "empty entries are skipped");

        let users = parse_users("alice@p@ss:w0rd").expect("a password may hold @ and :");
        assert_eq!(users["alice"].password, "p@ss:w0rd", "the first @ is the separator");
        assert!(
            !users["alice"].read_only,
            "a ':' in the password is not a role — that is why the role is on the name"
        );
    }

    #[test]
    fn a_malformed_account_list_is_refused_without_echoing_a_password() {
        let err = parse_users("alice@secret,bob").expect_err("no @");
        assert!(err.contains("entry 2"), "{err}");
        assert!(!err.contains("secret"), "{err}");

        assert!(parse_users("@secret").expect_err("no name").contains("empty user name"));
        assert!(
            parse_users("alice@")
                .expect_err("no password")
                .contains("empty password")
        );
        let err = parse_users("alice@a,alice@hunter2").expect_err("twice");
        assert!(err.contains("\"alice\" is listed twice"), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        assert!(parse_users("a:b@x").expect_err("colon").contains("':'"));
        assert!(parse_users("").expect_err("empty").contains("no accounts"));
        assert!(parse_users(" , ").expect_err("only separators").contains("no accounts"));
    }

    #[test]
    fn a_basic_header_signs_in_as_the_account_it_names() {
        let accounts = accounts("alice@se:cret,bob@hunter2");
        let basic = |pair: &str| format!("Basic {}", B64.encode(pair));
        assert_eq!(
            accounts.account_for_header(Some(&basic("alice:se:cret"))).as_deref(),
            Some("alice"),
            "the first : is the separator"
        );
        assert_eq!(
            accounts.account_for_header(Some(&basic("bob:hunter2"))).as_deref(),
            Some("bob")
        );
        let lowercase_scheme = format!("basic {}", B64.encode("alice:se:cret"));
        assert_eq!(
            accounts.account_for_header(Some(&lowercase_scheme)).as_deref(),
            Some("alice"),
            "the scheme is case-insensitive"
        );
        assert_eq!(accounts.account_for_header(Some(&basic("alice:wrong"))), None);
        assert_eq!(
            accounts.account_for_header(Some(&basic("carol:se:cret"))),
            None,
            "an unknown name"
        );
        assert_eq!(
            accounts.account_for_header(Some(&basic("alice"))),
            None,
            "no password at all"
        );
        assert_eq!(
            accounts.account_for_header(Some("Bearer se:cret")),
            None,
            "there is no bearer token any more"
        );
        assert_eq!(accounts.account_for_header(Some("Basic not-base64!")), None);
        assert_eq!(accounts.account_for_header(None), None);
    }

    #[test]
    fn a_password_is_checked_against_its_own_account_only() {
        let accounts = accounts("alice@secret,bob@hunter2");
        assert!(accounts.accepts_password("alice", "secret"));
        assert!(!accounts.accepts_password("alice", "Secret"));
        assert!(
            !accounts.accepts_password("alice", "hunter2"),
            "bob's password is not alice's"
        );
        assert!(!accounts.accepts_password("carol", "secret"));
        assert_eq!(accounts.len(), 2);
    }
}

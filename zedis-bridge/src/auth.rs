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

//! The bearer token that gates every route.
//!
//! Stored the way the server list is stored: a file in the same config
//! directory, written through the same atomic helper. Generated on first
//! start so a fresh deployment is never accidentally open.
//!
//! One shared token, because the deployment model is one shared server list
//! that everyone may read and edit (ADR 9). Holding this token therefore
//! means full access to every configured Redis instance, and the power to
//! repoint the configuration at new ones. Distribute it accordingly.
//!
//! Browsers do not hold that token. They post it once to `/v1/login` and get
//! back a cookie carrying a [`Logins`] id instead: `HttpOnly`, so no script
//! on the page can read it, `SameSite=Strict`, so it does not ride along with
//! a cross-site request, and revocable and expiring, which a copy of the
//! long-lived token in `localStorage` would be neither. Scripts and the CLI
//! keep using the bearer header.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use zedis_core::fs::{get_or_create_config_dir, write_file_atomic};

/// Where the token lives, next to `redis-servers.toml`.
pub fn token_path() -> Result<PathBuf, String> {
    let dir = get_or_create_config_dir().map_err(|e| format!("config directory: {e}"))?;
    Ok(dir.join("bridge-token"))
}

/// Read the token, creating one on first start.
pub fn load_or_create() -> Result<String, String> {
    let path = token_path()?;
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    // Two v4 halves: long enough that the token is not worth guessing, and
    // no new dependency — uuid is already here for session ids.
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    write_file_atomic(&path, token.as_bytes()).map_err(|e| format!("write {}: {e}", path.display()))?;
    restrict(&path);
    Ok(token)
}

/// Owner-only on unix. A token readable by every local account is not a
/// token; on Windows the file inherits the config directory's ACL, which is
/// the same protection `redis-servers.toml` gets.
#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, path = %path.display(), "could not restrict the token file");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

/// Whether `header` carries `expected`.
///
/// The comparison does not short-circuit on the first differing byte, so the
/// time it takes does not leak how much of a guess was right.
pub fn presented(header: Option<&str>, expected: &str) -> bool {
    let Some(value) = header else { return false };
    let Some(token) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) else {
        return false;
    };
    let (a, b) = (token.trim().as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_bearer_token_is_accepted() {
        assert!(presented(Some("Bearer secret"), "secret"));
        assert!(presented(Some("bearer secret"), "secret"));
        assert!(!presented(Some("Bearer wrong"), "secret"));
        assert!(!presented(Some("Bearer secre"), "secret"), "a prefix is not enough");
        assert!(
            !presented(Some("Bearer secrets"), "secret"),
            "a superstring is not enough"
        );
        assert!(!presented(Some("secret"), "secret"), "the scheme is required");
        assert!(!presented(None, "secret"));
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
    fn a_login_is_accepted_until_it_is_closed() {
        let logins = Logins::new();
        let id = logins.open();
        assert!(logins.accepts(&id));
        assert!(!logins.accepts("not-an-id"));
        logins.close(&id);
        assert!(!logins.accepts(&id), "a closed login must not work again");
    }

    #[test]
    fn two_logins_are_independent() {
        let logins = Logins::new();
        let (a, b) = (logins.open(), logins.open());
        assert_ne!(a, b);
        logins.close(&a);
        assert!(!logins.accepts(&a));
        assert!(logins.accepts(&b), "closing one browser must not log the others out");
    }

    #[test]
    fn the_cookie_is_not_readable_by_scripts_and_does_not_travel_cross_site() {
        let cookie = set_cookie("abc", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(!set_cookie("abc", false).contains("Secure"));
        assert!(clear_cookie(true).contains("Max-Age=0"));
    }

    #[test]
    fn an_empty_expectation_still_needs_the_scheme() {
        assert!(presented(Some("Bearer "), ""));
        assert!(!presented(Some(""), ""));
    }
}

/// Browser logins: an opaque cookie id, and when it was last used.
///
/// Server-side because the point of the cookie is that the real token never
/// reaches the browser. Dropping an entry logs that browser out.
#[derive(Clone, Default)]
pub struct Logins(Arc<Mutex<HashMap<String, Instant>>>);

/// How long a login may sit unused. A working day, so a browser tab left open
/// overnight asks for the token again in the morning.
pub const LOGIN_IDLE_TIMEOUT: Duration = Duration::from_secs(8 * 60 * 60);

/// The cookie the browser gets. Named for the app so it cannot collide with
/// another service sharing a host.
pub const COOKIE_NAME: &str = "zedis_bridge";

impl Logins {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint an id for a browser that presented the right token.
    pub fn open(&self) -> String {
        let id = Uuid::new_v4().simple().to_string();
        self.0.lock().expect("logins").insert(id.clone(), Instant::now());
        id
    }

    /// Whether `id` is a live login, refreshing its idle clock if so.
    pub fn accepts(&self, id: &str) -> bool {
        let mut guard = self.0.lock().expect("logins");
        match guard.get_mut(id) {
            Some(last) if last.elapsed() < LOGIN_IDLE_TIMEOUT => {
                *last = Instant::now();
                true
            }
            // Expired: drop it now rather than wait for the sweep.
            Some(_) => {
                guard.remove(id);
                false
            }
            None => false,
        }
    }

    pub fn close(&self, id: &str) {
        self.0.lock().expect("logins").remove(id);
    }

    /// Drop everything idle past [`LOGIN_IDLE_TIMEOUT`], returning how many.
    pub fn sweep(&self) -> usize {
        let mut guard = self.0.lock().expect("logins");
        let before = guard.len();
        guard.retain(|_, last| last.elapsed() < LOGIN_IDLE_TIMEOUT);
        before - guard.len()
    }
}

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

/// The `Set-Cookie` value that installs `id`.
///
/// `Secure` unless the operator opted out for a plain-http local run: a
/// deployment that forgets to enable TLS then sees a login that visibly does
/// not stick, rather than a credential travelling in the clear.
pub fn set_cookie(id: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{COOKIE_NAME}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        LOGIN_IDLE_TIMEOUT.as_secs()
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` value that removes it.
pub fn clear_cookie(secure: bool) -> String {
    let mut cookie = format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

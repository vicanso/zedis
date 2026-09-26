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

//! Which commands need a confirmation before the bridge will forward them.
//!
//! The rules are not written here. They are `zedis_connection::danger`, the
//! same classifier the desktop confirm dialog uses, so a command that makes
//! the desktop ask makes the bridge ask, and a server tagged production
//! escalates in both places. The web client renders the returned
//! [`DangerKind`]'s i18n key exactly as the desktop dialog does.
//!
//! This costs the bridge its perfect ignorance of Redis: to classify, it has
//! to read the command name and arguments out of the frame. It still learns
//! no *semantics* — every judgement is delegated — so adding a Redis feature
//! to Zedis still changes nothing here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zedis_connection::{
    ConfirmStrictness, DangerKind, RedisServer, WRITE_UNLOCK_SECS, classify_dangerous, confirm_strictness,
    is_read_only_command, is_write_command, requires_write_confirm,
};

/// What the bridge will do with a command.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Forward it.
    Allow,
    /// Forward it: it needed a confirmation and the caller gave one. Kept
    /// apart from [`Verdict::Allow`] for the audit log alone — a command a
    /// person had to confirm is a command the log keeps (ADR 11), and the
    /// forwarding treats the two the same.
    Confirmed {
        kind: DangerKind,
        strictness: ConfirmStrictness,
    },
    /// Refuse until the caller repeats the request with a confirmation that
    /// satisfies `strictness`.
    Confirm {
        kind: DangerKind,
        strictness: ConfirmStrictness,
    },
    /// Refuse, full stop. A read-only account asked for something that is
    /// not a read, and unlike [`Verdict::Confirm`] there is nothing the
    /// caller can send back to get past it — that is the whole difference
    /// between a question and a permission.
    Deny,
}

/// Decide whether `args` may be forwarded to `server`.
///
/// `read_only` is the caller's account, and it is checked **first and
/// separately**: the confirmation machinery below asks a question a caller
/// answers, which is exactly what a permission must not be. A read-only
/// account never reaches it.
///
/// The read test is an allowlist (`zedis_connection::is_read_only_command`),
/// not the inverse of the danger classifier — a command nobody has
/// classified is refused for a read-only account and merely unconfirmed for
/// a full one, which is the right way round for each.
///
/// `confirm` is what the caller sent back after being refused once: any
/// non-empty string satisfies [`ConfirmStrictness::Click`], while
/// [`ConfirmStrictness::TypeName`] — what a production-tagged server demands
/// for a destructive command — is satisfied only by the server's own name.
/// That mirrors the desktop dialog, where the same escalation makes the user
/// type the name rather than click once.
///
/// `unlocked` is whether this caller has an open write window on this entry
/// (`Unlocks`). An entry whose writes are locked (`RedisServer::write_locked`)
/// refuses every non-read outside such a window with
/// [`DangerKind::WriteLocked`] — a question, like the rest, so a script that
/// answers it (with the name, on production) gets that one command through,
/// while the page opens the window once and stops being asked (ADR 14).
pub fn check(
    server: &RedisServer,
    args: &[Vec<u8>],
    confirm: Option<&str>,
    read_only: bool,
    unlocked: bool,
) -> Verdict {
    if read_only && !reads_only(args) {
        return Verdict::Deny;
    }
    let kind = match classify(server, args) {
        Some(kind) => kind,
        None if server.write_locked() && !unlocked && !reads_only(args) => DangerKind::WriteLocked,
        None => return Verdict::Allow,
    };
    let strictness = confirm_strictness(server, &kind);
    let satisfied = match (strictness, confirm) {
        (_, None) => false,
        (ConfirmStrictness::Click, Some(token)) => !token.trim().is_empty(),
        (ConfirmStrictness::TypeName, Some(token)) => token.trim() == server.name.trim(),
    };
    if satisfied {
        Verdict::Confirmed { kind, strictness }
    } else {
        Verdict::Confirm { kind, strictness }
    }
}

/// Whether this command only reads. An empty frame is not a read: the
/// decoder refuses it anyway, and a gate must not be the place that lets an
/// unnameable command through.
fn reads_only(args: &[Vec<u8>]) -> bool {
    let Some((name, rest)) = words(args) else {
        return false;
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    is_read_only_command(&name, &rest)
}

/// The desktop's rule, in the desktop's order: the specific classifier first,
/// then the per-server "confirm every write" setting as a catch-all.
fn classify(server: &RedisServer, args: &[Vec<u8>]) -> Option<DangerKind> {
    let (name, rest) = words(args)?;
    if let Some(kind) = classify_dangerous(&name, &rest) {
        return Some(kind);
    }
    if requires_write_confirm(server) && is_write_command(&name) {
        return Some(DangerKind::GenericWrite);
    }
    None
}

/// The open write windows: which account has unlocked which entry, until
/// when. Shared by the routes and swept with the sessions; a window that
/// has ended is one that was never there.
#[derive(Clone, Default)]
pub struct Unlocks(Arc<Mutex<HashMap<(String, String), Instant>>>);

impl Unlocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or extend) the window for `account` on `server_id`, answering
    /// how long it is.
    pub fn unlock(&self, account: &str, server_id: &str) -> Duration {
        let window = Duration::from_secs(WRITE_UNLOCK_SECS);
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((account.to_string(), server_id.to_string()), Instant::now() + window);
        window
    }

    /// Close the window; `true` when there was one to close.
    pub fn lock(&self, account: &str, server_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(account.to_string(), server_id.to_string()))
            .is_some_and(|until| until > Instant::now())
    }

    pub fn active(&self, account: &str, server_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(account.to_string(), server_id.to_string()))
            .is_some_and(|until| *until > Instant::now())
    }

    /// Drop the windows that have ended; how many.
    pub fn sweep(&self) -> usize {
        let mut windows = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = windows.len();
        let now = Instant::now();
        windows.retain(|_, until| *until > now);
        before - windows.len()
    }
}

/// The command name and its arguments as text, for the classifier and the
/// audit log only.
///
/// Lossy on purpose: a key can be arbitrary bytes, and the classifier reads
/// argument text (`DEBUG SLEEP`, `KEYS <pattern>`). The bytes that reach
/// Redis are never these — the forwarded command is rebuilt from the raw
/// arguments.
pub(crate) fn words(args: &[Vec<u8>]) -> Option<(String, Vec<String>)> {
    let (name, rest) = args.split_first()?;
    Some((
        String::from_utf8_lossy(name).into_owned(),
        rest.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|p| p.as_bytes().to_vec()).collect()
    }

    fn plain() -> RedisServer {
        RedisServer {
            id: "s1".to_string(),
            name: "staging".to_string(),
            ..Default::default()
        }
    }

    /// A production-tagged entry, where a destructive command has to be
    /// confirmed by typing the name.
    fn prod() -> RedisServer {
        RedisServer {
            id: "s2".to_string(),
            name: "production".to_string(),
            tag_color: Some("red".to_string()),
            ..Default::default()
        }
    }

    /// A read-only account is refused, and cannot talk its way out: unlike a
    /// confirmation, there is no token that turns the answer round.
    #[test]
    fn a_read_only_account_may_read_and_nothing_else() {
        assert_eq!(check(&plain(), &args(&["GET", "k"]), None, true, false), Verdict::Allow);
        assert_eq!(
            check(&plain(), &args(&["SCAN", "0"]), None, true, false),
            Verdict::Allow
        );
        assert_eq!(check(&plain(), &args(&["INFO"]), None, true, false), Verdict::Allow);
        assert_eq!(
            check(&plain(), &args(&["SET", "k", "v"]), None, true, false),
            Verdict::Deny
        );
        assert_eq!(check(&plain(), &args(&["DEL", "k"]), None, true, false), Verdict::Deny);
        assert_eq!(check(&plain(), &args(&["FLUSHALL"]), None, true, false), Verdict::Deny);
        // Feature probe on connect (`ACL LOG 0`) is a read; RESET is not.
        assert_eq!(
            check(&plain(), &args(&["ACL", "LOG", "0"]), None, true, false),
            Verdict::Allow
        );
        assert_eq!(
            check(&plain(), &args(&["ACL", "LOG", "RESET"]), None, true, false),
            Verdict::Deny
        );
    }

    /// The confirmation is a question and the role is a permission, so the
    /// answer to the question must not reach the permission. A confirmed
    /// destructive command from a read-only account is still refused.
    #[test]
    fn a_confirmation_does_not_buy_a_read_only_account_a_write() {
        for confirm in [None, Some(""), Some("yes"), Some("staging")] {
            assert_eq!(
                check(&plain(), &args(&["FLUSHALL"]), confirm, true, false),
                Verdict::Deny,
                "confirm={confirm:?}"
            );
        }
        // And on a production server, where the strict token is the name.
        assert_eq!(
            check(&prod(), &args(&["FLUSHALL"]), Some("production"), true, false),
            Verdict::Deny
        );
    }

    /// The commands a denylist of writes would have waved through. This is
    /// the test that says why `is_read_only_command` is an allowlist.
    #[test]
    fn a_read_only_account_cannot_reach_a_write_that_reads_like_one() {
        for cmd in [
            vec!["EVAL", "return redis.call('set', KEYS[1], '1')", "1", "k"],
            vec!["BITFIELD", "k", "SET", "u8", "0", "1"],
            vec!["GETDEL", "k"],
            vec!["GETEX", "k", "EX", "1"],
            vec!["JSON.SET", "doc", "$", "1"],
            vec!["TS.ADD", "series", "*", "1"],
            vec!["SOMETHING.NEW", "k"],
        ] {
            assert_eq!(
                check(&plain(), &args(&cmd), Some("yes"), true, false),
                Verdict::Deny,
                "{cmd:?}"
            );
        }
    }

    /// A full account is unchanged by any of this: the role only ever
    /// subtracts, so every existing verdict has to survive it.
    #[test]
    fn a_full_account_is_judged_exactly_as_before() {
        assert_eq!(
            check(&plain(), &args(&["GETDEL", "k"]), None, false, false),
            Verdict::Allow
        );
        assert_eq!(
            check(&plain(), &args(&["EVAL", "x", "0"]), None, false, false),
            Verdict::Allow
        );
        assert!(matches!(
            check(&plain(), &args(&["FLUSHALL"]), None, false, false),
            Verdict::Confirm { .. }
        ));
    }

    /// An empty frame is not a read. The decoder refuses it anyway, but a
    /// gate that answered "allow" to a command with no name would be the
    /// wrong thing to have behind it.
    #[test]
    fn an_empty_command_is_not_a_read() {
        assert_eq!(check(&plain(), &[], None, true, false), Verdict::Deny);
    }

    #[test]
    fn a_read_goes_straight_through() {
        assert_eq!(
            check(&plain(), &args(&["GET", "k"]), None, false, false),
            Verdict::Allow
        );
        assert_eq!(check(&plain(), &args(&["PING"]), None, false, false), Verdict::Allow);
    }

    #[test]
    fn a_destructive_command_is_refused_until_confirmed() {
        let server = plain();
        let refused = check(&server, &args(&["FLUSHALL"]), None, false, false);
        let Verdict::Confirm { kind, .. } = refused else {
            panic!("FLUSHALL must be gated, got {refused:?}");
        };
        assert_eq!(kind, DangerKind::FlushAll);
        // The same command with a confirmation goes through — as *confirmed*,
        // which is how the audit log tells it from a plain read.
        assert_eq!(
            check(&server, &args(&["FLUSHALL"]), Some("yes"), false, false),
            Verdict::Confirmed {
                kind: DangerKind::FlushAll,
                strictness: ConfirmStrictness::Click
            }
        );
        assert_eq!(
            check(&prod(), &args(&["FLUSHALL"]), Some("production"), false, false),
            Verdict::Confirmed {
                kind: DangerKind::FlushAll,
                strictness: ConfirmStrictness::TypeName
            }
        );
    }

    #[test]
    fn an_empty_confirmation_does_not_count() {
        assert!(matches!(
            check(&plain(), &args(&["FLUSHALL"]), Some("   "), false, false),
            Verdict::Confirm { .. }
        ));
    }

    #[test]
    fn a_binary_key_does_not_break_classification() {
        let server = plain();
        let mut a = args(&["GET"]);
        a.push(vec![0xff, 0x00, 0xfe]);
        assert_eq!(check(&server, &a, None, false, false), Verdict::Allow);
    }

    #[test]
    fn an_empty_command_is_allowed_here_and_refused_by_the_decoder() {
        // `resp::decode_command` rejects an empty frame before policy runs;
        // this only pins that policy itself does not panic on one.
        assert_eq!(check(&plain(), &[], None, false, false), Verdict::Allow);
    }

    #[test]
    fn the_write_confirm_setting_gates_plain_writes() {
        let mut server = plain();
        assert_eq!(
            check(&server, &args(&["SET", "k", "v"]), None, false, false),
            Verdict::Allow
        );
        server.require_confirm_writes = Some(true);
        assert!(
            matches!(
                check(&server, &args(&["SET", "k", "v"]), None, false, false),
                Verdict::Confirm {
                    kind: DangerKind::GenericWrite,
                    ..
                }
            ),
            "require_confirm_writes must gate an ordinary write"
        );
        // Reads stay free even then.
        assert_eq!(check(&server, &args(&["GET", "k"]), None, false, false), Verdict::Allow);
    }

    /// A locked entry refuses a write until unlocked — as a question, so a
    /// script that answers it gets the one command through — and reads are
    /// never asked. A window opened for the caller stops the question.
    #[test]
    fn a_locked_entry_asks_for_every_write_until_it_is_unlocked() {
        let mut locked = plain();
        locked.write_lock = Some(true);
        assert_eq!(check(&locked, &args(&["GET", "k"]), None, false, false), Verdict::Allow);
        assert!(matches!(
            check(&locked, &args(&["SET", "k", "v"]), None, false, false),
            Verdict::Confirm {
                kind: DangerKind::WriteLocked,
                strictness: ConfirmStrictness::Click
            }
        ));
        assert!(matches!(
            check(&locked, &args(&["SET", "k", "v"]), Some("yes"), false, false),
            Verdict::Confirmed {
                kind: DangerKind::WriteLocked,
                ..
            }
        ));
        assert_eq!(
            check(&locked, &args(&["SET", "k", "v"]), None, false, true),
            Verdict::Allow
        );
        // A destructive command keeps its own question inside the window.
        assert!(matches!(
            check(&locked, &args(&["FLUSHDB"]), None, false, true),
            Verdict::Confirm {
                kind: DangerKind::FlushDb,
                ..
            }
        ));
        // A read-only account is refused before the lock is consulted.
        assert_eq!(
            check(&locked, &args(&["SET", "k", "v"]), Some("yes"), true, true),
            Verdict::Deny
        );
    }

    /// Production is locked by its tag alone, and unlocking it asks for the
    /// name; an entry may say otherwise either way.
    #[test]
    fn production_is_locked_by_default_and_unlocked_by_name() {
        let production = prod();
        assert!(production.write_locked());
        assert!(matches!(
            check(&production, &args(&["SET", "k", "v"]), Some("yes"), false, false),
            Verdict::Confirm {
                kind: DangerKind::WriteLocked,
                strictness: ConfirmStrictness::TypeName
            }
        ));
        assert!(matches!(
            check(&production, &args(&["SET", "k", "v"]), Some("production"), false, false),
            Verdict::Confirmed { .. }
        ));
        let mut opted_out = prod();
        opted_out.write_lock = Some(false);
        assert_eq!(
            check(&opted_out, &args(&["SET", "k", "v"]), None, false, false),
            Verdict::Allow
        );
        assert!(!plain().write_locked());
    }

    #[test]
    fn a_window_is_per_account_and_entry_and_ends() {
        let unlocks = Unlocks::new();
        assert!(!unlocks.active("alice", "s1"));
        assert_eq!(unlocks.unlock("alice", "s1"), Duration::from_secs(WRITE_UNLOCK_SECS));
        assert!(unlocks.active("alice", "s1"));
        assert!(!unlocks.active("alice", "s2"), "another entry");
        assert!(!unlocks.active("bob", "s1"), "another account");
        assert!(unlocks.lock("alice", "s1"), "there was a window to close");
        assert!(!unlocks.active("alice", "s1"));
        assert!(!unlocks.lock("alice", "s1"), "and now there is not");
        // An ended window is swept, and was already inactive.
        unlocks.0.lock().expect("lock").insert(
            ("alice".to_string(), "s3".to_string()),
            Instant::now() - Duration::from_secs(1),
        );
        assert!(!unlocks.active("alice", "s3"));
        assert_eq!(unlocks.sweep(), 1);
    }
}

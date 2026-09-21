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

use zedis_connection::{
    ConfirmStrictness, DangerKind, RedisServer, classify_dangerous, confirm_strictness, is_read_only_command,
    is_write_command, requires_write_confirm,
};

/// What the bridge will do with a command.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Forward it.
    Allow,
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
pub fn check(server: &RedisServer, args: &[Vec<u8>], confirm: Option<&str>, read_only: bool) -> Verdict {
    if read_only && !reads_only(args) {
        return Verdict::Deny;
    }
    let Some(kind) = classify(server, args) else {
        return Verdict::Allow;
    };
    let strictness = confirm_strictness(server, &kind);
    let satisfied = match (strictness, confirm) {
        (_, None) => false,
        (ConfirmStrictness::Click, Some(token)) => !token.trim().is_empty(),
        (ConfirmStrictness::TypeName, Some(token)) => token.trim() == server.name.trim(),
    };
    if satisfied {
        Verdict::Allow
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

/// The command name and its arguments as text, for the classifier only.
///
/// Lossy on purpose: a key can be arbitrary bytes, and the classifier reads
/// argument text (`DEBUG SLEEP`, `KEYS <pattern>`). The bytes that reach
/// Redis are never these — the forwarded command is rebuilt from the raw
/// arguments.
fn words(args: &[Vec<u8>]) -> Option<(String, Vec<String>)> {
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
        assert_eq!(check(&plain(), &args(&["GET", "k"]), None, true), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["SCAN", "0"]), None, true), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["INFO"]), None, true), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["SET", "k", "v"]), None, true), Verdict::Deny);
        assert_eq!(check(&plain(), &args(&["DEL", "k"]), None, true), Verdict::Deny);
        assert_eq!(check(&plain(), &args(&["FLUSHALL"]), None, true), Verdict::Deny);
        // Feature probe on connect (`ACL LOG 0`) is a read; RESET is not.
        assert_eq!(check(&plain(), &args(&["ACL", "LOG", "0"]), None, true), Verdict::Allow);
        assert_eq!(
            check(&plain(), &args(&["ACL", "LOG", "RESET"]), None, true),
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
                check(&plain(), &args(&["FLUSHALL"]), confirm, true),
                Verdict::Deny,
                "confirm={confirm:?}"
            );
        }
        // And on a production server, where the strict token is the name.
        assert_eq!(
            check(&prod(), &args(&["FLUSHALL"]), Some("production"), true),
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
                check(&plain(), &args(&cmd), Some("yes"), true),
                Verdict::Deny,
                "{cmd:?}"
            );
        }
    }

    /// A full account is unchanged by any of this: the role only ever
    /// subtracts, so every existing verdict has to survive it.
    #[test]
    fn a_full_account_is_judged_exactly_as_before() {
        assert_eq!(check(&plain(), &args(&["GETDEL", "k"]), None, false), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["EVAL", "x", "0"]), None, false), Verdict::Allow);
        assert!(matches!(
            check(&plain(), &args(&["FLUSHALL"]), None, false),
            Verdict::Confirm { .. }
        ));
    }

    /// An empty frame is not a read. The decoder refuses it anyway, but a
    /// gate that answered "allow" to a command with no name would be the
    /// wrong thing to have behind it.
    #[test]
    fn an_empty_command_is_not_a_read() {
        assert_eq!(check(&plain(), &[], None, true), Verdict::Deny);
    }

    #[test]
    fn a_read_goes_straight_through() {
        assert_eq!(check(&plain(), &args(&["GET", "k"]), None, false), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["PING"]), None, false), Verdict::Allow);
    }

    #[test]
    fn a_destructive_command_is_refused_until_confirmed() {
        let server = plain();
        let refused = check(&server, &args(&["FLUSHALL"]), None, false);
        let Verdict::Confirm { kind, .. } = refused else {
            panic!("FLUSHALL must be gated, got {refused:?}");
        };
        assert_eq!(kind, DangerKind::FlushAll);
        // The same command with a confirmation goes through.
        assert_eq!(check(&server, &args(&["FLUSHALL"]), Some("yes"), false), Verdict::Allow);
    }

    #[test]
    fn an_empty_confirmation_does_not_count() {
        assert!(matches!(
            check(&plain(), &args(&["FLUSHALL"]), Some("   "), false),
            Verdict::Confirm { .. }
        ));
    }

    #[test]
    fn a_binary_key_does_not_break_classification() {
        let server = plain();
        let mut a = args(&["GET"]);
        a.push(vec![0xff, 0x00, 0xfe]);
        assert_eq!(check(&server, &a, None, false), Verdict::Allow);
    }

    #[test]
    fn an_empty_command_is_allowed_here_and_refused_by_the_decoder() {
        // `resp::decode_command` rejects an empty frame before policy runs;
        // this only pins that policy itself does not panic on one.
        assert_eq!(check(&plain(), &[], None, false), Verdict::Allow);
    }

    #[test]
    fn the_write_confirm_setting_gates_plain_writes() {
        let mut server = plain();
        assert_eq!(check(&server, &args(&["SET", "k", "v"]), None, false), Verdict::Allow);
        server.require_confirm_writes = Some(true);
        assert!(
            matches!(
                check(&server, &args(&["SET", "k", "v"]), None, false),
                Verdict::Confirm {
                    kind: DangerKind::GenericWrite,
                    ..
                }
            ),
            "require_confirm_writes must gate an ordinary write"
        );
        // Reads stay free even then.
        assert_eq!(check(&server, &args(&["GET", "k"]), None, false), Verdict::Allow);
    }
}

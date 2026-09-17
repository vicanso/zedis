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
    ConfirmStrictness, DangerKind, RedisServer, classify_dangerous, confirm_strictness, is_write_command,
    requires_write_confirm,
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
}

/// Decide whether `args` may be forwarded to `server`.
///
/// `confirm` is what the caller sent back after being refused once: any
/// non-empty string satisfies [`ConfirmStrictness::Click`], while
/// [`ConfirmStrictness::TypeName`] — what a production-tagged server demands
/// for a destructive command — is satisfied only by the server's own name.
/// That mirrors the desktop dialog, where the same escalation makes the user
/// type the name rather than click once.
pub fn check(server: &RedisServer, args: &[Vec<u8>], confirm: Option<&str>) -> Verdict {
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

    #[test]
    fn a_read_goes_straight_through() {
        assert_eq!(check(&plain(), &args(&["GET", "k"]), None), Verdict::Allow);
        assert_eq!(check(&plain(), &args(&["PING"]), None), Verdict::Allow);
    }

    #[test]
    fn a_destructive_command_is_refused_until_confirmed() {
        let server = plain();
        let refused = check(&server, &args(&["FLUSHALL"]), None);
        let Verdict::Confirm { kind, .. } = refused else {
            panic!("FLUSHALL must be gated, got {refused:?}");
        };
        assert_eq!(kind, DangerKind::FlushAll);
        // The same command with a confirmation goes through.
        assert_eq!(check(&server, &args(&["FLUSHALL"]), Some("yes")), Verdict::Allow);
    }

    #[test]
    fn an_empty_confirmation_does_not_count() {
        assert!(matches!(
            check(&plain(), &args(&["FLUSHALL"]), Some("   ")),
            Verdict::Confirm { .. }
        ));
    }

    #[test]
    fn a_binary_key_does_not_break_classification() {
        let server = plain();
        let mut a = args(&["GET"]);
        a.push(vec![0xff, 0x00, 0xfe]);
        assert_eq!(check(&server, &a, None), Verdict::Allow);
    }

    #[test]
    fn an_empty_command_is_allowed_here_and_refused_by_the_decoder() {
        // `resp::decode_command` rejects an empty frame before policy runs;
        // this only pins that policy itself does not panic on one.
        assert_eq!(check(&plain(), &[], None), Verdict::Allow);
    }

    #[test]
    fn the_write_confirm_setting_gates_plain_writes() {
        let mut server = plain();
        assert_eq!(check(&server, &args(&["SET", "k", "v"]), None), Verdict::Allow);
        server.require_confirm_writes = Some(true);
        assert!(
            matches!(
                check(&server, &args(&["SET", "k", "v"]), None),
                Verdict::Confirm {
                    kind: DangerKind::GenericWrite,
                    ..
                }
            ),
            "require_confirm_writes must gate an ordinary write"
        );
        // Reads stay free even then.
        assert_eq!(check(&server, &args(&["GET", "k"]), None), Verdict::Allow);
    }
}

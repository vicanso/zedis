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

//! What an audit log keeps of a command, and what it must never carry.
//!
//! The HTTP bridge records the door it is (ADR 11). Which commands deserve a
//! line is command knowledge, so it lives here beside `danger.rs` rather
//! than in the bridge: [`is_administration_command`] names the commands that
//! change the *server* rather than its data — who may connect, what code it
//! runs, where its data comes from, whether it is running — and
//! [`redact_secrets`] blanks the arguments a line must not keep, the
//! passwords that `AUTH`, `ACL SETUSER`, `CONFIG SET requirepass` and
//! `MIGRATE … AUTH` take in the clear.

/// Whether `name args…` administers the server.
///
/// Distinct from [`classify_dangerous`](crate::classify_dangerous): that
/// list is what a person has to *confirm*, this one is what a log has to
/// *keep*. Everything confirmed is logged as well — the bridge arranges that
/// — and this set is the wider of the two on purpose: `ACL SETUSER` or
/// `MODULE LOAD` destroy nothing and still change what the server is.
/// `DEBUG` is left to the classifier, whose destructive subcommands are
/// confirmed and so logged; the rest of it is inspection.
pub fn is_administration_command(name: &str, args: &[String]) -> bool {
    let sub = args.first().map(|s| s.to_ascii_uppercase());
    let sub = sub.as_deref();
    match name.to_ascii_uppercase().as_str() {
        "FLUSHALL" | "FLUSHDB" | "SWAPDB" | "SHUTDOWN" | "REPLICAOF" | "SLAVEOF" | "FAILOVER" | "SAVE" | "BGSAVE"
        | "BGREWRITEAOF" | "AUTH" | "PSYNC" | "SYNC" => true,
        // `HELLO 3` negotiates a protocol; `HELLO 3 AUTH user pass` signs in.
        "HELLO" => args.iter().any(|a| a.eq_ignore_ascii_case("AUTH")),
        "CONFIG" => matches!(sub, Some("SET" | "REWRITE" | "RESETSTAT")),
        "ACL" => matches!(sub, Some("SETUSER" | "DELUSER" | "LOAD" | "SAVE")),
        "MODULE" => matches!(sub, Some("LOAD" | "LOADEX" | "UNLOAD")),
        "FUNCTION" => matches!(sub, Some("LOAD" | "DELETE" | "FLUSH" | "RESTORE" | "KILL")),
        "SCRIPT" => matches!(sub, Some("FLUSH" | "KILL")),
        "CLIENT" => matches!(sub, Some("KILL" | "PAUSE" | "UNPAUSE")),
        "CLUSTER" => matches!(
            sub,
            Some(
                "MEET"
                    | "FORGET"
                    | "REPLICATE"
                    | "FAILOVER"
                    | "RESET"
                    | "ADDSLOTS"
                    | "ADDSLOTSRANGE"
                    | "DELSLOTS"
                    | "DELSLOTSRANGE"
                    | "SETSLOT"
                    | "FLUSHSLOTS"
                    | "BUMPEPOCH"
                    | "SET-CONFIG-EPOCH"
                    | "SAVECONFIG"
                    | "MIGRATION"
            )
        ),
        "SENTINEL" => match sub {
            Some("SET" | "FAILOVER" | "MONITOR" | "REMOVE" | "RESET") => true,
            Some("CONFIG") => args.get(1).is_some_and(|a| a.eq_ignore_ascii_case("SET")),
            _ => false,
        },
        _ => false,
    }
}

/// What a blanked argument becomes.
const MASK: &str = "***";

/// `args` with every credential blanked — the only form of a command's
/// arguments a log may keep. Unknown commands come back untouched: a
/// credential this function has not been taught reaches the file, which is
/// why a command that takes one in an argument is added here in the same
/// change that sends it.
pub fn redact_secrets(name: &str, args: &[String]) -> Vec<String> {
    let mut out = args.to_vec();
    let sub = args.first().map(|s| s.to_ascii_uppercase());
    match (name.to_ascii_uppercase().as_str(), sub.as_deref()) {
        // `AUTH [username] password`: everything is a credential.
        ("AUTH", _) => out.iter_mut().for_each(|a| *a = MASK.to_string()),
        // `HELLO 3 AUTH username password [SETNAME …]`: the password.
        ("HELLO", _) => {
            if let Some(i) = position_from(args, 1, "AUTH") {
                mask(&mut out, i + 2);
            }
        }
        // `ACL SETUSER name >add <remove #hash !hash …`: the password tokens,
        // keeping the operator so the line still says what was done.
        ("ACL", Some("SETUSER")) => {
            for arg in out.iter_mut().skip(2) {
                if arg.starts_with(['>', '<', '#', '!']) {
                    *arg = format!("{}{MASK}", &arg[..1]);
                }
            }
        }
        // `CONFIG SET name value [name value …]`: the values of the
        // credential settings.
        ("CONFIG", Some("SET")) => {
            let mut i = 1;
            while i + 1 < out.len() {
                if is_secret_setting(&args[i]) {
                    out[i + 1] = MASK.to_string();
                }
                i += 2;
            }
        }
        // `MIGRATE host port key db timeout [COPY] [REPLACE] [AUTH password]
        // [AUTH2 username password] [KEYS …]`: the option words start after
        // the five fixed arguments, so a key named `AUTH` is not one.
        ("MIGRATE", _) => {
            if let Some(i) = position_from(args, 5, "AUTH") {
                mask(&mut out, i + 1);
            }
            if let Some(i) = position_from(args, 5, "AUTH2") {
                mask(&mut out, i + 2);
            }
        }
        // `SENTINEL SET master auth-pass password`.
        ("SENTINEL", Some("SET")) => {
            if let Some(i) = position_from(args, 2, "auth-pass") {
                mask(&mut out, i + 1);
            }
        }
        _ => {}
    }
    out
}

/// The settings whose value is a credential.
fn is_secret_setting(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "requirepass" | "masterauth" | "tls-key-file-pass" | "tls-client-key-file-pass"
    )
}

/// Index of the first `word` at or after `from`, compared without case.
fn position_from(args: &[String], from: usize, word: &str) -> Option<usize> {
    args.iter()
        .enumerate()
        .skip(from)
        .find(|(_, a)| a.eq_ignore_ascii_case(word))
        .map(|(i, _)| i)
}

fn mask(out: &mut [String], index: usize) {
    if let Some(slot) = out.get_mut(index) {
        *slot = MASK.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn administration_is_what_changes_the_server_not_its_data() {
        for (name, rest) in [
            ("CONFIG", vec!["SET", "maxmemory", "1gb"]),
            ("config", vec!["rewrite"]),
            ("ACL", vec!["SETUSER", "bob", "on"]),
            ("ACL", vec!["DELUSER", "bob"]),
            ("REPLICAOF", vec!["NO", "ONE"]),
            ("CLUSTER", vec!["SETSLOT", "5", "NODE", "abc"]),
            ("MODULE", vec!["LOAD", "/x.so"]),
            ("FUNCTION", vec!["FLUSH"]),
            ("SCRIPT", vec!["KILL"]),
            ("CLIENT", vec!["KILL", "ID", "7"]),
            ("SHUTDOWN", vec![]),
            ("FLUSHDB", vec![]),
            ("SWAPDB", vec!["0", "1"]),
            ("BGSAVE", vec![]),
            ("AUTH", vec!["secret"]),
            ("HELLO", vec!["3", "AUTH", "u", "p"]),
            ("SENTINEL", vec!["CONFIG", "SET", "resolve-hostnames", "yes"]),
        ] {
            assert!(is_administration_command(name, &args(&rest)), "{name} {rest:?}");
        }
        for (name, rest) in [
            ("GET", vec!["k"]),
            ("SET", vec!["k", "v"]),
            ("DEL", vec!["k"]),
            ("CONFIG", vec!["GET", "*"]),
            ("ACL", vec!["LIST"]),
            ("ACL", vec!["LOG", "0"]),
            ("CLUSTER", vec!["NODES"]),
            ("CLIENT", vec!["LIST"]),
            ("CLIENT", vec!["SETNAME", "zedis"]),
            ("HELLO", vec!["3"]),
            ("MODULE", vec!["LIST"]),
            ("FUNCTION", vec!["LIST"]),
            ("SCRIPT", vec!["EXISTS", "sha"]),
            ("SENTINEL", vec!["CONFIG", "GET", "*"]),
            ("DEBUG", vec!["OBJECT", "k"]),
        ] {
            assert!(!is_administration_command(name, &args(&rest)), "{name} {rest:?}");
        }
    }

    #[test]
    fn every_credential_argument_is_blanked_and_nothing_else_is() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("AUTH", &["hunter2"], &["***"]),
            ("auth", &["alice", "hunter2"], &["***", "***"]),
            (
                "HELLO",
                &["3", "AUTH", "alice", "hunter2", "SETNAME", "z"],
                &["3", "AUTH", "alice", "***", "SETNAME", "z"],
            ),
            ("HELLO", &["3"], &["3"]),
            (
                "ACL",
                &[
                    "SETUSER", "bob", "on", ">hunter2", "<old", "#abc", "!def", "+@read", "~k*",
                ],
                &["SETUSER", "bob", "on", ">***", "<***", "#***", "!***", "+@read", "~k*"],
            ),
            (
                "CONFIG",
                &["SET", "requirepass", "hunter2"],
                &["SET", "requirepass", "***"],
            ),
            (
                "CONFIG",
                &["SET", "maxmemory", "1gb", "MasterAuth", "s", "tls-key-file-pass", "p"],
                &[
                    "SET",
                    "maxmemory",
                    "1gb",
                    "MasterAuth",
                    "***",
                    "tls-key-file-pass",
                    "***",
                ],
            ),
            ("CONFIG", &["GET", "requirepass"], &["GET", "requirepass"]),
            (
                "MIGRATE",
                &["h", "6379", "AUTH", "0", "5000", "AUTH", "p", "AUTH2", "u", "q"],
                &["h", "6379", "AUTH", "0", "5000", "AUTH", "***", "AUTH2", "u", "***"],
            ),
            (
                "SENTINEL",
                &["SET", "m", "auth-pass", "p", "down-after-milliseconds", "5"],
                &["SET", "m", "auth-pass", "***", "down-after-milliseconds", "5"],
            ),
            ("SET", &["k", "hunter2"], &["k", "hunter2"]),
        ];
        for (name, given, want) in cases {
            assert_eq!(redact_secrets(name, &args(given)), args(want), "{name} {given:?}");
        }
    }

    #[test]
    fn a_short_command_does_not_panic_the_redaction() {
        for (name, rest) in [
            ("HELLO", vec!["3", "AUTH"]),
            ("MIGRATE", vec!["h"]),
            ("ACL", vec!["SETUSER"]),
            ("CONFIG", vec!["SET", "requirepass"]),
            ("SENTINEL", vec!["SET"]),
            ("AUTH", vec![]),
        ] {
            let given = args(&rest);
            assert_eq!(redact_secrets(name, &given).len(), given.len(), "{name}");
        }
    }
}

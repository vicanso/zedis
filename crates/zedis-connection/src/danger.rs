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

use super::config::RedisServer;
use crate::read_only::is_read_only_command;

/// Categorical risk classification for a Redis command. Each variant decides
/// the wording of the confirm dialog and how strict the confirmation is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DangerKind {
    FlushAll,
    FlushDb,
    ConfigSet,
    ConfigResetStat,
    ConfigRewrite,
    Debug,
    Shutdown,
    ScriptFlush,
    /// `FUNCTION FLUSH` / `FUNCTION DELETE`: Redis Functions libraries gone,
    /// and every `FCALL` that used them failing until they are loaded again.
    FunctionDelete,
    /// `SWAPDB`: two whole databases exchanged under every client of either.
    SwapDb,
    ClusterReset,
    /// `REPLICAOF` / `SLAVEOF` / `FAILOVER`: `REPLICAOF host port` throws this
    /// node's dataset away for a full sync, `FAILOVER` pauses writes.
    Replication,
    /// `KEYS *` against a non-trivial pattern. Cheap to mistype, expensive to run.
    KeysGlob,
    /// Multi-key delete (`DEL k1 k2 ...`) above an arbitrary threshold.
    BatchDelete {
        count: usize,
    },
    /// Generic catch-all for write commands when the server has
    /// `require_confirm_writes = true`.
    GenericWrite,
    /// A write to an entry whose writes are locked (`RedisServer::write_locked`)
    /// with no unlock window open: what the unlock dialog asks, and what the
    /// bridge answers a script with.
    WriteLocked,
    /// `EVAL` / `EVALSHA` / `FCALL` to a guarded entry, asked by the bridge
    /// alone ([`classify_guarded_script`]).
    Script,
}

/// How long one unlock of a locked entry's writes lasts. Long enough to fix
/// a thing, short enough that a tab left open re-locks before the day
/// moves on; the same number on the desktop, whose timer re-engages
/// `SafeMode`, and on the bridge, which keeps the window per account.
pub const WRITE_UNLOCK_SECS: u64 = 15 * 60;

impl DangerKind {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            DangerKind::FlushAll => "danger.flushall",
            DangerKind::FlushDb => "danger.flushdb",
            DangerKind::ConfigSet => "danger.config_set",
            DangerKind::ConfigResetStat => "danger.config_resetstat",
            DangerKind::ConfigRewrite => "danger.config_rewrite",
            DangerKind::Debug => "danger.debug",
            DangerKind::Shutdown => "danger.shutdown",
            DangerKind::ScriptFlush => "danger.script_flush",
            DangerKind::FunctionDelete => "danger.function_delete",
            DangerKind::SwapDb => "danger.swapdb",
            DangerKind::ClusterReset => "danger.cluster_reset",
            DangerKind::Replication => "danger.replication",
            DangerKind::KeysGlob => "danger.keys_glob",
            DangerKind::BatchDelete { .. } => "danger.batch_delete",
            DangerKind::GenericWrite => "danger.generic_write",
            DangerKind::WriteLocked => "danger.write_locked",
            DangerKind::Script => "danger.script",
        }
    }
    /// Severity affects whether a tagged "PROD" server requires typing the
    /// server name or just clicking confirm.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            DangerKind::FlushAll
                | DangerKind::FlushDb
                | DangerKind::Shutdown
                | DangerKind::ClusterReset
                | DangerKind::Replication
                | DangerKind::Debug
                | DangerKind::ScriptFlush
                | DangerKind::FunctionDelete
                | DangerKind::SwapDb
                | DangerKind::Script
        )
    }
}

/// What kind of confirmation dialog to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmStrictness {
    /// Single click confirm.
    Click,
    /// User must type the server name to confirm.
    TypeName,
}

const KEYS_GLOB_HARMLESS: &[&str] = &["", "*", "?"];

/// Words that we treat as "name" args inside `DEBUG ...` or `CLUSTER ...`
/// where the second arg is what makes them destructive vs. read-only.
fn is_destructive_debug(sub: &str) -> bool {
    matches!(
        sub.to_ascii_uppercase().as_str(),
        "SLEEP"
            | "SEGFAULT"
            | "PANIC"
            | "ASSERT"
            | "OOM"
            | "RESTART"
            | "CRASH-AND-RECOVER"
            | "POPULATE"
            | "SET-ACTIVE-EXPIRE"
            | "RELOAD"
            | "LOADAOF"
            | "JMAP"
            | "CHANGE-REPL-ID"
            | "OBJECT"
            | "QUICKLIST-PACKED-THRESHOLD"
    )
}

fn is_destructive_cluster(sub: &str) -> bool {
    matches!(
        sub.to_ascii_uppercase().as_str(),
        "RESET" | "FORGET" | "FAILOVER" | "FLUSHSLOTS" | "DELSLOTS" | "BUMPEPOCH" | "SET-CONFIG-EPOCH"
    )
}

/// Classifier for a single Redis command. Returns `None` for benign reads
/// or writes that do not warrant a special prompt.
///
/// `cmd_name` and `args` should already be split (e.g. via `shlex::split`).
/// The classifier is case-insensitive on the command name.
pub fn classify_dangerous(cmd_name: &str, args: &[String]) -> Option<DangerKind> {
    let upper = cmd_name.to_ascii_uppercase();
    match upper.as_str() {
        "FLUSHALL" => Some(DangerKind::FlushAll),
        "FLUSHDB" => Some(DangerKind::FlushDb),
        "SHUTDOWN" => Some(DangerKind::Shutdown),
        "REPLICAOF" | "SLAVEOF" | "FAILOVER" => Some(DangerKind::Replication),
        "CONFIG" => match args.first().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("SET") => Some(DangerKind::ConfigSet),
            Some("RESETSTAT") => Some(DangerKind::ConfigResetStat),
            Some("REWRITE") => Some(DangerKind::ConfigRewrite),
            _ => None,
        },
        "SCRIPT" => match args.first().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("FLUSH") => Some(DangerKind::ScriptFlush),
            _ => None,
        },
        "FUNCTION" => match args.first().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("FLUSH" | "DELETE") => Some(DangerKind::FunctionDelete),
            _ => None,
        },
        "SWAPDB" => Some(DangerKind::SwapDb),
        "CLUSTER" => match args.first().map(|s| s.as_str()) {
            Some(sub) if is_destructive_cluster(sub) => Some(DangerKind::ClusterReset),
            _ => None,
        },
        "DEBUG" => match args.first().map(|s| s.as_str()) {
            Some(sub) if is_destructive_debug(sub) => Some(DangerKind::Debug),
            _ => None,
        },
        "KEYS" => {
            let pat = args.first().map(|s| s.trim()).unwrap_or("");
            if KEYS_GLOB_HARMLESS.contains(&pat) {
                Some(DangerKind::KeysGlob)
            } else {
                None
            }
        }
        "DEL" | "UNLINK" => {
            // args are key list; warn when above a threshold. The threshold is
            // intentionally low — typing 50 keys by hand into the CLI is a
            // strong signal you meant it; pasting hundreds is the foot-gun.
            let count = args.len();
            if count >= 50 {
                Some(DangerKind::BatchDelete { count })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Classify based on a raw single-line command string. Returns `None` if
/// the line could not be split.
pub fn classify_dangerous_line(line: &str) -> Option<DangerKind> {
    let parts = shlex::split(line)?;
    let cmd = parts.first()?.clone();
    let rest: Vec<String> = parts.iter().skip(1).cloned().collect();
    classify_dangerous(&cmd, &rest)
}

/// The bridge's question for a script: `EVAL`, `EVALSHA` and `FCALL` (not
/// the `_RO` forms, which cannot write) sent to an entry whose writes are
/// guarded — write-locked ([`RedisServer::write_locked`]) or tagged
/// production — are confirmed every time, inside an unlock window too, and
/// on production by the name.
///
/// A script runs whatever it holds: `EVAL "return redis.call('FLUSHALL')" 0`
/// is a FLUSHALL that [`classify_dangerous`] cannot see, and the window,
/// which lets plain writes through, used to let it through unasked. The
/// desktop keeps its own rule (its terminal does not ask about scripts);
/// the browser asks before sending, since a refusal from the bridge carries
/// no dialog of its own.
pub fn classify_guarded_script(server: &RedisServer, cmd_name: &str) -> Option<DangerKind> {
    let script = ["EVAL", "EVALSHA", "FCALL"]
        .iter()
        .any(|name| cmd_name.eq_ignore_ascii_case(name));
    (script && (server.write_locked() || server.is_high_risk_tag())).then_some(DangerKind::Script)
}

/// Whether a command writes — what *Confirm Writes* (`require_confirm_writes`)
/// asks about and `--audit-writes` logs.
///
/// Anything the read-only allowlist ([`is_read_only_command`], generated
/// from Redis's own `READONLY` flag) does not name. This used to be a list
/// of writes of its own, and a denylist cannot keep up: it missed `EVAL`
/// (any script), `BITFIELD`, `GETDEL` / `GETEX`, the blocking pops,
/// `HSETEX`, `SWAPDB`, `MIGRATE` and every module's writes, none of which
/// were confirmed or logged. An unknown command counts as a write, which
/// costs one extra question at worst. The arguments matter for containers
/// (`CONFIG GET` reads, `CONFIG SET` does not).
pub fn is_write_command(cmd_name: &str, args: &[impl AsRef<str>]) -> bool {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    !is_read_only_command(cmd_name, &args)
}

/// Compose the final policy for a server: which commands need a confirm,
/// and how strict that confirm should be.
pub fn confirm_strictness(server: &RedisServer, kind: &DangerKind) -> ConfirmStrictness {
    // Unlocking production's writes is the one non-destructive act that asks
    // for the name: it opens the door to every destructive one for a while.
    if server.is_high_risk_tag() && (kind.is_destructive() || matches!(kind, DangerKind::WriteLocked)) {
        ConfirmStrictness::TypeName
    } else {
        ConfirmStrictness::Click
    }
}

/// True when this server requires a click confirm even on benign-looking writes.
pub fn requires_write_confirm(server: &RedisServer) -> bool {
    server.require_confirm_writes.unwrap_or(false)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_write_is_whatever_the_read_only_allowlist_does_not_name() {
        // Writes under names that do not say so, which the old list missed.
        for (name, args) in [
            ("EVAL", vec!["return 1", "0"]),
            ("GETDEL", vec!["k"]),
            ("BITFIELD", vec!["k", "SET", "u8", "0", "1"]),
            ("BLPOP", vec!["q", "0"]),
            ("HSETEX", vec!["h", "FIELDS", "1", "f", "v"]),
            ("SWAPDB", vec!["0", "1"]),
            ("TS.ADD", vec!["s", "*", "1"]),
            ("CONFIG", vec!["SET", "maxmemory", "1gb"]),
        ] {
            assert!(is_write_command(name, &args), "{name} {args:?}");
        }
        for (name, args) in [
            ("GET", vec!["k"]),
            ("EVAL_RO", vec!["return 1", "0"]),
            ("CONFIG", vec!["GET", "maxmemory"]),
            ("SCAN", vec!["0"]),
        ] {
            assert!(!is_write_command(name, &args), "{name} {args:?}");
        }
    }

    #[test]
    fn deleting_functions_swapping_databases_and_crashing_debug_ask_first() {
        let words = |line: &str| line.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let kind = |line: &str| {
            let parts = words(line);
            classify_dangerous(&parts[0], &parts[1..])
        };
        assert_eq!(kind("FUNCTION FLUSH"), Some(DangerKind::FunctionDelete));
        assert_eq!(kind("function delete mylib"), Some(DangerKind::FunctionDelete));
        assert_eq!(kind("FUNCTION LIST"), None);
        assert_eq!(kind("SWAPDB 0 1"), Some(DangerKind::SwapDb));
        for sub in ["ASSERT", "OOM", "RESTART", "CRASH-AND-RECOVER", "POPULATE 1000"] {
            assert_eq!(kind(&format!("DEBUG {sub}")), Some(DangerKind::Debug), "{sub}");
        }
        assert!(DangerKind::FunctionDelete.is_destructive());
        assert!(DangerKind::SwapDb.is_destructive());
    }

    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_script_to_a_guarded_entry_is_asked_about_and_nowhere_else() {
        let plain = RedisServer {
            name: "staging".to_string(),
            ..Default::default()
        };
        let locked = RedisServer {
            write_lock: Some(true),
            ..plain.clone()
        };
        let production = RedisServer {
            tag_color: Some("red".to_string()),
            ..plain.clone()
        };
        // Production with its lock switched off is still production.
        let opted_out = RedisServer {
            write_lock: Some(false),
            ..production.clone()
        };
        for server in [&locked, &production, &opted_out] {
            for name in ["EVAL", "evalsha", "FCALL"] {
                assert_eq!(
                    classify_guarded_script(server, name),
                    Some(DangerKind::Script),
                    "{name}"
                );
            }
            for name in ["EVAL_RO", "EVALSHA_RO", "FCALL_RO", "SET", "SCRIPT"] {
                assert_eq!(classify_guarded_script(server, name), None, "{name}");
            }
        }
        assert_eq!(classify_guarded_script(&plain, "EVAL"), None);
        assert!(DangerKind::Script.is_destructive());
        assert_eq!(
            confirm_strictness(&production, &DangerKind::Script),
            ConfirmStrictness::TypeName
        );
        assert_eq!(
            confirm_strictness(&locked, &DangerKind::Script),
            ConfirmStrictness::Click
        );
    }

    #[test]
    fn replication_commands_are_destructive() {
        for (command, rest) in [
            ("REPLICAOF", &["NO", "ONE"][..]),
            ("slaveof", &["h", "1"]),
            ("FAILOVER", &[]),
        ] {
            let kind = classify_dangerous(command, &args(rest));
            assert_eq!(kind, Some(DangerKind::Replication), "{command}");
            assert!(kind.is_some_and(|k| k.is_destructive()), "{command}");
        }
    }

    #[test]
    fn flushall_classified() {
        assert_eq!(classify_dangerous("FLUSHALL", &[]), Some(DangerKind::FlushAll));
        assert_eq!(classify_dangerous("flushall", &[]), Some(DangerKind::FlushAll));
    }

    #[test]
    fn config_set_only_destructive() {
        assert_eq!(
            classify_dangerous("CONFIG", &args(&["SET", "maxmemory", "0"])),
            Some(DangerKind::ConfigSet)
        );
        assert_eq!(classify_dangerous("CONFIG", &args(&["GET", "*"])), None);
    }

    #[test]
    fn keys_glob_only_when_pattern_is_wide() {
        assert_eq!(classify_dangerous("KEYS", &args(&["*"])), Some(DangerKind::KeysGlob));
        assert_eq!(classify_dangerous("KEYS", &args(&["user:*"])), None);
    }

    #[test]
    fn batch_delete_threshold() {
        let many: Vec<String> = (0..60).map(|i| format!("k{i}")).collect();
        assert_eq!(
            classify_dangerous("DEL", &many),
            Some(DangerKind::BatchDelete { count: 60 })
        );
        let few: Vec<String> = (0..5).map(|i| format!("k{i}")).collect();
        assert_eq!(classify_dangerous("DEL", &few), None);
    }

    #[test]
    fn classify_line_uses_shlex() {
        assert_eq!(classify_dangerous_line("FLUSHALL"), Some(DangerKind::FlushAll));
        assert_eq!(
            classify_dangerous_line("CONFIG SET maxmemory 0"),
            Some(DangerKind::ConfigSet)
        );
        assert_eq!(classify_dangerous_line("GET foo"), None);
    }

    #[test]
    fn debug_subcommand_only_destructive() {
        assert_eq!(
            classify_dangerous("DEBUG", &args(&["SLEEP", "5"])),
            Some(DangerKind::Debug)
        );
        // OBJECT introspection in modern Redis is read-only but we keep it
        // flagged because older versions allow mutating side effects.
        assert_eq!(
            classify_dangerous("DEBUG", &args(&["OBJECT", "k"])),
            Some(DangerKind::Debug)
        );
        assert_eq!(classify_dangerous("DEBUG", &args(&["HELP"])), None);
    }
}

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
}

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
            DangerKind::ClusterReset => "danger.cluster_reset",
            DangerKind::Replication => "danger.replication",
            DangerKind::KeysGlob => "danger.keys_glob",
            DangerKind::BatchDelete { .. } => "danger.batch_delete",
            DangerKind::GenericWrite => "danger.generic_write",
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

/// True when a command should be treated as a write side-effect by the
/// `require_confirm_writes` toggle. Read-only commands (GET / HGET / etc.)
/// return false.
pub fn is_write_command(cmd_name: &str) -> bool {
    let upper = cmd_name.to_ascii_uppercase();
    // spellchecker:off
    matches!(
        upper.as_str(),
        "SET"
            | "SETEX"
            | "SETNX"
            | "MSET"
            | "MSETNX"
            | "APPEND"
            | "GETSET"
            | "INCR"
            | "DECR"
            | "INCRBY"
            | "INCRBYFLOAT"
            | "DECRBY"
            | "BITOP"
            | "SETBIT"
            | "SETRANGE"
            | "DEL"
            | "UNLINK"
            | "RENAME"
            | "RENAMENX"
            | "EXPIRE"
            | "EXPIREAT"
            | "PEXPIRE"
            | "PEXPIREAT"
            | "PERSIST"
            | "MOVE"
            | "RESTORE"
            | "COPY"
            | "HSET"
            | "HMSET"
            | "HDEL"
            | "HSETNX"
            | "HINCRBY"
            | "HINCRBYFLOAT"
            | "HEXPIRE"
            | "HPEXPIRE"
            | "HPERSIST"
            | "LPUSH"
            | "RPUSH"
            | "LPUSHX"
            | "RPUSHX"
            | "LPOP"
            | "RPOP"
            | "LSET"
            | "LREM"
            | "LTRIM"
            | "LINSERT"
            | "LMOVE"
            | "RPOPLPUSH"
            | "SADD"
            | "SREM"
            | "SPOP"
            | "SMOVE"
            | "SDIFFSTORE"
            | "SINTERSTORE"
            | "SUNIONSTORE"
            | "ZADD"
            | "ZINCRBY"
            | "ZREM"
            | "ZPOPMIN"
            | "ZPOPMAX"
            | "ZREMRANGEBYRANK"
            | "ZREMRANGEBYSCORE"
            | "ZREMRANGEBYLEX"
            | "ZRANGESTORE"
            | "ZUNIONSTORE"
            | "ZINTERSTORE"
            | "ZDIFFSTORE"
            | "XADD"
            | "XDEL"
            | "XTRIM"
            | "XSETID"
            | "XGROUP"
            | "XACK"
            | "XCLAIM"
            | "XAUTOCLAIM"
            | "PFADD"
            | "PFMERGE"
            | "PUBLISH"
            | "GEOADD"
            | "JSON.SET"
            | "JSON.MERGE"
            | "JSON.DEL"
            | "JSON.NUMINCRBY"
            | "JSON.NUMMULTBY"
            | "JSON.STRAPPEND"
            | "JSON.ARRAPPEND"
            | "JSON.ARRINSERT"
            | "JSON.ARRPOP"
            | "JSON.ARRTRIM"
            | "JSON.OBJDEL"
            | "JSON.TOGGLE"
            | "JSON.CLEAR"
    )
    // spellchecker:on
}

/// Compose the final policy for a server: which commands need a confirm,
/// and how strict that confirm should be.
pub fn confirm_strictness(server: &RedisServer, kind: &DangerKind) -> ConfirmStrictness {
    if server.is_high_risk_tag() && kind.is_destructive() {
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
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
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

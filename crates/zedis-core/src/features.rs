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

//! Server feature matrix — which Redis commands the connected server actually
//! offers *this* user.
//!
//! Proxies (Twemproxy / Codis / Envoy) answer `unknown command` for anything
//! outside their whitelist, managed clouds (ElastiCache / Azure Cache / Tair)
//! rename or ACL-deny `CONFIG` / `MONITOR` / `CLIENT`, and Redis-compatible
//! servers (Dragonfly / KeyDB / Kvrocks / Garnet) ship different subsets.
//! Rather than a hand-maintained table per brand, the app *probes* the
//! server once per connection (`zedis-connection::probe`) and keeps the
//! per-command outcome here, so every panel can say "CONFIG is unavailable on
//! this server" instead of surfacing a raw error.
//!
//! This module is the pure part: the command list, the status type, reply
//! classification and the brand sniffing. No I/O.

use std::collections::HashMap;

/// A Redis command (or subcommand) whose availability a panel depends on.
/// Each variant is one probe; the UI maps panels and buttons onto them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerCommand {
    Info,
    Scan,
    Dbsize,
    MemoryUsage,
    ObjectEncoding,
    Dump,
    Restore,
    Migrate,
    Unlink,
    ConfigGet,
    ConfigSet,
    SlowlogGet,
    LatencyLatest,
    ClientList,
    ClientKill,
    AclList,
    AclSetUser,
    FunctionList,
    FunctionLoad,
    ScriptExists,
    Eval,
    Monitor,
    Bgsave,
    Lastsave,
    PubsubChannels,
    Subscribe,
    Publish,
    ClusterInfo,
    ClusterSlotStats,
    FlushDb,
    HotkeysGet,
    HotkeysStart,
    /// `HSETEX` — a hash field and its TTL in one write (Redis 8.0).
    HSetEx,
    /// `REPLICAOF host port` / `REPLICAOF NO ONE` — the standalone
    /// replication link (managed clouds reject it).
    Replicaof,
    /// `FAILOVER` — the coordinated standalone primary switch (Redis 6.2).
    Failover,
}

impl ServerCommand {
    /// Every variant; the probe runs all of them.
    pub const ALL: &'static [ServerCommand] = &[
        ServerCommand::Info,
        ServerCommand::Scan,
        ServerCommand::Dbsize,
        ServerCommand::MemoryUsage,
        ServerCommand::ObjectEncoding,
        ServerCommand::Dump,
        ServerCommand::Restore,
        ServerCommand::Migrate,
        ServerCommand::Unlink,
        ServerCommand::ConfigGet,
        ServerCommand::ConfigSet,
        ServerCommand::SlowlogGet,
        ServerCommand::LatencyLatest,
        ServerCommand::ClientList,
        ServerCommand::ClientKill,
        ServerCommand::AclList,
        ServerCommand::AclSetUser,
        ServerCommand::FunctionList,
        ServerCommand::FunctionLoad,
        ServerCommand::ScriptExists,
        ServerCommand::Eval,
        ServerCommand::Monitor,
        ServerCommand::Bgsave,
        ServerCommand::Lastsave,
        ServerCommand::PubsubChannels,
        ServerCommand::Subscribe,
        ServerCommand::Publish,
        ServerCommand::ClusterInfo,
        ServerCommand::ClusterSlotStats,
        ServerCommand::FlushDb,
        ServerCommand::HotkeysGet,
        ServerCommand::HotkeysStart,
        ServerCommand::HSetEx,
        ServerCommand::Replicaof,
        ServerCommand::Failover,
    ];

    /// Top-level command word, as sent on the wire (`CONFIG`, `SCAN`, …).
    pub const fn word(self) -> &'static str {
        match self {
            ServerCommand::Info => "INFO",
            ServerCommand::Scan => "SCAN",
            ServerCommand::Dbsize => "DBSIZE",
            ServerCommand::MemoryUsage => "MEMORY",
            ServerCommand::ObjectEncoding => "OBJECT",
            ServerCommand::Dump => "DUMP",
            ServerCommand::Restore => "RESTORE",
            ServerCommand::Migrate => "MIGRATE",
            ServerCommand::Unlink => "UNLINK",
            ServerCommand::ConfigGet | ServerCommand::ConfigSet => "CONFIG",
            ServerCommand::SlowlogGet => "SLOWLOG",
            ServerCommand::LatencyLatest => "LATENCY",
            ServerCommand::ClientList | ServerCommand::ClientKill => "CLIENT",
            ServerCommand::AclList | ServerCommand::AclSetUser => "ACL",
            ServerCommand::FunctionList | ServerCommand::FunctionLoad => "FUNCTION",
            ServerCommand::ScriptExists => "SCRIPT",
            ServerCommand::Eval => "EVAL",
            ServerCommand::Monitor => "MONITOR",
            ServerCommand::Bgsave => "BGSAVE",
            ServerCommand::Lastsave => "LASTSAVE",
            ServerCommand::PubsubChannels => "PUBSUB",
            ServerCommand::Subscribe => "SUBSCRIBE",
            ServerCommand::Publish => "PUBLISH",
            ServerCommand::ClusterInfo | ServerCommand::ClusterSlotStats => "CLUSTER",
            ServerCommand::FlushDb => "FLUSHDB",
            ServerCommand::HotkeysGet | ServerCommand::HotkeysStart => "HOTKEYS",
            ServerCommand::HSetEx => "HSETEX",
            ServerCommand::Replicaof => "REPLICAOF",
            ServerCommand::Failover => "FAILOVER",
        }
    }

    /// Subcommand word, when the variant is a container subcommand.
    pub const fn subcommand(self) -> Option<&'static str> {
        match self {
            ServerCommand::MemoryUsage => Some("USAGE"),
            ServerCommand::ObjectEncoding => Some("ENCODING"),
            ServerCommand::ConfigGet => Some("GET"),
            ServerCommand::ConfigSet => Some("SET"),
            ServerCommand::SlowlogGet => Some("GET"),
            ServerCommand::LatencyLatest => Some("LATEST"),
            ServerCommand::ClientList => Some("LIST"),
            ServerCommand::ClientKill => Some("KILL"),
            ServerCommand::AclList => Some("LIST"),
            ServerCommand::AclSetUser => Some("SETUSER"),
            ServerCommand::FunctionList => Some("LIST"),
            ServerCommand::FunctionLoad => Some("LOAD"),
            ServerCommand::ScriptExists => Some("EXISTS"),
            ServerCommand::PubsubChannels => Some("CHANNELS"),
            ServerCommand::ClusterInfo => Some("INFO"),
            ServerCommand::ClusterSlotStats => Some("SLOT-STATS"),
            ServerCommand::HotkeysGet => Some("GET"),
            ServerCommand::HotkeysStart => Some("START"),
            _ => None,
        }
    }

    /// Human label as shown in the UI: `CONFIG GET`, `SCAN`, ….
    pub fn label(self) -> String {
        match self.subcommand() {
            Some(sub) => format!("{} {sub}", self.word()),
            None => self.word().to_string(),
        }
    }

    /// Whether the command mutates data or server state. Those are never
    /// *executed* by the probe — their availability comes from `COMMAND INFO`
    /// (existence) and `ACL DRYRUN` (permission) instead.
    pub const fn is_mutating(self) -> bool {
        matches!(
            self,
            ServerCommand::Restore
                | ServerCommand::Migrate
                | ServerCommand::Unlink
                | ServerCommand::ConfigSet
                | ServerCommand::ClientKill
                | ServerCommand::AclSetUser
                | ServerCommand::FunctionLoad
                | ServerCommand::Eval
                | ServerCommand::Monitor
                | ServerCommand::Bgsave
                | ServerCommand::Publish
                | ServerCommand::Subscribe
                | ServerCommand::FlushDb
                | ServerCommand::HotkeysStart
                | ServerCommand::HSetEx
                | ServerCommand::Replicaof
                | ServerCommand::Failover
        )
    }

    /// Variants whose top-level word is `word` (case-insensitive), optionally
    /// narrowed to one subcommand. `CONFIG` alone matches both `CONFIG GET`
    /// and `CONFIG SET` — a Redis 6 `NOPERM` names only the container.
    pub fn matching(word: &str, subcommand: Option<&str>) -> Vec<ServerCommand> {
        ServerCommand::ALL
            .iter()
            .copied()
            .filter(|c| c.word().eq_ignore_ascii_case(word))
            .filter(|c| match (subcommand, c.subcommand()) {
                (Some(wanted), Some(have)) => wanted.eq_ignore_ascii_case(have),
                (Some(_), None) => false,
                (None, _) => true,
            })
            .collect()
    }
}

/// Outcome of probing one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandStatus {
    /// Not probed yet, or the probe itself failed (timeout, dropped link).
    /// Treated as usable — the UI must not grey out a panel on a guess.
    #[default]
    Unknown,
    Available,
    /// The server answered `unknown command` — a proxy whitelist, a renamed
    /// command, or a server that never implemented it.
    Missing,
    /// The server has it but this ACL user may not run it (`NOPERM`).
    Denied,
}

impl CommandStatus {
    /// Usable from the UI's point of view: only a definite `Missing` /
    /// `Denied` takes an affordance away.
    pub const fn is_usable(self) -> bool {
        !matches!(self, CommandStatus::Missing | CommandStatus::Denied)
    }

    /// i18n key (in the `features` section) naming this status.
    pub const fn i18n_key(self) -> &'static str {
        match self {
            CommandStatus::Unknown => "status_unknown",
            CommandStatus::Available => "status_available",
            CommandStatus::Missing => "status_missing",
            CommandStatus::Denied => "status_denied",
        }
    }
}

/// What a Redis error reply says about the command that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyClass {
    /// `unknown command` / `unknown subcommand` / `unsupported command`.
    Missing,
    /// `NOPERM`.
    Denied,
    /// Any other error: the command exists and ran (wrong arity, bad
    /// argument, feature disabled, …).
    Other,
}

/// Classifies a Redis error reply from its code (`ERR`, `NOPERM`, …) and
/// message. Pure string logic so the connection layer and the UI agree.
pub fn classify_reply(code: Option<&str>, message: &str) -> ReplyClass {
    if code == Some("NOPERM") {
        return ReplyClass::Denied;
    }
    let lower = message.to_ascii_lowercase();
    // Redis: "unknown command", "unknown subcommand"; Envoy: "unsupported
    // command"; Kvrocks / some proxies: "not supported" / "is not supported".
    if lower.contains("unknown command")
        || lower.contains("unknown subcommand")
        || lower.contains("unsupported command")
        || lower.contains("not supported")
        || lower.contains("command not allowed")
    {
        return ReplyClass::Missing;
    }
    if lower.contains("no permissions") {
        return ReplyClass::Denied;
    }
    ReplyClass::Other
}

/// The command a Redis error reply names, if any. Redis quotes it:
/// `ERR unknown command 'CONFIG', with args beginning with: 'GET'` (7.x),
/// `NOPERM User u has no permissions to run the 'config|get' command` (7.x),
/// `NOPERM this user has no permissions to run the 'config' command or its
/// subcommand` (6.x). Returns every variant the quoted name covers.
pub fn commands_in_reply(message: &str) -> Vec<ServerCommand> {
    let mut quoted = message.split('\'');
    // Text before the first quote, the quoted token, the rest.
    let _before = quoted.next();
    let Some(token) = quoted.next() else {
        return Vec::new();
    };
    let token = token.trim();
    if token.is_empty() || token.contains(' ') {
        return Vec::new();
    }
    // Redis 7 spells subcommands as `container|sub`.
    let (word, sub) = match token.split_once('|') {
        Some((w, s)) => (w, Some(s)),
        None => (token, None),
    };
    if sub.is_none()
        // `ERR unknown command 'CONFIG', with args beginning with: 'GET'` —
        // the next quoted token is the first argument; when it names a
        // subcommand we know, narrow to it.
        && let Some(arg) = quoted.nth(1).map(str::trim)
        && !arg.is_empty()
    {
        let narrowed = ServerCommand::matching(word, Some(arg));
        if !narrowed.is_empty() {
            return narrowed;
        }
    }
    ServerCommand::matching(word, sub)
}

/// Redis-compatible server brands, for explanatory copy only — availability
/// always comes from probing, never from the brand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerFlavor {
    #[default]
    Redis,
    Valkey,
    Dragonfly,
    KeyDb,
    Kvrocks,
    Garnet,
}

impl ServerFlavor {
    /// Sniffs the brand from `INFO server` key/value pairs.
    pub fn from_info<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> ServerFlavor {
        let mut flavor = ServerFlavor::Redis;
        for (key, value) in fields {
            let key = key.trim();
            match key {
                "dragonfly_version" => return ServerFlavor::Dragonfly,
                "kvrocks_version" | "kvrocks_mode" => return ServerFlavor::Kvrocks,
                "garnet_version" => return ServerFlavor::Garnet,
                "keydb_version" | "keydb_mode" => return ServerFlavor::KeyDb,
                "valkey_version" => flavor = ServerFlavor::Valkey,
                // Dragonfly / Garnet advertise a Redis version for client
                // compatibility; the `*_version` keys above take precedence.
                "server_name" if value.eq_ignore_ascii_case("valkey") => flavor = ServerFlavor::Valkey,
                _ => {}
            }
        }
        flavor
    }

    pub const fn label(self) -> &'static str {
        match self {
            ServerFlavor::Redis => "Redis",
            ServerFlavor::Valkey => "Valkey",
            ServerFlavor::Dragonfly => "Dragonfly",
            ServerFlavor::KeyDb => "KeyDB",
            ServerFlavor::Kvrocks => "Kvrocks",
            ServerFlavor::Garnet => "Garnet",
        }
    }
}

/// Per-server probe results. `Default` is the un-probed state, in which
/// every command reads as usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerFeatures {
    /// False until the first probe completes (or fails outright).
    pub probed: bool,
    pub flavor: ServerFlavor,
    statuses: HashMap<ServerCommand, CommandStatus>,
}

impl ServerFeatures {
    /// An empty matrix flagged as probed — the probe's starting point.
    pub fn probed_empty() -> Self {
        Self {
            probed: true,
            ..Default::default()
        }
    }

    pub fn status(&self, command: ServerCommand) -> CommandStatus {
        self.statuses.get(&command).copied().unwrap_or_default()
    }

    pub fn set(&mut self, command: ServerCommand, status: CommandStatus) {
        self.statuses.insert(command, status);
    }

    /// Usable from the UI's point of view (`Unknown` counts as usable).
    pub fn is_usable(&self, command: ServerCommand) -> bool {
        self.status(command).is_usable()
    }

    /// First command in `commands` that is definitely unusable, with why.
    pub fn first_unusable(&self, commands: &[ServerCommand]) -> Option<(ServerCommand, CommandStatus)> {
        commands
            .iter()
            .map(|c| (*c, self.status(*c)))
            .find(|(_, status)| !status.is_usable())
    }

    /// Every command that is definitely unusable, in `ServerCommand::ALL`
    /// order.
    pub fn unusable(&self) -> Vec<(ServerCommand, CommandStatus)> {
        ServerCommand::ALL
            .iter()
            .map(|c| (*c, self.status(*c)))
            .filter(|(_, status)| !status.is_usable())
            .collect()
    }

    /// Whether `message` is an unsupported/denied reply for commands this
    /// matrix already marks unusable — nothing new: the UI has degraded for
    /// it already and needn't toast again.
    pub fn already_explains(&self, code: Option<&str>, message: &str) -> bool {
        if classify_reply(code, message) == ReplyClass::Other {
            return false;
        }
        let named = commands_in_reply(message);
        !named.is_empty() && named.iter().all(|c| !self.is_usable(*c))
    }

    /// Records what a runtime error reply revealed: a `NOPERM` or `unknown
    /// command` for a command the probe thought usable flips it, so the UI
    /// degrades on first contact instead of toasting forever. Returns the
    /// commands that changed.
    pub fn note_reply_error(&mut self, code: Option<&str>, message: &str) -> Vec<(ServerCommand, CommandStatus)> {
        let status = match classify_reply(code, message) {
            ReplyClass::Missing => CommandStatus::Missing,
            ReplyClass::Denied => CommandStatus::Denied,
            ReplyClass::Other => return Vec::new(),
        };
        let mut changed = Vec::new();
        for command in commands_in_reply(message) {
            if self.status(command) != status {
                self.set(command, status);
                changed.push((command, status));
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_label_and_word() {
        for &c in ServerCommand::ALL {
            assert!(!c.word().is_empty(), "{c:?}");
            assert!(c.label().starts_with(c.word()), "{c:?}");
        }
        assert_eq!(ServerCommand::ConfigGet.label(), "CONFIG GET");
        assert_eq!(ServerCommand::Scan.label(), "SCAN");
    }

    #[test]
    fn matching_narrows_by_subcommand_but_container_alone_matches_all() {
        assert_eq!(
            ServerCommand::matching("config", Some("get")),
            vec![ServerCommand::ConfigGet]
        );
        assert_eq!(
            ServerCommand::matching("CONFIG", None),
            vec![ServerCommand::ConfigGet, ServerCommand::ConfigSet]
        );
        assert!(ServerCommand::matching("config", Some("rewrite")).is_empty());
        assert_eq!(ServerCommand::matching("scan", Some("x")), vec![]);
        assert_eq!(ServerCommand::matching("scan", None), vec![ServerCommand::Scan]);
    }

    #[test]
    fn classifies_replies_from_real_servers() {
        assert_eq!(
            classify_reply(Some("ERR"), "unknown command 'CONFIG', with args beginning with: 'GET'"),
            ReplyClass::Missing
        );
        assert_eq!(
            classify_reply(Some("ERR"), "unknown subcommand 'FOO'. Try CONFIG HELP."),
            ReplyClass::Missing
        );
        // Envoy redis proxy.
        assert_eq!(
            classify_reply(Some("ERR"), "unsupported command 'SCAN'"),
            ReplyClass::Missing
        );
        assert_eq!(
            classify_reply(
                Some("NOPERM"),
                "User limited has no permissions to run the 'config|get' command"
            ),
            ReplyClass::Denied
        );
        // Arity / semantic errors prove the command exists.
        assert_eq!(
            classify_reply(Some("ERR"), "wrong number of arguments for 'memory|usage' command"),
            ReplyClass::Other
        );
        assert_eq!(classify_reply(None, "connection reset"), ReplyClass::Other);
    }

    #[test]
    fn extracts_the_command_a_reply_names() {
        assert_eq!(
            commands_in_reply("unknown command 'CONFIG', with args beginning with: 'GET' "),
            vec![ServerCommand::ConfigGet]
        );
        assert_eq!(
            commands_in_reply("User u has no permissions to run the 'config|set' command"),
            vec![ServerCommand::ConfigSet]
        );
        // Redis 6 names only the container: both subcommands are affected.
        assert_eq!(
            commands_in_reply("this user has no permissions to run the 'config' command or its subcommand"),
            vec![ServerCommand::ConfigGet, ServerCommand::ConfigSet]
        );
        assert_eq!(
            commands_in_reply("unknown command 'scan', with args beginning with: '0' "),
            vec![ServerCommand::Scan]
        );
        assert!(commands_in_reply("unknown command 'FOOBAR'").is_empty());
        assert!(commands_in_reply("no quotes here").is_empty());
    }

    #[test]
    fn flavor_is_sniffed_from_info_server_keys() {
        assert_eq!(
            ServerFlavor::from_info([("redis_version", "7.2.4"), ("dragonfly_version", "df-v1.2")]),
            ServerFlavor::Dragonfly
        );
        assert_eq!(
            ServerFlavor::from_info([("redis_version", "7.2.4"), ("valkey_version", "8.0.1")]),
            ServerFlavor::Valkey
        );
        assert_eq!(
            ServerFlavor::from_info([("kvrocks_version", "2.8")]),
            ServerFlavor::Kvrocks
        );
        assert_eq!(
            ServerFlavor::from_info([("redis_version", "7.2.4")]),
            ServerFlavor::Redis
        );
    }

    #[test]
    fn unprobed_features_are_optimistic_and_runtime_errors_degrade_them() {
        let mut features = ServerFeatures::default();
        assert!(!features.probed);
        assert!(features.is_usable(ServerCommand::ConfigGet));
        assert!(features.unusable().is_empty());
        assert_eq!(
            features.first_unusable(&[ServerCommand::Scan, ServerCommand::ConfigGet]),
            None
        );

        let changed = features.note_reply_error(
            Some("NOPERM"),
            "User u has no permissions to run the 'config|get' command",
        );
        assert_eq!(changed, vec![(ServerCommand::ConfigGet, CommandStatus::Denied)]);
        assert!(!features.is_usable(ServerCommand::ConfigGet));
        assert!(features.is_usable(ServerCommand::ConfigSet));
        assert_eq!(
            features.first_unusable(&[ServerCommand::Scan, ServerCommand::ConfigGet]),
            Some((ServerCommand::ConfigGet, CommandStatus::Denied))
        );
        // Same error again: nothing new.
        assert!(
            features
                .note_reply_error(
                    Some("NOPERM"),
                    "User u has no permissions to run the 'config|get' command"
                )
                .is_empty()
        );
        // Unrelated errors never touch the matrix.
        assert!(
            features
                .note_reply_error(Some("ERR"), "value is not an integer")
                .is_empty()
        );
        // A repeat of a known denial is "already explained"; a new one isn't.
        assert!(features.already_explains(
            Some("NOPERM"),
            "User u has no permissions to run the 'config|get' command"
        ));
        assert!(!features.already_explains(
            Some("NOPERM"),
            "User u has no permissions to run the 'client|kill' command"
        ));
        assert!(!features.already_explains(Some("ERR"), "value is not an integer"));
    }
}

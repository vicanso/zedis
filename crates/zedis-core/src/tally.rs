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

//! What a diagnostic sample saw, counted and grouped: a slow log by
//! command, a `MONITOR` sample by client, command and key prefix, a memory
//! sample by key prefix. The desktop panels and the bridge's MCP tools both
//! count with these, so the two say the same thing about the same data.

use std::collections::{BTreeMap, HashMap};

/// The most distinct names a [`Tally`] keeps. A sample of random keys or
/// short-lived client ports would otherwise grow without bound; what comes
/// after the cap is still counted, as `other`.
pub const MAX_DISTINCT: usize = 10_000;

/// One command's part of a log: how often it appears, its amount in all and
/// at most — microseconds in a slow log, bytes in a size log — and its share
/// of the log's whole amount, in percent.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandTotal {
    pub command: String,
    pub count: usize,
    pub total: u64,
    pub max: u64,
    pub share_pct: f64,
}

/// Group `(command, amount)` entries by command, the heaviest first (by
/// total, then by name). "Which kind of command is eating the slow log"
/// rather than the entries one by one.
pub fn totals_by_command<'a>(entries: impl IntoIterator<Item = (&'a str, u64)>) -> Vec<CommandTotal> {
    let mut by_command: HashMap<&str, (usize, u64, u64)> = HashMap::new();
    for (command, amount) in entries {
        let entry = by_command.entry(command).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(amount);
        entry.2 = entry.2.max(amount);
    }
    let grand_total: u64 = by_command.values().map(|(_, total, _)| *total).sum();
    let mut totals: Vec<CommandTotal> = by_command
        .into_iter()
        .map(|(command, (count, total, max))| CommandTotal {
            command: command.to_string(),
            count,
            total,
            max,
            share_pct: if grand_total == 0 {
                0.0
            } else {
                total as f64 * 100.0 / grand_total as f64
            },
        })
        .collect();
    totals.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.command.cmp(&b.command)));
    totals
}

/// The group a key falls in: everything up to its last separator, then `*`
/// (`user:profile:42` → `user:profile:*`) — the memory analyzer's rule. A
/// key without the separator is a group of its own, and so is every key
/// when there is no separator at all.
pub fn key_prefix(key: &str, separator: &str) -> String {
    if separator.is_empty() {
        return key.to_string();
    }
    match key.rfind(separator) {
        Some(at) => format!("{}{separator}*", &key[..at]),
        None => key.to_string(),
    }
}

/// Whether a command's first argument names a key. Most do; these take
/// none, or something else first — a cursor, a pattern, a script, an
/// index, a channel, a subcommand — and a sample would otherwise count
/// `SCAN 0` under a key called `0`.
pub fn first_arg_is_key(command: &str) -> bool {
    let command = command.to_ascii_uppercase();
    if command.starts_with("FT.") {
        return false;
    }
    !matches!(
        command.as_str(),
        "ACL"
            | "ASKING"
            | "AUTH"
            | "BGREWRITEAOF"
            | "BGSAVE"
            | "BITOP"
            | "CLIENT"
            | "CLUSTER"
            | "COMMAND"
            | "COMMANDLOG"
            | "CONFIG"
            | "DBSIZE"
            | "DEBUG"
            | "DISCARD"
            | "ECHO"
            | "EVAL"
            | "EVALSHA"
            | "EVALSHA_RO"
            | "EVAL_RO"
            | "EXEC"
            | "FAILOVER"
            | "FCALL"
            | "FCALL_RO"
            | "FLUSHALL"
            | "FLUSHDB"
            | "FUNCTION"
            | "HELLO"
            | "HOTKEYS"
            | "INFO"
            | "KEYS"
            | "LASTSAVE"
            | "LATENCY"
            | "LOLWUT"
            | "MEMORY"
            | "MODULE"
            | "MONITOR"
            | "MULTI"
            | "OBJECT"
            | "PING"
            | "PSUBSCRIBE"
            | "PUBLISH"
            | "PUBSUB"
            | "PUNSUBSCRIBE"
            | "QUIT"
            | "READONLY"
            | "READWRITE"
            | "REPLICAOF"
            | "RESET"
            | "ROLE"
            | "SAVE"
            | "SCAN"
            | "SCRIPT"
            | "SELECT"
            | "SHUTDOWN"
            | "SLAVEOF"
            | "SLOWLOG"
            | "SPUBLISH"
            | "SSUBSCRIBE"
            | "SUBSCRIBE"
            | "SUNSUBSCRIBE"
            | "SWAPDB"
            | "TIME"
            | "TS.MGET"
            | "TS.MRANGE"
            | "TS.MREVRANGE"
            | "TS.QUERYINDEX"
            | "UNSUBSCRIBE"
            | "UNWATCH"
            | "WAIT"
            | "WAITAOF"
            | "XREAD"
            | "XREADGROUP"
    )
}

/// How often each name was seen, up to [`MAX_DISTINCT`] names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    counts: HashMap<String, u64>,
    other: u64,
    total: u64,
}

impl Tally {
    pub fn add(&mut self, name: &str) {
        self.total += 1;
        if let Some(count) = self.counts.get_mut(name) {
            *count += 1;
        } else if self.counts.len() < MAX_DISTINCT {
            self.counts.insert(name.to_string(), 1);
        } else {
            self.other += 1;
        }
    }

    /// Everything counted, the names past the cap included.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// What was counted under names past the cap.
    pub fn other(&self) -> u64 {
        self.other
    }

    /// The `n` most frequent names, the most frequent first (ties by name).
    pub fn top(&self, n: usize) -> Vec<(String, u64)> {
        let mut ranked: Vec<(&String, &u64)> = self.counts.iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        ranked
            .into_iter()
            .take(n)
            .map(|(name, count)| (name.clone(), *count))
            .collect()
    }
}

/// What a `MONITOR` sample saw: each command once, under the client that
/// sent it, the command's name, — where its first argument is a key —
/// that key's prefix, plus cross-tabulations (`command × prefix` and
/// `client × command`) to answer "who is hammering what".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonitorTally {
    pub clients: Tally,
    pub commands: Tally,
    pub prefixes: Tally,
    pub command_prefixes: Tally,
    pub client_commands: Tally,
}

impl MonitorTally {
    /// Count one command. `key` is its first argument, when it has one.
    pub fn add(&mut self, client: &str, command: &str, key: Option<&str>, separator: &str) {
        self.clients.add(client);
        let command = command.to_ascii_uppercase();
        self.client_commands.add(&format!("{client} {command}"));
        if let Some(key) = key.filter(|_| first_arg_is_key(&command)) {
            let prefix = key_prefix(key, separator);
            self.command_prefixes.add(&format!("{command} {prefix}"));
            self.prefixes.add(&prefix);
        }
        self.commands.add(&command);
    }

    /// Commands counted.
    pub fn total(&self) -> u64 {
        self.commands.total()
    }
}

/// One prefix of a memory sample: its keys, their bytes, and how many of
/// each type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrefixMemory {
    pub prefix: String,
    pub keys: u64,
    pub bytes: u64,
    pub types: BTreeMap<String, u64>,
}

/// Group `(key, type, bytes)` samples by key prefix, the heaviest first (by
/// bytes, then by prefix).
pub fn memory_by_prefix<'a>(
    samples: impl IntoIterator<Item = (&'a str, &'a str, u64)>,
    separator: &str,
) -> Vec<PrefixMemory> {
    let mut by_prefix: HashMap<String, PrefixMemory> = HashMap::new();
    for (key, key_type, bytes) in samples {
        let prefix = key_prefix(key, separator);
        let group = by_prefix.entry(prefix.clone()).or_insert_with(|| PrefixMemory {
            prefix,
            ..PrefixMemory::default()
        });
        group.keys += 1;
        group.bytes = group.bytes.saturating_add(bytes);
        *group.types.entry(key_type.to_string()).or_insert(0) += 1;
    }
    let mut groups: Vec<PrefixMemory> = by_prefix.into_values().collect();
    groups.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.prefix.cmp(&b.prefix)));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slow log grouped by command, heaviest first, shares adding up.
    #[test]
    fn commands_are_ranked_by_their_total() {
        let totals = totals_by_command([("GET", 2_000), ("HGETALL", 30_000), ("GET", 4_000), ("KEYS", 20_000)]);
        let order: Vec<&str> = totals.iter().map(|t| t.command.as_str()).collect();
        assert_eq!(order, ["HGETALL", "KEYS", "GET"]);
        assert_eq!((totals[2].count, totals[2].total, totals[2].max), (2, 6_000, 4_000));
        let shares: f64 = totals.iter().map(|t| t.share_pct).sum();
        assert!((shares - 100.0).abs() < 1e-6);
        assert!(totals_by_command([]).is_empty());
    }

    /// A key's group is everything before its last separator.
    #[test]
    fn a_key_is_grouped_by_what_comes_before_its_last_separator() {
        assert_eq!(key_prefix("user:profile:42", ":"), "user:profile:*");
        assert_eq!(key_prefix("session", ":"), "session");
        assert_eq!(key_prefix("a/b/c", "/"), "a/b/*");
        assert_eq!(key_prefix("user:42", ""), "user:42");
    }

    /// A sample counts the client and the command of every line, and the
    /// key's prefix only where the first argument is a key.
    #[test]
    fn a_monitor_sample_counts_clients_commands_and_key_prefixes() {
        let mut tally = MonitorTally::default();
        tally.add("10.0.0.1:5000", "get", Some("user:1"), ":");
        tally.add("10.0.0.1:5000", "GET", Some("user:2"), ":");
        tally.add("10.0.0.2:6000", "hgetall", Some("order:9"), ":");
        tally.add("10.0.0.2:6000", "SCAN", Some("0"), ":");
        tally.add("10.0.0.3:7000", "PING", None, ":");
        assert_eq!(tally.total(), 5);
        assert_eq!(
            tally.clients.top(2),
            [("10.0.0.1:5000".to_string(), 2), ("10.0.0.2:6000".to_string(), 2)]
        );
        assert_eq!(tally.commands.top(1), [("GET".to_string(), 2)]);
        assert_eq!(
            tally.prefixes.top(5),
            [("user:*".to_string(), 2), ("order:*".to_string(), 1)],
            "a cursor is not a key"
        );
        assert_eq!(
            tally.command_prefixes.top(5),
            [("GET user:*".to_string(), 2), ("HGETALL order:*".to_string(), 1)]
        );
        assert_eq!(
            tally.client_commands.top(2),
            [
                ("10.0.0.1:5000 GET".to_string(), 2),
                ("10.0.0.2:6000 HGETALL".to_string(), 1)
            ]
        );
    }

    /// Past the cap, new names are counted, as other.
    #[test]
    fn names_past_the_cap_are_counted_as_other() {
        let mut tally = Tally::default();
        for n in 0..MAX_DISTINCT + 5 {
            tally.add(&n.to_string());
        }
        tally.add("0");
        assert_eq!(tally.total(), (MAX_DISTINCT + 6) as u64);
        assert_eq!(tally.other(), 5);
        assert_eq!(tally.top(1), [("0".to_string(), 2)]);
    }

    /// A memory sample grouped by prefix, the heaviest first, types counted.
    #[test]
    fn a_memory_sample_is_grouped_by_prefix() {
        let groups = memory_by_prefix(
            [
                ("user:1", "hash", 300),
                ("user:2", "hash", 200),
                ("cache:a", "string", 900),
                ("user:3", "string", 50),
            ],
            ":",
        );
        assert_eq!(groups[0].prefix, "cache:*");
        assert_eq!(
            (groups[1].prefix.as_str(), groups[1].keys, groups[1].bytes),
            ("user:*", 3, 550)
        );
        assert_eq!(groups[1].types.get("hash"), Some(&2));
        assert_eq!(groups[1].types.get("string"), Some(&1));
    }
}

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

//! Every command this crate sends is classified, and the writes say so here.
//!
//! `read_only::is_read_only_command` is an allowlist, so it fails safe: a
//! read nobody listed is refused rather than waved through. Safe is not the
//! same as noticed, though — the symptom is a panel that quietly says it is
//! unavailable to every read-only account, and the person who sees it is not
//! the person who added the command. This test is what turns that into a
//! build failure the author gets.
//!
//! It scans for `cmd("…")` the way `tests/command_floors.rs` does, then asks
//! for each: is it allowed for a read-only caller? If not, it has to be named
//! in [`WRITES`] — so adding a command to this crate forces the one decision
//! that matters, in the one place a reviewer can see it.
//!
//! What it does **not** claim: that [`WRITES`] is exhaustive of Redis, or
//! that a command outside this crate is classified. It is scoped to what
//! Zedis itself sends, because that is what a read-only account can actually
//! ask the bridge for through the app. A hand-written command from the
//! terminal panel is judged by the same allowlist at the bridge, and an
//! unknown one is refused there whether or not it appears here.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use zedis_connection::is_read_only_command;

/// Commands this crate sends that change something, and so are refused for a
/// read-only account. Being on this list is not an accusation — it is the
/// record that somebody looked. A new entry belongs here only if running it
/// can alter the server or its data.
const WRITES: &[&str] = &[
    // Keyspace writes — the value editors and the key menu.
    "APPEND",
    "BITFIELD",
    "BITOP",
    "DEL",
    "EXPIRE",
    "EXPIREAT",
    "GEOADD",
    "GETEX",
    "HDEL",
    "HEXPIRE",
    "HINCRBY",
    "HSET",
    "HSETEX",
    "INCRBY",
    "INCRBYFLOAT",
    "LREM",
    "LSET",
    "LTRIM",
    "PERSIST",
    "PEXPIRE",
    "PFADD",
    "PFMERGE",
    "RENAME",
    "RENAMENX",
    "RESTORE",
    "RPUSH",
    "SADD",
    "SET",
    "SETBIT",
    "SREM",
    "VREM",
    "VSETATTR",
    "ZADD",
    "ZINCRBY",
    "ZREM",
    // Streams.
    "XACK",
    "XACKDEL",
    "XADD",
    "XAUTOCLAIM",
    "XCLAIM",
    "XDEL",
    "XGROUP CREATE",
    "XGROUP CREATECONSUMER",
    "XGROUP DELCONSUMER",
    "XGROUP DESTROY",
    "XGROUP SETID",
    "XNACK",
    "XSETID",
    "XTRIM",
    // Modules.
    "BF.ADD",
    "CF.ADD",
    "CMS.INCRBY",
    "FT.ALTER",
    "FT.CREATE",
    "FT.DROPINDEX",
    "JSON.MERGE",
    "JSON.SET",
    "TDIGEST.ADD",
    "TOPK.ADD",
    "TS.ADD",
    "TS.ALTER",
    "TS.CREATERULE",
    "TS.DELETERULE",
    // Scripting: may write whatever the script says.
    "FUNCTION",
    "FUNCTION DELETE",
    "SCRIPT",
    "SCRIPT LOAD",
    // The server itself. A container is named with its subcommand where its read half is allowed, so that listing the write half does not take the read half with it.
    "ACL DELUSER",
    "ACL LOAD",
    "ACL SAVE",
    "BGREWRITEAOF",
    "BGSAVE",
    "CLUSTER CANCELSLOTMIGRATIONS",
    "CLUSTER GETSLOTMIGRATIONS",
    "CLUSTER MIGRATION",
    "CLUSTER REPLICATE",
    "CLUSTER SETSLOT",
    "COMMANDLOG RESET",
    "CONFIG RESETSTAT",
    "CONFIG REWRITE",
    "CONFIG SET",
    "FAILOVER",
    "FAILOVER ABORT",
    "FLUSHALL",
    "FLUSHDB",
    "HOTKEYS RESET",
    "HOTKEYS STOP",
    "LATENCY",
    "MIGRATE",
    "REPLICAOF",
    "REPLICAOF NO",
    "SENTINEL FAILOVER",
    "SENTINEL SET",
    "SLOWLOG RESET",
    "SSUBSCRIBE",
];

/// Container commands whose subcommand the scanner could not read off the
/// source — the argument is built up over several statements, or comes from
/// a variable. The runtime check always sees the real subcommand and judges
/// the pair (`OBJECT ENCODING` is a read, `CONFIG SET` is not), so there is
/// nothing to decide here; naming them keeps the scanner's blind spot
/// visible instead of letting it look like an unclassified command.
const JUDGED_BY_SUBCOMMAND: &[&str] = &[
    "ACL", "CLIENT", "CLUSTER", "CONFIG", "HOTKEYS", "OBJECT", "PUBSUB", "SENTINEL",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The string literal starting at `from`, if that is where one starts.
fn literal_at(source: &str, from: usize) -> Option<(String, usize)> {
    let rest = source.get(from..)?;
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((rest[..end].to_string(), from + 1 + end + 1))
}

/// Every `cmd("NAME")` in `source`, with the first literal `.arg("…")` that
/// follows — enough to tell `CONFIG GET` from `CONFIG SET`. Kept in step
/// with `tests/command_floors.rs`, which scans the same crate the same way.
fn commands_sent(source: &str) -> BTreeSet<(String, Option<String>)> {
    let code = source.split("#[cfg(test)]").next().unwrap_or_default();
    let mut found = BTreeSet::new();
    let mut at = 0;
    while let Some(offset) = code[at..].find("cmd(") {
        let start = at + offset + "cmd(".len();
        at = start;
        let Some((name, after)) = literal_at(code, start) else {
            continue;
        };
        let tail = &code[after..];
        let stop = tail
            .find(';')
            .unwrap_or(tail.len())
            .min(tail.find(".query_async").unwrap_or(tail.len()))
            .min(tail.find(".exec_async").unwrap_or(tail.len()));
        let sub = tail[..stop]
            .find(".arg(")
            .and_then(|o| literal_at(tail, o + ".arg(".len()))
            .map(|(literal, _)| literal)
            .filter(|literal| literal.chars().all(|c| c.is_ascii_alphabetic() || c == '-'));
        found.insert((name.to_ascii_uppercase(), sub.map(|s| s.to_ascii_uppercase())));
    }
    found
}

#[test]
fn every_command_this_crate_sends_is_a_known_read_or_a_declared_write() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(!files.is_empty(), "no source files found under {}", root.display());

    let writes: BTreeSet<&str> = WRITES.iter().copied().collect();
    let mut unclassified = BTreeSet::new();
    let mut sent = BTreeSet::new();
    for file in &files {
        let Ok(source) = fs::read_to_string(file) else { continue };
        for (name, sub) in commands_sent(&source) {
            sent.insert(name.clone());
            let spelled = match &sub {
                Some(sub) => format!("{name} {sub}"),
                None => name.clone(),
            };
            let args: Vec<&str> = sub.iter().map(String::as_str).collect();
            if is_read_only_command(&name, &args) || writes.contains(name.as_str()) || writes.contains(spelled.as_str())
            {
                continue;
            }
            // A container with no subcommand in sight says nothing either way.
            if sub.is_none() && JUDGED_BY_SUBCOMMAND.contains(&name.as_str()) {
                continue;
            }
            unclassified.insert(spelled);
        }
    }
    assert!(
        unclassified.is_empty(),
        "these commands are neither a read this crate allows nor a declared write:\n  {}\n\n\
         If it only reads, add it to `read_only.rs` — a read-only account cannot run it today.\n\
         If it changes something, add its name to WRITES in this file.",
        unclassified.iter().cloned().collect::<Vec<_>>().join("\n  ")
    );

    // The other direction: a name that no longer appears stops being a
    // decision anyone has to carry.
    let stale: Vec<&str> = writes
        .iter()
        .copied()
        .filter(|entry| !sent.contains(entry.split(' ').next().unwrap_or(entry)))
        .collect();
    assert!(
        stale.is_empty(),
        "WRITES names commands this crate no longer sends; drop them: {stale:?}"
    );
}

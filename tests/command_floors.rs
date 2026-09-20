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

//! Every command newer than the oldest server we support has to be gated,
//! and this is the list that says so.
//!
//! Four compatibility defects landed in one week, all the same shape: a
//! command (or an option, or a reply field) that exists on the maintainer's
//! laptop and not on a server in the CI matrix. `HPERSIST` was the plainest
//! — sent on every hash-field write that asked for no TTL, and an unknown
//! command before Redis 7.4 / Valkey 9.0, so every such write reported a
//! failure that had actually succeeded. It shipped because nothing checks
//! what a command costs in server versions, and the author's Redis was the
//! newest one.
//!
//! So: scan what `zedis-connection` sends, look each command up in
//! `assets/commands.json` (which carries `since`), and fail on anything the
//! oldest tested server would not know — unless it is named below with the
//! gate that guards it. Adding a gated command means adding a line here,
//! which is also where a reviewer can see the gate without hunting.
//!
//! What it cannot see, and what the CI matrix is still for: *options* within
//! a command (`SET … IFEQ`, `TS.RANGE … ALIGN` — `commands.json` carries no
//! per-argument `since`), reply *fields* that appear in a later version
//! (`XINFO GROUPS`'s `lag`), and a command that exists but misbehaves
//! (`CLIENT NO-TOUCH` crashes Redis 8.0–8.2.6). Those need a server to find.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The oldest server `.github/workflows/integration.yml` runs — "the oldest
/// line still seen in the wild", as the matrix puts it. A command older than
/// this needs no gate because every server we test has it.
const OLDEST_SUPPORTED: (u32, u32, u32) = (6, 2, 0);

/// Commands newer than [`OLDEST_SUPPORTED`] that we send anyway, each with
/// what stops it reaching a server that lacks it. Three kinds of gate:
///
/// - a `floors::` constant checked through `supports(...)`;
/// - a `ServerCommand` probed at connect, which greys the panel or button;
/// - *best-effort*: sent once at connect, the error logged and ignored, the
///   feature simply absent. Only legitimate where nothing depends on it.
const GATED: &[(&str, &str)] = &[
    ("ACL DRYRUN", "floors::ACL_V2, and probe.rs asks before it dryruns"),
    ("CLIENT NO-EVICT", "best-effort: configure_client_connection logs and moves on"),
    (
        "CLIENT NO-TOUCH",
        "floors::no_touch_is_safe — a regression window, not a floor: it crashes Redis 8.0–8.2.6",
    ),
    ("CLIENT SETINFO", "best-effort: fills lib-name/lib-ver or does not"),
    ("FUNCTION", "floors::FUNCTIONS + ServerCommand::FunctionList"),
    ("FUNCTION DELETE", "floors::FUNCTIONS + ServerCommand::FunctionLoad"),
    ("FUNCTION DUMP", "floors::FUNCTIONS"),
    ("FUNCTION STATS", "floors::FUNCTIONS"),
    ("HEXPIRE", "floors::HASH_FIELD_TTL — the editor has no TTL column below it"),
    ("HTTL", "floors::HASH_FIELD_TTL"),
    ("HOTKEYS", "floors::HOTKEYS + ServerCommand::HotkeysStart"),
    ("HOTKEYS GET", "floors::HOTKEYS + ServerCommand::HotkeysGet"),
    ("HOTKEYS RESET", "floors::HOTKEYS"),
    ("HOTKEYS STOP", "floors::HOTKEYS"),
    ("HSETEX", "ServerCommand::HSetEx, probed — an unprobed server takes the HSET path"),
    ("SSUBSCRIBE", "floors::SHARDED_PUBSUB"),
    ("XACKDEL", "floors::STREAM_REF_POLICIES"),
    ("XNACK", "floors::STREAM_NACK"),
];

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|p| p.trim().parse::<u32>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

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
/// follows it — enough to tell `CLIENT NO-TOUCH` from `CLIENT SETNAME`.
/// `#[cfg(test)]` onwards is not scanned: a test's commands run against a
/// server the test itself chose.
fn commands_sent(source: &str) -> BTreeSet<(String, Option<String>)> {
    let code = source.split("#[cfg(test)]").next().unwrap_or_default();
    let mut found = BTreeSet::new();
    let mut at = 0;
    while let Some(offset) = code[at..].find("cmd(") {
        let start = at + offset + "cmd(".len();
        at = start;
        let Some((name, after)) = literal_at(code, start) else { continue };
        // The chain that follows, up to whatever ends the statement.
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
fn a_command_newer_than_the_oldest_tested_server_is_gated_and_says_so() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw = fs::read_to_string(root.join("assets/commands.json")).expect("read assets/commands.json");
    let meta: BTreeMap<String, serde_json::Value> = serde_json::from_str(&raw).expect("parse commands.json");

    let mut files = Vec::new();
    rust_files(&root.join("crates/zedis-connection/src"), &mut files);
    assert!(!files.is_empty(), "no source to scan — did the crate move?");

    let gated: BTreeMap<&str, &str> = GATED.iter().copied().collect();
    let mut ungated: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for file in &files {
        let source = fs::read_to_string(file).unwrap_or_default();
        for (name, sub) in commands_sent(&source) {
            // Prefer the subcommand's own entry: `CLIENT NO-TOUCH` is 7.2
            // while `CLIENT` itself is ancient.
            let full = sub.map(|s| format!("{name} {s}")).filter(|key| meta.contains_key(key));
            let key = full.unwrap_or(name);
            let Some(entry) = meta.get(&key) else { continue };
            let Some(since) = entry.get("since").and_then(|v| v.as_str()).and_then(parse_version) else {
                continue;
            };
            if since <= OLDEST_SUPPORTED || gated.contains_key(key.as_str()) {
                seen.insert(key);
                continue;
            }
            ungated.push(format!(
                "{key} (since {}.{}.{}) — sent in {}",
                since.0,
                since.1,
                since.2,
                file.strip_prefix(root).unwrap_or(file).display()
            ));
        }
    }
    ungated.sort();
    ungated.dedup();
    assert!(
        ungated.is_empty(),
        "a command newer than Redis {}.{}.{} is sent without a gate. Add a `floors::` \
         constant (with the Valkey side researched) or a probed `ServerCommand`, then name the \
         gate in GATED in this file:\n  {}",
        OLDEST_SUPPORTED.0,
        OLDEST_SUPPORTED.1,
        OLDEST_SUPPORTED.2,
        ungated.join("\n  ")
    );

    // An entry that no longer matches anything is a gate nobody needs: the
    // command stopped being sent, or was renamed. Keeping it would let the
    // next real one hide behind a stale line.
    let stale: Vec<&str> = GATED
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| !seen.contains(*key))
        .collect();
    assert!(
        stale.is_empty(),
        "GATED names commands zedis-connection no longer sends — drop them:\n  {}",
        stale.join("\n  ")
    );
}

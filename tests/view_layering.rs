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

//! A ratchet on how much the view layer knows about Redis — run directly
//! with `make check-layering` (also part of `make test`).
//!
//! The goal is a view layer that could be lifted out on its own: `src/views`
//! draws and asks, `zedis-connection` knows what a command looks like. Views
//! used to build commands (`cmd("BF.ADD")`), run them (`.query_async`), take
//! connections (`get_connection_manager()`, the `open_*_connection` dialers)
//! and name `redis::` types — 33 files of it on 2026-09-19 — and that could
//! not be undone in one change. So this test held the line while it was:
//! `tests/view_layering.baseline` records, per file, how many of each there
//! are, and the test fails when
//!
//! - a count **grows**, or a file that is not listed starts doing any of it —
//!   the new code belongs in `zedis-connection` as a typed operation (see
//!   `hyperloglog.rs`, `module_ops.rs`, `acl.rs`), reached through a
//!   [`ServerDb`](zedis_connection::ServerDb), not in the view;
//! - a count **shrinks** without the baseline following — so every cleanup
//!   tightens the ratchet in the same commit and cannot be undone quietly.
//!   Rewrite it with `ZEDIS_VIEW_LAYERING_WRITE=1 make check-layering`, which
//!   refuses to record an increase.
//!
//! The views reached an empty baseline on 2026-09-20, so for `src/views` this
//! reads as a plain rule: **no file there does any of this**. The scan was
//! then widened to all of `src`, because the state layer is the second half
//! of ADR 10: `src/states` still builds its commands inline, and they go the
//! same way, one file at a time. Done is an empty baseline for the whole
//! crate — the point at which its manifest can drop `redis`. Test modules
//! (`#[cfg(test)]` onwards) and comment lines are not counted.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const SOURCES: &str = "src";
const BASELINE: &str = "tests/view_layering.baseline";
const KINDS: [&str; 4] = ["cmd", "exec", "conn", "redis"];

/// Every way a view has of getting a connection into its hands: the pooled
/// manager, and the dialers for a dedicated one.
const CONNECTION_SOURCES: [&str; 8] = [
    "get_connection_manager()",
    "open_single_connection(",
    "open_seed_connection(",
    "open_monitor_connection(",
    "open_node_connection",
    "open_dedicated_connection(",
    "get_pubsub_connection(",
    "get_sharded_pubsub(",
];

/// Names that are Redis by another spelling: the connection type itself, and
/// the two traits a file imports in the browser build for the sole purpose of
/// running a command (`CLAUDE.md`, *The one tolerated repeat*). Counted with
/// the `redis::` paths.
const REDIS_BY_ANOTHER_NAME: [&str; 3] = ["RedisAsyncConn", "BridgeQuery", "BridgePipeline"];

/// `[cmd, exec, conn, redis]` for one file.
type Counts = [usize; 4];

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

/// `redis::` as a path, not the tail of another word (`rust_i18n_redis::`).
fn redis_paths(line: &str) -> usize {
    line.match_indices("redis::")
        .filter(|(at, _)| {
            line[..*at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
        .count()
}

fn count(source: &str) -> Counts {
    let code = source.split("#[cfg(test)]").next().unwrap_or_default();
    let mut counts = [0; 4];
    for line in code.lines().filter(|line| !line.trim_start().starts_with("//")) {
        counts[0] += line.matches("cmd(\"").count();
        counts[1] += line.matches(".query_async").count() + line.matches(".exec_async").count();
        counts[1] += line.matches("pipe()").count();
        counts[2] += CONNECTION_SOURCES
            .iter()
            .map(|source| line.matches(source).count())
            .sum::<usize>();
        counts[3] += redis_paths(line);
        counts[3] += REDIS_BY_ANOTHER_NAME
            .iter()
            .map(|name| line.matches(name).count())
            .sum::<usize>();
    }
    counts
}

fn measure(root: &Path) -> BTreeMap<String, Counts> {
    let mut files = Vec::new();
    rust_files(&root.join(SOURCES), &mut files);
    let mut measured = BTreeMap::new();
    for file in files {
        let counts = count(&fs::read_to_string(&file).unwrap_or_default());
        if counts.iter().any(|n| *n > 0) {
            let name = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            measured.insert(name, counts);
        }
    }
    measured
}

fn parse_baseline(text: &str) -> BTreeMap<String, Counts> {
    let mut baseline = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let mut parts = line.split_whitespace();
        let Some(file) = parts.next() else { continue };
        let mut counts = [0; 4];
        for part in parts {
            if let Some((kind, n)) = part.split_once('=')
                && let Some(slot) = KINDS.iter().position(|k| *k == kind)
            {
                counts[slot] = n.parse().unwrap_or(0);
            }
        }
        baseline.insert(file.to_string(), counts);
    }
    baseline
}

fn render(measured: &BTreeMap<String, Counts>) -> String {
    let mut totals = [0; 4];
    for counts in measured.values() {
        for (total, n) in totals.iter_mut().zip(counts) {
            *total += n;
        }
    }
    let mut out = String::from(
        "# What the GUI crate (src/) still knows about Redis — see tests/view_layering.rs.\n\
         # cmd = commands built, exec = commands run, conn = connections taken, redis = `redis::` paths.\n\
         # Only ever shrinks. Rewrite with: ZEDIS_VIEW_LAYERING_WRITE=1 make check-layering\n",
    );
    let _ = writeln!(
        out,
        "# {} files: cmd={} exec={} conn={} redis={}",
        measured.len(),
        totals[0],
        totals[1],
        totals[2],
        totals[3]
    );
    for (file, counts) in measured {
        let _ = write!(out, "{file}");
        for (kind, n) in KINDS.iter().zip(counts) {
            if *n > 0 {
                let _ = write!(out, " {kind}={n}");
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_view_layer_knows_no_more_about_redis_than_it_did() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let measured = measure(root);
    let baseline_path = root.join(BASELINE);
    let baseline = parse_baseline(&fs::read_to_string(&baseline_path).unwrap_or_default());

    let mut grew = Vec::new();
    let mut shrank = Vec::new();
    for (file, counts) in &measured {
        let allowed = baseline.get(file).copied().unwrap_or([0; 4]);
        for ((kind, now), was) in KINDS.iter().zip(counts).zip(allowed) {
            if *now > was {
                grew.push(format!("{file}: {kind} {was} -> {now}"));
            } else if *now < was {
                shrank.push(format!("{file}: {kind} {was} -> {now}"));
            }
        }
    }
    for file in baseline.keys().filter(|file| !measured.contains_key(*file)) {
        shrank.push(format!("{file}: clean now"));
    }

    let write = std::env::var_os("ZEDIS_VIEW_LAYERING_WRITE").is_some();
    if write && !baseline_path.exists() {
        // The first recording: there is nothing yet for it to have grown from.
        fs::write(&baseline_path, render(&measured)).expect("write the baseline");
        return;
    }
    assert!(
        grew.is_empty(),
        "the GUI crate took on more Redis — move it into zedis-connection as a typed operation \
         (reached through a `ServerDb`) instead; under src/views that is a rule, not a count:\n  {}",
        grew.join("\n  ")
    );
    if write {
        fs::write(&baseline_path, render(&measured)).expect("write the baseline");
        return;
    }
    assert!(
        shrank.is_empty(),
        "good — the GUI crate got cleaner; record it so it stays that way \
         (ZEDIS_VIEW_LAYERING_WRITE=1 make check-layering):\n  {}",
        shrank.join("\n  ")
    );
}

#[test]
fn the_counting_rules_count_what_they_say() {
    let source = "use redis::{Value, cmd};\n\
                  // cmd(\"IGNORED\") in a comment, redis::Value too\n\
                  let n: u64 = cmd(\"PFCOUNT\").arg(k).query_async(&mut conn).await?;\n\
                  let c = get_connection_manager().get_connection(id, db).await?;\n\
                  rust_i18n_redis::t!(); pipe().exec_async(&mut c);\n\
                  use crate::connection::{BridgeQuery as _, RedisAsyncConn};\n\
                  #[cfg(test)]\n\
                  mod tests { fn f() { cmd(\"TEST\"); redis::cmd(\"X\"); } }\n";
    assert_eq!(count(source), [1, 3, 1, 3]);
    assert_eq!(count("fn render() {}"), [0; 4]);
}

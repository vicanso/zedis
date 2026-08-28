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

//! Locale hygiene beyond the compile-time gate in `build.rs` — run
//! directly with `make check-locales` (also part of `make test`).
//!
//! - **Key parity**: every `locales/<lang>.toml` must carry exactly the
//!   key set of `locales/en.toml`. `build.rs` enforces this too, but its
//!   `rerun-if-changed=locales/` watches the directory, and editing a
//!   file in place does not always bump the directory mtime — hence the
//!   old `touch locales/en.toml` workaround. This test always reads the
//!   current files.
//! - **Orphan keys**: every en key must be reachable from source. A key
//!   counts as used when it appears quoted as the full dotted key
//!   (`t!("section.key")`), quoted as its last segment (the
//!   `i18n_<section>(cx, "key")` helpers), or composed from a quoted
//!   base plus a known dynamic suffix (`{base}_title` / `{base}_body`
//!   in the danger confirm dialog, `{base}_desc` in the settings page).
//!   The match is deliberately loose — a key is only flagged when
//!   nothing in the source could possibly produce it.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Suffixes the app appends to a quoted base key at runtime
/// (`views/danger_confirm.rs`, `views/setting_editor.rs`). A new
/// composed-key family belongs here, with its call site named.
const DYNAMIC_SUFFIXES: &[&str] = &["_title", "_body", "_desc"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Recursively collect all dotted key paths from a TOML table —
/// `[common]\nsubmit = "…"` → `common.submit` (mirrors `build.rs`).
fn collect_keys(table: &toml::Table, prefix: &str, keys: &mut BTreeSet<String>) {
    for (k, v) in table {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => collect_keys(t, &path, keys),
            _ => {
                keys.insert(path);
            }
        }
    }
}

/// `(stem, dotted key set)` for every `locales/*.toml`.
fn locale_key_sets() -> Vec<(String, BTreeSet<String>)> {
    let dir = repo_root().join("locales");
    let entries = fs::read_dir(&dir).expect("locales/ directory not found");
    let mut sets = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let table: toml::Table =
            toml::from_str(&src).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
        let mut keys = BTreeSet::new();
        collect_keys(&table, "", &mut keys);
        sets.push((stem.to_string(), keys));
    }
    sets.sort();
    sets
}

#[test]
fn locales_share_the_exact_key_set_of_en() {
    let sets = locale_key_sets();
    let en_keys = sets
        .iter()
        .find(|(stem, _)| stem == "en")
        .map(|(_, keys)| keys.clone())
        .expect("locales/en.toml must exist");
    assert!(sets.len() > 1, "expected more locales than just en");

    let mut failures = Vec::new();
    for (stem, keys) in &sets {
        if stem == "en" {
            continue;
        }
        for key in en_keys.difference(keys) {
            failures.push(format!("locales/{stem}.toml is missing `{key}`"));
        }
        for key in keys.difference(&en_keys) {
            failures.push(format!("locales/{stem}.toml has extra `{key}` (not in en.toml)"));
        }
    }
    assert!(
        failures.is_empty(),
        "locale key sets are out of sync with en.toml:\n  {}",
        failures.join("\n  ")
    );
}

/// Every `"…"` literal on one line. Line-based on purpose: the key names
/// we look for never span lines, and confining the scan to a line keeps a
/// stray quote (in a comment, or a raw string) from derailing more than
/// that line.
fn quoted_literals_of_line(line: &str, out: &mut HashSet<String>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        // The char literal `'"'` is not a string delimiter.
        if i > 0 && bytes[i - 1] == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
            i += 2;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        let mut escaped = false;
        while j < bytes.len() {
            let b = bytes[j];
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() {
            // Unterminated on this line — drop the remainder.
            return;
        }
        if let Some(s) = line.get(start..j) {
            out.insert(s.to_string());
        }
        i = j + 1;
    }
}

fn push_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            push_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// All string literals in the app and sub-crate sources. `crates/*/src`
/// is included because key names originate there too (`danger.rs`
/// `i18n_key()`, `zedis-core` `CommandStatus::i18n_key`, …).
fn source_literals() -> HashSet<String> {
    let root = repo_root();
    let mut files = Vec::new();
    push_rs_files(&root.join("src"), &mut files);
    if let Ok(entries) = fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            push_rs_files(&entry.path().join("src"), &mut files);
        }
    }
    assert!(files.len() > 50, "source walk looks broken: {} files", files.len());

    let mut literals = HashSet::new();
    for file in files {
        let src = fs::read_to_string(&file).unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        for line in src.lines() {
            quoted_literals_of_line(line, &mut literals);
        }
    }
    literals
}

#[test]
fn every_en_key_is_reachable_from_source() {
    let literals = source_literals();
    let sets = locale_key_sets();
    let en_keys = sets
        .iter()
        .find(|(stem, _)| stem == "en")
        .map(|(_, keys)| keys.clone())
        .expect("locales/en.toml must exist");

    let reachable = |full: &str| -> bool {
        if literals.contains(full) {
            return true;
        }
        let segment = full.rsplit('.').next().unwrap_or(full);
        if literals.contains(segment) {
            return true;
        }
        for suffix in DYNAMIC_SUFFIXES {
            if let Some(base) = full.strip_suffix(suffix) {
                if literals.contains(base) {
                    return true;
                }
                let base_segment = base.rsplit('.').next().unwrap_or(base);
                if literals.contains(base_segment) {
                    return true;
                }
            }
        }
        false
    };

    let orphans: Vec<&String> = en_keys.iter().filter(|key| !reachable(key)).collect();
    assert!(
        orphans.is_empty(),
        "{} locale key(s) are referenced nowhere in src/ or crates/*/src:\n  {}\n\
         Either use the key, delete it from all locale files, or — if it is\n\
         built dynamically in a new way — teach tests/locale_keys.rs the pattern.",
        orphans.len(),
        orphans.iter().map(|k| k.as_str()).collect::<Vec<_>>().join("\n  ")
    );
}

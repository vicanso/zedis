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

//! Criterion benches for the pure hot paths behind the "millions of keys
//! at 60 FPS" pitch: fuzzy matching (the ⌘K / ⌘P palettes re-score every
//! loaded key per keystroke), RDB parsing (the offline memory analyzer
//! walks whole dump files), JSONPath evaluation (re-run per render while
//! a path is active — parse included, matching the app's call shape) and
//! key segmentation (the key-tree build splits every loaded key once per
//! rebuild). Run with `make bench` and compare reports before/after
//! touching these paths; `make lint` (clippy `--all-targets`) keeps the
//! benches compiling.
//!
//! Key segmentation is the inner loop of the key-tree build. The build
//! itself (`new_key_tree_items`) stays in the binary crate because its
//! rows are gpui view models, but the per-key work it repeats — and the
//! only part whose cost grows with the keyspace — is `key_segments`,
//! which lives here and is benched directly.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use zedis_core::fuzzy::{fuzzy_score_prepared, prepare_fuzzy_query};
use zedis_core::jsonpath::run_jsonpath;
use zedis_core::key_segments::{folder_prefixes, split_key_segments};
use zedis_core::rdb::RdbParser;

/// Realistic key names in the shapes the tree and palettes see.
fn key_corpus() -> Vec<String> {
    (0..10_000)
        .map(|i| match i % 4 {
            0 => format!("user:{i}:profile"),
            1 => format!("session:{i:08x}:token"),
            2 => format!("cache:product:{i}:detail"),
            _ => format!("queue:orders:{i}"),
        })
        .collect()
}

/// One palette keystroke: prepare the query once, score the whole corpus.
fn bench_fuzzy(c: &mut Criterion) {
    let corpus = key_corpus();
    c.bench_function("fuzzy_scan_10k_keys", |b| {
        b.iter(|| {
            let query = prepare_fuzzy_query(black_box("usrprof"));
            let mut hits = 0u32;
            for key in &corpus {
                if fuzzy_score_prepared(&query, key).is_some() {
                    hits += 1;
                }
            }
            black_box(hits)
        })
    });
}

/// A minimal valid RDB: `REDIS0011` header, one SELECTDB, `entries`
/// string key/value pairs in the 6-bit length form, EOF with the
/// checksum trailer zeroed (0 = checksum disabled, as Redis writes when
/// `rdbchecksum no`). Mirrors the parser's own test fixture format.
fn rdb_fixture(entries: usize) -> Vec<u8> {
    let mut data = b"REDIS0011".to_vec();
    data.push(0xFE); // SELECTDB
    data.push(0);
    for i in 0..entries {
        data.push(0x00); // type: string
        let key = format!("bench:key:{i:06}");
        data.push(key.len() as u8);
        data.extend_from_slice(key.as_bytes());
        let value = format!("value-{i:06}-0123456789abcdef");
        data.push(value.len() as u8);
        data.extend_from_slice(value.as_bytes());
    }
    data.push(0xFF); // EOF
    data.extend_from_slice(&[0u8; 8]);
    data
}

fn bench_rdb(c: &mut Criterion) {
    let data = rdb_fixture(10_000);
    c.bench_function("rdb_parse_10k_strings", |b| {
        b.iter(|| {
            let mut parser = RdbParser::new(black_box(data.as_slice())).expect("valid RDB header");
            let mut count = 0u32;
            while parser.next_entry().expect("valid RDB entry").is_some() {
                count += 1;
            }
            black_box(count)
        })
    });
}

fn json_doc(items: usize) -> String {
    let items: Vec<String> = (0..items)
        .map(|i| format!(r#"{{"id":{i},"name":"item-{i}","tags":["a","b"],"price":{i}.5}}"#))
        .collect();
    format!(r#"{{"items":[{}],"total":{}}}"#, items.join(","), items.len())
}

fn bench_jsonpath(c: &mut Criterion) {
    let doc = json_doc(1_000);
    c.bench_function("jsonpath_project_1k_items", |b| {
        b.iter(|| black_box(run_jsonpath(black_box(&doc), "$.items[*].name")))
    });
}

/// Keys whose separators are *not* all level boundaries: a cluster hash tag,
/// an ISO timestamp and an embedded JSON blob each hold the scanner in a
/// suppressed run, and the unbalanced brace pays a second full pass (the
/// fallback rescan). Real keyspaces mix these in with the plain ones.
fn awkward_key_corpus() -> Vec<String> {
    (0..10_000)
        .map(|i| match i % 4 {
            0 => format!("user:{{tenant:{i}}}:profile"),
            1 => format!("evt:2022-05-11T11:55:44.487892+00:00:{i}"),
            2 => format!(r#"item:{i}/{{"name":"x","at":"12:13:05"}}"#),
            _ => format!("broken:{{{i}:unclosed"),
        })
        .collect()
}

/// One key-tree rebuild's segmentation pass. Every loaded key is split once
/// per rebuild — an expand/collapse, a filter change or a new SCAN page all
/// pay this — so it is the part of the build whose cost tracks the keyspace.
fn bench_key_segments(c: &mut Criterion) {
    let plain = key_corpus();
    c.bench_function("key_segments_split_10k", |b| {
        b.iter(|| {
            let mut segments = 0usize;
            for key in &plain {
                segments += split_key_segments(black_box(key), ":", 10).len();
            }
            black_box(segments)
        })
    });

    let awkward = awkward_key_corpus();
    c.bench_function("key_segments_split_10k_awkward", |b| {
        b.iter(|| {
            let mut segments = 0usize;
            for key in &awkward {
                segments += split_key_segments(black_box(key), ":", 10).len();
            }
            black_box(segments)
        })
    });

    // The allocating variant: the tag-aggregate pass walks a key's folder
    // prefixes, so this one is String-bound where the splits above are not.
    c.bench_function("key_segments_folder_prefixes_10k", |b| {
        b.iter(|| {
            let mut prefixes = 0usize;
            for key in &plain {
                prefixes += folder_prefixes(black_box(key), ":", 10).len();
            }
            black_box(prefixes)
        })
    });
}

criterion_group!(benches, bench_fuzzy, bench_rdb, bench_jsonpath, bench_key_segments);
criterion_main!(benches);

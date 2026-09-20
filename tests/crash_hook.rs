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

//! The installed panic hook writes a crash report for a panicking thread.
//!
//! **A test binary of its own, and it has to stay that way.** The hook is
//! process-global: once installed it fires for *every* panic in the process,
//! and a test's failed assertion is a panic. Living beside the other unit
//! tests, this one turned any unrelated failure in the same binary into a
//! second, mystifying failure here — and wrote a crash report per failed
//! assertion into the logs directory along the way. Cargo gives each
//! `tests/*.rs` its own process, so the hook here reaches nothing else.
//!
//! Keep this file to the one test for the same reason.

#![cfg(not(target_family = "wasm"))]

use std::fs;
use zedis_gui::helpers::{CrashContext, install_panic_hook, logs_dir};

/// What `helpers/crash.rs` writes into a report's header.
const CRASH_REPORT_PREFIX: &str = "crash-";

#[test]
fn the_installed_hook_writes_a_report_for_a_panicking_thread() {
    zedis_core::fs::override_config_dir(std::env::temp_dir().join(format!("zedis-crash-hook-{}", std::process::id())));
    install_panic_hook(CrashContext {
        version: "0.0.0",
        git_sha: "deadbeef",
        os: "TestOS-1".into(),
        arch: "arm64".into(),
    });

    // A marker unique to this run, so a leftover report from an earlier one
    // cannot pass the test.
    let marker = format!("crash-hook-probe-{}", std::process::id());
    let probe = marker.clone();
    let joined = std::thread::Builder::new()
        .name("crash-probe".into())
        .spawn(move || panic!("{probe}"))
        .expect("spawn")
        .join();
    assert!(joined.is_err(), "the probe thread must have panicked");

    let dir = logs_dir().expect("logs dir");
    let found = fs::read_dir(&dir)
        .expect("read logs dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(CRASH_REPORT_PREFIX))
        .any(|entry| {
            fs::read_to_string(entry.path())
                .is_ok_and(|text| text.contains(&marker) && text.contains("thread: crash-probe"))
        });
    assert!(found, "no crash report containing {marker} under {}", dir.display());
    let _ = fs::remove_dir_all(dir.parent().unwrap_or(&dir));
}

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

//! Crash reporting.
//!
//! Release builds abort on panic (`panic = "abort"` in `Cargo.toml`), so the
//! non-blocking file logger never gets to flush the one line that matters —
//! which is how "the window just disappeared" reports end up with nothing to
//! attach. The hook installed here writes a self-contained report
//! *synchronously* to `<logs>/crash-<unix-secs>.log` and leaves a
//! `crash.pending` marker beside it; the next launch turns the marker into a
//! dialog that points at the report ([`take_pending_crash`]).

use super::{logs_dir, unix_ts};
use chrono::Local;
use std::backtrace::Backtrace;
use std::fs;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::error;

/// File name prefix of every report; the logs pruner matches on it.
pub const CRASH_REPORT_PREFIX: &str = "crash-";
const PENDING_MARKER: &str = "crash.pending";

/// A crash report left behind by a previous run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    pub path: PathBuf,
    /// The panic message — the one line worth showing in the dialog.
    pub summary: String,
}

/// Build/platform facts written into every report. Captured once at startup
/// so the hook itself does nothing that could fail mid-panic.
#[derive(Debug, Clone)]
pub struct CrashContext {
    pub version: &'static str,
    pub git_sha: &'static str,
    pub os: String,
    pub arch: String,
}

/// Installs the process-wide panic hook. Chains to the previous hook so the
/// usual stderr message still appears when running from a terminal.
pub fn install_panic_hook(context: CrashContext) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(&context, info);
        previous(info);
    }));
}

fn record_panic(context: &CrashContext, info: &PanicHookInfo<'_>) {
    // A panic while writing the report must not re-enter the hook (a second
    // panic inside a hook aborts without any output at all).
    static IN_HOOK: AtomicBool = AtomicBool::new(false);
    if IN_HOOK.swap(true, Ordering::SeqCst) {
        return;
    }
    let message = panic_message(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let thread = std::thread::current().name().unwrap_or("<unnamed>").to_string();
    let backtrace = Backtrace::force_capture().to_string();
    error!(message = %message, location = %location, thread = %thread, "panic");
    let report = format_report(context, &message, &location, &thread, &backtrace);
    match logs_dir().map(|dir| write_report(&dir, &report)) {
        Some(Ok(path)) => error!(path = %path.display(), "crash report written"),
        Some(Err(e)) => error!(error = %e, "crash report could not be written"),
        None => error!("crash report could not be written: no logs directory"),
    }
    IN_HOOK.store(false, Ordering::SeqCst);
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn format_report(context: &CrashContext, message: &str, location: &str, thread: &str, backtrace: &str) -> String {
    format!(
        "Zedis crash report\n\
         time: {}\n\
         version: {} ({})\n\
         os: {}\n\
         arch: {}\n\
         thread: {}\n\
         location: {}\n\
         message: {}\n\
         \n\
         backtrace:\n{}\n",
        Local::now().to_rfc3339(),
        context.version,
        context.git_sha,
        context.os,
        context.arch,
        thread,
        location,
        message,
        backtrace,
    )
}

/// Writes `report` to `<dir>/crash-<unix-secs>.log` and points the pending
/// marker at it. Plain synchronous writes: this runs inside the panic hook.
fn write_report(dir: &Path, report: &str) -> std::io::Result<PathBuf> {
    let name = format!("{CRASH_REPORT_PREFIX}{}.log", unix_ts());
    let path = dir.join(&name);
    fs::write(&path, report)?;
    fs::write(dir.join(PENDING_MARKER), name)?;
    Ok(path)
}

/// The report the previous run left behind, if any. Consumes the marker, so
/// a crash is reported exactly once.
pub fn take_pending_crash() -> Option<CrashReport> {
    take_pending_crash_in(&logs_dir()?)
}

fn take_pending_crash_in(dir: &Path) -> Option<CrashReport> {
    let marker = dir.join(PENDING_MARKER);
    let name = fs::read_to_string(&marker).ok()?;
    let _ = fs::remove_file(&marker);
    // The marker holds a bare file name; anything else is not ours.
    let name = Path::new(name.trim()).file_name()?.to_os_string();
    let path = dir.join(name);
    let summary = fs::read_to_string(&path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("message: ").map(str::to_string))
        .unwrap_or_default();
    Some(CrashReport { path, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("zedis-crash-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn context() -> CrashContext {
        CrashContext {
            version: "0.0.0",
            git_sha: "deadbeef",
            os: "TestOS-1".into(),
            arch: "arm64".into(),
        }
    }

    #[test]
    fn report_round_trips_through_the_pending_marker_exactly_once() {
        let scratch = Scratch::new("roundtrip");
        let report = format_report(&context(), "index out of bounds", "src/x.rs:1:2", "main", "  0: frame");
        let written = write_report(&scratch.0, &report).expect("write report");
        assert!(
            written
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(CRASH_REPORT_PREFIX))
        );
        let text = fs::read_to_string(&written).expect("read report");
        assert!(text.contains("version: 0.0.0 (deadbeef)"));
        assert!(text.contains("location: src/x.rs:1:2"));
        assert!(text.contains("backtrace:\n  0: frame"));

        let pending = take_pending_crash_in(&scratch.0).expect("pending crash");
        assert_eq!(pending.path, written);
        assert_eq!(pending.summary, "index out of bounds");
        // Consumed: nothing to report on the launch after that.
        assert!(take_pending_crash_in(&scratch.0).is_none());
        assert!(written.exists(), "the report itself is kept for the user");
    }

    /// The real hook, end to end: a panic on a worker thread must leave a
    /// report under the (isolated) logs dir. Process-global by nature, so it
    /// only asserts on its own uniquely-named report.
    #[test]
    fn installed_hook_writes_a_report_for_a_panicking_thread() {
        zedis_core::fs::override_config_dir(
            std::env::temp_dir().join(format!("zedis-test-config-{}", std::process::id())),
        );
        install_panic_hook(context());
        let marker = format!("crash-hook-probe-{}", unix_ts());
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
            .filter(|e| e.file_name().to_string_lossy().starts_with(CRASH_REPORT_PREFIX))
            .any(|e| {
                fs::read_to_string(e.path()).is_ok_and(|t| t.contains(&marker) && t.contains("thread: crash-probe"))
            });
        assert!(found, "no crash report containing {marker} under {}", dir.display());
    }

    #[test]
    fn a_marker_whose_report_is_gone_is_dropped_silently() {
        let scratch = Scratch::new("dangling");
        fs::write(scratch.0.join(PENDING_MARKER), "crash-0.log").expect("write marker");
        assert!(take_pending_crash_in(&scratch.0).is_none());
        assert!(
            !scratch.0.join(PENDING_MARKER).exists(),
            "marker is consumed either way"
        );
    }

    #[test]
    fn a_marker_cannot_point_outside_the_logs_dir() {
        let scratch = Scratch::new("escape");
        fs::write(scratch.0.join(PENDING_MARKER), "../../etc/passwd").expect("write marker");
        // Resolves to `<dir>/passwd`, which doesn't exist → nothing reported.
        assert!(take_pending_crash_in(&scratch.0).is_none());
    }
}

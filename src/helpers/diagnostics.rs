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

//! The diagnostics bundle: one zip a user can attach to an issue instead of
//! hunting through the config directory — build/OS facts, the active
//! connection's state and capability matrix, the two config files with every
//! secret redacted, the most recent logs and every crash report.

use super::crash::CRASH_REPORT_PREFIX;
use super::logs_dir;
use super::zip::ZipWriter;
use chrono::Local;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use zedis_core::fs::{get_download_dir, get_or_create_config_dir, write_file_atomic};

/// Rolling log files included, newest first.
const LOG_FILES: usize = 3;
/// Per-file cap; logs are tail-truncated to keep the bundle attachable.
const LOG_BYTES: usize = 2 * 1024 * 1024;

/// What the caller assembles from live state; the file system parts
/// (logs, crash reports) are gathered here.
pub struct DiagnosticsInput {
    /// Free-text facts: version, OS, config dir, active connection, …
    pub summary: String,
    /// `zedis.toml` with secrets redacted.
    pub app_config: String,
    /// `redis-servers.toml` with secrets redacted.
    pub servers_config: String,
}

/// Writes `zedis-diagnostics-<stamp>.zip` to the Downloads folder (the config
/// dir when there is none — App Store sandbox) and returns its path.
pub fn export_diagnostics(input: &DiagnosticsInput) -> io::Result<PathBuf> {
    let dir = get_download_dir()
        .or_else(|| get_or_create_config_dir().ok())
        .ok_or_else(|| io::Error::other("no directory to write the bundle to"))?;
    let path = dir.join(format!(
        "zedis-diagnostics-{}.zip",
        Local::now().format("%Y%m%d-%H%M%S")
    ));
    let archive = build_archive(input, logs_dir().as_deref())?;
    write_file_atomic(&path, &archive)?;
    Ok(path)
}

fn build_archive(input: &DiagnosticsInput, logs: Option<&Path>) -> io::Result<Vec<u8>> {
    let mut zip = ZipWriter::new();
    zip.add("summary.txt", input.summary.as_bytes())?;
    zip.add("zedis.toml", input.app_config.as_bytes())?;
    zip.add("redis-servers.toml", input.servers_config.as_bytes())?;
    if let Some(logs) = logs {
        for (name, bytes) in collect_logs(logs) {
            zip.add(&format!("logs/{name}"), &bytes)?;
        }
    }
    Ok(zip.finish())
}

/// The newest rolling logs (tail-capped) plus every crash report.
fn collect_logs(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    // `zedis.log.YYYY-MM-DD` sorts chronologically by name.
    names.sort();
    let mut rolling: Vec<&String> = names.iter().filter(|n| n.starts_with("zedis.log")).collect();
    rolling.reverse();
    let crashes = names.iter().filter(|n| n.starts_with(CRASH_REPORT_PREFIX));
    rolling
        .into_iter()
        .take(LOG_FILES)
        .chain(crashes)
        .filter_map(|name| read_tail(&dir.join(name)).ok().map(|bytes| (name.clone(), bytes)))
        .collect()
}

/// The last [`LOG_BYTES`] of a file, read from there — a long-running
/// session's log is read for its tail, not loaded whole to keep the end.
fn read_tail(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let cap = LOG_BYTES as u64;
    if len <= cap {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    file.seek(SeekFrom::Start(len - cap))?;
    let mut out = b"[... truncated: only the last 2 MiB is included ...]\n".to_vec();
    file.take(cap).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_holds_summary_configs_latest_logs_and_crash_reports() {
        let dir = std::env::temp_dir().join(format!("zedis-diag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        for day in ["2026-08-18", "2026-08-19", "2026-08-20", "2026-08-21", "2026-08-22"] {
            fs::write(dir.join(format!("zedis.log.{day}")), format!("log {day}")).expect("log");
        }
        fs::write(dir.join("crash-1.log"), "message: boom").expect("crash");
        fs::write(dir.join("crash.pending"), "crash-1.log").expect("marker");
        fs::write(dir.join("unrelated.txt"), "x").expect("other");

        let input = DiagnosticsInput {
            summary: "version: 0.0.0".into(),
            app_config: "locale = \"en\"\n".into(),
            servers_config: "[[servers]]\nid = \"a\"\n".into(),
        };
        let archive = build_archive(&input, Some(&dir)).expect("archive");
        // Names are stored verbatim in the local headers; check the selection.
        let text = String::from_utf8_lossy(&archive);
        for expected in [
            "summary.txt",
            "zedis.toml",
            "redis-servers.toml",
            "logs/zedis.log.2026-08-22",
            "logs/zedis.log.2026-08-21",
            "logs/zedis.log.2026-08-20",
            "logs/crash-1.log",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        for excluded in [
            "zedis.log.2026-08-19",
            "zedis.log.2026-08-18",
            "crash.pending",
            "unrelated.txt",
        ] {
            assert!(!text.contains(excluded), "{excluded} must not be bundled");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_logs_keep_only_their_tail() {
        let dir = std::env::temp_dir().join(format!("zedis-diag-tail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        let mut big = vec![b'a'; LOG_BYTES + 10];
        big[..10].copy_from_slice(b"0123456789");
        fs::write(dir.join("big.log"), &big).expect("big");
        fs::write(dir.join("small.log"), b"small").expect("small");

        let out = read_tail(&dir.join("big.log")).expect("tail");
        assert!(out.starts_with(b"[... truncated"));
        assert_eq!(
            out.len(),
            LOG_BYTES + "[... truncated: only the last 2 MiB is included ...]\n".len()
        );
        assert!(
            !out.windows(10).any(|w| w == b"0123456789"),
            "the head is what is dropped"
        );
        assert_eq!(read_tail(&dir.join("small.log")).expect("small"), b"small".to_vec());
        let _ = fs::remove_dir_all(&dir);
    }
}

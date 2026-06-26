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

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt};

use super::{get_or_create_config_dir, is_development};

/// `<config_dir>/logs/`, created if missing. `None` if the config dir can't be
/// resolved or the directory can't be created. Shared by [`init_logger`] (where
/// the rolling file appender writes) and the "Open Logs Folder" action.
pub fn logs_dir() -> Option<PathBuf> {
    let dir = get_or_create_config_dir().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Delete rolling log files older than ~3 months so the logs directory doesn't
/// grow without bound. Best-effort: any error (unreadable dir, busy file) is
/// silently ignored — this runs at startup and must never block launch.
fn prune_old_logs(dir: &Path) {
    const MAX_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
    let Some(cutoff) = SystemTime::now().checked_sub(MAX_AGE) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Only touch our own rolling log files (zedis.log.YYYY-MM-DD).
        let is_log = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("zedis.log"));
        if !is_log {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified())
            && modified < cutoff
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Initialise logging to both stdout and a daily-rolling file under
/// `<config_dir>/logs/zedis.log.<date>`. The returned [`WorkerGuard`] flushes
/// the non-blocking file writer and MUST be kept alive for the whole run; the
/// file layer is best-effort (returns `None` if the logs dir can't be created),
/// in which case logging still goes to stdout.
pub fn init_logger() -> Result<Option<WorkerGuard>, Box<dyn std::error::Error>> {
    let mut level = Level::INFO;
    if let Ok(log_level) = std::env::var("RUST_LOG")
        && let Ok(value) = Level::from_str(log_level.as_str())
    {
        level = value;
    }
    // Detect the local offset once, up front (before the appender spawns its
    // worker thread), then reuse it for both layers.
    let timer = tracing_subscriber::fmt::time::OffsetTime::local_rfc_3339().unwrap_or_else(|_| {
        tracing_subscriber::fmt::time::OffsetTime::new(
            time::UtcOffset::from_hms(0, 0, 0).unwrap_or(time::UtcOffset::UTC),
            time::format_description::well_known::Rfc3339,
        )
    });

    let (file_layer, guard) = match logs_dir() {
        Some(logs_dir) => {
            prune_old_logs(&logs_dir);
            let appender = tracing_appender::rolling::daily(&logs_dir, "zedis.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_timer(timer.clone());
            (Some(layer), Some(guard))
        }
        None => {
            eprintln!("file logging disabled: could not resolve or create the logs directory");
            (None, None)
        }
    };

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(is_development())
        .with_timer(timer);

    let subscriber = tracing_subscriber::registry()
        .with(LevelFilter::from_level(level))
        .with(stdout_layer);
    match file_layer {
        Some(file_layer) => subscriber.with(file_layer).init(),
        None => subscriber.init(),
    }
    Ok(guard)
}

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

//! The Metrics panel's history on disk: one sample a minute per server, a
//! week of them. Desktop only. In the browser "disk" is the page's memory
//! (`mem_store`), so a stored sample would be gone with the next reload —
//! the 1h / 24h / 7d windows could only ever show "since this page opened",
//! which the Live window already does. The browser build therefore neither
//! writes samples nor offers those windows (`MetricsRange::offered`).

use super::*;
use crate::db::{insert_metrics_sample, prune_metrics_history};

/// Persist at most one sample per minute per server — the in-memory cache
/// keeps the 2s-resolution live window, disk only needs trend resolution.
const METRICS_PERSIST_INTERVAL_MS: i64 = 60_000;
/// Keep 7 days of samples (~10k rows per server at the 1/min cadence).
const METRICS_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Per-server timestamp of the last persisted sample (this process).
static METRICS_LAST_PERSISTED: LazyLock<RwLock<HashMap<String, i64>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Throttled write-behind of one metrics sample: skips unless a minute has
/// passed since the server's last persisted sample, serializes on the
/// caller, and hands the (blocking) redb write to the background executor.
/// The first persist of a session also prunes samples past retention.
/// Failures only warn — history is best-effort and must never break the
/// heartbeat.
pub(super) fn maybe_persist_metrics(server_id: &str, metrics: RedisMetrics, cx: &mut Context<ZedisServerState>) {
    let timestamp_ms = metrics.timestamp_ms;
    let first_this_session;
    {
        let mut last = METRICS_LAST_PERSISTED.write();
        let prev = last.get(server_id).copied();
        if let Some(prev) = prev
            && timestamp_ms - prev < METRICS_PERSIST_INTERVAL_MS
        {
            return;
        }
        first_this_session = prev.is_none();
        last.insert(server_id.to_string(), timestamp_ms);
    }
    let Ok(payload) = serde_json::to_vec(&metrics) else {
        return;
    };
    let server_id = server_id.to_string();
    cx.background_executor()
        .spawn(async move {
            if first_this_session {
                match prune_metrics_history(&server_id, timestamp_ms - METRICS_RETENTION_MS) {
                    Ok(removed) if removed > 0 => debug!(server_id, removed, "pruned metrics history"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "prune metrics history failed"),
                }
            }
            if let Err(e) = insert_metrics_sample(&server_id, timestamp_ms, &payload) {
                warn!(error = %e, "persist metrics sample failed");
            }
        })
        .detach();
}

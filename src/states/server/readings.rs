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

//! What the heartbeat and `DBSIZE` read from the server, with what paces
//! them: the last `INFO`, the key total, the slow-log sample, and the
//! `CLIENT PAUSE` this app sent.

use super::*;

const MAX_RECENT_ALERTS: usize = 50;

#[derive(Debug, Clone, Default)]
pub(super) struct Readings {
    redis_info: Option<RedisInfo>,

    /// Total number of keys in the database (from DBSIZE command)
    dbsize: Option<u64>,

    /// Last time `dbsize` was filled from `DBSIZE` — the heartbeat asks at
    /// most once a minute; a user-driven add/delete asks at once.
    last_dbsize_refreshed_at: i64,

    /// A user-driven `DBSIZE` is in flight (`refresh_dbsize_now`). A second
    /// add/delete while it runs sets `dbsize_refresh_again` so the total is
    /// asked again after this one lands, rather than applying a stale count.
    dbsize_refresh_inflight: bool,

    dbsize_refresh_again: bool,

    last_slow_logs_checked_at: i64,

    last_slow_log_count: usize,

    slow_logs: Vec<SlowLogEntry>,

    /// Bumped whenever `slow_logs` is replaced or cleared, so the panel can
    /// tell a new log from a heartbeat that brought none without building
    /// every row to compare the newest one.
    slow_logs_generation: u64,

    /// The `CLIENT PAUSE` this app sent (actions, until), for the status
    /// bar on a server that does not report pauses — Redis; Valkey 8.1+
    /// says so in `INFO clients` and is believed instead.
    client_pause: Option<(String, Instant)>,

    /// Stateful threshold alert evaluator for the current server.
    alert_evaluator: AlertEvaluator,

    /// Recently triggered alerts in this session (newest first), capped at
    /// [`MAX_RECENT_ALERTS`].
    recent_alerts: VecDeque<TriggeredAlert>,
}

impl Readings {
    /// Everything read from the previous server forgotten — the slow log as
    /// a new generation of it, not the first one again.
    pub(super) fn reset(&mut self) {
        self.redis_info = None;
        self.dbsize = None;
        self.client_pause = None;
        self.last_dbsize_refreshed_at = 0;
        self.dbsize_refresh_inflight = false;
        self.dbsize_refresh_again = false;
        self.last_slow_logs_checked_at = 0;
        self.last_slow_log_count = 0;
        self.slow_logs.clear();
        self.slow_logs_generation += 1;
        self.alert_evaluator.reset();
        self.recent_alerts.clear();
    }

    pub(super) fn redis_info(&self) -> Option<&RedisInfo> {
        self.redis_info.as_ref()
    }

    pub(super) fn set_redis_info(&mut self, info: RedisInfo) {
        self.redis_info = Some(info);
    }

    pub(super) fn dbsize(&self) -> Option<u64> {
        self.dbsize
    }

    /// A key total a connect or a topology reload brought; the heartbeat's
    /// own minute is left as it was.
    pub(super) fn set_dbsize(&mut self, dbsize: u64) {
        self.dbsize = Some(dbsize);
    }

    /// A key total asked for after a keyspace change: shown, and the
    /// heartbeat's minute starts again from it.
    pub(super) fn note_dbsize(&mut self, dbsize: u64) {
        self.dbsize = Some(dbsize);
        self.last_dbsize_refreshed_at = unix_ts();
    }

    /// The database was flushed: no keys, and the next beat asks again
    /// rather than wait out its minute.
    pub(super) fn flushed(&mut self) {
        self.dbsize = Some(0);
        self.last_dbsize_refreshed_at = 0;
    }

    /// When the key total was last asked for — the heartbeat's stamp.
    pub(super) fn dbsize_refreshed_at(&self) -> i64 {
        self.last_dbsize_refreshed_at
    }

    /// What a beat that asked for the key total found, taken unless a total
    /// asked for after a keyspace change has landed since the beat left
    /// (`stamp`). Attempted counts, whether or not it answered — a denied
    /// `DBSIZE` is asked again next minute, not next tick.
    pub(super) fn note_beat_dbsize(&mut self, stamp: i64, dbsize: Option<u64>) {
        if self.last_dbsize_refreshed_at != stamp {
            return;
        }
        self.last_dbsize_refreshed_at = unix_ts();
        if let Some(dbsize) = dbsize {
            self.dbsize = Some(dbsize);
        }
    }

    /// Start a user-driven `DBSIZE`, or remember that another one is owed
    /// when one is already in flight. Returns whether the caller should spawn.
    pub(super) fn request_dbsize_refresh(&mut self) -> bool {
        if self.dbsize_refresh_inflight {
            self.dbsize_refresh_again = true;
            return false;
        }
        self.dbsize_refresh_inflight = true;
        true
    }

    /// Clear the in-flight flag; answers whether another `DBSIZE` is owed.
    pub(super) fn finish_dbsize_refresh(&mut self) -> bool {
        self.dbsize_refresh_inflight = false;
        std::mem::take(&mut self.dbsize_refresh_again)
    }

    /// When the slow log was last sampled; 0 before the first sample.
    pub(super) fn slow_logs_checked_at(&self) -> i64 {
        self.last_slow_logs_checked_at
    }

    /// A slow-log sample: the entries, how many arrived since `since`, and
    /// a new generation of the log.
    pub(super) fn note_slow_logs(&mut self, slow_logs: Vec<SlowLogEntry>, since: i64) {
        self.last_slow_log_count = slow_logs.iter().filter(|item| item.timestamp >= since).count();
        self.slow_logs = slow_logs;
        self.slow_logs_generation += 1;
        self.last_slow_logs_checked_at = unix_ts();
    }

    pub(super) fn slow_logs(&self) -> &Vec<SlowLogEntry> {
        &self.slow_logs
    }

    pub(super) fn slow_logs_generation(&self) -> u64 {
        self.slow_logs_generation
    }

    pub(super) fn last_slow_log_count(&self) -> usize {
        self.last_slow_log_count
    }

    /// The log was reset on the server.
    pub(super) fn clear_slow_logs(&mut self) {
        self.slow_logs.clear();
        self.slow_logs_generation += 1;
        self.last_slow_log_count = 0;
    }

    /// The `CLIENT PAUSE` this app sent, until it runs out.
    pub(super) fn note_client_pause(&mut self, mode: PauseMode, timeout_ms: u64) {
        let until = Instant::now() + Duration::from_millis(timeout_ms);
        self.client_pause = Some((mode.as_str().to_ascii_lowercase(), until));
    }

    pub(super) fn clear_client_pause(&mut self) {
        self.client_pause = None;
    }

    /// Whether clients are paused right now, for how long and by whose
    /// word — the server's (Valkey 8.1+), else this app's own record.
    pub(super) fn client_pause(&self) -> Option<ClientPause> {
        client_pause_now(
            self.redis_info.as_ref().map(|info| &info.metrics),
            self.client_pause
                .as_ref()
                .map(|(actions, until)| (actions.as_str(), *until)),
            Instant::now(),
        )
    }

    pub(super) fn evaluate_alerts(&mut self, rules: &[AlertRule], sample: AlertSample) -> Vec<TriggeredAlert> {
        let fired = self.alert_evaluator.evaluate(rules, sample);
        for alert in &fired {
            if self.recent_alerts.len() >= MAX_RECENT_ALERTS {
                self.recent_alerts.pop_back();
            }
            self.recent_alerts.push_front(alert.clone());
        }
        fired
    }

    pub(super) fn active_alerts(&self) -> &[TriggeredAlert] {
        self.alert_evaluator.active_alerts()
    }

    pub(super) fn recent_alerts(&self) -> &VecDeque<TriggeredAlert> {
        &self.recent_alerts
    }

    pub(super) fn clear_recent_alerts(&mut self) {
        self.recent_alerts.clear();
    }

    pub(super) fn reset_alert_evaluator(&mut self) {
        self.alert_evaluator.reset();
    }
}

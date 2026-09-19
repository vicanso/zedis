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

//! How often the app polls a server on its own — the one place the desktop
//! and the browser differ in *pace*.
//!
//! On the desktop a beat is a command on a pooled socket, and 2s keeps the
//! status bar's chips live. In the browser the same beat is two or three
//! HTTP requests through the bridge, from every open page of every account,
//! against a bridge and a Redis that are shared — so the browser build polls
//! about five times slower. Only the *unprompted* polls are paced here: a
//! refresh that follows something the user did (a failover, a reshard, the
//! Refresh button) still goes out at once, because `refresh_redis_info` is
//! called directly there and the pace only decides how often the metronome
//! ticks.
//!
//! Nothing downstream assumes the desktop values. Chart rates divide by the
//! gap between two samples' timestamps, the offline threshold counts misses
//! rather than seconds, and the backoff doubles from whatever the heartbeat
//! is. Each pair is two constants under their own gate, desktop first: the
//! desktop values are not derived from the browser's or the other way round,
//! so changing one cannot move the other (CLAUDE.md, *Desktop first*).

use std::time::Duration;

/// The status bar's metronome: `PING` + `INFO` per master. Also the first
/// wait of the reconnect backoff, and the Metrics panel's redraw tick (it
/// only re-reads the cache this beat fills).
#[cfg(not(target_family = "wasm"))]
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(target_family = "wasm")]
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// A workspace tab that is not the active one still beats, this far apart
/// (seconds).
#[cfg(not(target_family = "wasm"))]
pub const BACKGROUND_REFRESH_SECS: i64 = 30;
#[cfg(target_family = "wasm")]
pub const BACKGROUND_REFRESH_SECS: i64 = 60;

/// The status bar's key total: one `DBSIZE` at most this often (seconds).
#[cfg(not(target_family = "wasm"))]
pub const DBSIZE_REFRESH_SECS: i64 = 60;
#[cfg(target_family = "wasm")]
pub const DBSIZE_REFRESH_SECS: i64 = 300;

/// The slow-log sample behind the status bar's badge (seconds).
#[cfg(not(target_family = "wasm"))]
pub const SLOW_LOG_CHECK_SECS: i64 = 60;
#[cfg(target_family = "wasm")]
pub const SLOW_LOG_CHECK_SECS: i64 = 300;

/// The Latency tab's `LATENCY LATEST` poll, while that tab is showing.
#[cfg(not(target_family = "wasm"))]
pub const LATENCY_POLL_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(target_family = "wasm")]
pub const LATENCY_POLL_INTERVAL: Duration = Duration::from_secs(15);

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop's pace is part of its behaviour, and this file is where a
    /// change meant for the browser would land by mistake — so the native
    /// values are pinned. (The test binary is native; the browser's values
    /// are not visible here, which is the point.)
    #[test]
    fn the_desktop_pace_is_unchanged() {
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(2));
        assert_eq!(BACKGROUND_REFRESH_SECS, 30);
        assert_eq!(DBSIZE_REFRESH_SECS, 60);
        assert_eq!(SLOW_LOG_CHECK_SECS, 60);
        assert_eq!(LATENCY_POLL_INTERVAL, Duration::from_secs(5));
    }
}

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

/// The Server Load panel's `INFO commandstats` sample, while it is open (a
/// payload that grows with the number of distinct commands). Seconds, because
/// the panel prints it ("sampled Ns ago · every Ns").
#[cfg(not(target_family = "wasm"))]
pub const SERVER_LOAD_POLL_SECS: u64 = 3;
#[cfg(target_family = "wasm")]
pub const SERVER_LOAD_POLL_SECS: u64 = 10;

/// The Hot Keys panel's report, while it is open (seconds).
#[cfg(not(target_family = "wasm"))]
pub const HOTKEYS_POLL_SECS: u64 = 2;
#[cfg(target_family = "wasm")]
pub const HOTKEYS_POLL_SECS: u64 = 10;

/// Those two panels sleep in slices so that their Refresh button is answered
/// within one: five wake-ups a second on the desktop, one in the browser,
/// where a page that never goes idle is a page the browser cannot rest.
#[cfg(not(target_family = "wasm"))]
pub const PANEL_WAKE_MS: u64 = 200;
#[cfg(target_family = "wasm")]
pub const PANEL_WAKE_MS: u64 = 1000;

/// The root's housekeeping tick: expired connection caches on every one, and
/// once an hour the idle key histories and the recycle bin. The browser has
/// no SSH sessions, no socket pool and no bin, so only the hourly part means
/// anything there — it ticks once an hour and sweeps on every tick.
#[cfg(not(target_family = "wasm"))]
pub const HOUSEKEEPING_TICK: Duration = Duration::from_secs(30);
#[cfg(target_family = "wasm")]
pub const HOUSEKEEPING_TICK: Duration = Duration::from_secs(3600);
/// How many housekeeping ticks make the hourly sweep.
#[cfg(not(target_family = "wasm"))]
pub const HOUSEKEEPING_HOURLY_TICKS: u64 = 120;
#[cfg(target_family = "wasm")]
pub const HOUSEKEEPING_HOURLY_TICKS: u64 = 1;

/// Whether the page is one nobody is looking at — a browser tab in the
/// background, a minimised window. A desktop window has no such state worth
/// acting on (its inactive *workspace tabs* are already relaxed), so there it
/// is a constant and everything that asks compiles to what it was. In the
/// browser a page is routinely left open for days, and every beat of it is
/// the bridge's and the Redis server's work: hidden, it polls like a
/// background workspace tab. The page tells us through `zedis-web`'s
/// `set_page_visible` (a `visibilitychange` listener in `index.html`).
#[cfg(not(target_family = "wasm"))]
pub const fn page_hidden() -> bool {
    false
}
#[cfg(target_family = "wasm")]
pub fn page_hidden() -> bool {
    PAGE_HIDDEN.load(std::sync::atomic::Ordering::Relaxed)
}
#[cfg(target_family = "wasm")]
static PAGE_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Browser only: the page's `visibilitychange`.
#[cfg(target_family = "wasm")]
pub fn set_page_hidden(hidden: bool) {
    PAGE_HIDDEN.store(hidden, std::sync::atomic::Ordering::Relaxed);
}

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
        assert_eq!(SERVER_LOAD_POLL_SECS, 3);
        assert_eq!(HOTKEYS_POLL_SECS, 2);
        assert_eq!(PANEL_WAKE_MS, 200);
        assert_eq!(HOUSEKEEPING_TICK, Duration::from_secs(30));
        assert_eq!(HOUSEKEEPING_HOURLY_TICKS, 120);
        assert!(!page_hidden(), "a desktop window is never \"hidden\" to the pacing");
    }
}

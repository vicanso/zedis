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

/// Whether nobody is looking: a browser page in a background tab, or a
/// desktop window that has not been the active one for [`WINDOW_IDLE_AFTER`].
/// An unattended app polls like a background workspace tab instead of at the
/// heartbeat — `ZedisServerState::is_background()` ORs this in, and the
/// panels that pause on `is_background()` pause with it.
///
/// Why it matters on the desktop too: a beat is commands sent to the server,
/// and on a Redis billed per command (Upstash and the like) a window left
/// open over a weekend is tens of thousands of them that nobody saw the
/// result of. The browser sets this from the page's `visibilitychange`
/// (`zedis-web`'s `set_page_visible`); the desktop from the main window's
/// activation (`root/desktop.rs`), after a grace period so that a glance at
/// another window changes nothing.
pub fn unattended() -> bool {
    UNATTENDED.load(std::sync::atomic::Ordering::Relaxed)
}
static UNATTENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_unattended(unattended: bool) {
    UNATTENDED.store(unattended, std::sync::atomic::Ordering::Relaxed);
}

/// How long the desktop's main window may be inactive before the app counts
/// as unattended. Long enough to read a document, answer a message or use
/// Zedis's own Settings window without the status bar going stale.
#[cfg(not(target_family = "wasm"))]
pub const WINDOW_IDLE_AFTER: Duration = Duration::from_secs(120);

/// The longest a cluster's heartbeat is stretched to, however many masters.
const HEARTBEAT_STRETCH_CAP: Duration = Duration::from_secs(30);

/// The heartbeat interval for a server with `masters` master nodes.
///
/// A beat sends one `INFO` to *every* master, so at a fixed interval a
/// cluster's heartbeat grows with its size — thirty masters at 2s is fifteen
/// `INFO`s a second, for a status bar. The interval is stretched in whole
/// beats so the rate stays at or under two `INFO`s a second: up to four
/// masters beat at the plain interval (a standalone, a Sentinel pair and the
/// common three-master cluster are untouched), 5–8 at twice it, 9–12 at three
/// times, capped at [`HEARTBEAT_STRETCH_CAP`]. In the browser the base is
/// 10s, so the same rule starts stretching at twenty-one masters.
pub fn heartbeat_interval(masters: usize) -> Duration {
    // Masters one beat may cover at two INFOs a second.
    let per_beat = (HEARTBEAT_INTERVAL.as_secs() * 2).max(1) as usize;
    let beats = masters.div_ceil(per_beat).max(1) as u32;
    HEARTBEAT_INTERVAL
        .saturating_mul(beats)
        .min(HEARTBEAT_STRETCH_CAP.max(HEARTBEAT_INTERVAL))
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
        assert_eq!(WINDOW_IDLE_AFTER, Duration::from_secs(120));
    }

    #[test]
    fn a_cluster_heartbeat_stretches_with_its_masters_and_nothing_smaller_does() {
        // Standalone, Sentinel and the usual small cluster: the plain beat.
        for masters in [0, 1, 2, 3, 4] {
            assert_eq!(heartbeat_interval(masters), Duration::from_secs(2), "{masters} masters");
        }
        assert_eq!(heartbeat_interval(5), Duration::from_secs(4));
        assert_eq!(heartbeat_interval(8), Duration::from_secs(4));
        assert_eq!(heartbeat_interval(9), Duration::from_secs(6));
        assert_eq!(heartbeat_interval(30), Duration::from_secs(16));
        // Capped: a status bar older than half a minute is not a status bar.
        assert_eq!(heartbeat_interval(60), Duration::from_secs(30));
        assert_eq!(heartbeat_interval(500), Duration::from_secs(30));
        // Never more than two INFOs a second below the cap.
        for masters in 1..=60usize {
            let rate = masters as f64 / heartbeat_interval(masters).as_secs_f64();
            assert!(rate <= 2.0, "{masters} masters beat at {rate:.2} INFO/s");
        }
    }
}

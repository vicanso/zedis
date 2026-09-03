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

//! Feature floors that decide **both** flavors.
//!
//! Valkey forked from Redis 7.2.4 and numbers its own releases (7.2 → 8.0
//! → 8.1 → 9.0) on a track that runs *ahead* of Redis's. A bare `>= x.y.z`
//! written for a Redis feature therefore passes on a Valkey that never
//! shipped it (HOTKEYS, `INFO keysizes`, XACKDEL) or shipped it at another
//! version (`SET IFEQ`: Redis 8.4 vs Valkey 8.1; hash field TTL: Redis 7.4
//! vs Valkey 9.0). Every version gate in the app goes through a [`Floor`]
//! from this table, so the Valkey side is always written down —
//! `Some(version)` or `None` (never shipped) — and reviewable in one place.
//!
//! Command *availability* on proxies / managed clouds is a different axis:
//! that is probed (`probe.rs`), not versioned.

use semver::Version;

/// Valkey forked at Redis 7.2.4: every Valkey release is at least this, so
/// a feature Redis had by 7.2 is "all of Valkey".
pub const VALKEY_BASELINE: &str = "7.2.0";

/// First Redis version that ships a feature, and the Valkey side of the
/// same question — both mandatory, so a gate cannot forget one flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floor {
    pub redis: &'static str,
    /// `Some(first Valkey version)`, or `None` when Valkey never shipped it.
    pub valkey: Option<&'static str>,
}

impl Floor {
    /// Redis and Valkey adopted the feature at different versions.
    const fn both(redis: &'static str, valkey: &'static str) -> Self {
        Self {
            redis,
            valkey: Some(valkey),
        }
    }

    /// Redis had it by the fork point, so every Valkey release has it.
    const fn since_fork(redis: &'static str) -> Self {
        Self {
            redis,
            valkey: Some(VALKEY_BASELINE),
        }
    }

    /// Valkey never shipped it.
    const fn redis_only(redis: &'static str) -> Self {
        Self { redis, valkey: None }
    }

    /// Whether a server of this flavor and version clears the floor. A
    /// malformed floor literal never clears — a typo fails closed.
    pub fn met_by(self, is_valkey: bool, version: &Version) -> bool {
        let floor = if is_valkey { self.valkey } else { Some(self.redis) };
        floor
            .and_then(|f| Version::parse(f).ok())
            .is_some_and(|f| *version >= f)
    }
}

/// `MEMORY USAGE` (Redis 4.0).
pub const MEMORY_USAGE: Floor = Floor::since_fork("4.0.0");
/// `UNLINK` (Redis 4.0) — bulk deletes fall back to `DEL` below it; the
/// Windows 3.0 / 3.2 ports still in use answer `unknown command 'UNLINK'`.
pub const UNLINK: Floor = Floor::since_fork("4.0.0");
/// `SET … KEEPTTL` (Redis 6.0).
pub const SET_KEEPTTL: Floor = Floor::since_fork("6.0.0");
/// `SCAN … TYPE` (Redis 6.0).
pub const SCAN_TYPE: Floor = Floor::since_fork("6.0.0");
/// The ACL family — `ACL USERS / GETUSER / SETUSER / WHOAMI` (Redis 6.0).
pub const ACL: Floor = Floor::since_fork("6.0.0");
/// ACL v2 — `ACL DRYRUN` and `(…)` selectors (Redis 7.0).
pub const ACL_V2: Floor = Floor::since_fork("7.0.0");
/// Functions — `FUNCTION LIST / LOAD / DELETE / FCALL` (Redis 7.0).
pub const FUNCTIONS: Floor = Floor::since_fork("7.0.0");
/// Sharded Pub/Sub — `SSUBSCRIBE` / `SPUBLISH` (Redis 7.0).
pub const SHARDED_PUBSUB: Floor = Floor::since_fork("7.0.0");
/// `EXPIRE … NX | XX | GT | LT` (Redis 7.0).
pub const EXPIRE_CONDITIONS: Floor = Floor::since_fork("7.0.0");
/// `EVAL_RO` / `EVALSHA_RO` — read-only script execution (Redis 7.0).
pub const EVAL_RO: Floor = Floor::since_fork("7.0.0");
/// `CLIENT SETINFO LIB-NAME / LIB-VER` (Redis 7.2; every Valkey release).
pub const CLIENT_SETINFO: Floor = Floor::since_fork("7.2.0");
/// Hash field TTL — `HEXPIRE / HTTL / HPERSIST` (Redis 7.4; Valkey 9.0).
pub const HASH_FIELD_TTL: Floor = Floor::both("7.4.0", "9.0.0");
/// `INFO keysizes` per-type histograms (Redis 8.0; not in Valkey).
pub const INFO_KEYSIZES: Floor = Floor::redis_only("8.0.0");
/// `CLUSTER SLOT-STATS` — Valkey 8.0 shipped it first; Redis 8.2 adopted
/// the same reply format.
pub const CLUSTER_SLOT_STATS: Floor = Floor::both("8.2.0", "8.0.0");
/// `XACKDEL` / `XDELEX` and the `XTRIM`/`XADD` KEEPREF / DELREF / ACKED
/// words (Redis 8.2; not in Valkey).
pub const STREAM_REF_POLICIES: Floor = Floor::redis_only("8.2.0");
/// `XNACK` — release a pending entry without acking (Redis 8.8; not in
/// Valkey).
pub const STREAM_NACK: Floor = Floor::redis_only("8.8.0");
/// `XGROUP CREATECONSUMER` (Redis 6.2).
pub const STREAM_CREATE_CONSUMER: Floor = Floor::since_fork("6.2.0");
/// `SET … IFEQ` compare-and-set (Redis 8.4; Valkey 8.1, valkey-io/valkey#1324).
pub const SET_IFEQ: Floor = Floor::both("8.4.0", "8.1.0");
/// `HOTKEYS START / GET / STOP / RESET` (Redis 8.6; not in Valkey).
pub const HOTKEYS: Floor = Floor::redis_only("8.6.0");
/// `maxmemory-policy allkeys-lrm` / `volatile-lrm` — least recently
/// *modified* eviction (Redis 8.6; not in Valkey).
pub const MAXMEMORY_LRM: Floor = Floor::redis_only("8.6.0");
/// `VSIM … WITHATTRIBS` — neighbour attributes inline (Redis 8.2; vector
/// sets exist only in Redis).
pub const VSIM_WITHATTRIBS: Floor = Floor::redis_only("8.2.0");

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("test version")
    }

    #[test]
    fn redis_floor_is_a_plain_comparison() {
        assert!(SET_IFEQ.met_by(false, &v("8.4.0")));
        assert!(SET_IFEQ.met_by(false, &v("8.6.1")));
        assert!(!SET_IFEQ.met_by(false, &v("8.2.9")));
    }

    #[test]
    fn unlink_is_missing_on_the_windows_ports() {
        // MSOpenTech / tporadowski builds report 3.0.504 and 3.2.100.
        assert!(!UNLINK.met_by(false, &v("3.0.504")));
        assert!(!UNLINK.met_by(false, &v("3.2.100")));
        assert!(UNLINK.met_by(false, &v("4.0.0")));
        assert!(UNLINK.met_by(true, &v("7.2.4")));
    }

    #[test]
    fn valkey_uses_its_own_floor() {
        // Earlier than Redis's 8.4 — the whole point of the second column.
        assert!(SET_IFEQ.met_by(true, &v("8.1.0")));
        assert!(!SET_IFEQ.met_by(true, &v("8.0.3")));
        // Later than Redis's 7.4: a Valkey 8.x would pass a bare floor.
        assert!(!HASH_FIELD_TTL.met_by(true, &v("8.1.0")));
        assert!(HASH_FIELD_TTL.met_by(true, &v("9.0.0")));
    }

    #[test]
    fn redis_only_features_never_clear_on_valkey() {
        for floor in [
            INFO_KEYSIZES,
            STREAM_REF_POLICIES,
            STREAM_NACK,
            HOTKEYS,
            MAXMEMORY_LRM,
            VSIM_WITHATTRIBS,
        ] {
            assert!(!floor.met_by(true, &v("99.0.0")), "{floor:?}");
            assert!(floor.met_by(false, &v("99.0.0")), "{floor:?}");
        }
    }

    #[test]
    fn since_fork_covers_every_valkey_release() {
        assert!(EXPIRE_CONDITIONS.met_by(true, &v("7.2.4")));
        assert!(MEMORY_USAGE.met_by(true, &v("8.0.0")));
        assert!(!MEMORY_USAGE.met_by(false, &v("3.2.0")));
    }

    #[test]
    fn a_malformed_floor_fails_closed() {
        let broken = Floor {
            redis: "eight",
            valkey: None,
        };
        assert!(!broken.met_by(false, &v("99.0.0")));
    }
}

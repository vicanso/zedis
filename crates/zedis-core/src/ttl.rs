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

/// Threshold below which a live TTL is rendered with the warning colour
/// (`Expiring`). Two minutes is the smallest window that still leaves
/// enough time for the user to spot the chip, open the key, and run
/// `EXPIRE` or copy the value before it vanishes.
pub const TTL_EXPIRING_THRESHOLD_SECS: i64 = 120;

const SECS_PER_HOUR: i64 = 3600;
const SECS_PER_DAY: i64 = 86400;
const SECS_PER_WEEK: i64 = 7 * SECS_PER_DAY;

/// Key-tree TTL range filter applied **locally** on the already-loaded
/// `key_ttls` cache (no extra Redis round-trips). Presets align with the
/// memory-analyzer histogram buckets plus an ops-oriented "expiring soon".
///
/// Wire form (`as_str` / `from_name`) is used by the key-tree action payload
/// so the colour/type enums stay free of gpui `Action` bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtlFilter {
    /// No TTL constraint — every key passes this dimension.
    #[default]
    All,
    /// Permanent keys (`TTL` == `-1`).
    NoTtl,
    /// About to expire: `0 ≤ ttl < TTL_EXPIRING_THRESHOLD_SECS`.
    Expiring,
    /// Live expiry within an hour: `0 ≤ ttl < 3600`.
    Lt1h,
    /// Live expiry within a day: `0 ≤ ttl < 86400`.
    Lt1d,
    /// Live expiry within a week: `0 ≤ ttl < 7d`.
    Lt7d,
    /// Long-lived keys: `ttl ≥ 7d`.
    Gte7d,
}

impl TtlFilter {
    /// Stable id for menus / action payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            TtlFilter::All => "all",
            TtlFilter::NoTtl => "no_ttl",
            TtlFilter::Expiring => "expiring",
            TtlFilter::Lt1h => "lt_1h",
            TtlFilter::Lt1d => "lt_1d",
            TtlFilter::Lt7d => "lt_7d",
            TtlFilter::Gte7d => "gte_7d",
        }
    }

    /// Parse a wire id; unknown values fall back to [`TtlFilter::All`].
    pub fn from_name(s: &str) -> Self {
        match s {
            "no_ttl" => TtlFilter::NoTtl,
            "expiring" => TtlFilter::Expiring,
            "lt_1h" => TtlFilter::Lt1h,
            "lt_1d" => TtlFilter::Lt1d,
            "lt_7d" => TtlFilter::Lt7d,
            "gte_7d" => TtlFilter::Gte7d,
            _ => TtlFilter::All,
        }
    }

    /// Whether a cached TTL value matches this filter.
    ///
    /// `None` (not yet loaded) and `-2` (key vanished between SCAN and TTL)
    /// never match a non-`All` filter — the UI filters only on already-known
    /// TTL samples, never triggers a full-keyspace TTL pass.
    pub fn matches(self, ttl_secs: Option<i64>) -> bool {
        if matches!(self, TtlFilter::All) {
            return true;
        }
        let Some(ttl) = ttl_secs else {
            return false;
        };
        match self {
            TtlFilter::All => true,
            TtlFilter::NoTtl => ttl == -1,
            TtlFilter::Expiring => (0..TTL_EXPIRING_THRESHOLD_SECS).contains(&ttl),
            TtlFilter::Lt1h => (0..SECS_PER_HOUR).contains(&ttl),
            TtlFilter::Lt1d => (0..SECS_PER_DAY).contains(&ttl),
            TtlFilter::Lt7d => (0..SECS_PER_WEEK).contains(&ttl),
            TtlFilter::Gte7d => ttl >= SECS_PER_WEEK,
        }
    }
}

/// Semantic classification of a key's TTL for chip rendering. The view
/// layer maps this to actual theme colours — keeping the mapping out of
/// the helper avoids dragging the `gpui` theme into a pure-logic module
/// and lets dark/light themes pick their own palettes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlChipKind {
    /// `-1`: no expiry. Rendered in a neutral / muted colour.
    Perm,
    /// `0..TTL_EXPIRING_THRESHOLD_SECS`: about to expire. Rendered as
    /// a single warning colour so the user can spot keys that won't
    /// outlast the current session.
    Expiring,
    /// `>= TTL_EXPIRING_THRESHOLD_SECS`: comfortably live. Healthy colour.
    Live,
}

/// Decide whether a chip should be rendered for this TTL and what kind.
/// `-2` (key vanished between SCAN and TTL) returns `None` — that's a
/// transient race we don't surface.
pub fn ttl_chip_kind(ttl_secs: i64) -> Option<TtlChipKind> {
    match ttl_secs {
        -2 => None,
        -1 => Some(TtlChipKind::Perm),
        s if s < TTL_EXPIRING_THRESHOLD_SECS => Some(TtlChipKind::Expiring),
        _ => Some(TtlChipKind::Live),
    }
}

/// Compact TTL chip label used in the Key Tree. Two-digit cap on the
/// number, then a unit letter — `12s`, `4m`, `23h`, `7d`. `∞` is used
/// for perm keys. Anything longer than 99 days is clipped to `99d` so
/// the chip width stays uniform.
///
/// Returns `None` only for `-2` (missing key — no chip rendered).
pub fn format_ttl_chip(ttl_secs: i64) -> Option<String> {
    if ttl_secs == -2 {
        return None;
    }
    if ttl_secs == -1 {
        return Some("∞".into());
    }
    let s = if ttl_secs < 60 {
        format!("{ttl_secs}s")
    } else if ttl_secs < 3600 {
        format!("{}m", ttl_secs / 60)
    } else if ttl_secs < 86400 {
        format!("{}h", ttl_secs / 3600)
    } else {
        let days = ttl_secs / 86400;
        if days < 100 { format!("{days}d") } else { "99d".into() }
    };
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::{TtlChipKind, TtlFilter, format_ttl_chip, ttl_chip_kind};

    fn chip(s: Option<String>) -> Option<String> {
        s
    }

    #[test]
    fn kind_dispatch() {
        assert_eq!(ttl_chip_kind(-2), None);
        assert_eq!(ttl_chip_kind(-1), Some(TtlChipKind::Perm));
        // Below 2 minutes ⇒ Expiring
        assert_eq!(ttl_chip_kind(0), Some(TtlChipKind::Expiring));
        assert_eq!(ttl_chip_kind(59), Some(TtlChipKind::Expiring));
        assert_eq!(ttl_chip_kind(119), Some(TtlChipKind::Expiring));
        // The 2-minute boundary itself is Live — strictly less-than.
        assert_eq!(ttl_chip_kind(120), Some(TtlChipKind::Live));
        assert_eq!(ttl_chip_kind(3600), Some(TtlChipKind::Live));
    }

    #[test]
    fn filter_all_passes_everything() {
        assert!(TtlFilter::All.matches(None));
        assert!(TtlFilter::All.matches(Some(-2)));
        assert!(TtlFilter::All.matches(Some(-1)));
        assert!(TtlFilter::All.matches(Some(30)));
    }

    #[test]
    fn filter_no_ttl_and_expiring() {
        assert!(TtlFilter::NoTtl.matches(Some(-1)));
        assert!(!TtlFilter::NoTtl.matches(Some(0)));
        assert!(!TtlFilter::NoTtl.matches(None));
        assert!(TtlFilter::Expiring.matches(Some(0)));
        assert!(TtlFilter::Expiring.matches(Some(119)));
        assert!(!TtlFilter::Expiring.matches(Some(120)));
        assert!(!TtlFilter::Expiring.matches(Some(-1)));
        // Unknown / missing never match a constrained filter.
        assert!(!TtlFilter::Expiring.matches(None));
        assert!(!TtlFilter::Expiring.matches(Some(-2)));
    }

    #[test]
    fn filter_hour_day_week_buckets() {
        assert!(TtlFilter::Lt1h.matches(Some(3599)));
        assert!(!TtlFilter::Lt1h.matches(Some(3600)));
        assert!(TtlFilter::Lt1d.matches(Some(3600)));
        assert!(!TtlFilter::Lt1d.matches(Some(86400)));
        assert!(TtlFilter::Lt7d.matches(Some(86400)));
        assert!(!TtlFilter::Lt7d.matches(Some(7 * 86400)));
        assert!(TtlFilter::Gte7d.matches(Some(7 * 86400)));
        assert!(!TtlFilter::Gte7d.matches(Some(7 * 86400 - 1)));
    }

    #[test]
    fn filter_roundtrip_wire_ids() {
        for f in [
            TtlFilter::All,
            TtlFilter::NoTtl,
            TtlFilter::Expiring,
            TtlFilter::Lt1h,
            TtlFilter::Lt1d,
            TtlFilter::Lt7d,
            TtlFilter::Gte7d,
        ] {
            assert_eq!(TtlFilter::from_name(f.as_str()), f);
        }
        assert_eq!(TtlFilter::from_name("nope"), TtlFilter::All);
    }

    #[test]
    fn chip_seconds_minutes_hours_days() {
        assert_eq!(chip(format_ttl_chip(0)).as_deref(), Some("0s"));
        assert_eq!(chip(format_ttl_chip(59)).as_deref(), Some("59s"));
        assert_eq!(chip(format_ttl_chip(60)).as_deref(), Some("1m"));
        assert_eq!(chip(format_ttl_chip(3599)).as_deref(), Some("59m"));
        assert_eq!(chip(format_ttl_chip(3600)).as_deref(), Some("1h"));
        assert_eq!(chip(format_ttl_chip(86399)).as_deref(), Some("23h"));
        assert_eq!(chip(format_ttl_chip(86400)).as_deref(), Some("1d"));
    }

    #[test]
    fn chip_clipped_at_99_days() {
        // 100 days → still shows 99d so the chip width stays 2-digit-bound.
        assert_eq!(chip(format_ttl_chip(100 * 86400)).as_deref(), Some("99d"));
        assert_eq!(chip(format_ttl_chip(365 * 86400)).as_deref(), Some("99d"));
    }

    #[test]
    fn chip_perm_uses_infinity_glyph() {
        assert_eq!(chip(format_ttl_chip(-1)).as_deref(), Some("∞"));
    }

    #[test]
    fn chip_missing_renders_nothing() {
        assert!(format_ttl_chip(-2).is_none());
    }
}

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

//! Read-only decoders for the value viewer's native format detection: each
//! turns one serialization into a `serde_json::Value` (or text) the editor
//! shows as a preview, and answers `None` when the bytes are not that
//! format. Every decoder is also its own detector — a decode that reads
//! *every* byte and ends where the format says it ends is the evidence, so
//! a plain string that merely resembles one (a hex digest that fits the
//! Base64 alphabet, `i:1;` that parses as PHP) does not get re-interpreted.
//!
//! Binary formats carry a signature (Java's `AC ED 00 05`, a pickle's
//! `PROTO` byte, a BSON document's own length); the text ones (Base64, URL
//! encoding, JWT, PHP `serialize()`) are only tried on a UTF-8 value that
//! is not JSON, in that order of confidence. Nothing here writes back: the
//! previews are read-only, and the hex view stays the way to edit bytes.

pub mod base64_text;
pub mod bson;
pub mod java;
pub mod jwt;
pub mod php;
pub mod pickle;
pub mod url;

use serde_json::{Map, Number, Value};
use std::time::Duration;

/// A tagged single-field object, the shape MongoDB's extended JSON uses
/// for values plain JSON has no type for (`{"$oid": …}`, `{"$date": …}`).
fn tagged(tag: &str, value: Value) -> Value {
    let mut map = Map::new();
    map.insert(tag.to_string(), value);
    Value::Object(map)
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A float as JSON, with the values JSON cannot carry spelled out.
fn float(f: f64) -> Value {
    Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(f.to_string()))
}

/// RFC 3339 (UTC, seconds) for a Unix timestamp in milliseconds, `None`
/// outside the years 0000–9999 that RFC 3339 can spell.
///
/// Computed here rather than by `humantime`, which panics on both sides of
/// its range — an `expect` for anything before 1970 and a formatting error
/// past 9999 — and a release build aborts on a panic. A BSON date of 1960,
/// a birthday in a serialized `java.util.Date` or a JWT `exp` of -1 would
/// each close the app the moment the key was opened.
fn rfc3339_millis(millis: i64) -> Option<String> {
    let secs = millis.div_euclid(1000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    if !(0..=9999).contains(&year) {
        return None;
    }
    let second_of_day = secs.rem_euclid(86_400);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    ))
}

/// The proleptic Gregorian `(year, month, day)` of a day count since
/// 1970-01-01 — Howard Hinnant's `civil_from_days`, exact for every `i64`
/// day a millisecond timestamp can name.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Seconds since the Unix epoch, now.
///
/// Spelled through `web_time`, which is std on the desktop and the browser's
/// clock on wasm, where `std::time::SystemTime::now()` panics — the only call
/// std cannot make there (ADR 9).
fn unix_now_secs() -> Option<i64> {
    let since_epoch = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok()?;
    Some(since_epoch.as_secs() as i64)
}

/// `rfc3339_millis` plus how far from now: `2026-09-04T08:00:00Z (in 2h 5m)`
/// or `… (3d 4h ago)`, to the minute.
fn describe_instant(seconds: i64) -> Option<String> {
    let stamp = rfc3339_millis(seconds.checked_mul(1000)?)?;
    let now = unix_now_secs()?;
    let delta = seconds - now;
    let rounded = Duration::from_secs(delta.unsigned_abs() - delta.unsigned_abs() % 60);
    let relative = if rounded.is_zero() {
        "now".to_string()
    } else if delta > 0 {
        format!("in {}", humantime::format_duration(rounded))
    } else {
        format!("{} ago", humantime::format_duration(rounded))
    };
    Some(format!("{stamp} ({relative})"))
}

#[cfg(test)]
mod tests {
    use super::rfc3339_millis;

    #[test]
    fn an_instant_on_either_side_of_the_epoch_is_formatted_not_a_panic() {
        assert_eq!(rfc3339_millis(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            rfc3339_millis(1_700_000_000_000).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(rfc3339_millis(951_782_400_000).as_deref(), Some("2000-02-29T00:00:00Z"));
        // Before 1970 — a BSON `ISODate("1960-01-01")`, a JWT `exp` of -1.
        assert_eq!(
            rfc3339_millis(-315_619_200_000).as_deref(),
            Some("1960-01-01T00:00:00Z")
        );
        assert_eq!(rfc3339_millis(-1).as_deref(), Some("1969-12-31T23:59:59Z"));
        assert_eq!(rfc3339_millis(-1000).as_deref(), Some("1969-12-31T23:59:59Z"));
        assert_eq!(
            rfc3339_millis(-62_167_219_200_000).as_deref(),
            Some("0000-01-01T00:00:00Z")
        );
        // The last second RFC 3339 can spell, and past it.
        assert_eq!(
            rfc3339_millis(253_402_300_799_999).as_deref(),
            Some("9999-12-31T23:59:59Z")
        );
        assert_eq!(rfc3339_millis(253_402_300_800_000), None);
        assert_eq!(rfc3339_millis(-62_167_219_200_001), None);
        assert_eq!(rfc3339_millis(i64::MAX), None);
        assert_eq!(rfc3339_millis(i64::MIN), None);
    }
}

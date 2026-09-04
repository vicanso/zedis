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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
/// outside what `SystemTime` can represent.
fn rfc3339_millis(millis: i64) -> Option<String> {
    let time = if millis >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_millis(millis as u64))?
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_millis(millis.unsigned_abs()))?
    };
    Some(humantime::format_rfc3339_seconds(time).to_string())
}

/// `rfc3339_millis` plus how far from now: `2026-09-04T08:00:00Z (in 2h 5m)`
/// or `… (3d 4h ago)`, to the minute.
fn describe_instant(seconds: i64) -> Option<String> {
    let stamp = rfc3339_millis(seconds.checked_mul(1000)?)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
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

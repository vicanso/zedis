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

use std::time::Duration;

/// Performs fast case-insensitive substring search with ASCII optimization.
///
/// This function is optimized for performance with two strategies:
/// 1. **ASCII fast path**: Uses byte-level comparison for ASCII strings (~10x faster)
/// 2. **Unicode fallback**: Falls back to full Unicode lowercase comparison for non-ASCII
///
/// # Arguments
/// * `haystack` - The string to search in
/// * `needle_lower` - The substring to search for (must already be lowercase)
///
/// # Returns
/// `true` if `needle_lower` is found in `haystack` (case-insensitive), `false` otherwise
///
/// # Performance Notes
/// - Early returns if needle is longer than haystack
/// - For ASCII strings, uses efficient byte-level sliding window comparison
/// - For Unicode strings, falls back to standard case-insensitive search
///
/// # Examples
/// ```
/// # use zedis_core::string::fast_contains_ignore_case;
/// assert!(fast_contains_ignore_case("Hello World", "hello"));
/// assert!(fast_contains_ignore_case("测试ABC", "abc"));
/// assert!(!fast_contains_ignore_case("short", "longer"));
/// ```
pub fn fast_contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    // Early return: needle cannot be found if it's longer than haystack
    if needle_lower.len() > haystack.len() {
        return false;
    }

    // Fast path for ASCII strings: use byte-level comparison
    if haystack.is_ascii() {
        let needle_bytes = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle_bytes.len())
            .any(|window| window.eq_ignore_ascii_case(needle_bytes));
    }

    // Fallback for Unicode strings: full lowercase conversion
    haystack.to_lowercase().contains(needle_lower)
}
/// Compact, human-readable duration with **floor** to one decimal place:
/// `6.9d`, `23.4h`, `4.5m`, `12s`. We deliberately avoid `{:.1}` rounding
/// because it can carry e.g. 6.99 days up to `7.0d`, contradicting the
/// Key Tree's `format_ttl_chip` (which floors to `6d`). The integer part
/// here always agrees with the chip's single-letter form.
const SECONDS_PER_DAY: u64 = 86400;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_MINUTE: u64 = 60;

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();

    if seconds >= SECONDS_PER_DAY {
        return format_floor_tenths(seconds, SECONDS_PER_DAY, 'd');
    }

    if seconds >= SECONDS_PER_HOUR {
        return format_floor_tenths(seconds, SECONDS_PER_HOUR, 'h');
    }

    if seconds >= SECONDS_PER_MINUTE {
        return format_floor_tenths(seconds, SECONDS_PER_MINUTE, 'm');
    }

    format!("{}s", seconds)
}
/// Floor `seconds / unit_secs` to one decimal place, then format as
/// `"{whole}.{tenth}{suffix}"`. Pure integer math — no float rounding,
/// so 6.99d formats as `6.9d`, never `7.0d`.
fn format_floor_tenths(seconds: u64, unit_secs: u64, suffix: char) -> String {
    let tenths = seconds.saturating_mul(10) / unit_secs;
    format!("{}.{}{}", tenths / 10, tenths % 10, suffix)
}
pub fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }

    match haystack.get(..needle.len()) {
        Some(sub) => sub.eq_ignore_ascii_case(needle),
        None => false,
    }
}
/// Groups a count into thousands (`500000` → `"500,000"`) — six-digit key /
/// client / slowlog counts are unreadable without it. Hand-rolled to keep the
/// dependency surface lean (no `num-format` for a formatting one-liner).
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_thousands_inserts_separators() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(500_000), "500,000");
        assert_eq!(group_thousands(1_234_567_890), "1,234,567,890");
    }

    #[test]
    fn format_duration_floors_to_one_decimal_and_never_rounds_up() {
        // ~6.99 days would round up to "7.0d" with `{:.1}`; floor keeps the
        // integer part agreeing with the Key Tree chip's "6d".
        assert_eq!(format_duration(Duration::from_secs(604_000)), "6.9d");
        assert_eq!(format_duration(Duration::from_secs(7 * 86_400)), "7.0d");
        // Sub-day precision is preserved (we lose it only when below 1m).
        assert_eq!(format_duration(Duration::from_secs(3600 + 1800)), "1.5h");
        // Just under an hour falls into the minute branch and still floors.
        assert_eq!(format_duration(Duration::from_secs(3599)), "59.9m");
        assert_eq!(format_duration(Duration::from_secs(60)), "1.0m");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }
}

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

use std::borrow::Cow;
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
/// The largest whole unit only, floored: `66d`, `1h`, `4m`, `12s`, `0s`.
/// For columns read at a glance (a client's connected / idle time), where
/// the sort goes by the raw seconds anyway and `6.9d` or `66d 6h` is more
/// than the eye needs.
pub fn format_duration_units(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let units = [
        (SECONDS_PER_DAY, 'd'),
        (SECONDS_PER_HOUR, 'h'),
        (SECONDS_PER_MINUTE, 'm'),
        (1, 's'),
    ];
    match units.iter().find(|(unit, _)| seconds >= *unit) {
        Some((unit, suffix)) => format!("{}{suffix}", seconds / unit),
        None => "0s".to_string(),
    }
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
    fn format_duration_units_keeps_only_the_largest_whole_unit() {
        assert_eq!(format_duration_units(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration_units(Duration::from_secs(12)), "12s");
        assert_eq!(format_duration_units(Duration::from_secs(121)), "2m");
        assert_eq!(format_duration_units(Duration::from_secs(3600 + 1800)), "1h");
        assert_eq!(
            format_duration_units(Duration::from_secs(66 * 86_400 + 6 * 3600)),
            "66d"
        );
        assert_eq!(format_duration_units(Duration::from_secs(604_000)), "6d");
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

/// `host:port` → `(host, port)`, split at the **last** colon so an
/// unbracketed IPv6 literal (`::1:6379`, the shape `CLUSTER NODES` prints)
/// parses; a bracketed host (`[::1]:6379`) has its brackets stripped.
/// `None` when there is no port.
pub fn split_host_port(addr: &str) -> Option<(&str, u16)> {
    let (host, port) = addr.trim().rsplit_once(':')?;
    let port = port.parse().ok()?;
    Some((strip_ipv6_brackets(host), port))
}

/// A user-typed endpoint whose port is optional: `host`, `host:22`,
/// `[::1]`, `[::1]:22`, or a bare IPv6 literal (`::1` — which is then the
/// whole host: an IPv6 address *with* a port must be bracketed, as
/// everywhere else). An unparsable port falls back to `default_port`.
pub fn split_host_port_or(addr: &str, default_port: u16) -> (&str, u16) {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return (host, port);
    }
    if addr.matches(':').count() > 1 {
        return (addr, default_port);
    }
    match addr.split_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(default_port)),
        None => (addr, default_port),
    }
}

/// `host:port` for URLs and labels, with an IPv6 literal bracketed.
pub fn format_host_port(host: &str, port: u16) -> String {
    format!("{}:{port}", bracket_ipv6(host))
}

/// `[::1]` for an IPv6 literal, any other host unchanged. A colon in a host
/// can only be an IPv6 literal (hostnames and IPv4 have none); an already
/// bracketed one is left alone.
pub fn bracket_ipv6(host: &str) -> Cow<'_, str> {
    if host.contains(':') && !host.starts_with('[') {
        Cow::Owned(format!("[{host}]"))
    } else {
        Cow::Borrowed(host)
    }
}

/// `[::1]` → `::1`; anything else unchanged.
pub fn strip_ipv6_brackets(host: &str) -> &str {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

#[cfg(test)]
mod host_port_tests {
    use super::{bracket_ipv6, format_host_port, split_host_port, split_host_port_or, strip_ipv6_brackets};

    #[test]
    fn last_colon_is_the_port_separator() {
        assert_eq!(split_host_port("localhost:6379"), Some(("localhost", 6379)));
        assert_eq!(split_host_port("10.0.0.1:7000"), Some(("10.0.0.1", 7000)));
        // The unbracketed form CLUSTER NODES prints.
        assert_eq!(split_host_port("::1:7000"), Some(("::1", 7000)));
        assert_eq!(split_host_port("fe80::1%en0:7000"), Some(("fe80::1%en0", 7000)));
        // Bracketed, as a user or a URL would write it.
        assert_eq!(split_host_port("[::1]:7000"), Some(("::1", 7000)));
        assert_eq!(split_host_port("localhost"), None);
        assert_eq!(split_host_port("localhost:port"), None);
    }

    #[test]
    fn optional_port_endpoints() {
        assert_eq!(split_host_port_or("bastion", 22), ("bastion", 22));
        assert_eq!(split_host_port_or("bastion:2200", 22), ("bastion", 2200));
        assert_eq!(split_host_port_or(" bastion:bad ", 22), ("bastion", 22));
        assert_eq!(split_host_port_or("[::1]", 22), ("::1", 22));
        assert_eq!(split_host_port_or("[::1]:2200", 22), ("::1", 2200));
        assert_eq!(split_host_port_or("[2001:db8::1]:22", 22), ("2001:db8::1", 22));
        // A bare literal is a host, never "host ::1 plus port 1".
        assert_eq!(split_host_port_or("2001:db8::1", 22), ("2001:db8::1", 22));
    }

    #[test]
    fn ipv6_is_bracketed_for_urls_and_labels() {
        assert_eq!(format_host_port("::1", 6379), "[::1]:6379");
        assert_eq!(format_host_port("[::1]", 6379), "[::1]:6379");
        assert_eq!(format_host_port("redis.example.com", 6379), "redis.example.com:6379");
        assert_eq!(bracket_ipv6("127.0.0.1"), "127.0.0.1");
        assert_eq!(strip_ipv6_brackets("[::1]"), "::1");
        assert_eq!(strip_ipv6_brackets("::1"), "::1");
        assert_eq!(strip_ipv6_brackets("[::1"), "[::1");
    }
}

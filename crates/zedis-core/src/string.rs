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
use std::cmp::Ordering;
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
/// `1.9y`, `6.9d`, `23.4h`, `4.5m`, `12s`. We deliberately avoid `{:.1}` rounding
/// because it can carry e.g. 6.99 days up to `7.0d`, contradicting the
/// Key Tree's `format_ttl_chip` (which floors to `6d`). The integer part
/// here always agrees with the chip's single-letter form.
const SECONDS_PER_DAY: u64 = 86400;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_YEAR: u64 = 365 * SECONDS_PER_DAY;
/// From here a duration reads in years, where the Key Tree's chip starts to:
/// 700 days is `1.9y`, and a TTL of 56,831 years is that — it read
/// `20743315.0d` (#190).
const YEARS_FROM_SECONDS: u64 = 700 * SECONDS_PER_DAY;

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();

    if seconds >= YEARS_FROM_SECONDS {
        return format_floor_tenths(seconds, SECONDS_PER_YEAR, 'y');
    }

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
/// `s` with every glob metacharacter escaped, so a `SCAN MATCH` built
/// from it matches the text itself: the keys under a folder named
/// `user[1]` are those starting with `user[1]:`, not with `user1:`, and a
/// name with `*` in it is not a wildcard. Borrowed when there is nothing
/// to escape, which is nearly always.
pub fn escape_glob(s: &str) -> Cow<'_, str> {
    const SPECIAL: [char; 5] = ['*', '?', '[', ']', '\\'];
    if !s.contains(SPECIAL) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if SPECIAL.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    Cow::Owned(out)
}

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

/// A server's clients as the status bar and the Metrics page both show
/// them: connected, then the blocked ones among them. The two used to put
/// them in opposite orders, and "0 / 9" in one place was "9 / 0" in the
/// other (#201).
pub fn client_counts(connected: u64, blocked: u64) -> String {
    format!("{} / {}", group_thousands(connected), group_thousands(blocked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_label_keeps_an_ipv6_hosts_colons() {
        assert_eq!(split_label("10.0.0.1:7000"), ("10.0.0.1", 7000));
        assert_eq!(split_label("::1:7000"), ("::1", 7000));
        assert_eq!(split_label("[fe80::1]:7001"), ("fe80::1", 7001));
        assert_eq!(split_label("redis.internal"), ("redis.internal", 0));
    }

    #[test]
    fn client_counts_put_the_connected_first() {
        assert_eq!(client_counts(9, 0), "9 / 0");
        assert_eq!(client_counts(12_345, 2), "12,345 / 2");
    }

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
        // Days up to where the tree's chip turns to years, then years.
        assert_eq!(format_duration(Duration::from_secs(699 * 86_400)), "699.0d");
        assert_eq!(format_duration(Duration::from_secs(700 * 86_400)), "1.9y");
        assert_eq!(format_duration(Duration::from_secs(1_792_222_416_000)), "56831.0y");
        // Sub-day precision is preserved (we lose it only when below 1m).
        assert_eq!(format_duration(Duration::from_secs(3600 + 1800)), "1.5h");
        // Just under an hour falls into the minute branch and still floors.
        assert_eq!(format_duration(Duration::from_secs(3599)), "59.9m");
        assert_eq!(format_duration(Duration::from_secs(60)), "1.0m");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }
    #[test]
    fn escape_glob_makes_a_name_mean_itself() {
        // Nothing to escape: borrowed, unchanged.
        assert!(matches!(escape_glob("user:1:"), Cow::Borrowed("user:1:")));
        assert_eq!(escape_glob("user[1]:"), "user\\[1\\]:");
        assert_eq!(escape_glob("a*b?c\\d"), "a\\*b\\?c\\\\d");
        // Escaped, then a wildcard appended, is the prefix and only the prefix.
        assert_eq!(format!("{}*", escape_glob("h[o]t:")), "h\\[o\\]t:*");
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

/// A `host:port` node label as the bridge writes one: the port after the
/// *last* colon, so a bare IPv6 host (`::1:7000`) keeps its own — read as a
/// user-typed endpoint ([`split_host_port_or`]) it was all host and port 0,
/// matched no node, and the browser's key tree stayed empty on an IPv6
/// cluster. Brackets are unwrapped; a label without a port is all host.
pub fn split_label(label: &str) -> (&str, u16) {
    split_host_port(label).unwrap_or((label.trim(), 0))
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

/// Compare two strings in natural order: contiguous ASCII digit runs are
/// compared numerically (ignoring leading zeros), while other bytes are
/// compared lexicographically. Leading-zero differences (e.g. `"01"` vs `"1"`)
/// fall back to `a.cmp(b)` so `natural_cmp` defines a strict total order.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut i = 0;
    let mut j = 0;
    while i < ab.len() && j < bb.len() {
        if ab[i].is_ascii_digit() && bb[j].is_ascii_digit() {
            let mut a_end = i + 1;
            while a_end < ab.len() && ab[a_end].is_ascii_digit() {
                a_end += 1;
            }
            let mut b_end = j + 1;
            while b_end < bb.len() && bb[b_end].is_ascii_digit() {
                b_end += 1;
            }
            let mut a_sig = i;
            while a_sig < a_end && ab[a_sig] == b'0' {
                a_sig += 1;
            }
            let mut b_sig = j;
            while b_sig < b_end && bb[b_sig] == b'0' {
                b_sig += 1;
            }
            let ord = (a_end - a_sig)
                .cmp(&(b_end - b_sig))
                .then_with(|| ab[a_sig..a_end].cmp(&bb[b_sig..b_end]));
            if ord != Ordering::Equal {
                return ord;
            }
            i = a_end;
            j = b_end;
        } else {
            let ord = ab[i].cmp(&bb[j]);
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j)).then_with(|| a.cmp(b))
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

#[cfg(test)]
mod natural_cmp_tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn sorts_numeric_ids_and_prefixed_keys_in_natural_order() {
        let mut ids = [
            "4430", "11", "1099", "443", "198", "1000", "78", "19", "4429", "200", "1100",
        ];
        ids.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            ids,
            [
                "11", "19", "78", "198", "200", "443", "1000", "1099", "1100", "4429", "4430"
            ]
        );

        let mut prefixed = [
            "DATA-CACHE-STAFF:10:1",
            "DATA-CACHE-STAFF:3:1000",
            "DATA-CACHE-STAFF:3:198",
            "DATA-CACHE-STAFF:3:11",
        ];
        prefixed.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            prefixed,
            [
                "DATA-CACHE-STAFF:3:11",
                "DATA-CACHE-STAFF:3:198",
                "DATA-CACHE-STAFF:3:1000",
                "DATA-CACHE-STAFF:10:1",
            ]
        );
    }

    #[test]
    fn handles_huge_numbers_and_non_ascii_without_overflow() {
        let big = "99999999999999999999999999999999999999999999999991";
        let bigger = "99999999999999999999999999999999999999999999999992";
        assert_eq!(natural_cmp(big, bigger), Ordering::Less);
        assert_eq!(natural_cmp("用户:2", "用户:10"), Ordering::Less);
    }

    #[test]
    fn is_a_strict_total_order() {
        let samples = [
            "", "/", "0", "00", "01", "1", "001", "01a", "1a", "01b", "1b", "2", "10", ":", "a", "a0", "a00", "a0/",
            "a0:",
        ];
        for (pos_x, x) in samples.iter().enumerate() {
            assert_eq!(natural_cmp(x, x), Ordering::Equal, "{x}");
            for (pos_y, y) in samples.iter().enumerate() {
                let xy = natural_cmp(x, y);
                let yx = natural_cmp(y, x);
                assert_eq!(xy, yx.reverse(), "antisymmetry failed for ({x}, {y})");
                if pos_x != pos_y {
                    assert_ne!(xy, Ordering::Equal, "distinct inputs compared equal: ({x}, {y})");
                }
                for z in &samples {
                    let yz = natural_cmp(y, z);
                    let xz = natural_cmp(x, z);
                    if xy == Ordering::Less && yz == Ordering::Less {
                        assert_eq!(xz, Ordering::Less, "transitivity failed for ({x}, {y}, {z})");
                    }
                    if xy == Ordering::Greater && yz == Ordering::Greater {
                        assert_eq!(xz, Ordering::Greater, "transitivity failed for ({x}, {y}, {z})");
                    }
                }
            }
        }
    }
}

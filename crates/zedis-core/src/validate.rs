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

pub fn validate_ttl(s: &str) -> bool {
    if s.is_empty() || s.parse::<usize>().is_ok() {
        return true;
    }
    humantime::parse_duration(s).is_ok()
}

pub fn validate_long_string(s: &str) -> bool {
    s.len() <= 4096
}

/// Normalise one end of a sorted-set score range into what `ZRANGEBYSCORE`
/// accepts: a number, `-inf` / `+inf`, or either prefixed with `(` for an
/// exclusive bound. Returns `None` when the text is none of those.
///
/// Validating client-side rather than letting the server answer "min or max
/// is not a float": by the time that error arrives the panel has already
/// cleared its rows for a query that was never going to run.
///
/// Blank is deliberately *not* an error — the caller decides what an empty
/// end means (the filter reads it as the open end) — so it maps to `-inf` /
/// `+inf` there, not here.
pub fn normalize_score_bound(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (prefix, number) = match trimmed.strip_prefix('(') {
        Some(rest) => ("(", rest.trim()),
        None => ("", trimmed),
    };
    let lower = number.to_ascii_lowercase();
    if matches!(lower.as_str(), "-inf" | "+inf" | "inf") {
        // Redis spells the open ends `-inf` / `+inf`; a bare `inf` is the
        // upper one, and normalising it here keeps the sent command
        // literal rather than hoping the server is lenient.
        let canonical = if lower == "-inf" { "-inf" } else { "+inf" };
        return Some(format!("{prefix}{canonical}"));
    }
    let value: f64 = lower.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(format!("{prefix}{number}"))
}

#[cfg(test)]
mod score_bound_tests {
    use super::normalize_score_bound;

    #[test]
    fn plain_numbers_and_infinities_pass_through() {
        assert_eq!(normalize_score_bound("10").as_deref(), Some("10"));
        assert_eq!(normalize_score_bound(" -2.5 ").as_deref(), Some("-2.5"));
        assert_eq!(normalize_score_bound("-inf").as_deref(), Some("-inf"));
        assert_eq!(normalize_score_bound("+inf").as_deref(), Some("+inf"));
        // A bare `inf` is the upper end, and is spelled the way Redis wants.
        assert_eq!(normalize_score_bound("inf").as_deref(), Some("+inf"));
        assert_eq!(normalize_score_bound("INF").as_deref(), Some("+inf"));
    }

    #[test]
    fn the_exclusive_prefix_is_kept() {
        assert_eq!(normalize_score_bound("(5").as_deref(), Some("(5"));
        assert_eq!(normalize_score_bound("( 5").as_deref(), Some("(5"));
        assert_eq!(normalize_score_bound("(-inf").as_deref(), Some("(-inf"));
    }

    #[test]
    fn anything_the_server_would_reject_is_refused_here() {
        assert_eq!(normalize_score_bound(""), None);
        assert_eq!(normalize_score_bound("   "), None);
        assert_eq!(normalize_score_bound("abc"), None);
        assert_eq!(normalize_score_bound("nan"), None);
        assert_eq!(normalize_score_bound("1..2"), None);
        assert_eq!(normalize_score_bound("[5"), None);
        // A stray prefix with nothing after it.
        assert_eq!(normalize_score_bound("("), None);
    }
}

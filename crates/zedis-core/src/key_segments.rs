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

//! How a Redis key name maps onto key-tree levels.
//!
//! The per-key inner loop of the key-tree build: every rebuild runs
//! [`split_key_segments`] once per key, so on a million-key keyspace this is
//! the hot path that decides whether expanding a folder feels instant. It is
//! pure `&str` work with no gpui types, which is why it lives here rather
//! than in the view that consumes it — the app crate cannot be benched, and
//! `benches/hot_paths.rs` measures this module directly.
//!
//! Splitting is deliberately not `str::splitn`: Redis keys carry hash tags,
//! JSON blobs and timestamps whose separators are not level boundaries. See
//! [`split_key_segments`] for the rules and the issues that motivated them.

/// Split a key into tree segments — the single source of truth every
/// tree-building pass shares, so folder ids, expansion bookkeeping and tag
/// aggregates can never disagree about where a key's levels are.
///
/// Behaves like `key.splitn(max_depth, separator)` except that separators
/// **inside `{…}`, inside double quotes, or inside a timestamp are not level
/// boundaries**:
///
/// * `{…}` is Redis's cluster hash tag, so `user:{tenant:42}:profile` has
///   three levels, not four.
/// * Keys that carry a JSON-ish blob (`item:1/{"name": "x", "at": "12:13:05"}`)
///   would otherwise shatter along the colons *inside* the payload, which is
///   what made such keys look truncated in the tree (issue #119).
/// * A timestamp stamped into a key (`…/2022-05-11 11:55:44.487892+00:00/…`)
///   brings its own colons; splitting there produced folders labelled `55`
///   and `44.487892+00` — fragments that read like numbers Zedis had invented
///   rather than parts of the key (issue #127). Only a full ISO-8601 date
///   anchors this: a bare `12:30:45` stays three levels, because digits alone
///   are an ordinary namespace (`shard:03:07`) far more often than a time.
///
/// A key whose braces or quotes never close is rescanned with both treated as
/// ordinary text, so one malformed key can't collapse into a single
/// unsplittable segment.
pub fn split_key_segments<'a>(key: &'a str, separator: &str, max_depth: usize) -> Vec<&'a str> {
    let depth = max_depth.max(1);
    if depth == 1 || separator.is_empty() {
        return vec![key];
    }
    let (segments, balanced) = scan_key_segments(key, separator, depth, true);
    if balanced {
        return segments;
    }
    // Unterminated brace / quote: the scan above suppressed separators that a
    // reader would consider real, so redo it with braces and quotes as plain
    // text. Timestamps still hold — they don't depend on a closing delimiter.
    scan_key_segments(key, separator, depth, false).0
}

/// One splitting pass. `honor_groups` off makes `{`, `}` and `"` ordinary
/// bytes. The returned flag says whether every group opened in this pass also
/// closed — always true when groups aren't honoured, since nothing was
/// suppressed on their account.
fn scan_key_segments<'a>(key: &'a str, separator: &str, depth: usize, honor_groups: bool) -> (Vec<&'a str>, bool) {
    let bytes = key.as_bytes();
    let sep = separator.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut brace_depth = 0usize;
    let mut in_quote = false;
    // End (exclusive) of the timestamp the scan is currently inside, 0 when it
    // is not in one. Timestamps never nest, so a single cursor covers it.
    let mut stamp_end = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // `{`, `}`, `"`, ASCII digits and any separator byte are ASCII-leading,
        // so byte scanning never lands mid-codepoint (continuation bytes are
        // 0x80-0xBF).
        if i >= stamp_end && b.is_ascii_digit() {
            let len = iso_timestamp_len(bytes, i);
            if len > 0 {
                stamp_end = i + len;
            }
        }
        if honor_groups {
            if b == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_quote = !in_quote;
                i += 1;
                continue;
            }
            if !in_quote {
                if b == b'{' {
                    brace_depth += 1;
                    i += 1;
                    continue;
                }
                if b == b'}' {
                    brace_depth = brace_depth.saturating_sub(1);
                    i += 1;
                    continue;
                }
            }
        }
        let splittable = brace_depth == 0 && !in_quote && i >= stamp_end && segments.len() + 1 < depth;
        if splittable && bytes[i..].starts_with(sep) {
            segments.push(&key[start..i]);
            i += sep.len();
            start = i;
            continue;
        }
        i += 1;
    }
    segments.push(&key[start..]);
    (segments, brace_depth == 0 && !in_quote)
}

/// Byte length of the ISO-8601 timestamp starting at `bytes[at]`, or 0 when
/// there isn't one. Shape: `YYYY-MM-DD`, optionally followed by `T`/space +
/// `HH:MM`, then an optional `:SS`, an optional `.`/`,` fraction, and an
/// optional `Z` or `±HH:MM` / `±HHMM` offset.
///
/// The date is mandatory by design. It is the part that cannot be mistaken
/// for a namespace, and it is what licenses swallowing the separators that
/// follow it; a bare `12:30:45` is left to split normally.
fn iso_timestamp_len(bytes: &[u8], at: usize) -> usize {
    let two_digits = |i: usize| i + 1 < bytes.len() && bytes[i].is_ascii_digit() && bytes[i + 1].is_ascii_digit();
    // YYYY-MM-DD
    if at + 10 > bytes.len()
        || !bytes[at..at + 4].iter().all(u8::is_ascii_digit)
        || bytes[at + 4] != b'-'
        || !two_digits(at + 5)
        || bytes[at + 7] != b'-'
        || !two_digits(at + 8)
    {
        return 0;
    }
    let mut i = at + 10;
    // [T ]HH:MM — the whole time part is optional, so a plain date is a match.
    if !(matches!(bytes.get(i), Some(b'T' | b't' | b' '))
        && two_digits(i + 1)
        && bytes.get(i + 3) == Some(&b':')
        && two_digits(i + 4))
    {
        return i - at;
    }
    i += 6;
    if bytes.get(i) == Some(&b':') && two_digits(i + 1) {
        i += 3;
    }
    if matches!(bytes.get(i), Some(b'.' | b',')) && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    match bytes.get(i) {
        Some(b'Z' | b'z') => i += 1,
        Some(b'+' | b'-') if two_digits(i + 1) => {
            i += 3;
            if bytes.get(i) == Some(&b':') && two_digits(i + 1) {
                i += 3;
            } else if two_digits(i) {
                i += 2;
            }
        }
        _ => {}
    }
    i - at
}

/// Folder path prefixes for a key under the same segmentation rules as
/// [`split_key_segments`] — every intermediate segment is a folder id, the
/// final segment is the leaf (not returned).
///
/// `a:b:c` yields `["a", "a:b"]`; a key with no separator yields nothing,
/// which is what makes it a top-level leaf.
pub fn folder_prefixes(key: &str, separator: &str, max_key_tree_depth: usize) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut dir = String::new();
    for (index, part) in split_key_segments(key, separator, max_key_tree_depth)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            prefixes.push(dir.clone());
            dir.push_str(separator);
        }
        dir.push_str(part);
    }
    prefixes
}

#[cfg(test)]
mod split_key_segments_tests {
    use super::split_key_segments;

    #[test]
    fn plain_keys_split_like_splitn() {
        assert_eq!(
            split_key_segments("user:1:profile", ":", 5),
            vec!["user", "1", "profile"]
        );
        // No separator at all — one segment, so callers treat it as a leaf.
        assert_eq!(split_key_segments("plainkey", ":", 5), vec!["plainkey"]);
        // The depth cap keeps the remainder in the last segment.
        assert_eq!(split_key_segments("a:b:c:d", ":", 3), vec!["a", "b", "c:d"]);
        // Multi-character separators work the same way.
        assert_eq!(split_key_segments("a::b::c", "::", 5), vec!["a", "b", "c"]);
    }

    #[test]
    fn hash_tags_stay_one_segment() {
        // Redis cluster hash tag containing the separator (issue #119).
        assert_eq!(
            split_key_segments("user:{tenant:42}:profile", ":", 5),
            vec!["user", "{tenant:42}", "profile"]
        );
    }

    #[test]
    fn json_blob_in_key_is_not_shattered() {
        // The reported key shape: metadata JSON appended to the key name.
        let key = r#"table:{j-groups}:item:007dc97e/{"name": "td-splunk", "at": "12:13:05"}"#;
        assert_eq!(
            split_key_segments(key, ":", 5),
            vec![
                "table",
                "{j-groups}",
                "item",
                r#"007dc97e/{"name": "td-splunk", "at": "12:13:05"}"#,
            ]
        );
    }

    #[test]
    fn quoted_separator_outside_braces_is_ignored_too() {
        assert_eq!(
            split_key_segments(r#"log:"12:13:05":done"#, ":", 5),
            vec!["log", r#""12:13:05""#, "done"]
        );
        // An escaped quote does not open a quoted run.
        assert_eq!(split_key_segments(r#"a:\":b"#, ":", 5), vec!["a", r#"\""#, "b"]);
    }

    /// Issue #127: the colons of a timestamp are not namespace levels.
    #[test]
    fn timestamps_keep_their_own_colons() {
        // The reported key: an ISO timestamp with fraction and UTC offset,
        // sitting in a keyspace that separates its levels with "/".
        let key = "jtable#abc/user/2022-05-11 11:55:44.487892+00:00/x";
        assert_eq!(split_key_segments(key, ":", 5), vec![key]);
        // Colons around the timestamp still separate levels.
        assert_eq!(
            split_key_segments("evt:2022-05-11T11:55:44Z:done", ":", 5),
            vec!["evt", "2022-05-11T11:55:44Z", "done"]
        );
        assert_eq!(
            split_key_segments("a:2022-05-11 11:55:44.487892+00:00:b", ":", 5),
            vec!["a", "2022-05-11 11:55:44.487892+00:00", "b"]
        );
        // A date holds together for a "-" separator the same way.
        assert_eq!(
            split_key_segments("log-2022-05-11-x", "-", 5),
            vec!["log", "2022-05-11", "x"]
        );
    }

    /// The date is what marks a timestamp: digits on their own are a normal
    /// namespace far more often than a time, so they keep splitting.
    #[test]
    fn digits_without_a_date_still_split() {
        assert_eq!(
            split_key_segments("app:a02:svc06:prod:sh03:k005", ":", 10),
            vec!["app", "a02", "svc06", "prod", "sh03", "k005"]
        );
        assert_eq!(
            split_key_segments("job:12:30:45", ":", 10),
            vec!["job", "12", "30", "45"]
        );
        // Year-like runs are not dates either.
        assert_eq!(
            split_key_segments("stats:2024:01:02", ":", 10),
            vec!["stats", "2024", "01", "02"]
        );
    }

    #[test]
    fn unbalanced_braces_or_quotes_fall_back_to_plain_split() {
        // Without the fallback these would collapse into a single segment.
        assert_eq!(split_key_segments("a:{b:c", ":", 5), vec!["a", "{b", "c"]);
        assert_eq!(split_key_segments(r#"a:"b:c"#, ":", 5), vec!["a", r#""b"#, "c"]);
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert_eq!(split_key_segments("a:b", "", 5), vec!["a:b"]);
        assert_eq!(split_key_segments("a:b", ":", 1), vec!["a:b"]);
        assert_eq!(split_key_segments("a:b", ":", 0), vec!["a:b"]);
        // Multi-byte content must not panic or split mid-codepoint.
        assert_eq!(split_key_segments("用户:1:名字", ":", 5), vec!["用户", "1", "名字"]);
    }
}

#[cfg(test)]
mod folder_prefixes_tests {
    use super::folder_prefixes;

    #[test]
    fn folder_prefixes_match_tree_splitn() {
        assert!(folder_prefixes("solo", ":", 10).is_empty());
        assert_eq!(
            folder_prefixes("a:b:c", ":", 10),
            vec!["a".to_string(), "a:b".to_string()]
        );
        // Depth cap: remaining path is one leaf segment.
        assert_eq!(
            folder_prefixes("a:b:c:d", ":", 3),
            vec!["a".to_string(), "a:b".to_string()]
        );
    }
}

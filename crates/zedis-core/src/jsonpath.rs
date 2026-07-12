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

//! JSONPath query helper for the bytes editor.
//!
//! Evaluates an RFC 9535 JSONPath against a JSON-formatted Redis string and
//! returns a printable, pretty-formatted result. Designed to work for keys
//! stored as plain strings — no ReJSON module required.

use serde::de::IgnoredAny;
use serde_json::Value;
use serde_json_path::JsonPath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonPathOutcome {
    /// Original value isn't valid JSON. Don't expose the query UI.
    NotJson,
    /// Path syntax invalid — surface message to the user.
    InvalidPath(String),
    /// Path is valid but matched nothing.
    NoMatch,
    /// Single match — render the value directly.
    Single(String),
    /// Multiple matches — render as a JSON array.
    Multiple(String),
}

/// Run a JSONPath query against a raw JSON string.
///
/// `raw` is the JSON source text (e.g. the value of a Redis string key).
/// `path` is a JSONPath expression like `$.user.email` or `$.items[?(@.ok)]`.
///
/// Empty / whitespace-only `path` returns `NotJson` if the source can't be
/// parsed; otherwise the caller should not invoke this function with an
/// empty path (UI should disable the query button when path is empty).
pub fn run_jsonpath(raw: &str, path: &str) -> JsonPathOutcome {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return JsonPathOutcome::NotJson;
    };
    let compiled = match JsonPath::parse(path) {
        Ok(p) => p,
        Err(e) => return JsonPathOutcome::InvalidPath(e.to_string()),
    };
    let matches = compiled.query(&value).all();
    match matches.len() {
        0 => JsonPathOutcome::NoMatch,
        1 => JsonPathOutcome::Single(format_value(matches[0])),
        _ => {
            let arr = Value::Array(matches.into_iter().cloned().collect());
            JsonPathOutcome::Multiple(format_value(&arr))
        }
    }
}

/// Is this string a JSON *container* (object or array)? Used to decide
/// whether to show the JSONPath input above the value editor.
///
/// Bare JSON scalars (`123`, `"abc"`, `true`, `null`) are valid JSON per
/// RFC 8259 but have no structure to query — the only matching path is
/// `$`, which just echoes the scalar back — so they deliberately return
/// `false` and the JSONPath bar stays hidden for them.
pub fn is_json_container(raw: &str) -> bool {
    // Fast path: the only JSON values with queryable structure start
    // with `{` or `[`. Scalars, plain text and binary-as-text (the
    // common case) are rejected here in O(1) without invoking the
    // parser at all.
    if !matches!(raw.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        return false;
    }
    // It looks like a container — confirm it's well-formed JSON, but
    // validate with `IgnoredAny` so serde_json only scans/validates
    // tokens instead of allocating a full `Value` DOM. The DOM is
    // built once, later, only if the user actually runs a query
    // (`run_jsonpath`), so a large value is never DOM-parsed just to
    // decide whether to show the bar.
    serde_json::from_str::<IgnoredAny>(raw).is_ok()
}

fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.to_string(),
        _ => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    }
}

// ---------------------------------------------------------------------------
// JSONPath completion (Tier 2): context-aware key suggestions.
//
// Pure, dependency-light logic so it is fully unit-testable without a
// GUI. The view layer wraps these in a `CompletionProvider`.
// ---------------------------------------------------------------------------

/// One fully-typed, navigable step of a JSONPath prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    /// `.name` or `["name"]` object member.
    Key(String),
    /// `[123]` array index.
    Index(usize),
    /// `[*]` wildcard over array elements / object values.
    Wildcard,
    /// `..` recursive descent: expands to the node itself plus every
    /// descendant, so the following selector matches at any depth.
    Descend,
}

/// Parsed completion context for a partial JSONPath up to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPathPrefix {
    /// Completed segments before the token being typed.
    pub segments: Vec<PathSeg>,
    /// The partial key text under the cursor (empty right after a `.`
    /// or an opening bracket-quote).
    pub partial: String,
    /// Byte offset where `partial` starts; completion replaces
    /// `[replace_start, cursor)` with the chosen key.
    pub replace_start: usize,
}

/// Minimal unescape for a bracket-quoted JSONPath key. Keys rarely
/// carry escapes; we only need enough to navigate the JSON document.
fn unescape_bracket_key(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            // `\\`, `\"`, `\'`, `\/`, anything else: take literally.
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Parse `text[..cursor]` into a completion prefix, or `None` when
/// completion should not fire here.
///
/// Supported (the common navigation subset): `$`, `.name`,
/// `["name"]` / `['name']`, `[123]`, `[*]`, and recursive descent
/// `..name` (suggests keys anywhere in the subtree). Anything else in
/// the prefix — filters `[?…]`, slices `[a:b]`, unions `[a,b]`,
/// whitespace — returns `None` (we don't guess and risk a misleading
/// suggestion).
pub fn jsonpath_completion_prefix(text: &str, cursor: usize) -> Option<JsonPathPrefix> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let head = &text[..cursor];
    let b = head.as_bytes();
    // Leading `$` required; no leading whitespace tolerated (strict).
    if b.first() != Some(&b'$') {
        return None;
    }
    let mut i = 1;
    let mut segments: Vec<PathSeg> = Vec::new();

    loop {
        if i == b.len() {
            // Ended on a segment boundary (`$`, after `]`): no partial
            // token to complete — the user must type `.`/`[` first.
            return None;
        }
        match b[i] {
            b'.' => {
                i += 1;
                // `..` is a recursive-descent segment; the member name
                // (if any) follows it.
                let descend = b.get(i) == Some(&b'.');
                if descend {
                    i += 1;
                    segments.push(PathSeg::Descend);
                }
                let name_start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                if i == b.len() {
                    // Trailing `.name` / `..name` (or a bare `.`/`..`)
                    // under the cursor: this is the partial being typed.
                    return Some(JsonPathPrefix {
                        segments,
                        partial: head[name_start..i].to_string(),
                        replace_start: name_start,
                    });
                }
                if name_start == i {
                    // No name before the next separator. `..[` (descend
                    // then bracket) is valid — keep going so the `[`
                    // arm handles it; anything else (`$.[`, `..*`, …)
                    // we don't complete.
                    if descend && b.get(i) == Some(&b'[') {
                        continue;
                    }
                    return None;
                }
                segments.push(PathSeg::Key(head[name_start..i].to_string()));
            }
            b'[' => {
                i += 1;
                match b.get(i) {
                    Some(b'*') => {
                        if b.get(i + 1) != Some(&b']') {
                            return None; // incomplete `[*`
                        }
                        segments.push(PathSeg::Wildcard);
                        i += 2;
                    }
                    Some(&q @ b'"') | Some(&q @ b'\'') => {
                        i += 1;
                        let inner_start = i;
                        let mut j = i;
                        let mut close = None;
                        while j < b.len() {
                            if b[j] == b'\\' {
                                j += 2;
                                continue;
                            }
                            if b[j] == q {
                                close = Some(j);
                                break;
                            }
                            j += 1;
                        }
                        match close {
                            Some(cq) if b.get(cq + 1) == Some(&b']') => {
                                let key = unescape_bracket_key(&head[inner_start..cq]);
                                segments.push(PathSeg::Key(key));
                                i = cq + 2;
                            }
                            _ => {
                                // Unclosed, or closed but `]` not yet
                                // typed → still typing this key.
                                let upto = close.unwrap_or(b.len());
                                return Some(JsonPathPrefix {
                                    segments,
                                    partial: unescape_bracket_key(&head[inner_start..upto]),
                                    replace_start: inner_start,
                                });
                            }
                        }
                    }
                    Some(d) if d.is_ascii_digit() => {
                        let num_start = i;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                        if b.get(i) != Some(&b']') {
                            return None; // incomplete index
                        }
                        let n: usize = head[num_start..i].parse().ok()?;
                        segments.push(PathSeg::Index(n));
                        i += 1;
                    }
                    _ => return None, // `[?`, `[a:b]`, `[a,b]`, empty …
                }
            }
            _ => return None,
        }
    }
}

/// Resolve `prefix.segments` against `value` and return candidate
/// object-key names at that location (sorted, de-duplicated, filtered
/// by `prefix.partial`). Empty when the path doesn't resolve to any
/// object container.
pub fn jsonpath_key_suggestions(value: &Value, prefix: &JsonPathPrefix) -> Vec<String> {
    // Bound the recursive-descent walk so a pathologically large
    // document can't stall the UI thread on every keystroke.
    const MAX_DESCEND_NODES: usize = 50_000;

    let mut nodes: Vec<&Value> = vec![value];
    for seg in &prefix.segments {
        let mut next: Vec<&Value> = Vec::new();
        for n in &nodes {
            match seg {
                PathSeg::Key(k) => {
                    if let Some(v) = n.get(k.as_str()) {
                        next.push(v);
                    }
                }
                PathSeg::Index(idx) => {
                    if let Some(v) = n.get(*idx) {
                        next.push(v);
                    }
                }
                PathSeg::Wildcard => match n {
                    Value::Array(a) => next.extend(a.iter()),
                    Value::Object(o) => next.extend(o.values()),
                    _ => {}
                },
                PathSeg::Descend => {
                    // The node itself plus every descendant (iterative
                    // DFS to avoid recursion on deep documents).
                    let mut stack: Vec<&Value> = vec![*n];
                    while let Some(v) = stack.pop() {
                        if next.len() >= MAX_DESCEND_NODES {
                            break;
                        }
                        next.push(v);
                        match v {
                            Value::Array(a) => stack.extend(a.iter()),
                            Value::Object(o) => stack.extend(o.values()),
                            _ => {}
                        }
                    }
                }
            }
        }
        if next.is_empty() {
            return Vec::new();
        }
        nodes = next;
    }

    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for n in nodes {
        if let Value::Object(o) = n {
            for k in o.keys() {
                if prefix.partial.is_empty() || k.starts_with(&prefix.partial) {
                    keys.insert(k.clone());
                }
            }
        }
    }
    keys.into_iter().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "user": { "id": 42, "email": "alice@co.com", "verified": true },
        "items": [
            { "name": "a", "price": 50 },
            { "name": "b", "price": 150 },
            { "name": "c", "price": 200 }
        ],
        "meta": { "tags": ["red", "blue"] }
    }"#;

    #[test]
    fn rejects_non_json() {
        assert!(matches!(run_jsonpath("not json", "$.foo"), JsonPathOutcome::NotJson));
        assert!(!is_json_container("not json"));
        assert!(is_json_container(r#"{"a":1}"#));
        assert!(is_json_container("[1,2,3]"));
        // Bare JSON scalars are valid JSON but have nothing to query —
        // the JSONPath bar must stay hidden for them.
        assert!(!is_json_container("123"));
        assert!(!is_json_container(r#""abc""#));
        assert!(!is_json_container("true"));
        assert!(!is_json_container("null"));
        // Leading whitespace must not defeat the fast-path container
        // check, and malformed "looks-like-JSON" still rejects.
        assert!(is_json_container(" \n\t {\"a\":1}"));
        assert!(is_json_container("  [1, 2]  "));
        assert!(!is_json_container("{not valid"));
        assert!(!is_json_container(""));
    }

    #[test]
    fn invalid_path_returns_message() {
        let outcome = run_jsonpath(SAMPLE, "$[unclosed");
        assert!(matches!(outcome, JsonPathOutcome::InvalidPath(_)));
    }

    #[test]
    fn scalar_extract() {
        // String scalar renders without surrounding quotes.
        assert_eq!(
            run_jsonpath(SAMPLE, "$.user.email"),
            JsonPathOutcome::Single("alice@co.com".to_string())
        );
        assert_eq!(
            run_jsonpath(SAMPLE, "$.user.id"),
            JsonPathOutcome::Single("42".to_string())
        );
        assert_eq!(
            run_jsonpath(SAMPLE, "$.user.verified"),
            JsonPathOutcome::Single("true".to_string())
        );
    }

    #[test]
    fn object_match_pretty_prints() {
        let outcome = run_jsonpath(SAMPLE, "$.user");
        match outcome {
            JsonPathOutcome::Single(s) => {
                assert!(s.contains("alice@co.com"));
                assert!(s.contains('\n')); // pretty-printed (multi-line)
            }
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn multi_match_wraps_in_array() {
        let outcome = run_jsonpath(SAMPLE, "$.items[*].name");
        match outcome {
            JsonPathOutcome::Multiple(s) => {
                assert!(s.contains("\"a\""));
                assert!(s.contains("\"b\""));
                assert!(s.contains("\"c\""));
            }
            other => panic!("expected Multiple, got {other:?}"),
        }
    }

    #[test]
    fn no_match_returns_no_match() {
        assert_eq!(run_jsonpath(SAMPLE, "$.does.not.exist"), JsonPathOutcome::NoMatch);
    }

    // ---- completion: prefix tokenizer ----

    fn prefix(text: &str) -> Option<JsonPathPrefix> {
        jsonpath_completion_prefix(text, text.len())
    }

    #[test]
    fn prefix_requires_dollar_and_separator() {
        assert_eq!(prefix(""), None);
        assert_eq!(prefix("foo"), None);
        assert_eq!(prefix("$"), None); // need a `.`/`[` first
        assert_eq!(prefix("$x"), None);
    }

    #[test]
    fn prefix_dot_segments() {
        assert_eq!(
            prefix("$."),
            Some(JsonPathPrefix {
                segments: vec![],
                partial: String::new(),
                replace_start: 2
            })
        );
        assert_eq!(
            prefix("$.us"),
            Some(JsonPathPrefix {
                segments: vec![],
                partial: "us".into(),
                replace_start: 2
            })
        );
        assert_eq!(
            prefix("$.user."),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("user".into())],
                partial: String::new(),
                replace_start: 7,
            })
        );
        assert_eq!(
            prefix("$.user.em"),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("user".into())],
                partial: "em".into(),
                replace_start: 7,
            })
        );
    }

    #[test]
    fn prefix_bracket_segments() {
        // Index then dot.
        assert_eq!(
            prefix("$.items[0].na"),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("items".into()), PathSeg::Index(0)],
                partial: "na".into(),
                replace_start: 11,
            })
        );
        // Wildcard then dot.
        assert_eq!(
            prefix("$.items[*]."),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("items".into()), PathSeg::Wildcard],
                partial: String::new(),
                replace_start: 11,
            })
        );
        // Quoted key, completed.
        assert_eq!(
            prefix(r#"$["user"].i"#),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("user".into())],
                partial: "i".into(),
                replace_start: 10,
            })
        );
        // Quoted key, still typing inside the quotes.
        assert_eq!(
            prefix(r#"$.user["em"#),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("user".into())],
                partial: "em".into(),
                replace_start: 8,
            })
        );
    }

    #[test]
    fn prefix_recursive_descent() {
        // `$..` — descend from root, no partial yet.
        assert_eq!(
            prefix("$.."),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Descend],
                partial: String::new(),
                replace_start: 3,
            })
        );
        // `$..ema` — descend from root, typing a member name.
        assert_eq!(
            prefix("$..ema"),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Descend],
                partial: "ema".into(),
                replace_start: 3,
            })
        );
        // `$.items..pr` — descend scoped under a key.
        assert_eq!(
            prefix("$.items..pr"),
            Some(JsonPathPrefix {
                segments: vec![PathSeg::Key("items".into()), PathSeg::Descend],
                partial: "pr".into(),
                replace_start: 9,
            })
        );
    }

    #[test]
    fn prefix_bails_on_unsupported_constructs() {
        assert_eq!(prefix("$.items[?(@.x>1)]."), None); // filter
        assert_eq!(prefix("$.items[0:2]."), None); // slice
        assert_eq!(prefix("$.a[1,2]."), None); // union
        assert_eq!(prefix("$.items[*"), None); // incomplete wildcard
        assert_eq!(prefix("$.a[12"), None); // incomplete index
    }

    // ---- completion: key resolver ----

    fn suggest(v: &Value, text: &str) -> Vec<String> {
        let p = prefix(text).expect("prefix");
        jsonpath_key_suggestions(v, &p)
    }

    #[test]
    fn suggest_top_level_keys() {
        let v: Value = serde_json::from_str(SAMPLE).expect("sample json");
        assert_eq!(suggest(&v, "$."), ["items", "meta", "user"]);
    }

    #[test]
    fn suggest_nested_keys_with_partial_filter() {
        let v: Value = serde_json::from_str(SAMPLE).expect("sample json");
        assert_eq!(suggest(&v, "$.user."), ["email", "id", "verified"]);
        assert_eq!(suggest(&v, "$.user.e"), ["email"]);
    }

    #[test]
    fn suggest_through_index_and_wildcard() {
        let v: Value = serde_json::from_str(SAMPLE).expect("sample json");
        assert_eq!(suggest(&v, "$.items[0]."), ["name", "price"]);
        // `[*]` unions keys across all array elements.
        assert_eq!(suggest(&v, "$.items[*]."), ["name", "price"]);
    }

    #[test]
    fn suggest_recursive_descent() {
        let v: Value = serde_json::from_str(SAMPLE).expect("sample json");
        // `$.items..` — every object key anywhere under `items`
        // (the element objects).
        assert_eq!(suggest(&v, "$.items.."), ["name", "price"]);
        assert_eq!(suggest(&v, "$.items..pr"), ["price"]);
        // `$..` — union of every object key in the whole document.
        assert_eq!(
            suggest(&v, "$.."),
            [
                "email", "id", "items", "meta", "name", "price", "tags", "user", "verified"
            ]
        );
        assert_eq!(suggest(&v, "$..ema"), ["email"]);
    }

    #[test]
    fn suggest_empty_when_path_misses_or_not_object() {
        let v: Value = serde_json::from_str(SAMPLE).expect("sample json");
        // `nope` doesn't exist.
        let p = prefix("$.nope.").expect("prefix");
        assert!(jsonpath_key_suggestions(&v, &p).is_empty());
        // `user.id` is a number — no keys to suggest after it.
        let p = prefix("$.user.id.").expect("prefix");
        assert!(jsonpath_key_suggestions(&v, &p).is_empty());
    }

    #[test]
    fn filter_expression() {
        let outcome = run_jsonpath(SAMPLE, "$.items[?(@.price > 100)].name");
        match outcome {
            JsonPathOutcome::Multiple(s) => {
                // Should match "b" and "c" only.
                assert!(s.contains("\"b\""));
                assert!(s.contains("\"c\""));
                assert!(!s.contains("\"a\""));
            }
            // serde_json_path may surface a single-match in array form too
            // depending on result count — accept both shapes here.
            JsonPathOutcome::Single(s) => assert!(s.contains('b') || s.contains('c')),
            other => panic!("unexpected outcome {other:?}"),
        }
    }
}

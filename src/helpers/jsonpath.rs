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

/// Cheap check: is this string parseable as JSON? Used to decide whether to
/// show the JSONPath input above the value editor at all.
pub fn is_json(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw).is_ok()
}

fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.to_string(),
        _ => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    }
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
        assert!(!is_json("not json"));
        assert!(is_json(r#"{"a":1}"#));
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

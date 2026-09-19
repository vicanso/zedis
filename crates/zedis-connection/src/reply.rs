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

//! Reading a [`redis::Value`] by hand.
//!
//! Most replies are typed by `FromRedisValue`. The ones that are not — `ACL
//! GETUSER`, `FUNCTION LIST`, `FT.INFO`, `LATENCY LATEST`, `HOTKEYS`, `CLUSTER
//! SLOT-STATS`, a module's `*.INFO` — are nested, loosely specified, and change
//! shape between RESP2 and RESP3 and between server versions, so they are
//! walked as `Value`s. Every file that did so had its own copy of these few
//! functions, and the copies had drifted: one accepted a `SimpleString` where
//! its twin did not, one knew `VerbatimString`, one cast a negative count where
//! another refused it. None of that was a decision. This is the one copy, and
//! it takes the *widest* reading each of them had — a new RESP3 shape is
//! taught here once.
//!
//! The one distinction that is a decision is [`text`] against [`text_lossy`].

use redis::{RedisError, Value};

/// A scalar as text, strictly: a bulk string that is not UTF-8 is `None`.
/// For field names, enum-like values and anything about to be parsed.
pub fn text(value: &Value) -> Option<String> {
    match value {
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Int(n) => Some(n.to_string()),
        Value::Double(n) => Some(format!("{n}")),
        _ => None,
    }
}

/// A string as text, whatever its bytes: invalid UTF-8 becomes U+FFFD instead
/// of `None`. For what the *user* named — a key in a hot-keys report, a
/// module's item — where dropping the row is worse than showing it
/// imperfectly. Strings only: a number is not a name.
pub fn text_lossy(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s.clone()),
        _ => None,
    }
}

/// An integer, from an `Int`, a `Double` (truncated — RESP3 reports some
/// counters that way) or a numeric string.
pub fn int(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => Some(*n),
        Value::Double(d) => Some(*d as i64),
        _ => text(value)?.trim().parse().ok(),
    }
}

/// A count: like [`int`], and `None` for a negative one rather than a wrapped
/// cast.
pub fn uint(value: &Value) -> Option<u64> {
    match value {
        Value::Int(n) => u64::try_from(*n).ok(),
        _ => text(value)?.trim().parse().ok(),
    }
}

/// A float, from a `Double`, an `Int` or a numeric string.
pub fn float(value: &Value) -> Option<f64> {
    match value {
        Value::Double(d) => Some(*d),
        Value::Int(n) => Some(*n as f64),
        _ => text(value)?.trim().parse().ok(),
    }
}

/// A list of strings, from an array or (RESP3) a set; entries that are not
/// text are skipped. `None` when the value is neither.
pub fn string_array(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) | Value::Set(items) => Some(items.iter().filter_map(text).collect()),
        _ => None,
    }
}

/// A field → value listing: RESP2's flat `[k, v, k, v, …]` array or RESP3's
/// map. `None` for anything else, and for an array with a key left over or a
/// key that is not text — a reply this cannot read is better reported than
/// half-read.
pub fn pairs(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len() / 2);
            for pair in items.chunks(2) {
                let [key, val] = pair else { return None };
                out.push((text(key)?, val.clone()));
            }
            Some(out)
        }
        Value::Map(items) => Some(items.iter().filter_map(|(k, v)| Some((text(k)?, v.clone()))).collect()),
        _ => None,
    }
}

/// Any value as one line for a details pane: scalars as themselves, nil as a
/// dash, anything nested in its debug form (it is shown, not parsed).
pub fn display(value: &Value) -> String {
    match value {
        Value::Int(i) => i.to_string(),
        Value::Double(d) => format!("{d}"),
        Value::Boolean(b) => b.to_string(),
        Value::Nil => "—".to_string(),
        other => text_lossy(other).unwrap_or_else(|| format!("{other:?}")),
    }
}

/// A module's `*.INFO` reply as rows for a details pane: field names with
/// their values [`display`]ed. Lenient where [`pairs`] is strict — a row that
/// cannot be read is skipped, because a partial listing is still worth showing.
pub fn display_pairs(value: &Value) -> Vec<(String, String)> {
    let named = |key: &Value, val: &Value| Some((text_lossy(key)?, display(val)));
    match value {
        Value::Map(items) => items.iter().filter_map(|(k, v)| named(k, v)).collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|chunk| named(chunk.first()?, chunk.get(1)?))
            .collect(),
        _ => Vec::new(),
    }
}

/// The server does not have the command at all — an older version, a proxy, a
/// managed cloud, a missing module — as opposed to the command failing. Every
/// wording seen so far: Redis (`unknown command`), proxies that capitalise it
/// (`ERR Unknown`), and managed services (`not available`).
pub fn is_unsupported(err: &RedisError) -> bool {
    let msg = err.to_string();
    ["unknown command", "ERR unknown", "ERR Unknown", "not available"]
        .iter()
        .any(|wording| msg.contains(wording))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }
    fn simple(s: &str) -> Value {
        Value::SimpleString(s.to_string())
    }
    fn verbatim(s: &str) -> Value {
        Value::VerbatimString {
            format: redis::VerbatimFormat::Text,
            text: s.to_string(),
        }
    }

    #[test]
    fn text_is_strict_about_bytes_and_text_lossy_is_not() {
        for v in [bulk("abc"), simple("abc"), verbatim("abc")] {
            assert_eq!(text(&v).as_deref(), Some("abc"));
            assert_eq!(text_lossy(&v).as_deref(), Some("abc"));
        }
        let binary = Value::BulkString(vec![b'k', 0xff, b'1']);
        assert_eq!(text(&binary), None, "not UTF-8: nothing to parse");
        assert_eq!(
            text_lossy(&binary).as_deref(),
            Some("k\u{fffd}1"),
            "but still a row to show"
        );
        // A number is text to `text` (it may be about to be parsed) and not a
        // *name* to `text_lossy`.
        assert_eq!(text(&Value::Int(7)).as_deref(), Some("7"));
        assert_eq!(text_lossy(&Value::Int(7)), None);
        assert_eq!(text(&Value::Nil), None);
    }

    #[test]
    fn numbers_are_read_from_every_shape_a_server_sends_them_in() {
        for v in [
            Value::Int(42),
            bulk("42"),
            bulk(" 42 "),
            simple("42"),
            verbatim("42"),
            Value::Double(42.9),
        ] {
            assert_eq!(int(&v), Some(42), "{v:?}");
        }
        for v in [Value::Int(42), bulk("42"), simple("42")] {
            assert_eq!(uint(&v), Some(42), "{v:?}");
        }
        // A count is never a wrapped negative.
        assert_eq!(uint(&Value::Int(-1)), None);
        assert_eq!(uint(&bulk("-1")), None);
        assert_eq!(int(&Value::Int(-1)), Some(-1));
        for v in [Value::Double(1.5), bulk("1.5"), simple(" 1.5")] {
            assert_eq!(float(&v), Some(1.5), "{v:?}");
        }
        assert_eq!(float(&Value::Int(2)), Some(2.0));
        for v in [bulk("x"), Value::Nil, Value::Array(vec![])] {
            assert_eq!((int(&v), uint(&v), float(&v)), (None, None, None), "{v:?}");
        }
    }

    #[test]
    fn pairs_read_both_protocols_and_refuse_what_they_cannot_read() {
        let flat = Value::Array(vec![bulk("a"), Value::Int(1), simple("b"), bulk("two")]);
        let map = Value::Map(vec![(bulk("a"), Value::Int(1)), (simple("b"), bulk("two"))]);
        let expected = vec![("a".to_string(), Value::Int(1)), ("b".to_string(), bulk("two"))];
        assert_eq!(pairs(&flat), Some(expected.clone()));
        assert_eq!(pairs(&map), Some(expected));
        // An odd array is not a listing, and neither is a scalar.
        assert_eq!(pairs(&Value::Array(vec![bulk("a")])), None);
        assert_eq!(pairs(&Value::Array(vec![Value::Nil, Value::Int(1)])), None);
        assert_eq!(pairs(&Value::Int(1)), None);

        assert_eq!(
            string_array(&Value::Array(vec![bulk("x"), Value::Nil, simple("y")])),
            Some(vec!["x".to_string(), "y".to_string()])
        );
        assert_eq!(string_array(&Value::Set(vec![bulk("x")])), Some(vec!["x".to_string()]));
        assert_eq!(string_array(&bulk("x")), None);
    }

    #[test]
    fn display_shows_everything_and_display_pairs_skips_what_it_cannot_name() {
        assert_eq!(display(&Value::Nil), "—");
        assert_eq!(display(&Value::Boolean(true)), "true");
        assert_eq!(display(&Value::Double(0.5)), "0.5");
        assert_eq!(display(&bulk("v")), "v");
        assert!(
            display(&Value::Array(vec![Value::Int(1)])).contains("1"),
            "nested: shown, not dropped"
        );

        let info = Value::Array(vec![
            bulk("Capacity"),
            Value::Int(100),
            Value::Nil,
            Value::Int(1),
            bulk("Size"),
        ]);
        assert_eq!(
            display_pairs(&info),
            vec![("Capacity".to_string(), "100".to_string())],
            "a row without a name, and a name without a value, are skipped"
        );
        assert_eq!(
            display_pairs(&Value::Map(vec![(simple("k"), Value::Nil)])),
            vec![("k".to_string(), "—".to_string())]
        );
        assert!(display_pairs(&Value::Int(1)).is_empty());
    }
}

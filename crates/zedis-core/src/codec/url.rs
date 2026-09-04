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

//! Percent-encoded text: a URL, a query string, or form data.
//!
//! Detection wants at least one `%XX` escape, only characters a URL may
//! carry, and a decoding that is valid UTF-8. Form data
//! (`a=1&b=hello%20world`, every segment a `key=value`) comes back as a
//! JSON object with `+` read as a space, the way a form encoder wrote it;
//! anything else — a full URL, a path — as the decoded string.

use serde_json::{Map, Value};

/// The decoded value: an object for form pairs, else a string.
pub fn decode(text: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(is_url_byte) || !has_escape(text) {
        return None;
    }
    if let Some(pairs) = form_pairs(text) {
        return Some(Value::Object(pairs));
    }
    percent_decode(text, false).map(Value::String)
}

fn is_form_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_.[]%+".contains(&b)
}

fn is_url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b)
}

/// Whether `text` holds a well-formed `%XX`.
fn has_escape(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

/// `key=value&key=value` with every segment carrying `=` and a key that
/// is a plain token (a URL's `scheme://host/path?k` is not one); a
/// repeated key collects into an array.
fn form_pairs(text: &str) -> Option<Map<String, Value>> {
    let mut map = Map::new();
    for segment in text.split('&') {
        let (key, value) = segment.split_once('=')?;
        if key.is_empty() || !key.bytes().all(is_form_key_byte) {
            return None;
        }
        let key = percent_decode(key, true)?;
        let value = Value::String(percent_decode(value, true)?);
        match map.get_mut(&key) {
            Some(Value::Array(items)) => items.push(value),
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value]);
            }
            None => {
                map.insert(key, value);
            }
        }
    }
    Some(map)
}

/// `%XX` → byte, `+` → space when `form`; a malformed escape or non-UTF-8
/// result is a refusal, not a lossy guess.
fn percent_decode(text: &str, form: bool) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16))?;
                let lo = bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16))?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b'+' if form => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn form_data_becomes_an_object_and_repeats_collect() {
        assert_eq!(
            decode("q=hello%20world&tag=a&tag=b%2Bc&empty="),
            Some(json!({ "q": "hello world", "tag": ["a", "b+c"], "empty": "" }))
        );
        assert_eq!(decode("name=Zh%C3%A9+Li"), Some(json!({ "name": "Zhé Li" })));
    }

    #[test]
    fn a_url_or_path_decodes_to_a_string() {
        assert_eq!(
            decode("https://x.io/search?q=caf%C3%A9&x=1"),
            Some(json!("https://x.io/search?q=café&x=1"))
        );
        assert_eq!(decode("/docs/%E4%B8%AD%E6%96%87"), Some(json!("/docs/中文")));
    }

    #[test]
    fn refuses_plain_text_and_bad_escapes() {
        assert_eq!(decode("a=1&b=2"), None, "no escape at all");
        assert_eq!(decode("hello world"), None, "a space is not a URL byte");
        assert_eq!(decode("100%25 sure"), None);
        assert_eq!(decode("%E4%B8"), None, "truncated UTF-8");
        assert_eq!(decode("a=%zz"), None, "malformed escape");
    }
}

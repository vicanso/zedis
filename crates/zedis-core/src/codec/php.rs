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

//! PHP `serialize()` output → JSON: `a:` arrays (a list when the keys run
//! `0..n`, else an object), `O:` objects (`"__class"` first, then the
//! properties with their visibility markers stripped), `C:` custom
//! serializations (`"__data"` kept raw), `r:` / `R:` references and the
//! scalars. Only a value that starts with a container, an object or a
//! string is considered — a bare `i:1;` is too ordinary to re-read.

use serde_json::{Map, Value};
use std::str::FromStr;

const MAX_DEPTH: usize = 64;

/// The decoded value, if `bytes` is one complete PHP serialization.
pub fn decode(bytes: &[u8]) -> Option<Value> {
    if !matches!(bytes.first(), Some(b'a' | b'O' | b'C' | b's')) {
        return None;
    }
    let mut parser = Parser {
        bytes,
        pos: 0,
        depth: 0,
    };
    let value = parser.value().ok()?;
    (parser.pos == bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

type Parsed<T> = Result<T, ()>;

impl Parser<'_> {
    fn next(&mut self) -> Parsed<u8> {
        let b = *self.bytes.get(self.pos).ok_or(())?;
        self.pos += 1;
        Ok(b)
    }

    fn expect(&mut self, want: u8) -> Parsed<()> {
        (self.next()? == want).then_some(()).ok_or(())
    }

    /// The ASCII run up to (and consuming) `delim`.
    fn until(&mut self, delim: u8) -> Parsed<&str> {
        let rest = &self.bytes[self.pos..];
        let end = rest.iter().position(|b| *b == delim).ok_or(())?;
        let text = std::str::from_utf8(&rest[..end]).map_err(|_| ())?;
        self.pos += end + 1;
        Ok(text)
    }

    fn number<T: FromStr>(&mut self, delim: u8) -> Parsed<T> {
        self.until(delim)?.parse().map_err(|_| ())
    }

    /// `LEN:"…"` — a length-prefixed, binary-safe string body.
    fn sized_string(&mut self) -> Parsed<String> {
        let len: usize = self.number(b':')?;
        self.expect(b'"')?;
        let end = self.pos.checked_add(len).ok_or(())?;
        let body = self.bytes.get(self.pos..end).ok_or(())?;
        self.pos = end;
        self.expect(b'"')?;
        Ok(String::from_utf8_lossy(body).into_owned())
    }

    /// `COUNT:{ key value … }` for arrays and objects.
    fn entries(&mut self) -> Parsed<Vec<(String, Value)>> {
        let count: usize = self.number(b':')?;
        self.expect(b'{')?;
        let mut entries = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let key = match self.value()? {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                _ => return Err(()),
            };
            entries.push((key, self.value()?));
        }
        self.expect(b'}')?;
        Ok(entries)
    }

    fn value(&mut self) -> Parsed<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(());
        }
        let value = match self.next()? {
            b'N' => {
                self.expect(b';')?;
                Value::Null
            }
            b'b' => {
                self.expect(b':')?;
                match self.until(b';')? {
                    "0" => Value::Bool(false),
                    "1" => Value::Bool(true),
                    _ => return Err(()),
                }
            }
            b'i' => {
                self.expect(b':')?;
                let text = self.until(b';')?;
                match text.parse::<i64>() {
                    Ok(n) => Value::from(n),
                    // Out of i64 range: PHP would have written a float, but
                    // keep whatever digits are there.
                    Err(_) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit() || b == b'-') => {
                        Value::String(text.to_string())
                    }
                    Err(_) => return Err(()),
                }
            }
            b'd' => {
                self.expect(b':')?;
                let text = self.until(b';')?;
                match text {
                    "INF" | "-INF" | "NAN" => Value::String(text.to_string()),
                    _ => super::float(text.parse::<f64>().map_err(|_| ())?),
                }
            }
            b's' => {
                self.expect(b':')?;
                let s = self.sized_string()?;
                self.expect(b';')?;
                Value::String(s)
            }
            b'a' => {
                self.expect(b':')?;
                array_value(self.entries()?)
            }
            b'O' => {
                self.expect(b':')?;
                let class = self.sized_string()?;
                self.expect(b':')?;
                let mut object = Map::new();
                object.insert("__class".to_string(), Value::String(class));
                for (name, value) in self.entries()? {
                    object.insert(property_name(&name), value);
                }
                Value::Object(object)
            }
            b'C' => {
                self.expect(b':')?;
                let class = self.sized_string()?;
                self.expect(b':')?;
                let len: usize = self.number(b':')?;
                self.expect(b'{')?;
                let end = self.pos.checked_add(len).ok_or(())?;
                let data = self.bytes.get(self.pos..end).ok_or(())?;
                self.pos = end;
                self.expect(b'}')?;
                let mut object = Map::new();
                object.insert("__class".to_string(), Value::String(class));
                object.insert(
                    "__data".to_string(),
                    Value::String(String::from_utf8_lossy(data).into_owned()),
                );
                Value::Object(object)
            }
            b'r' | b'R' => {
                self.expect(b':')?;
                let target: u64 = self.number(b';')?;
                super::tagged("__ref", Value::from(target))
            }
            _ => return Err(()),
        };
        self.depth -= 1;
        Ok(value)
    }
}

/// A PHP array is a list when its keys are exactly `0..n` in order.
fn array_value(entries: Vec<(String, Value)>) -> Value {
    let is_list = entries
        .iter()
        .enumerate()
        .all(|(ix, (key, _))| key.parse::<usize>() == Ok(ix));
    if is_list {
        Value::Array(entries.into_iter().map(|(_, v)| v).collect())
    } else {
        Value::Object(entries.into_iter().collect())
    }
}

/// `\0*\0name` (protected) and `\0Class\0name` (private) → `name`.
fn property_name(raw: &str) -> String {
    raw.rsplit('\0').next().unwrap_or(raw).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arrays_objects_and_scalars_round_trip_to_json() {
        let list = br#"a:3:{i:0;s:1:"a";i:1;i:2;i:2;b:1;}"#;
        assert_eq!(decode(list), Some(json!(["a", 2, true])));

        let map = br#"a:2:{s:4:"name";s:5:"zedis";s:5:"stars";d:4.5;}"#;
        assert_eq!(decode(map), Some(json!({ "name": "zedis", "stars": 4.5 })));

        let object = b"O:4:\"User\":3:{s:2:\"id\";i:7;s:7:\"\0*\0role\";s:5:\"admin\";s:11:\"\0User\0token\";N;}";
        assert_eq!(
            decode(object),
            Some(json!({ "__class": "User", "id": 7, "role": "admin", "token": null }))
        );

        assert_eq!(
            decode(br#"s:5:"he"lo";"#),
            Some(json!("he\"lo")),
            "length-delimited, quotes inside are data"
        );
    }

    #[test]
    fn custom_serializations_and_references_are_kept_visible() {
        let custom = br#"C:11:"ArrayObject":21:{x:i:0;a:0:{};m:a:0:{}}"#;
        assert_eq!(
            decode(custom),
            Some(json!({ "__class": "ArrayObject", "__data": "x:i:0;a:0:{};m:a:0:{}" }))
        );
        let refs = br#"a:2:{i:0;a:0:{}i:1;R:2;}"#;
        assert_eq!(decode(refs), Some(json!([[], { "__ref": 2 }])));
    }

    #[test]
    fn refuses_scalars_partial_input_and_text_that_only_starts_like_php() {
        assert_eq!(decode(b"i:1;"), None, "bare scalar");
        assert_eq!(decode(b"a:1:{i:0;s:1:\"a\";}trailing"), None, "must consume everything");
        assert_eq!(decode(b"a:1:{i:0;s:9:\"a\";}"), None, "length past the end");
        assert_eq!(decode(b"session:abc"), None);
        assert_eq!(decode(b"order:42:paid"), None);
    }
}

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

//! A BSON document → JSON in MongoDB's extended-JSON spelling for the
//! types plain JSON lacks (`{"$oid": …}`, `{"$date": …}`, `{"$binary": …}`).
//! A document announces its own byte length and ends in a NUL, and every
//! element inside is length-checked, so a decode that consumes exactly
//! the value is the detection.

use super::{float, hex, rfc3339_millis, tagged};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

const MAX_DEPTH: usize = 64;

/// Whether `bytes` has a BSON document's frame: its leading length is the
/// whole value and the last byte is the terminator.
pub fn looks_like(bytes: &[u8]) -> bool {
    bytes.len() >= 5 && document_len(bytes) == Some(bytes.len()) && bytes[bytes.len() - 1] == 0
}

/// The document as JSON, if every element parses and nothing is left over.
pub fn decode(bytes: &[u8]) -> Option<Value> {
    if !looks_like(bytes) {
        return None;
    }
    let mut reader = Reader {
        bytes,
        pos: 0,
        depth: 0,
    };
    let document = reader.document().ok()?;
    (reader.pos == bytes.len()).then_some(Value::Object(document))
}

fn document_len(bytes: &[u8]) -> Option<usize> {
    let len = i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    usize::try_from(len).ok().filter(|len| *len >= 5)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

type Parsed<T> = Result<T, ()>;

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Parsed<&[u8]> {
        let end = self.pos.checked_add(n).ok_or(())?;
        let slice = self.bytes.get(self.pos..end).ok_or(())?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Parsed<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Parsed<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }

    fn i64(&mut self) -> Parsed<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }

    fn f64(&mut self) -> Parsed<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }

    /// NUL-terminated UTF-8.
    fn cstring(&mut self) -> Parsed<String> {
        let rest = &self.bytes[self.pos..];
        let end = rest.iter().position(|b| *b == 0).ok_or(())?;
        let text = std::str::from_utf8(&rest[..end]).map_err(|_| ())?.to_string();
        self.pos += end + 1;
        Ok(text)
    }

    /// Length-prefixed UTF-8 (the length counts the trailing NUL).
    fn string(&mut self) -> Parsed<String> {
        let len = usize::try_from(self.i32()?).map_err(|_| ())?;
        if len == 0 {
            return Err(());
        }
        let body = self.take(len)?;
        if body[len - 1] != 0 {
            return Err(());
        }
        std::str::from_utf8(&body[..len - 1])
            .map(str::to_string)
            .map_err(|_| ())
    }

    fn document(&mut self) -> Parsed<Map<String, Value>> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(());
        }
        let start = self.pos;
        let len = usize::try_from(self.i32()?).map_err(|_| ())?;
        let end = start.checked_add(len).ok_or(())?;
        if len < 5 || end > self.bytes.len() {
            return Err(());
        }
        let mut map = Map::new();
        loop {
            let kind = self.u8()?;
            if kind == 0 {
                break;
            }
            let name = self.cstring()?;
            let value = self.element(kind)?;
            map.insert(name, value);
        }
        if self.pos != end {
            return Err(());
        }
        self.depth -= 1;
        Ok(map)
    }

    fn element(&mut self, kind: u8) -> Parsed<Value> {
        Ok(match kind {
            0x01 => float(self.f64()?),
            0x02 => Value::String(self.string()?),
            0x03 => Value::Object(self.document()?),
            0x04 => array_value(self.document()?),
            0x05 => {
                let len = usize::try_from(self.i32()?).map_err(|_| ())?;
                let subtype = self.u8()?;
                let data = self.take(len)?;
                if subtype == 0x04 && data.len() == 16 {
                    let h = hex(data);
                    tagged(
                        "$uuid",
                        json!(format!(
                            "{}-{}-{}-{}-{}",
                            &h[..8],
                            &h[8..12],
                            &h[12..16],
                            &h[16..20],
                            &h[20..]
                        )),
                    )
                } else {
                    tagged(
                        "$binary",
                        json!({ "base64": STANDARD.encode(data), "subType": format!("{subtype:02x}") }),
                    )
                }
            }
            0x06 => tagged("$undefined", Value::Bool(true)),
            0x07 => tagged("$oid", Value::String(hex(self.take(12)?))),
            0x08 => Value::Bool(self.u8()? != 0),
            0x09 => {
                let millis = self.i64()?;
                tagged(
                    "$date",
                    rfc3339_millis(millis)
                        .map(Value::String)
                        .unwrap_or_else(|| json!(millis)),
                )
            }
            0x0A => Value::Null,
            0x0B => {
                let pattern = self.cstring()?;
                let options = self.cstring()?;
                tagged("$regularExpression", json!({ "pattern": pattern, "options": options }))
            }
            0x0C => {
                let name = self.string()?;
                let id = hex(self.take(12)?);
                tagged("$dbPointer", json!({ "$ref": name, "$id": { "$oid": id } }))
            }
            0x0D => tagged("$code", Value::String(self.string()?)),
            0x0E => tagged("$symbol", Value::String(self.string()?)),
            0x0F => {
                let _total = self.i32()?;
                let code = self.string()?;
                let scope = self.document()?;
                json!({ "$code": code, "$scope": scope })
            }
            0x10 => Value::from(self.i32()?),
            0x11 => {
                let raw = self.i64()? as u64;
                tagged("$timestamp", json!({ "t": raw >> 32, "i": raw & 0xffff_ffff }))
            }
            0x12 => Value::from(self.i64()?),
            0x13 => tagged("$numberDecimalBytes", Value::String(hex(self.take(16)?))),
            0x7F => tagged("$maxKey", Value::from(1)),
            0xFF => tagged("$minKey", Value::from(1)),
            _ => return Err(()),
        })
    }
}

/// A BSON array is a document keyed `"0"`, `"1"`, …; anything else stays
/// an object so nothing is silently reordered.
fn array_value(map: Map<String, Value>) -> Value {
    let sequential = map.keys().enumerate().all(|(ix, key)| key.parse::<usize>() == Ok(ix));
    if sequential {
        Value::Array(map.into_iter().map(|(_, v)| v).collect())
    } else {
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-assembled documents, so the test pins the wire format rather
    /// than an encoder's opinion of it.
    fn doc(elements: &[u8]) -> Vec<u8> {
        let len = (elements.len() + 5) as i32;
        let mut out = len.to_le_bytes().to_vec();
        out.extend_from_slice(elements);
        out.push(0);
        out
    }
    fn string(name: &str, value: &str) -> Vec<u8> {
        let mut out = vec![0x02];
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&((value.len() + 1) as i32).to_le_bytes());
        out.extend_from_slice(value.as_bytes());
        out.push(0);
        out
    }
    fn int32(name: &str, value: i32) -> Vec<u8> {
        let mut out = vec![0x10];
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    #[test]
    fn decodes_scalars_nested_documents_and_arrays() {
        let inner = doc(&[int32("0", 7), int32("1", 8)].concat());
        let mut nested = vec![0x04];
        nested.extend_from_slice(b"tags\0");
        nested.extend_from_slice(&inner);
        let mut oid = vec![0x07];
        oid.extend_from_slice(b"_id\0");
        oid.extend_from_slice(&[0x65, 0x4a, 0x1c, 0x8e, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
        let mut date = vec![0x09];
        date.extend_from_slice(b"at\0");
        date.extend_from_slice(&0i64.to_le_bytes());
        let bytes = doc(&[oid, string("name", "zedis"), nested, date].concat());
        assert!(looks_like(&bytes));
        assert_eq!(
            decode(&bytes),
            Some(json!({
                "_id": { "$oid": "654a1c8e123456789abcdef0" },
                "name": "zedis",
                "tags": [7, 8],
                "at": { "$date": "1970-01-01T00:00:00Z" },
            }))
        );
    }

    #[test]
    fn refuses_a_frame_that_lies_about_its_length_or_content() {
        let good = doc(&int32("n", 1));
        let mut long = good.clone();
        long.push(0);
        assert_eq!(decode(&long), None, "trailing byte");
        let mut short = good.clone();
        short.truncate(short.len() - 1);
        assert_eq!(decode(&short), None, "no terminator");
        let mut unknown = doc(&int32("n", 1));
        unknown[4] = 0x99;
        assert_eq!(decode(&unknown), None, "unknown element type");
        assert!(!looks_like(b"hello"));
    }
}

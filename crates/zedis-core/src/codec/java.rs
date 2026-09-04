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

//! A Java Object Serialization stream (`AC ED 00 05`, what
//! `ObjectOutputStream` and Spring's `JdkSerializationRedisSerializer`
//! write) → JSON, without any class on hand: the class descriptors in the
//! stream say which fields to read and how wide they are. An object is
//! `{"@class": …, field: value, …}`; what a `writeObject` method appended
//! beyond the fields is kept under `"@annotation"` — except for the
//! collections everyone stores, whose custom formats are read into
//! `"entries"` / `"items"` (`HashMap`, `LinkedHashMap`, `Hashtable`,
//! `TreeMap`, `ArrayList`, `LinkedList`, `HashSet`, `LinkedHashSet`,
//! `TreeSet`, `ArrayDeque`), boxed primitives into their value and
//! `java.util.Date` into a timestamp. `Externalizable` classes and
//! dynamic proxies carry class-defined bytes and stop the parse; whatever
//! was read before is shown with the error.

use super::{float, hex, rfc3339_millis, tagged};
use serde_json::{Map, Value, json};
use std::rc::Rc;

const TC_NULL: u8 = 0x70;
const TC_REFERENCE: u8 = 0x71;
const TC_CLASSDESC: u8 = 0x72;
const TC_OBJECT: u8 = 0x73;
const TC_STRING: u8 = 0x74;
const TC_ARRAY: u8 = 0x75;
const TC_CLASS: u8 = 0x76;
const TC_BLOCKDATA: u8 = 0x77;
const TC_ENDBLOCKDATA: u8 = 0x78;
const TC_RESET: u8 = 0x79;
const TC_BLOCKDATALONG: u8 = 0x7A;
const TC_EXCEPTION: u8 = 0x7B;
const TC_LONGSTRING: u8 = 0x7C;
const TC_PROXYCLASSDESC: u8 = 0x7D;
const TC_ENUM: u8 = 0x7E;
const BASE_WIRE_HANDLE: u32 = 0x7E_0000;

const SC_WRITE_METHOD: u8 = 0x01;
const SC_SERIALIZABLE: u8 = 0x02;
const SC_EXTERNALIZABLE: u8 = 0x04;
const SC_BLOCK_DATA: u8 = 0x08;

const MAX_DEPTH: usize = 64;

/// The stream magic and version.
pub fn looks_like(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xAC, 0xED, 0x00, 0x05])
}

/// The stream's contents as JSON — one value, or an array when the stream
/// holds several. A parse that stops early keeps what it read under
/// `"@parsed"` with the reason under `"@error"`; nothing readable at all
/// is `None`.
pub fn decode(bytes: &[u8]) -> Option<Value> {
    if !looks_like(bytes) {
        return None;
    }
    let mut reader = Reader {
        bytes,
        pos: 4,
        handles: Vec::new(),
        depth: 0,
    };
    let mut contents = Vec::new();
    let mut error = None;
    while reader.pos < bytes.len() {
        match reader.content() {
            Ok(value) => contents.push(value),
            Err(reason) => {
                error = Some(format!("{reason} at byte {}", reader.pos));
                break;
            }
        }
    }
    if contents.is_empty() {
        return None;
    }
    let value = if contents.len() == 1 {
        contents.remove(0)
    } else {
        Value::Array(contents)
    };
    Some(match error {
        Some(error) => json!({ "@parsed": value, "@error": error }),
        None => value,
    })
}

struct Field {
    code: u8,
    name: String,
}

struct ClassDesc {
    name: String,
    flags: u8,
    fields: Vec<Field>,
    super_desc: Option<Rc<ClassDesc>>,
}

enum Handle {
    Class(Rc<ClassDesc>),
    Value(Value),
}

/// Something a `writeObject` appended after the fields.
enum Annotation {
    Block(Vec<u8>),
    Object(Value),
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    handles: Vec<Handle>,
    depth: usize,
}

type Parsed<T> = Result<T, &'static str>;

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Parsed<&[u8]> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let slice = self.bytes.get(self.pos..end).ok_or("truncated stream")?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Parsed<u8> {
        Ok(self.take(1)?[0])
    }
    fn peek(&self) -> Parsed<u8> {
        self.bytes.get(self.pos).copied().ok_or("truncated stream")
    }
    fn u16(&mut self) -> Parsed<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| "u16")?))
    }
    fn i32(&mut self) -> Parsed<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().map_err(|_| "i32")?))
    }
    fn i64(&mut self) -> Parsed<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().map_err(|_| "i64")?))
    }
    /// `(short)length` + modified UTF-8.
    fn utf(&mut self) -> Parsed<String> {
        let len = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
    fn long_utf(&mut self) -> Parsed<String> {
        let len = usize::try_from(self.i64()?).map_err(|_| "negative length")?;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn new_handle(&mut self, handle: Handle) -> usize {
        self.handles.push(handle);
        self.handles.len() - 1
    }

    fn reference(&mut self) -> Parsed<&Handle> {
        let raw = self.i32()? as u32;
        let index = raw.checked_sub(BASE_WIRE_HANDLE).ok_or("handle below base")? as usize;
        self.handles.get(index).ok_or("dangling handle")
    }

    /// One top-level item: an object, or a block of raw data.
    fn content(&mut self) -> Parsed<Value> {
        match self.peek()? {
            TC_BLOCKDATA | TC_BLOCKDATALONG => Ok(tagged("@blockdata", Value::String(hex(&self.block()?)))),
            _ => self.object(),
        }
    }

    fn block(&mut self) -> Parsed<Vec<u8>> {
        let len = match self.u8()? {
            TC_BLOCKDATA => self.u8()? as usize,
            TC_BLOCKDATALONG => usize::try_from(self.i32()?).map_err(|_| "negative block length")?,
            _ => return Err("expected block data"),
        };
        Ok(self.take(len)?.to_vec())
    }

    fn object(&mut self) -> Parsed<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("nesting too deep");
        }
        let value = match self.u8()? {
            TC_NULL => Value::Null,
            TC_REFERENCE => match self.reference()? {
                Handle::Value(value) => value.clone(),
                Handle::Class(desc) => tagged("@classref", Value::String(desc.name.clone())),
            },
            TC_STRING => {
                let index = self.new_handle(Handle::Value(Value::Null));
                let text = Value::String(self.utf()?);
                self.handles[index] = Handle::Value(text.clone());
                text
            }
            TC_LONGSTRING => {
                let index = self.new_handle(Handle::Value(Value::Null));
                let text = Value::String(self.long_utf()?);
                self.handles[index] = Handle::Value(text.clone());
                text
            }
            TC_CLASS => {
                let desc = self.class_desc()?.ok_or("class without descriptor")?;
                let value = tagged("@class_object", Value::String(desc.name.clone()));
                self.new_handle(Handle::Value(value.clone()));
                value
            }
            TC_ENUM => {
                let desc = self.class_desc()?.ok_or("enum without descriptor")?;
                let index = self.new_handle(Handle::Value(Value::Null));
                let name = self.object()?;
                let value = json!({ "@enum": desc.name, "name": name });
                self.handles[index] = Handle::Value(value.clone());
                value
            }
            TC_ARRAY => {
                let desc = self.class_desc()?.ok_or("array without descriptor")?;
                let index = self.new_handle(Handle::Value(Value::Null));
                let len = usize::try_from(self.i32()?).map_err(|_| "negative array length")?;
                let value = self.array(&desc.name, len)?;
                self.handles[index] = Handle::Value(value.clone());
                value
            }
            TC_OBJECT => {
                let desc = self.class_desc()?.ok_or("object without descriptor")?;
                let index = self.new_handle(Handle::Value(Value::Null));
                let value = self.class_data(&desc)?;
                self.handles[index] = Handle::Value(value.clone());
                value
            }
            TC_CLASSDESC | TC_PROXYCLASSDESC => {
                self.pos -= 1;
                let desc = self.class_desc()?.ok_or("descriptor")?;
                tagged("@classdesc", Value::String(desc.name.clone()))
            }
            TC_RESET => {
                self.handles.clear();
                Value::Null
            }
            TC_BLOCKDATA | TC_BLOCKDATALONG => {
                self.pos -= 1;
                tagged("@blockdata", Value::String(hex(&self.block()?)))
            }
            TC_EXCEPTION => return Err("exception recorded in the stream"),
            _ => return Err("unknown type code"),
        };
        self.depth -= 1;
        Ok(value)
    }

    fn class_desc(&mut self) -> Parsed<Option<Rc<ClassDesc>>> {
        match self.u8()? {
            TC_NULL => Ok(None),
            TC_REFERENCE => match self.reference()? {
                Handle::Class(desc) => Ok(Some(desc.clone())),
                Handle::Value(_) => Err("handle is not a class"),
            },
            TC_CLASSDESC => {
                let name = self.utf()?;
                let _serial_version_uid = self.i64()?;
                let index = self.new_handle(Handle::Value(Value::Null));
                let flags = self.u8()?;
                let count = self.u16()? as usize;
                let mut fields = Vec::with_capacity(count.min(256));
                for _ in 0..count {
                    let code = self.u8()?;
                    let name = self.utf()?;
                    if code == b'L' || code == b'[' {
                        // The field's class name, a string object (or a
                        // back-reference to one).
                        self.object()?;
                    }
                    fields.push(Field { code, name });
                }
                self.annotations()?;
                let super_desc = self.class_desc()?;
                let desc = Rc::new(ClassDesc {
                    name,
                    flags,
                    fields,
                    super_desc,
                });
                self.handles[index] = Handle::Class(desc.clone());
                Ok(Some(desc))
            }
            TC_PROXYCLASSDESC => Err("dynamic proxy class"),
            _ => Err("expected a class descriptor"),
        }
    }

    /// Contents up to `TC_ENDBLOCKDATA`.
    fn annotations(&mut self) -> Parsed<Vec<Annotation>> {
        let mut out = Vec::new();
        loop {
            match self.peek()? {
                TC_ENDBLOCKDATA => {
                    self.pos += 1;
                    return Ok(out);
                }
                TC_BLOCKDATA | TC_BLOCKDATALONG => out.push(Annotation::Block(self.block()?)),
                _ => out.push(Annotation::Object(self.object()?)),
            }
        }
    }

    /// Field values class by class from the topmost superclass down, each
    /// followed by what its `writeObject` appended.
    fn class_data(&mut self, desc: &Rc<ClassDesc>) -> Parsed<Value> {
        let mut chain = Vec::new();
        let mut current = Some(desc.clone());
        while let Some(class) = current {
            current = class.super_desc.clone();
            chain.push(class);
        }
        chain.reverse();
        let mut fields = Map::new();
        let mut annotations = Vec::new();
        for class in &chain {
            if class.flags & SC_SERIALIZABLE != 0 {
                for field in &class.fields {
                    let value = self.field_value(field.code)?;
                    fields.insert(field.name.clone(), value);
                }
                if class.flags & SC_WRITE_METHOD != 0 {
                    annotations.extend(self.annotations()?);
                }
            } else if class.flags & SC_EXTERNALIZABLE != 0 {
                if class.flags & SC_BLOCK_DATA != 0 {
                    annotations.extend(self.annotations()?);
                } else {
                    return Err("externalizable class without block data");
                }
            }
        }
        Ok(simplify(&desc.name, fields, annotations))
    }

    fn field_value(&mut self, code: u8) -> Parsed<Value> {
        Ok(match code {
            b'B' => Value::from(self.u8()? as i8),
            b'C' => char::from_u32(self.u16()? as u32)
                .map(|c| Value::String(c.to_string()))
                .unwrap_or(Value::Null),
            b'D' => {
                let bits = self.i64()? as u64;
                float(f64::from_bits(bits))
            }
            b'F' => {
                let bits = self.i32()? as u32;
                float(f32::from_bits(bits) as f64)
            }
            b'I' => Value::from(self.i32()?),
            b'J' => Value::from(self.i64()?),
            b'S' => Value::from(self.u16()? as i16),
            b'Z' => Value::Bool(self.u8()? != 0),
            b'L' | b'[' => self.object()?,
            _ => return Err("unknown field type"),
        })
    }

    /// Elements by the array class's component type: `[I` is ints, `[B`
    /// is shown as hex, `[C` as text, `[L…;` / `[[…` as objects.
    fn array(&mut self, class: &str, len: usize) -> Parsed<Value> {
        let component = class.as_bytes().get(1).copied().ok_or("array class name")?;
        match component {
            b'B' => Ok(tagged("@bytes", Value::String(hex(self.take(len)?)))),
            b'C' => {
                let mut text = String::with_capacity(len);
                for _ in 0..len {
                    text.push(char::from_u32(self.u16()? as u32).unwrap_or('\u{fffd}'));
                }
                Ok(Value::String(text))
            }
            _ => {
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.field_value(component)?);
                }
                Ok(Value::Array(items))
            }
        }
    }
}

/// The readable shape of a known class, else the generic object.
fn simplify(class: &str, mut fields: Map<String, Value>, annotations: Vec<Annotation>) -> Value {
    let objects = || -> Vec<Value> {
        annotations
            .iter()
            .filter_map(|a| match a {
                Annotation::Object(v) => Some(v.clone()),
                Annotation::Block(_) => None,
            })
            .collect()
    };
    match class {
        "java.lang.Integer"
        | "java.lang.Long"
        | "java.lang.Short"
        | "java.lang.Byte"
        | "java.lang.Double"
        | "java.lang.Float"
        | "java.lang.Boolean"
        | "java.lang.Character" => {
            if let Some(value) = fields.remove("value") {
                return value;
            }
        }
        "java.util.HashMap"
        | "java.util.LinkedHashMap"
        | "java.util.Hashtable"
        | "java.util.TreeMap"
        | "java.util.concurrent.ConcurrentHashMap" => {
            let items = objects();
            let entries = if items.len().is_multiple_of(2) {
                map_entries(items)
            } else {
                Value::Array(items)
            };
            return json!({ "@class": class, "entries": entries });
        }
        "java.util.ArrayList"
        | "java.util.LinkedList"
        | "java.util.HashSet"
        | "java.util.LinkedHashSet"
        | "java.util.TreeSet"
        | "java.util.ArrayDeque"
        | "java.util.Vector" => {
            return json!({ "@class": class, "items": Value::Array(objects()) });
        }
        "java.util.Date" | "java.sql.Timestamp" | "java.sql.Date" => {
            if let Some(Annotation::Block(bytes)) = annotations.first()
                && bytes.len() >= 8
                && let Ok(raw) = bytes[..8].try_into()
            {
                let millis = i64::from_be_bytes(raw);
                let mut out = Map::new();
                out.insert("@class".into(), Value::String(class.into()));
                out.insert("time".into(), Value::from(millis));
                if let Some(iso) = rfc3339_millis(millis) {
                    out.insert("iso".into(), Value::String(iso));
                }
                return Value::Object(out);
            }
        }
        _ => {}
    }
    let mut out = Map::new();
    out.insert("@class".into(), Value::String(class.to_string()));
    out.extend(fields);
    if !annotations.is_empty() {
        let extra: Vec<Value> = annotations
            .into_iter()
            .map(|a| match a {
                Annotation::Block(bytes) => tagged("@blockdata", Value::String(hex(&bytes))),
                Annotation::Object(value) => value,
            })
            .collect();
        out.insert("@annotation".into(), Value::Array(extra));
    }
    Value::Object(out)
}

/// Alternating key / value objects as a JSON object when every key is a
/// string, else as `[[key, value], …]`.
fn map_entries(items: Vec<Value>) -> Value {
    let all_string_keys = items.iter().step_by(2).all(Value::is_string);
    let mut pairs = items.into_iter();
    if all_string_keys {
        let mut map = Map::new();
        while let (Some(Value::String(k)), Some(v)) = (pairs.next(), pairs.next()) {
            map.insert(k, v);
        }
        Value::Object(map)
    } else {
        let mut list = Vec::new();
        while let (Some(k), Some(v)) = (pairs.next(), pairs.next()) {
            list.push(Value::Array(vec![k, v]));
        }
        Value::Array(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream builder for the tests — the wire format spelled out, so a
    /// mistake in the reader shows up against bytes, not against an
    /// encoder that shares its assumptions.
    struct Stream(Vec<u8>);
    impl Stream {
        fn new() -> Self {
            Self(vec![0xAC, 0xED, 0x00, 0x05])
        }
        fn utf(&mut self, s: &str) -> &mut Self {
            self.0.extend_from_slice(&(s.len() as u16).to_be_bytes());
            self.0.extend_from_slice(s.as_bytes());
            self
        }
        fn bytes(&mut self, b: &[u8]) -> &mut Self {
            self.0.extend_from_slice(b);
            self
        }
        /// TC_CLASSDESC name uid flags fields… (annotation end, super null)
        fn class(&mut self, name: &str, flags: u8, fields: &[(u8, &str, Option<&str>)]) -> &mut Self {
            self.bytes(&[TC_CLASSDESC]).utf(name).bytes(&[0; 8]).bytes(&[flags]);
            self.0.extend_from_slice(&(fields.len() as u16).to_be_bytes());
            for (code, name, class) in fields {
                self.bytes(&[*code]).utf(name);
                if let Some(class) = class {
                    self.bytes(&[TC_STRING]).utf(class);
                }
            }
            self.bytes(&[TC_ENDBLOCKDATA, TC_NULL])
        }
    }

    #[test]
    fn a_plain_serializable_object_with_primitive_and_string_fields() {
        let mut s = Stream::new();
        s.bytes(&[TC_OBJECT])
            .class(
                "com.acme.User",
                SC_SERIALIZABLE,
                &[
                    (b'I', "id", None),
                    (b'Z', "active", None),
                    (b'L', "name", Some("Ljava/lang/String;")),
                ],
            )
            .bytes(&[0, 0, 0, 42, 1, TC_STRING])
            .utf("zedis");
        assert_eq!(
            decode(&s.0),
            Some(json!({ "@class": "com.acme.User", "id": 42, "active": true, "name": "zedis" }))
        );
    }

    #[test]
    fn a_hashmap_reads_its_entries_from_the_write_method_data() {
        // HashMap: fields loadFactor (F), threshold (I); writeObject adds a
        // block (capacity, size) then the key/value objects.
        let mut s = Stream::new();
        s.bytes(&[TC_OBJECT])
            .class(
                "java.util.HashMap",
                SC_SERIALIZABLE | SC_WRITE_METHOD,
                &[(b'F', "loadFactor", None), (b'I', "threshold", None)],
            )
            .bytes(&[0x3f, 0x40, 0, 0, 0, 0, 0, 12]) // 0.75f, 12
            .bytes(&[TC_BLOCKDATA, 8, 0, 0, 0, 16, 0, 0, 0, 2])
            .bytes(&[TC_STRING])
            .utf("count")
            .bytes(&[TC_OBJECT])
            .class("java.lang.Integer", SC_SERIALIZABLE, &[(b'I', "value", None)])
            .bytes(&[0, 0, 0, 7])
            .bytes(&[TC_STRING])
            .utf("name")
            .bytes(&[TC_REFERENCE])
            // Handles so far: 0 the map's descriptor, 1 the map, 2 "count".
            .bytes(&(BASE_WIRE_HANDLE + 2).to_be_bytes())
            .bytes(&[TC_ENDBLOCKDATA]);
        assert_eq!(
            decode(&s.0),
            Some(json!({ "@class": "java.util.HashMap", "entries": { "count": 7, "name": "count" } }))
        );
    }

    #[test]
    fn arrays_enums_and_a_partial_stream() {
        let mut s = Stream::new();
        s.bytes(&[TC_ARRAY])
            .class("[I", SC_SERIALIZABLE, &[])
            .bytes(&[0, 0, 0, 2, 0, 0, 0, 5, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(decode(&s.0), Some(json!([5, -1])));

        let mut s = Stream::new();
        s.bytes(&[TC_ARRAY])
            .class("[B", SC_SERIALIZABLE, &[])
            .bytes(&[0, 0, 0, 3, 0xde, 0xad, 0x01]);
        assert_eq!(decode(&s.0), Some(json!({ "@bytes": "dead01" })));

        let mut s = Stream::new();
        s.bytes(&[TC_ENUM])
            .class("com.acme.Color", 0x10 | SC_SERIALIZABLE, &[])
            .bytes(&[TC_STRING])
            .utf("RED");
        assert_eq!(decode(&s.0), Some(json!({ "@enum": "com.acme.Color", "name": "RED" })));

        // A second content that cannot be read: the first stays, the error is named.
        let mut s = Stream::new();
        s.bytes(&[TC_STRING]).utf("ok").bytes(&[TC_PROXYCLASSDESC]);
        let decoded = decode(&s.0).expect("partial");
        assert_eq!(decoded["@parsed"], json!("ok"));
        assert!(
            decoded["@error"]
                .as_str()
                .unwrap_or_default()
                .contains("dynamic proxy class")
        );

        assert_eq!(decode(b"\xac\xed\x00\x05"), None, "empty stream");
        assert!(!looks_like(b"\xac\xed\x00\x04"));
    }
}

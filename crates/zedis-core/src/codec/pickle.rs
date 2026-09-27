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

//! A Python pickle (protocol 2 and up — what `pickle.dumps` has written
//! since Python 3) → JSON, by running the opcode stream on a stack machine
//! the way the unpickler would, without importing anything: a class or
//! callable is its dotted name, a `REDUCE` / `NEWOBJ` is an object
//! `{"__class": …, "args": […]}`, `BUILD` attaches its state, bytes are
//! hex under `"__bytes"`, tuples and sets are arrays. Protocols 0 and 1
//! carry no signature and are not detected.
//!
//! Containers are values, not shared references: a list memoized and
//! then filled in place (the common `EMPTY_LIST MEMOIZE … APPENDS` shape)
//! is complete where it sits on the stack, but a later `BINGET` of that
//! memo slot sees the empty snapshot. Self-referential structures thus
//! render as far as they go instead of failing.

use super::{float, hex};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// How deep containers may nest. Decoding (and dropping) a value recurses
/// once per level, so a few megabytes of `EMPTY_LIST … APPEND` could nest a
/// million deep and overflow the stack; past this the value is not shown
/// decoded.
const MAX_DEPTH: usize = 256;

/// Nodes memo lookups may copy in all. A lookup repeats the memoized value,
/// so a list holding one twice, held twice by the next, doubles with each
/// level; past this a lookup is shown as the slot it names.
const MAX_EXPANDED: usize = 1_000_000;

/// A `PROTO` opcode announcing protocol 2–5.
pub fn looks_like(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0x80 && (2..=5).contains(&bytes[1])
}

/// The unpickled value as JSON, if the stream runs to `STOP` with one
/// value left.
pub fn decode(bytes: &[u8]) -> Option<Value> {
    if !looks_like(bytes) {
        return None;
    }
    let mut vm = Vm {
        bytes,
        pos: 0,
        stack: Vec::new(),
        memo: HashMap::new(),
        expanded: 0,
        too_deep: false,
    };
    let value = vm.run().ok()?;
    Some(to_json(value))
}

#[derive(Clone, Debug)]
enum PVal {
    Mark,
    None,
    Bool(bool),
    Int(i64),
    BigInt(String),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<PVal>),
    Tuple(Vec<PVal>),
    Dict(Vec<(PVal, PVal)>),
    Set(Vec<PVal>),
    Global(String),
    Object(Box<PObject>),
    Persistent(Box<PVal>),
}

#[derive(Clone, Debug)]
struct PObject {
    class: PVal,
    args: Vec<PVal>,
    kwargs: Option<PVal>,
    state: Option<PVal>,
    /// What `APPEND(S)` added: a list subclass rebuilt by `REDUCE`.
    list_items: Vec<PVal>,
    /// What `SETITEM(S)` added: `OrderedDict`, `defaultdict`, a dict
    /// subclass — each is a `REDUCE` whose entries come after, which used to
    /// stop the decode.
    dict_items: Vec<(PVal, PVal)>,
}

impl PVal {
    /// The values this one holds directly.
    fn children(&self) -> Vec<&PVal> {
        match self {
            PVal::List(items) | PVal::Tuple(items) | PVal::Set(items) => items.iter().collect(),
            PVal::Dict(entries) => entries.iter().flat_map(|(k, v)| [k, v]).collect(),
            PVal::Object(obj) => std::iter::once(&obj.class)
                .chain(&obj.args)
                .chain(&obj.kwargs)
                .chain(&obj.state)
                .chain(&obj.list_items)
                .chain(obj.dict_items.iter().flat_map(|(k, v)| [k, v]))
                .collect(),
            PVal::Persistent(id) => vec![id],
            _ => Vec::new(),
        }
    }
}

/// Whether `value` nests no deeper than `levels`.
fn within_depth(value: &PVal, levels: usize) -> bool {
    let children = value.children();
    children.is_empty() || (levels > 0 && children.into_iter().all(|child| within_depth(child, levels - 1)))
}

/// A value's size in nodes, what a memo lookup of it copies.
fn weight(value: &PVal) -> usize {
    1 + value.children().into_iter().map(weight).sum::<usize>()
}

struct Vm<'a> {
    bytes: &'a [u8],
    pos: usize,
    stack: Vec<PVal>,
    /// Memoized values with their [`weight`].
    memo: HashMap<u32, (PVal, usize)>,
    /// Nodes copied by memo lookups so far (see [`MAX_EXPANDED`]).
    expanded: usize,
    /// A value nested past [`MAX_DEPTH`]; the run stops at the next opcode.
    too_deep: bool,
}

type Step<T> = Result<T, ()>;

impl Vm<'_> {
    fn take(&mut self, n: usize) -> Step<&[u8]> {
        let end = self.pos.checked_add(n).ok_or(())?;
        let slice = self.bytes.get(self.pos..end).ok_or(())?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Step<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Step<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| ())?))
    }
    fn i32(&mut self) -> Step<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u32(&mut self) -> Step<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u64(&mut self) -> Step<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }
    fn len(&mut self, n: u64) -> Step<usize> {
        usize::try_from(n).map_err(|_| ())
    }
    /// A newline-terminated ASCII argument (the protocol-0 style opcodes).
    fn line(&mut self) -> Step<String> {
        let rest = &self.bytes[self.pos..];
        let end = rest.iter().position(|b| *b == b'\n').ok_or(())?;
        let text = std::str::from_utf8(&rest[..end]).map_err(|_| ())?.to_string();
        self.pos += end + 1;
        Ok(text)
    }
    fn bytes_of(&mut self, n: usize) -> Step<Vec<u8>> {
        Ok(self.take(n)?.to_vec())
    }
    fn text_of(&mut self, n: usize) -> Step<String> {
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn push(&mut self, value: PVal) {
        self.check_depth(&value, MAX_DEPTH);
        self.stack.push(value);
    }
    /// Mark the run too deep if `value` nests past `levels`.
    fn check_depth(&mut self, value: &PVal, levels: usize) {
        if !within_depth(value, levels) {
            self.too_deep = true;
        }
    }
    fn pop(&mut self) -> Step<PVal> {
        self.stack.pop().ok_or(())
    }
    fn top(&mut self) -> Step<&mut PVal> {
        self.stack.last_mut().ok_or(())
    }
    /// Everything above the last `MARK`, the mark removed.
    fn pop_mark(&mut self) -> Step<Vec<PVal>> {
        let mark = self.stack.iter().rposition(|v| matches!(v, PVal::Mark)).ok_or(())?;
        let items = self.stack.split_off(mark + 1);
        self.stack.pop();
        Ok(items)
    }
    fn get(&mut self, key: u32) -> Step<PVal> {
        let (value, size) = self.memo.get(&key).ok_or(())?;
        if self.expanded + size > MAX_EXPANDED {
            return Ok(PVal::Str(format!("<memo {key}>")));
        }
        self.expanded += size;
        Ok(value.clone())
    }
    fn put(&mut self, key: u32) -> Step<()> {
        let value = self.stack.last().ok_or(())?.clone();
        let size = weight(&value);
        self.memo.insert(key, (value, size));
        Ok(())
    }

    fn run(&mut self) -> Step<PVal> {
        loop {
            if self.too_deep {
                return Err(());
            }
            let op = self.u8()?;
            match op {
                0x80 => {
                    self.u8()?;
                }
                0x95 => {
                    self.u64()?;
                }
                b'.' => {
                    let value = self.pop()?;
                    return if self.stack.is_empty() { Ok(value) } else { Err(()) };
                }
                b'(' => self.push(PVal::Mark),
                b'0' => {
                    self.pop()?;
                }
                b'1' => {
                    self.pop_mark()?;
                }
                b'2' => {
                    let top = self.stack.last().ok_or(())?.clone();
                    self.push(top);
                }
                b'N' => self.push(PVal::None),
                0x88 => self.push(PVal::Bool(true)),
                0x89 => self.push(PVal::Bool(false)),
                b'J' => {
                    let n = self.i32()?;
                    self.push(PVal::Int(n.into()));
                }
                b'K' => {
                    let n = self.u8()?;
                    self.push(PVal::Int(n.into()));
                }
                b'M' => {
                    let n = self.u16()?;
                    self.push(PVal::Int(n.into()));
                }
                b'I' => {
                    let line = self.line()?;
                    self.push(match line.as_str() {
                        "00" => PVal::Bool(false),
                        "01" => PVal::Bool(true),
                        _ => PVal::Int(line.parse().map_err(|_| ())?),
                    });
                }
                b'L' => {
                    let line = self.line()?;
                    self.push(long_from_text(line.trim_end_matches('L')));
                }
                0x8a => {
                    let n = self.u8()? as usize;
                    let raw = self.bytes_of(n)?;
                    self.push(long_from_le(&raw));
                }
                0x8b => {
                    let n = self.i32()?;
                    let n = self.len(n.try_into().map_err(|_| ())?)?;
                    let raw = self.bytes_of(n)?;
                    self.push(long_from_le(&raw));
                }
                b'G' => {
                    let f = f64::from_be_bytes(self.take(8)?.try_into().map_err(|_| ())?);
                    self.push(PVal::Float(f));
                }
                b'F' => {
                    let line = self.line()?;
                    self.push(PVal::Float(line.parse().map_err(|_| ())?));
                }
                b'S' => {
                    let line = self.line()?;
                    let unquoted = line
                        .strip_prefix('\'')
                        .and_then(|s| s.strip_suffix('\''))
                        .or_else(|| line.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                        .unwrap_or(&line);
                    self.push(PVal::Str(unquoted.to_string()));
                }
                b'T' => {
                    let n = self.i32()?;
                    let n = self.len(n.try_into().map_err(|_| ())?)?;
                    let s = self.text_of(n)?;
                    self.push(PVal::Str(s));
                }
                b'U' => {
                    let n = self.u8()? as usize;
                    let s = self.text_of(n)?;
                    self.push(PVal::Str(s));
                }
                b'V' => {
                    let line = self.line()?;
                    self.push(PVal::Str(line));
                }
                b'X' => {
                    let n = self.u32()? as usize;
                    let s = self.text_of(n)?;
                    self.push(PVal::Str(s));
                }
                0x8c => {
                    let n = self.u8()? as usize;
                    let s = self.text_of(n)?;
                    self.push(PVal::Str(s));
                }
                0x8d => {
                    let n = self.u64()?;
                    let n = self.len(n)?;
                    let s = self.text_of(n)?;
                    self.push(PVal::Str(s));
                }
                b'B' => {
                    let n = self.u32()? as usize;
                    let b = self.bytes_of(n)?;
                    self.push(PVal::Bytes(b));
                }
                b'C' => {
                    let n = self.u8()? as usize;
                    let b = self.bytes_of(n)?;
                    self.push(PVal::Bytes(b));
                }
                0x8e | 0x96 => {
                    let n = self.u64()?;
                    let n = self.len(n)?;
                    let b = self.bytes_of(n)?;
                    self.push(PVal::Bytes(b));
                }
                b')' => self.push(PVal::Tuple(Vec::new())),
                0x85 => {
                    let a = self.pop()?;
                    self.push(PVal::Tuple(vec![a]));
                }
                0x86 => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(PVal::Tuple(vec![a, b]));
                }
                0x87 => {
                    let c = self.pop()?;
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(PVal::Tuple(vec![a, b, c]));
                }
                b't' => {
                    let items = self.pop_mark()?;
                    self.push(PVal::Tuple(items));
                }
                b']' => self.push(PVal::List(Vec::new())),
                b'l' => {
                    let items = self.pop_mark()?;
                    self.push(PVal::List(items));
                }
                b'a' => {
                    let item = self.pop()?;
                    self.check_depth(&item, MAX_DEPTH - 1);
                    match self.top()? {
                        PVal::List(items) => items.push(item),
                        PVal::Object(obj) => obj.list_items.push(item),
                        _ => return Err(()),
                    }
                }
                b'e' => {
                    let new = self.pop_mark()?;
                    for item in &new {
                        self.check_depth(item, MAX_DEPTH - 1);
                    }
                    match self.top()? {
                        PVal::List(items) => items.extend(new),
                        PVal::Object(obj) => obj.list_items.extend(new),
                        _ => return Err(()),
                    }
                }
                b'}' => self.push(PVal::Dict(Vec::new())),
                b'd' => {
                    let items = self.pop_mark()?;
                    self.push(PVal::Dict(pairs(items)?));
                }
                b's' => {
                    let value = self.pop()?;
                    let key = self.pop()?;
                    self.check_depth(&key, MAX_DEPTH - 1);
                    self.check_depth(&value, MAX_DEPTH - 1);
                    match self.top()? {
                        PVal::Dict(entries) => entries.push((key, value)),
                        PVal::Object(obj) => obj.dict_items.push((key, value)),
                        _ => return Err(()),
                    }
                }
                b'u' => {
                    let new = pairs(self.pop_mark()?)?;
                    for (key, value) in &new {
                        self.check_depth(key, MAX_DEPTH - 1);
                        self.check_depth(value, MAX_DEPTH - 1);
                    }
                    match self.top()? {
                        PVal::Dict(entries) => entries.extend(new),
                        PVal::Object(obj) => obj.dict_items.extend(new),
                        _ => return Err(()),
                    }
                }
                0x8f => self.push(PVal::Set(Vec::new())),
                0x90 => {
                    let new = self.pop_mark()?;
                    for item in &new {
                        self.check_depth(item, MAX_DEPTH - 1);
                    }
                    match self.top()? {
                        PVal::Set(items) => items.extend(new),
                        _ => return Err(()),
                    }
                }
                0x91 => {
                    let items = self.pop_mark()?;
                    self.push(PVal::Set(items));
                }
                b'p' => {
                    let key: u32 = self.line()?.parse().map_err(|_| ())?;
                    self.put(key)?;
                }
                b'q' => {
                    let key = self.u8()?;
                    self.put(key.into())?;
                }
                b'r' => {
                    let key = self.u32()?;
                    self.put(key)?;
                }
                0x94 => {
                    let key = self.memo.len() as u32;
                    self.put(key)?;
                }
                b'g' => {
                    let key: u32 = self.line()?.parse().map_err(|_| ())?;
                    let value = self.get(key)?;
                    self.push(value);
                }
                b'h' => {
                    let key = self.u8()?;
                    let value = self.get(key.into())?;
                    self.push(value);
                }
                b'j' => {
                    let key = self.u32()?;
                    let value = self.get(key)?;
                    self.push(value);
                }
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    self.push(PVal::Global(format!("{module}.{name}")));
                }
                0x93 => {
                    let name = self.pop()?;
                    let module = self.pop()?;
                    match (module, name) {
                        (PVal::Str(m), PVal::Str(n)) => self.push(PVal::Global(format!("{m}.{n}"))),
                        _ => return Err(()),
                    }
                }
                b'R' => {
                    let args = self.pop()?;
                    let callable = self.pop()?;
                    self.push(object(callable, args, None));
                }
                0x81 => {
                    let args = self.pop()?;
                    let class = self.pop()?;
                    self.push(object(class, args, None));
                }
                0x92 => {
                    let kwargs = self.pop()?;
                    let args = self.pop()?;
                    let class = self.pop()?;
                    self.push(object(class, args, Some(kwargs)));
                }
                b'o' => {
                    let mut items = self.pop_mark()?;
                    if items.is_empty() {
                        return Err(());
                    }
                    let class = items.remove(0);
                    self.push(object(class, PVal::Tuple(items), None));
                }
                b'i' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    let items = self.pop_mark()?;
                    self.push(object(
                        PVal::Global(format!("{module}.{name}")),
                        PVal::Tuple(items),
                        None,
                    ));
                }
                b'b' => {
                    let state = self.pop()?;
                    let target = self.pop()?;
                    let built = match target {
                        PVal::Object(mut obj) => {
                            obj.state = Some(state);
                            PVal::Object(obj)
                        }
                        other => PVal::Object(Box::new(PObject {
                            class: PVal::Global("(built)".into()),
                            args: vec![other],
                            kwargs: None,
                            state: Some(state),
                            list_items: Vec::new(),
                            dict_items: Vec::new(),
                        })),
                    };
                    self.push(built);
                }
                b'P' => {
                    let id = self.line()?;
                    self.push(PVal::Persistent(Box::new(PVal::Str(id))));
                }
                b'Q' => {
                    let id = self.pop()?;
                    self.push(PVal::Persistent(Box::new(id)));
                }
                // EXT1/2/4 (copyreg extension registry), NEXT_BUFFER,
                // READONLY_BUFFER: nothing to show without the registry or
                // the out-of-band buffers.
                _ => return Err(()),
            }
        }
    }
}

fn object(class: PVal, args: PVal, kwargs: Option<PVal>) -> PVal {
    let args = match args {
        PVal::Tuple(items) | PVal::List(items) => items,
        other => vec![other],
    };
    PVal::Object(Box::new(PObject {
        class,
        args,
        kwargs,
        state: None,
        list_items: Vec::new(),
        dict_items: Vec::new(),
    }))
}

fn pairs(items: Vec<PVal>) -> Step<Vec<(PVal, PVal)>> {
    if !items.len().is_multiple_of(2) {
        return Err(());
    }
    let mut out = Vec::with_capacity(items.len() / 2);
    let mut iter = items.into_iter();
    while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
        out.push((k, v));
    }
    Ok(out)
}

/// Widest integer spelled in decimal; the conversion is quadratic in the
/// width, so a longer one is spelled in hex (still signed).
const MAX_DECIMAL_BYTES: usize = 1024;

/// Two's-complement little-endian integer of any width. Past 64 bits it is
/// text: decimal, with its sign — the hex of its bytes it used to be showed
/// a negative number as its two's complement.
fn long_from_le(raw: &[u8]) -> PVal {
    if raw.is_empty() {
        return PVal::Int(0);
    }
    let negative = raw[raw.len() - 1] & 0x80 != 0;
    if raw.len() <= 8 {
        let mut buf = [if negative { 0xff } else { 0x00 }; 8];
        buf[..raw.len()].copy_from_slice(raw);
        return PVal::Int(i64::from_le_bytes(buf));
    }
    // The magnitude, big-endian: negated (inverted, plus one) when negative.
    let mut magnitude: Vec<u8> = raw.iter().rev().copied().collect();
    if negative {
        for byte in magnitude.iter_mut() {
            *byte = !*byte;
        }
        for byte in magnitude.iter_mut().rev() {
            let (sum, carry) = byte.overflowing_add(1);
            *byte = sum;
            if !carry {
                break;
            }
        }
    }
    let sign = if negative { "-" } else { "" };
    if raw.len() > MAX_DECIMAL_BYTES {
        return PVal::BigInt(format!("{sign}0x{}", hex(&magnitude)));
    }
    PVal::BigInt(format!("{sign}{}", decimal(magnitude)))
}

/// A big-endian unsigned integer in decimal.
fn decimal(mut big_endian: Vec<u8>) -> String {
    let mut digits = Vec::new();
    while big_endian.iter().any(|byte| *byte != 0) {
        let mut remainder = 0u32;
        for byte in big_endian.iter_mut() {
            let current = (remainder << 8) | u32::from(*byte);
            *byte = (current / 10) as u8;
            remainder = current % 10;
        }
        digits.push(char::from(b'0' + remainder as u8));
    }
    if digits.is_empty() {
        return "0".to_string();
    }
    digits.iter().rev().collect()
}

fn long_from_text(text: &str) -> PVal {
    text.parse::<i64>()
        .map(PVal::Int)
        .unwrap_or_else(|_| PVal::BigInt(text.to_string()))
}

fn to_json(value: PVal) -> Value {
    match value {
        PVal::Mark => Value::String("(mark)".into()),
        PVal::None => Value::Null,
        PVal::Bool(b) => Value::Bool(b),
        PVal::Int(i) => Value::from(i),
        PVal::BigInt(s) => Value::String(s),
        PVal::Float(f) => float(f),
        PVal::Str(s) => Value::String(s),
        PVal::Bytes(b) => super::tagged("__bytes", Value::String(hex(&b))),
        PVal::List(items) | PVal::Tuple(items) | PVal::Set(items) => {
            Value::Array(items.into_iter().map(to_json).collect())
        }
        PVal::Dict(entries) => {
            let mut map = Map::new();
            for (k, v) in entries {
                map.insert(key_text(k), to_json(v));
            }
            Value::Object(map)
        }
        PVal::Global(name) => Value::String(name),
        PVal::Object(obj) => {
            let mut map = Map::new();
            map.insert("__class".into(), to_json(obj.class));
            if !obj.args.is_empty() {
                map.insert("args".into(), Value::Array(obj.args.into_iter().map(to_json).collect()));
            }
            if let Some(kwargs) = obj.kwargs {
                map.insert("kwargs".into(), to_json(kwargs));
            }
            if let Some(state) = obj.state {
                map.insert("state".into(), to_json(state));
            }
            if !obj.list_items.is_empty() {
                map.insert(
                    "items".into(),
                    Value::Array(obj.list_items.into_iter().map(to_json).collect()),
                );
            }
            if !obj.dict_items.is_empty() {
                map.insert("dict".into(), to_json(PVal::Dict(obj.dict_items)));
            }
            Value::Object(map)
        }
        PVal::Persistent(id) => super::tagged("__persistent_id", to_json(*id)),
    }
}

/// A dict key as JSON's string key: text as is, everything else spelled.
fn key_text(key: PVal) -> String {
    match key {
        PVal::Str(s) => s,
        PVal::Int(i) => i.to_string(),
        other => match to_json(other) {
            Value::String(s) => s,
            v => v.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `OrderedDict` and `defaultdict` are a `REDUCE` whose entries follow
    /// as `SETITEM(S)`; a list subclass's follow as `APPEND(S)`.
    #[test]
    fn reduced_dicts_and_lists_keep_their_items() {
        // protocol 2: collections.OrderedDict() then SETITEM "a" = 1.
        let mut bytes = b"\x80\x02ccollections\nOrderedDict\n)R".to_vec();
        bytes.extend_from_slice(b"X\x01\x00\x00\x00aK\x01s.");
        assert_eq!(
            decode(&bytes),
            Some(json!({ "__class": "collections.OrderedDict", "dict": { "a": 1 } }))
        );
        // A list subclass: REDUCE, then APPENDS 1, 2.
        let mut bytes = b"\x80\x02cshop\nCart\n)R".to_vec();
        bytes.extend_from_slice(b"(K\x01K\x02e.");
        assert_eq!(decode(&bytes), Some(json!({ "__class": "shop.Cart", "items": [1, 2] })));
    }

    /// Lists nested past the cap are not decoded — decoding and dropping
    /// them recursed once per level.
    #[test]
    fn nesting_past_the_cap_is_not_decoded() {
        let nested = |levels: usize| {
            let mut bytes = b"\x80\x02".to_vec();
            bytes.extend(std::iter::repeat_n(b']', levels));
            bytes.extend(std::iter::repeat_n(b'a', levels - 1));
            bytes.push(b'.');
            bytes
        };
        assert!(decode(&nested(10)).is_some());
        assert!(decode(&nested(MAX_DEPTH + 10)).is_none());
        assert!(decode(&nested(200_000)).is_none(), "and without overflowing the stack");
    }

    /// Lists that each hold the one before twice, by memo, double with
    /// every level; the copies stop at the budget.
    #[test]
    fn memo_lookups_cannot_expand_without_bound() {
        let mut bytes = b"\x80\x02(K\x01K\x02lq\x000".to_vec();
        for level in 1..=40u8 {
            bytes.extend_from_slice(&[b'(', b'h', level - 1, b'h', level - 1, b'l', b'q', level]);
            bytes.push(b'0');
        }
        // Leave the last level on the stack for STOP.
        bytes.extend_from_slice(&[b'h', 40, b'.']);
        // Popped levels keep only their memo copy; the one on the stack is
        // the value.
        let decoded = decode(&bytes).expect("decoded");
        assert!(decoded.to_string().contains("<memo "), "the rest name their slot");
    }

    #[test]
    fn protocol_4_dict_with_nested_containers() {
        // pickle.dumps({"name": "zedis", "n": 3, "tags": ["a", "b"], "t": (1, 2.5), "ok": True, "none": None}, protocol=4)
        let bytes: &[u8] = &[
            0x80, 0x04, 0x95, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'}', 0x94, b'(', 0x8c, 0x04, b'n', b'a',
            b'm', b'e', 0x94, 0x8c, 0x05, b'z', b'e', b'd', b'i', b's', 0x94, 0x8c, 0x01, b'n', 0x94, b'K', 0x03, 0x8c,
            0x04, b't', b'a', b'g', b's', 0x94, b']', 0x94, b'(', 0x8c, 0x01, b'a', 0x94, 0x8c, 0x01, b'b', 0x94, b'e',
            0x8c, 0x01, b't', 0x94, b'K', 0x01, b'G', 0x40, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x86, 0x94, 0x8c,
            0x02, b'o', b'k', 0x94, 0x88, 0x8c, 0x04, b'n', b'o', b'n', b'e', 0x94, b'N', b'u', b'.',
        ];
        assert!(looks_like(bytes));
        assert_eq!(
            decode(bytes),
            Some(json!({
                "name": "zedis", "n": 3, "tags": ["a", "b"], "t": [1, 2.5], "ok": true, "none": null
            }))
        );
    }

    #[test]
    fn objects_keep_their_class_args_and_state() {
        // pickle.dumps(Point(1, 2), protocol=2) for a plain class with __dict__ state:
        // PROTO 2, GLOBAL "mod\nPoint", EMPTY_TUPLE, NEWOBJ, EMPTY_DICT, ... BUILD, STOP
        let mut bytes = vec![0x80, 0x02];
        bytes.extend_from_slice(b"cmod\nPoint\n");
        bytes.push(b')');
        bytes.push(0x81);
        bytes.push(b'}');
        bytes.extend_from_slice(&[b'U', 1, b'x', b'K', 1, b's']);
        bytes.extend_from_slice(&[b'U', 1, b'y', b'K', 2, b's']);
        bytes.push(b'b');
        bytes.push(b'.');
        assert_eq!(
            decode(&bytes),
            Some(json!({ "__class": "mod.Point", "state": { "x": 1, "y": 2 } }))
        );

        // A REDUCE with a bytes argument (the datetime shape).
        let mut bytes = vec![0x80, 0x02];
        bytes.extend_from_slice(b"cdatetime\ndatetime\n");
        bytes.extend_from_slice(&[b'C', 2, 0x07, 0xe6, 0x85, b'R', b'.']);
        assert_eq!(
            decode(&bytes),
            Some(json!({ "__class": "datetime.datetime", "args": [{ "__bytes": "07e6" }] }))
        );
    }

    #[test]
    fn integers_of_every_width_and_negative_longs() {
        let mut bytes = vec![0x80, 0x02, b'('];
        bytes.extend_from_slice(&[b'J', 0xff, 0xff, 0xff, 0xff]); // -1
        bytes.extend_from_slice(&[b'M', 0x00, 0x01]); // 256
        bytes.extend_from_slice(&[0x8a, 0x01, 0xfe]); // LONG1 -2
        bytes.extend_from_slice(&[0x8a, 0x09, 0, 0, 0, 0, 0, 0, 0, 0, 1]); // 2^64
        bytes.extend_from_slice(&[0x8a, 0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0xff]); // -2^64
        bytes.extend_from_slice(&[0x8a, 0x09, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff]); // -(2^63)-1
        bytes.extend_from_slice(b"L12345678901234567890L\n");
        bytes.extend_from_slice(b"l.");
        assert_eq!(
            decode(&bytes),
            Some(json!([
                -1,
                256,
                -2,
                "18446744073709551616",
                "-18446744073709551616",
                "-9223372036854775809",
                "12345678901234567890"
            ]))
        );
    }

    #[test]
    fn refuses_streams_that_do_not_run_to_a_single_value() {
        assert_eq!(decode(&[0x80, 0x04, b'K', 1]), None, "no STOP");
        assert_eq!(decode(&[0x80, 0x04, b'K', 1, b'K', 2, b'.']), None, "two values left");
        assert_eq!(decode(&[0x80, 0x04, 0x82, 0x01, b'.']), None, "EXT1 unsupported");
        assert!(!looks_like(b"\x80\x01."), "protocol 1 has no signature worth trusting");
        assert!(!looks_like(b"\x80"));
    }
}

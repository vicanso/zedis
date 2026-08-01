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

//! Streaming RDB file parser for offline memory analysis.
//!
//! Walks a Redis dump file and yields one [`RdbEntry`] per key — name,
//! type, expiry, and the number of bytes the entry occupies in the file —
//! **without decoding values**: every value encoding through Redis 8.6
//! (stream IDMP/NACK zones included) is length-skipped, so a multi-GB
//! dump parses at I/O speed with flat memory. Only key names are
//! materialized (including LZF decompression when a key itself is
//! compressed).
//!
//! The serialized size is not the live `MEMORY USAGE` — listpacks are
//! LZF-packed on disk and in-RAM overhead (dict entries, robj headers)
//! is absent — but relative sizes are faithful, which is what big-key
//! and prefix analysis need.

use std::fmt;
use std::io::Read;

/// RDB opcodes (`RDB_OPCODE_*` in redis `rdb.h`).
const OP_SLOT_INFO: u8 = 0xF4;
const OP_FUNCTION2: u8 = 0xF5;
const OP_FUNCTION_PRE_GA: u8 = 0xF6;
const OP_MODULE_AUX: u8 = 0xF7;
const OP_IDLE: u8 = 0xF8;
const OP_FREQ: u8 = 0xF9;
const OP_AUX: u8 = 0xFA;
const OP_RESIZEDB: u8 = 0xFB;
const OP_EXPIRETIME_MS: u8 = 0xFC;
const OP_EXPIRETIME: u8 = 0xFD;
const OP_SELECTDB: u8 = 0xFE;
const OP_EOF: u8 = 0xFF;

/// Value types (`RDB_TYPE_*` in redis `rdb.h`).
const T_STRING: u8 = 0;
const T_LIST: u8 = 1;
const T_SET: u8 = 2;
const T_ZSET: u8 = 3;
const T_HASH: u8 = 4;
const T_ZSET_2: u8 = 5;
const T_MODULE_PRE_GA: u8 = 6;
const T_MODULE_2: u8 = 7;
const T_HASH_ZIPMAP: u8 = 9;
const T_LIST_ZIPLIST: u8 = 10;
const T_SET_INTSET: u8 = 11;
const T_ZSET_ZIPLIST: u8 = 12;
const T_HASH_ZIPLIST: u8 = 13;
const T_LIST_QUICKLIST: u8 = 14;
const T_STREAM_LISTPACKS: u8 = 15;
const T_HASH_LISTPACK: u8 = 16;
const T_ZSET_LISTPACK: u8 = 17;
const T_LIST_QUICKLIST_2: u8 = 18;
const T_STREAM_LISTPACKS_2: u8 = 19;
const T_SET_LISTPACK: u8 = 20;
const T_STREAM_LISTPACKS_3: u8 = 21;
const T_HASH_METADATA_PRE_GA: u8 = 22;
const T_HASH_LISTPACK_EX_PRE_GA: u8 = 23;
const T_HASH_METADATA: u8 = 24;
const T_HASH_LISTPACK_EX: u8 = 25;
/// Redis 8.6+: stream with an IDMP (idempotent producer) zone appended.
const T_STREAM_LISTPACKS_4: u8 = 26;
/// Redis 8.6+: stream with per-group NACK zones on top of IDMP.
const T_STREAM_LISTPACKS_5: u8 = 27;

/// Module value opcodes (`RDB_MODULE_OPCODE_*`).
const MODULE_OP_EOF: u64 = 0;
const MODULE_OP_SINT: u64 = 1;
const MODULE_OP_UINT: u64 = 2;
const MODULE_OP_FLOAT: u64 = 3;
const MODULE_OP_DOUBLE: u64 = 4;
const MODULE_OP_STRING: u64 = 5;

/// Parse failure: I/O, truncation, or a malformed/unsupported encoding.
/// Carries the file offset where parsing stopped so a corrupt dump can be
/// reported precisely.
#[derive(Debug)]
pub struct RdbError {
    pub message: String,
    pub offset: u64,
}

impl fmt::Display for RdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for RdbError {}

type Result<T> = std::result::Result<T, RdbError>;

/// One key parsed out of the dump.
#[derive(Debug, Clone)]
pub struct RdbEntry {
    /// Database index the key lives in (`SELECTDB`).
    pub db: u64,
    /// Key name, lossily UTF-8 decoded (binary keys keep replacement chars,
    /// matching how the live key tree renders them).
    pub key: String,
    /// Canonical Redis type name: `string` / `list` / `set` / `zset` /
    /// `hash` / `stream` / `module`.
    pub key_type: &'static str,
    /// Absolute expiry in unix milliseconds, `None` for persistent keys.
    pub expire_at_ms: Option<i64>,
    /// Bytes this entry occupies in the file (expiry opcode + type byte +
    /// key + value).
    pub serialized_bytes: u64,
}

/// Streaming parser: construct with [`RdbParser::new`] (validates the
/// header), then call [`next_entry`](RdbParser::next_entry) until it
/// returns `Ok(None)`. [`bytes_read`](RdbParser::bytes_read) reports the
/// current file offset for progress against the file's total size.
pub struct RdbParser<R: Read> {
    reader: R,
    offset: u64,
    version: u32,
    current_db: u64,
    aux: Vec<(String, String)>,
    done: bool,
}

/// A decoded length prefix: an actual length, or a special string encoding.
enum Length {
    Len(u64),
    Encoded(u8),
}

impl<R: Read> RdbParser<R> {
    /// Reads and validates the `REDIS00NN` header.
    pub fn new(reader: R) -> Result<Self> {
        let mut parser = Self {
            reader,
            offset: 0,
            version: 0,
            current_db: 0,
            aux: Vec::new(),
            done: false,
        };
        let mut header = [0u8; 9];
        parser.read_exact(&mut header)?;
        if &header[0..5] != b"REDIS" {
            return Err(parser.err("not an RDB file (missing REDIS header)"));
        }
        let version = std::str::from_utf8(&header[5..9])
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| parser.err("invalid RDB version in header"))?;
        parser.version = version;
        Ok(parser)
    }

    /// RDB format version from the header (Redis 7.x writes 11/12).
    pub fn rdb_version(&self) -> u32 {
        self.version
    }

    /// AUX metadata seen so far (`redis-ver`, `used-mem`, ...). Fully
    /// populated once the first entry has been returned (aux fields sit
    /// at the top of the file).
    pub fn aux(&self) -> &[(String, String)] {
        &self.aux
    }

    /// Current file offset — drive a progress bar against the file size.
    pub fn bytes_read(&self) -> u64 {
        self.offset
    }

    /// Parses forward to the next key entry. `Ok(None)` on clean EOF.
    pub fn next_entry(&mut self) -> Result<Option<RdbEntry>> {
        if self.done {
            return Ok(None);
        }
        let mut expire_at_ms: Option<i64> = None;
        let mut entry_start: Option<u64> = None;
        loop {
            let opcode_offset = self.offset;
            let opcode = self.read_u8()?;
            match opcode {
                OP_EOF => {
                    self.done = true;
                    // CRC64 trailer (version >= 5). Best-effort: a dump
                    // written with `rdbchecksum no` still carries zeros.
                    let mut trailer = [0u8; 8];
                    let _ = self.reader.read(&mut trailer);
                    return Ok(None);
                }
                OP_SELECTDB => {
                    self.current_db = self.read_length_value()?;
                }
                OP_RESIZEDB => {
                    self.read_length_value()?;
                    self.read_length_value()?;
                }
                OP_AUX => {
                    let key = self.read_string()?;
                    let value = self.read_string()?;
                    self.aux.push((
                        String::from_utf8_lossy(&key).into_owned(),
                        String::from_utf8_lossy(&value).into_owned(),
                    ));
                }
                OP_EXPIRETIME_MS => {
                    expire_at_ms = Some(self.read_u64_le()? as i64);
                    entry_start.get_or_insert(opcode_offset);
                }
                OP_EXPIRETIME => {
                    expire_at_ms = Some(self.read_u32_le()? as i64 * 1000);
                    entry_start.get_or_insert(opcode_offset);
                }
                OP_IDLE => {
                    self.read_length_value()?;
                    entry_start.get_or_insert(opcode_offset);
                }
                OP_FREQ => {
                    self.read_u8()?;
                    entry_start.get_or_insert(opcode_offset);
                }
                OP_MODULE_AUX => {
                    self.skip_module_aux()?;
                }
                OP_FUNCTION2 => {
                    self.skip_string()?;
                }
                OP_FUNCTION_PRE_GA => {
                    return Err(self.err("pre-GA function payload (Redis 7.0 RC) is not supported"));
                }
                OP_SLOT_INFO => {
                    self.read_length_value()?;
                    self.read_length_value()?;
                    self.read_length_value()?;
                }
                value_type => {
                    let start = entry_start.unwrap_or(opcode_offset);
                    let key = self.read_string()?;
                    let key_type = self.skip_value(value_type)?;
                    return Ok(Some(RdbEntry {
                        db: self.current_db,
                        key: String::from_utf8_lossy(&key).into_owned(),
                        key_type,
                        expire_at_ms,
                        serialized_bytes: self.offset - start,
                    }));
                }
            }
        }
    }

    fn err(&self, message: impl Into<String>) -> RdbError {
        RdbError {
            message: message.into(),
            offset: self.offset,
        }
    }

    // --- primitive readers ---

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.reader
            .read_exact(buf)
            .map_err(|e| self.err(format!("read failed: {e}")))?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn read_u32_le(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn read_u64_le(&mut self) -> Result<u64> {
        let mut b = [0u8; 8];
        self.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        let copied = std::io::copy(&mut (&mut self.reader).take(n), &mut std::io::sink())
            .map_err(|e| self.err(format!("read failed: {e}")))?;
        self.offset += copied;
        if copied != n {
            return Err(self.err("unexpected end of file"));
        }
        Ok(())
    }

    // --- RDB encodings ---

    fn read_length(&mut self) -> Result<Length> {
        let b = self.read_u8()?;
        match b >> 6 {
            0 => Ok(Length::Len((b & 0x3F) as u64)),
            1 => {
                let next = self.read_u8()?;
                Ok(Length::Len((((b & 0x3F) as u64) << 8) | next as u64))
            }
            2 => match b {
                0x80 => {
                    let mut buf = [0u8; 4];
                    self.read_exact(&mut buf)?;
                    Ok(Length::Len(u32::from_be_bytes(buf) as u64))
                }
                0x81 => {
                    let mut buf = [0u8; 8];
                    self.read_exact(&mut buf)?;
                    Ok(Length::Len(u64::from_be_bytes(buf)))
                }
                _ => Err(self.err(format!("invalid length byte 0x{b:02X}"))),
            },
            _ => Ok(Length::Encoded(b & 0x3F)),
        }
    }

    /// A length that must be an actual length (not a string encoding).
    fn read_length_value(&mut self) -> Result<u64> {
        match self.read_length()? {
            Length::Len(n) => Ok(n),
            Length::Encoded(_) => Err(self.err("unexpected encoded length")),
        }
    }

    /// Materializes a string (keys, aux fields). Integer encodings render
    /// as decimal; LZF payloads are decompressed.
    fn read_string(&mut self) -> Result<Vec<u8>> {
        match self.read_length()? {
            Length::Len(n) => {
                // Keys/aux are small; the length still comes from the file,
                // so cap the pre-allocation against a corrupt header.
                let mut buf = vec![0u8; n.min(1 << 20) as usize];
                if (n as usize) <= buf.len() {
                    self.read_exact(&mut buf)?;
                    return Ok(buf);
                }
                buf.clear();
                let copied = std::io::copy(&mut (&mut self.reader).take(n), &mut buf)
                    .map_err(|e| self.err(format!("read failed: {e}")))?;
                self.offset += copied;
                if copied != n {
                    return Err(self.err("unexpected end of file"));
                }
                Ok(buf)
            }
            Length::Encoded(0) => Ok(format!("{}", self.read_u8()? as i8).into_bytes()),
            Length::Encoded(1) => {
                let mut b = [0u8; 2];
                self.read_exact(&mut b)?;
                Ok(format!("{}", i16::from_le_bytes(b)).into_bytes())
            }
            Length::Encoded(2) => {
                let mut b = [0u8; 4];
                self.read_exact(&mut b)?;
                Ok(format!("{}", i32::from_le_bytes(b)).into_bytes())
            }
            Length::Encoded(3) => {
                let compressed = self.read_length_value()?;
                let uncompressed = self.read_length_value()?;
                let mut buf = vec![0u8; compressed as usize];
                self.read_exact(&mut buf)?;
                lzf_decompress(&buf, uncompressed as usize).map_err(|m| self.err(m))
            }
            Length::Encoded(enc) => Err(self.err(format!("unknown string encoding {enc}"))),
        }
    }

    /// Skips over a string without materializing it (LZF stays compressed).
    fn skip_string(&mut self) -> Result<()> {
        match self.read_length()? {
            Length::Len(n) => self.skip(n),
            Length::Encoded(0) => self.skip(1),
            Length::Encoded(1) => self.skip(2),
            Length::Encoded(2) => self.skip(4),
            Length::Encoded(3) => {
                let compressed = self.read_length_value()?;
                self.read_length_value()?;
                self.skip(compressed)
            }
            Length::Encoded(enc) => Err(self.err(format!("unknown string encoding {enc}"))),
        }
    }

    /// Old-style zset score: 1-byte length then ASCII digits, with 253/254/255
    /// as nan/+inf/-inf sentinels.
    fn skip_double_string(&mut self) -> Result<()> {
        let len = self.read_u8()?;
        match len {
            253..=255 => Ok(()),
            n => self.skip(n as u64),
        }
    }

    /// Length-skips one value of the given type; returns the canonical
    /// Redis type name.
    fn skip_value(&mut self, value_type: u8) -> Result<&'static str> {
        match value_type {
            T_STRING => {
                self.skip_string()?;
                Ok("string")
            }
            T_LIST | T_SET => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.skip_string()?;
                }
                Ok(if value_type == T_LIST { "list" } else { "set" })
            }
            T_ZSET => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.skip_string()?;
                    self.skip_double_string()?;
                }
                Ok("zset")
            }
            T_ZSET_2 => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.skip_string()?;
                    self.skip(8)?;
                }
                Ok("zset")
            }
            T_HASH => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.skip_string()?;
                    self.skip_string()?;
                }
                Ok("hash")
            }
            // Single-blob compact encodings.
            T_HASH_ZIPMAP | T_HASH_ZIPLIST | T_HASH_LISTPACK | T_HASH_LISTPACK_EX_PRE_GA => {
                self.skip_string()?;
                Ok("hash")
            }
            T_LIST_ZIPLIST => {
                self.skip_string()?;
                Ok("list")
            }
            T_SET_INTSET | T_SET_LISTPACK => {
                self.skip_string()?;
                Ok("set")
            }
            T_ZSET_ZIPLIST | T_ZSET_LISTPACK => {
                self.skip_string()?;
                Ok("zset")
            }
            T_HASH_LISTPACK_EX => {
                // minExpire (unix ms, 8 bytes LE) + listpack blob.
                self.skip(8)?;
                self.skip_string()?;
                Ok("hash")
            }
            T_HASH_METADATA => {
                // minExpire + n × (ttl-delta, field, value).
                self.skip(8)?;
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.read_length_value()?;
                    self.skip_string()?;
                    self.skip_string()?;
                }
                Ok("hash")
            }
            T_HASH_METADATA_PRE_GA => {
                // n × (absolute ttl, field, value) — 7.4 RC layout.
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.read_length_value()?;
                    self.skip_string()?;
                    self.skip_string()?;
                }
                Ok("hash")
            }
            T_LIST_QUICKLIST => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    self.skip_string()?;
                }
                Ok("list")
            }
            T_LIST_QUICKLIST_2 => {
                let n = self.read_length_value()?;
                for _ in 0..n {
                    // Container marker: 1 = plain node, 2 = packed listpack.
                    self.read_length_value()?;
                    self.skip_string()?;
                }
                Ok("list")
            }
            T_STREAM_LISTPACKS | T_STREAM_LISTPACKS_2 | T_STREAM_LISTPACKS_3 | T_STREAM_LISTPACKS_4
            | T_STREAM_LISTPACKS_5 => {
                self.skip_stream(value_type)?;
                Ok("stream")
            }
            T_MODULE_2 => {
                self.read_length_value()?; // module id
                self.skip_module_opcodes()?;
                Ok("module")
            }
            T_MODULE_PRE_GA => Err(self.err("pre-GA module value (Redis < 4.0 GA) is not supported")),
            other => Err(self.err(format!("unknown RDB value type {other}"))),
        }
    }

    fn skip_stream(&mut self, value_type: u8) -> Result<()> {
        let listpacks = self.read_length_value()?;
        for _ in 0..listpacks {
            self.skip_string()?; // master stream id (16 raw bytes as string)
            self.skip_string()?; // listpack blob
        }
        self.read_length_value()?; // total items
        self.read_length_value()?; // last id ms
        self.read_length_value()?; // last id seq
        if value_type >= T_STREAM_LISTPACKS_2 {
            self.read_length_value()?; // first id ms
            self.read_length_value()?; // first id seq
            self.read_length_value()?; // max deleted id ms
            self.read_length_value()?; // max deleted id seq
            self.read_length_value()?; // entries added
        }
        let groups = self.read_length_value()?;
        for _ in 0..groups {
            self.skip_string()?; // group name
            self.read_length_value()?; // group last id ms
            self.read_length_value()?; // group last id seq
            if value_type >= T_STREAM_LISTPACKS_2 {
                self.read_length_value()?; // entries read
            }
            let pel = self.read_length_value()?;
            for _ in 0..pel {
                self.skip(16)?; // raw stream id
                self.skip(8)?; // delivery time (ms LE)
                self.read_length_value()?; // delivery count
            }
            let consumers = self.read_length_value()?;
            for _ in 0..consumers {
                self.skip_string()?; // consumer name
                self.skip(8)?; // seen time
                if value_type >= T_STREAM_LISTPACKS_3 {
                    self.skip(8)?; // active time
                }
                let consumer_pel = self.read_length_value()?;
                for _ in 0..consumer_pel {
                    self.skip(16)?; // raw stream id (metadata lives in group PEL)
                }
            }
            if value_type >= T_STREAM_LISTPACKS_5 {
                // NACK zone: count + raw ids of NACKed entries.
                let nacked = self.read_length_value()?;
                for _ in 0..nacked {
                    self.skip(16)?;
                }
            }
        }
        if value_type >= T_STREAM_LISTPACKS_4 {
            // IDMP (idempotent producer) zone — verified byte-for-byte
            // against a Redis 8.6 dump.
            self.read_length_value()?; // idmp duration (seconds)
            self.read_length_value()?; // idmp max entries
            let producers = self.read_length_value()?;
            for _ in 0..producers {
                self.skip_string()?; // producer id
                let entries = self.read_length_value()?;
                for _ in 0..entries {
                    self.skip_string()?; // iid
                    self.read_length_value()?; // stream id ms
                    self.read_length_value()?; // stream id seq
                }
            }
            self.read_length_value()?; // iids added
            self.read_length_value()?; // iids duplicates
        }
        Ok(())
    }

    /// Module payloads are self-describing opcode streams terminated by an
    /// explicit EOF opcode — skippable without knowing the module.
    fn skip_module_opcodes(&mut self) -> Result<()> {
        loop {
            match self.read_length_value()? {
                MODULE_OP_EOF => return Ok(()),
                MODULE_OP_SINT | MODULE_OP_UINT => {
                    self.read_length_value()?;
                }
                MODULE_OP_FLOAT => self.skip(4)?,
                MODULE_OP_DOUBLE => self.skip(8)?,
                MODULE_OP_STRING => self.skip_string()?,
                other => return Err(self.err(format!("unknown module opcode {other}"))),
            }
        }
    }

    fn skip_module_aux(&mut self) -> Result<()> {
        self.read_length_value()?; // module id
        self.read_length_value()?; // when opcode
        self.read_length_value()?; // when value
        self.skip_module_opcodes()
    }
}

/// LZF decompression (the only variant Redis uses). Hand-rolled to keep
/// the dependency surface lean — the format is a simple mix of literal
/// runs and back-references.
fn lzf_decompress(input: &[u8], expected_len: usize) -> std::result::Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(expected_len);
    let mut i = 0usize;
    while i < input.len() {
        let ctrl = input[i] as usize;
        i += 1;
        if ctrl < 32 {
            // Literal run of ctrl + 1 bytes.
            let len = ctrl + 1;
            let end = i.checked_add(len).filter(|&e| e <= input.len());
            let Some(end) = end else {
                return Err("lzf: truncated literal run".to_string());
            };
            out.extend_from_slice(&input[i..end]);
            i = end;
        } else {
            // Back-reference: length in the top 3 bits (7 = extended).
            let mut len = ctrl >> 5;
            if len == 7 {
                let Some(&ext) = input.get(i) else {
                    return Err("lzf: truncated length byte".to_string());
                };
                len += ext as usize;
                i += 1;
            }
            let Some(&low) = input.get(i) else {
                return Err("lzf: truncated offset byte".to_string());
            };
            i += 1;
            let distance = ((ctrl & 0x1F) << 8) | low as usize;
            let Some(mut pos) = out.len().checked_sub(distance + 1) else {
                return Err("lzf: back-reference before start".to_string());
            };
            for _ in 0..len + 2 {
                let byte = out[pos];
                out.push(byte);
                pos += 1;
            }
        }
    }
    if out.len() != expected_len {
        return Err(format!("lzf: expected {expected_len} bytes, got {}", out.len()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal RDB writer for fixtures.
    struct Fixture(Vec<u8>);

    impl Fixture {
        fn new() -> Self {
            Self(b"REDIS0011".to_vec())
        }
        fn op(mut self, op: u8) -> Self {
            self.0.push(op);
            self
        }
        /// 6-bit length-prefixed raw string (covers keys/values <= 63 bytes).
        fn str(mut self, s: &[u8]) -> Self {
            assert!(s.len() <= 63, "fixture strings use the 6-bit form");
            self.0.push(s.len() as u8);
            self.0.extend_from_slice(s);
            self
        }
        fn byte(mut self, b: u8) -> Self {
            self.0.push(b);
            self
        }
        fn bytes(mut self, b: &[u8]) -> Self {
            self.0.extend_from_slice(b);
            self
        }
        fn eof(self) -> Vec<u8> {
            let mut data = self.op(OP_EOF).0;
            data.extend_from_slice(&[0u8; 8]); // checksum trailer
            data
        }
    }

    fn parse_all(data: &[u8]) -> Vec<RdbEntry> {
        let mut parser = RdbParser::new(data).expect("valid header");
        let mut entries = Vec::new();
        while let Some(entry) = parser.next_entry().expect("parse entry") {
            entries.push(entry);
        }
        entries
    }

    #[test]
    fn parses_plain_string_with_aux_and_expire() {
        let data = Fixture::new()
            .op(OP_AUX)
            .str(b"redis-ver")
            .str(b"7.2.5")
            .op(OP_SELECTDB)
            .byte(2)
            .op(OP_EXPIRETIME_MS)
            .bytes(&2_000_000_000_000u64.to_le_bytes())
            .op(T_STRING)
            .str(b"user:1")
            .str(b"hello")
            .eof();

        let mut parser = RdbParser::new(data.as_slice()).expect("valid header");
        assert_eq!(parser.rdb_version(), 11);
        let entry = parser.next_entry().expect("parse").expect("one entry");
        assert_eq!(entry.key, "user:1");
        assert_eq!(entry.key_type, "string");
        assert_eq!(entry.db, 2);
        assert_eq!(entry.expire_at_ms, Some(2_000_000_000_000));
        // Expire opcode (1+8) + type (1) + key (1+6) + value (1+5) = 23.
        assert_eq!(entry.serialized_bytes, 23);
        assert!(parser.next_entry().expect("eof").is_none());
        assert_eq!(parser.aux(), &[("redis-ver".to_string(), "7.2.5".to_string())]);
    }

    #[test]
    fn parses_int_encoded_and_collection_values() {
        let data = Fixture::new()
            // int16-encoded string value
            .op(T_STRING)
            .str(b"count")
            .byte(0xC1)
            .bytes(&300i16.to_le_bytes())
            // hash with two field/value pairs
            .op(T_HASH)
            .str(b"h")
            .byte(2)
            .str(b"f1")
            .str(b"v1")
            .str(b"f2")
            .str(b"v2")
            // zset_2 with binary double
            .op(T_ZSET_2)
            .str(b"z")
            .byte(1)
            .str(b"member")
            .bytes(&1.5f64.to_le_bytes())
            // quicklist2 list: one packed node
            .op(T_LIST_QUICKLIST_2)
            .str(b"l")
            .byte(1)
            .byte(2) // container = packed
            .str(b"blob")
            .eof();

        let entries = parse_all(&data);
        let summary: Vec<(&str, &str)> = entries.iter().map(|e| (e.key.as_str(), e.key_type)).collect();
        assert_eq!(
            summary,
            vec![("count", "string"), ("h", "hash"), ("z", "zset"), ("l", "list")]
        );
        assert!(entries.iter().all(|e| e.expire_at_ms.is_none()));
        assert!(entries.iter().all(|e| e.db == 0));
    }

    #[test]
    fn decompresses_lzf_keys() {
        // "aaaaaaaaaaaaaaaa" (16 × 'a'): literal 'a' + back-reference.
        // ctrl=0 (1 literal), 'a', then len=13 ctrl -> (13+2)=15 copied.
        // ctrl 0xE0: len bits = 7 (extended), ext byte 0x06 -> len 13 -> 15 copied.
        let compressed = [0x00, b'a', 0xE0, 0x06, 0x00];
        // Verify against the reference decompressor first.
        let expect_err = lzf_decompress(&compressed, 16);
        // 0xE0 means len=7 extended; ext byte 0x06 -> len 13 -> copies 15.
        assert!(expect_err.is_ok(), "fixture must decode: {expect_err:?}");
        assert_eq!(expect_err.expect("decoded"), b"a".repeat(16));

        let mut data = Fixture::new().op(T_STRING).0;
        // LZF-encoded key: 0xC3, compressed len, uncompressed len, payload.
        data.push(0xC3);
        data.push(compressed.len() as u8);
        data.push(16);
        data.extend_from_slice(&compressed);
        // value
        data.push(1);
        data.push(b'x');
        data.push(OP_EOF);
        data.extend_from_slice(&[0u8; 8]);

        let entries = parse_all(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "a".repeat(16));
    }

    #[test]
    fn skips_stream_and_module_values() {
        let data = Fixture::new()
            // stream v3, empty (0 listpacks, 0 groups)
            .op(T_STREAM_LISTPACKS_3)
            .str(b"st")
            .byte(0) // listpacks
            .byte(0) // items
            .byte(0) // last ms
            .byte(0) // last seq
            .byte(0) // first ms
            .byte(0) // first seq
            .byte(0) // max deleted ms
            .byte(0) // max deleted seq
            .byte(0) // entries added
            .byte(0) // groups
            // module_2 value: id + string opcode + eof opcode
            .op(T_MODULE_2)
            .str(b"m")
            .byte(9) // module id (6-bit length form)
            .byte(MODULE_OP_STRING as u8)
            .str(b"payload")
            .byte(MODULE_OP_EOF as u8)
            .eof();

        let entries = parse_all(&data);
        let summary: Vec<(&str, &str)> = entries.iter().map(|e| (e.key.as_str(), e.key_type)).collect();
        assert_eq!(summary, vec![("st", "stream"), ("m", "module")]);
    }

    #[test]
    fn fourteen_bit_and_32_bit_lengths() {
        let data = Fixture::new()
            .op(OP_SELECTDB)
            .bytes(&[0x40 | 0x01, 0x2C]) // 14-bit: 300
            .eof();
        let mut parser = RdbParser::new(data.as_slice()).expect("header");
        assert!(parser.next_entry().expect("parse").is_none());
        assert_eq!(parser.current_db, 300);

        let mut data = Fixture::new().op(OP_SELECTDB).byte(0x80).0;
        data.extend_from_slice(&70000u32.to_be_bytes());
        data.push(OP_EOF);
        data.extend_from_slice(&[0u8; 8]);
        let mut parser = RdbParser::new(data.as_slice()).expect("header");
        assert!(parser.next_entry().expect("parse").is_none());
        assert_eq!(parser.current_db, 70000);
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(RdbParser::new(&b"NOTREDIS0"[..]).is_err());

        // Header + type byte + key, then the file just stops.
        let truncated = Fixture::new().op(T_STRING).str(b"k").0;
        let mut parser = RdbParser::new(truncated.as_slice()).expect("header ok");
        assert!(parser.next_entry().is_err());
    }

    /// Smoke test against a real dump: `ZEDIS_RDB_SMOKE=/path/to/dump.rdb
    /// cargo test -p zedis-core rdb -- --ignored`. Kept out of the normal
    /// run because it needs a locally generated file.
    #[test]
    #[ignore = "needs ZEDIS_RDB_SMOKE=<path to dump.rdb>"]
    fn smoke_parses_real_dump() {
        let path = std::env::var("ZEDIS_RDB_SMOKE").expect("set ZEDIS_RDB_SMOKE to a dump.rdb path");
        let file = std::fs::File::open(&path).expect("open dump");
        let mut parser = RdbParser::new(std::io::BufReader::new(file)).expect("valid header");
        let mut count = 0usize;
        let mut types = std::collections::HashSet::new();
        while let Some(entry) = parser.next_entry().expect("parse entry") {
            assert!(!entry.key.is_empty());
            assert!(entry.serialized_bytes > 0);
            types.insert(entry.key_type);
            count += 1;
        }
        println!(
            "parsed {count} keys, types: {types:?}, rdb v{}, aux: {:?}",
            parser.rdb_version(),
            parser.aux()
        );
        assert!(count > 0, "dump should contain keys");
    }

    #[test]
    fn old_expiry_seconds_scale_to_ms() {
        let data = Fixture::new()
            .op(OP_EXPIRETIME)
            .bytes(&1_700_000_000u32.to_le_bytes())
            .op(T_STRING)
            .str(b"k")
            .str(b"v")
            .eof();
        let entries = parse_all(&data);
        assert_eq!(entries[0].expire_at_ms, Some(1_700_000_000_000));
    }
}

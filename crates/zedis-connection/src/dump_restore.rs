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

//! Framed dump/restore codec and Redis I/O helpers.
//!
//! File layout (little-endian throughout):
//!
//! ```text
//! "ZDIS"          (4 bytes)  magic
//! u16             format version
//! u16             flags (reserved)
//! u32             header_len
//! header_len B    header JSON
//! repeated entries:
//!   u32           key_len
//!   key_len B     key bytes (raw)
//!   i64           pttl_ms (-1 = no TTL)
//!   u8            type_hint (0=unknown,1=string,2=list,3=set,4=zset,5=hash,6=stream)
//!   u32           payload_len
//!   payload_len B payload bytes (Redis DUMP output)
//! "ZEND"          (4 bytes)  footer magic
//! u32             CRC32 over every byte before the footer magic
//! ```

use super::async_connection::RedisAsyncConn;
use super::manager::get_connection_manager;
use crate::error::Error;
use futures::future::try_join_all;
use redis::cmd;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) const MAGIC_HEADER: &[u8; 4] = b"ZDIS";
const MAGIC_FOOTER: &[u8; 4] = b"ZEND";
const FORMAT_VERSION: u16 = 1;
const MAX_HEADER_LEN: u32 = 64 * 1024;
const MAX_KEY_LEN: u32 = 64 * 1024;
const MAX_PAYLOAD_LEN: u32 = 512 * 1024 * 1024;

/// Type hint stored alongside each entry. Display-only; restore does not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TypeHint {
    Unknown = 0,
    String = 1,
    List = 2,
    Set = 3,
    ZSet = 4,
    Hash = 5,
    Stream = 6,
}

impl TypeHint {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::String,
            2 => Self::List,
            3 => Self::Set,
            4 => Self::ZSet,
            5 => Self::Hash,
            6 => Self::Stream,
            _ => Self::Unknown,
        }
    }

    fn from_redis_type(s: &str) -> Self {
        match s {
            "string" => Self::String,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::ZSet,
            "hash" => Self::Hash,
            "stream" => Self::Stream,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpHeader {
    pub format_version: u16,
    pub source_server_name: String,
    pub source_redis_version: String,
    pub source_db: u32,
    /// 0 means unknown when streaming.
    pub key_count: u64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DumpEntry {
    pub key: Vec<u8>,
    /// -1 means no TTL.
    pub pttl_ms: i64,
    pub type_hint: TypeHint,
    pub payload: Vec<u8>,
}

/// What to do when `RESTORE` hits an existing key (`BUSYKEY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictMode {
    /// Leave the destination key unchanged (default).
    #[default]
    Skip,
    /// `RESTORE … REPLACE` — overwrite the destination.
    Overwrite,
    /// Stop the whole import on the first conflict.
    Abort,
}

impl ConflictMode {
    pub const ALL: [ConflictMode; 3] = [ConflictMode::Skip, ConflictMode::Overwrite, ConflictMode::Abort];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(ConflictMode::Skip)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&m| m == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreStatus {
    Written,
    Skipped,
    Failed(String),
}

// ---------------------------------------------------------------------------
// CRC32 (IEEE 802.3 polynomial 0xEDB88320), matching gzip / RFC 1952.
// Inline so we avoid an extra dependency.
// ---------------------------------------------------------------------------

static CRC32_TABLE: LazyLock<[u32; 256]> = LazyLock::new(|| {
    let mut table = [0u32; 256];
    let mut i: u32 = 0;
    while i < 256 {
        let mut c = i;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            j += 1;
        }
        table[i as usize] = c;
        i += 1;
    }
    table
});

#[derive(Clone, Copy)]
struct Crc32 {
    state: u32,
}

impl Crc32 {
    fn new() -> Self {
        Self { state: 0xFFFFFFFF }
    }

    fn update(&mut self, buf: &[u8]) {
        let table = &*CRC32_TABLE;
        let mut s = self.state;
        for &b in buf {
            s = (s >> 8) ^ table[((s ^ b as u32) & 0xFF) as usize];
        }
        self.state = s;
    }

    fn finalize(self) -> u32 {
        self.state ^ 0xFFFFFFFF
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

pub struct DumpWriter<W: Write> {
    inner: W,
    crc: Crc32,
}

impl<W: Write> DumpWriter<W> {
    pub fn new(inner: W, header: &DumpHeader) -> Result<Self> {
        let mut writer = Self {
            inner,
            crc: Crc32::new(),
        };
        let header_json = serde_json::to_vec(header)?;
        let header_len = u32::try_from(header_json.len()).map_err(|_| Error::Invalid {
            message: "dump header is too large".to_string(),
        })?;
        if header_len > MAX_HEADER_LEN {
            return Err(Error::Invalid {
                message: format!("dump header exceeds {MAX_HEADER_LEN} bytes"),
            });
        }
        writer.write_all_crc(MAGIC_HEADER)?;
        writer.write_all_crc(&FORMAT_VERSION.to_le_bytes())?;
        writer.write_all_crc(&0u16.to_le_bytes())?; // flags
        writer.write_all_crc(&header_len.to_le_bytes())?;
        writer.write_all_crc(&header_json)?;
        Ok(writer)
    }

    pub fn write_entry(&mut self, entry: &DumpEntry) -> Result<()> {
        let key_len = u32::try_from(entry.key.len()).map_err(|_| Error::Invalid {
            message: "key length overflows u32".to_string(),
        })?;
        if key_len > MAX_KEY_LEN {
            return Err(Error::Invalid {
                message: format!("key exceeds {MAX_KEY_LEN} bytes"),
            });
        }
        let payload_len = u32::try_from(entry.payload.len()).map_err(|_| Error::Invalid {
            message: "payload length overflows u32".to_string(),
        })?;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::Invalid {
                message: format!("payload exceeds {MAX_PAYLOAD_LEN} bytes"),
            });
        }
        self.write_all_crc(&key_len.to_le_bytes())?;
        self.write_all_crc(&entry.key)?;
        self.write_all_crc(&entry.pttl_ms.to_le_bytes())?;
        self.write_all_crc(&[entry.type_hint as u8])?;
        self.write_all_crc(&payload_len.to_le_bytes())?;
        self.write_all_crc(&entry.payload)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        let crc_value = self.crc.finalize();
        self.inner.write_all(MAGIC_FOOTER)?;
        self.inner.write_all(&crc_value.to_le_bytes())?;
        self.inner.flush()?;
        Ok(self.inner)
    }

    fn write_all_crc(&mut self, buf: &[u8]) -> Result<()> {
        self.inner.write_all(buf)?;
        self.crc.update(buf);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub struct DumpReader<R: Read> {
    inner: R,
    crc: Crc32,
    header: DumpHeader,
    finished: bool,
}

impl<R: Read> DumpReader<R> {
    pub fn open(inner: R) -> Result<Self> {
        let mut reader = Self {
            inner,
            crc: Crc32::new(),
            header: DumpHeader {
                format_version: 0,
                source_server_name: String::new(),
                source_redis_version: String::new(),
                source_db: 0,
                key_count: 0,
                created_at_ms: 0,
            },
            finished: false,
        };
        let mut magic = [0u8; 4];
        reader.read_exact_crc(&mut magic)?;
        if &magic != MAGIC_HEADER {
            return Err(Error::Invalid {
                message: "not a zedis dump file".to_string(),
            });
        }
        let mut version_buf = [0u8; 2];
        reader.read_exact_crc(&mut version_buf)?;
        let version = u16::from_le_bytes(version_buf);
        if version > FORMAT_VERSION {
            return Err(Error::Invalid {
                message: format!("unsupported dump format version {version}"),
            });
        }
        let mut flags_buf = [0u8; 2];
        reader.read_exact_crc(&mut flags_buf)?;
        let mut header_len_buf = [0u8; 4];
        reader.read_exact_crc(&mut header_len_buf)?;
        let header_len = u32::from_le_bytes(header_len_buf);
        if header_len > MAX_HEADER_LEN {
            return Err(Error::Invalid {
                message: format!("dump header exceeds {MAX_HEADER_LEN} bytes"),
            });
        }
        let mut header_bytes = vec![0u8; header_len as usize];
        reader.read_exact_crc(&mut header_bytes)?;
        reader.header = serde_json::from_slice(&header_bytes)?;
        Ok(reader)
    }

    pub fn header(&self) -> &DumpHeader {
        &self.header
    }

    /// Reads the next entry. Returns `Ok(None)` when the footer was consumed and CRC verified.
    pub fn read_entry(&mut self) -> Result<Option<DumpEntry>> {
        if self.finished {
            return Ok(None);
        }
        let mut head = [0u8; 4];
        self.inner.read_exact(&mut head)?;
        if &head == MAGIC_FOOTER {
            // Footer bytes are not part of the CRC payload.
            let mut crc_buf = [0u8; 4];
            self.inner.read_exact(&mut crc_buf)?;
            let stored = u32::from_le_bytes(crc_buf);
            let computed = self.crc.finalize();
            if stored != computed {
                return Err(Error::Invalid {
                    message: "dump file CRC mismatch".to_string(),
                });
            }
            self.finished = true;
            return Ok(None);
        }
        // Not a footer; treat as the key_len of the next entry.
        self.crc.update(&head);
        let key_len = u32::from_le_bytes(head);
        if key_len > MAX_KEY_LEN {
            return Err(Error::Invalid {
                message: format!("key length {key_len} exceeds limit"),
            });
        }
        let mut key = vec![0u8; key_len as usize];
        self.read_exact_crc(&mut key)?;
        let mut pttl_buf = [0u8; 8];
        self.read_exact_crc(&mut pttl_buf)?;
        let pttl_ms = i64::from_le_bytes(pttl_buf);
        let mut type_buf = [0u8; 1];
        self.read_exact_crc(&mut type_buf)?;
        let type_hint = TypeHint::from_u8(type_buf[0]);
        let mut payload_len_buf = [0u8; 4];
        self.read_exact_crc(&mut payload_len_buf)?;
        let payload_len = u32::from_le_bytes(payload_len_buf);
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::Invalid {
                message: format!("payload length {payload_len} exceeds limit"),
            });
        }
        let mut payload = vec![0u8; payload_len as usize];
        self.read_exact_crc(&mut payload)?;
        Ok(Some(DumpEntry {
            key,
            pttl_ms,
            type_hint,
            payload,
        }))
    }

    /// True after `read_entry` returned `None` (footer consumed and verified).
    #[allow(dead_code)]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn read_exact_crc(&mut self, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buf)?;
        self.crc.update(buf);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Redis I/O
// ---------------------------------------------------------------------------

/// Dumps a slice of keys with bounded concurrency. Missing or expired keys are skipped.
pub async fn dump_keys_chunk(conn: &mut RedisAsyncConn, keys: &[String]) -> Result<Vec<DumpEntry>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let futures = keys.iter().map(|key| {
        let mut c = conn.clone();
        let key = key.clone();
        async move { dump_single_key(&mut c, key).await }
    });
    let results = try_join_all(futures).await?;
    Ok(results.into_iter().flatten().collect())
}

async fn dump_single_key(conn: &mut RedisAsyncConn, key: String) -> Result<Option<DumpEntry>> {
    let key_str = key.as_str();
    // All three commands target the same key, so they hit the same cluster slot —
    // safe to pipeline. Folding them into one round-trip is ~3x faster than three
    // sequential awaits.
    let (pttl, ty, payload): (i64, String, Option<Vec<u8>>) = redis::pipe()
        .cmd("PTTL")
        .arg(key_str)
        .cmd("TYPE")
        .arg(key_str)
        .cmd("DUMP")
        .arg(key_str)
        .query_async(conn)
        .await?;
    // DUMP returning nil is the authoritative "key gone" signal; PTTL == -2 / TYPE == "none"
    // can race against expiration but DUMP cannot lie about whether it produced bytes.
    let Some(payload) = payload else {
        return Ok(None);
    };
    Ok(Some(DumpEntry {
        key: key_str.as_bytes().to_vec(),
        pttl_ms: if pttl < 0 { -1 } else { pttl },
        type_hint: TypeHint::from_redis_type(&ty),
        payload,
    }))
}

/// Restores a slice of entries. Concurrency is bounded by the slice length.
pub async fn restore_keys_chunk(
    conn: &mut RedisAsyncConn,
    entries: &[DumpEntry],
    conflict: ConflictMode,
) -> Result<Vec<RestoreStatus>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let futures = entries.iter().map(|entry| {
        let mut c = conn.clone();
        let entry = entry.clone();
        async move { restore_single_key(&mut c, entry, conflict).await }
    });
    try_join_all(futures).await
}

async fn restore_single_key(
    conn: &mut RedisAsyncConn,
    entry: DumpEntry,
    conflict: ConflictMode,
) -> Result<RestoreStatus> {
    let ttl_arg: i64 = if entry.pttl_ms < 0 { 0 } else { entry.pttl_ms };
    let mut command = cmd("RESTORE");
    command.arg(&entry.key).arg(ttl_arg).arg(&entry.payload);
    if matches!(conflict, ConflictMode::Overwrite) {
        command.arg("REPLACE");
    }
    match command.query_async::<()>(conn).await {
        Ok(()) => Ok(RestoreStatus::Written),
        Err(err) => {
            // Redis returns `BUSYKEY Target key name already exists.` when a key is present
            // and REPLACE wasn't supplied.
            let msg = err.to_string();
            if msg.contains("BUSYKEY") {
                match conflict {
                    ConflictMode::Skip => Ok(RestoreStatus::Skipped),
                    ConflictMode::Abort => Err(Error::Invalid {
                        message: format!(
                            "key {} already exists at destination",
                            String::from_utf8_lossy(&entry.key)
                        ),
                    }),
                    ConflictMode::Overwrite => Ok(RestoreStatus::Failed(msg)),
                }
            } else {
                Ok(RestoreStatus::Failed(msg))
            }
        }
    }
}

/// Batch `EXISTS` for binary key names. Returns one bool per key (order preserved).
pub async fn keys_exist(conn: &mut RedisAsyncConn, keys: &[Vec<u8>]) -> Result<Vec<bool>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut pipe = redis::pipe();
    for key in keys {
        pipe.cmd("EXISTS").arg(key.as_slice());
    }
    let results: Vec<i64> = pipe.query_async(conn).await?;
    Ok(results.into_iter().map(|n| n > 0).collect())
}

/// Dry-run import conflict scan: read every key name from a dump file and
/// check `EXISTS` on the destination. Does **not** write. `sample_limit`
/// caps how many conflicting key names are retained for the UI list.
pub async fn preview_dump_conflicts(
    server_id: &str,
    db: usize,
    input_path: PathBuf,
    sample_limit: usize,
    cancel: &AtomicBool,
) -> Result<ConflictPreview> {
    use std::fs::File;
    use std::io::BufReader;

    let client = get_connection_manager().get_client(server_id, db).await?;
    let mut conn = client.connection();

    let path_for_open = input_path.clone();
    let mut reader = smol::unblock(move || -> Result<DumpReader<BufReader<File>>> {
        let file = File::open(&path_for_open)?;
        DumpReader::open(BufReader::new(file))
    })
    .await?;

    let mut total = 0u64;
    let mut conflicting = 0u64;
    let mut free = 0u64;
    let mut sample: Vec<String> = Vec::new();
    const BATCH: usize = 64;
    type KeyBatch = (DumpReader<BufReader<File>>, Vec<Vec<u8>>, bool);

    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let (returned, batch, eof) = smol::unblock(move || -> Result<KeyBatch> {
            let mut batch = Vec::with_capacity(BATCH);
            let mut eof = false;
            while batch.len() < BATCH {
                match reader.read_entry()? {
                    Some(entry) => batch.push(entry.key),
                    None => {
                        eof = true;
                        break;
                    }
                }
            }
            Ok((reader, batch, eof))
        })
        .await?;
        reader = returned;
        if batch.is_empty() {
            break;
        }
        let exists = keys_exist(&mut conn, &batch).await?;
        for (key, is_there) in batch.into_iter().zip(exists) {
            total += 1;
            if is_there {
                conflicting += 1;
                if sample.len() < sample_limit {
                    sample.push(String::from_utf8_lossy(&key).into_owned());
                }
            } else {
                free += 1;
            }
        }
        if eof {
            break;
        }
    }

    Ok(ConflictPreview {
        total,
        conflicting,
        free,
        sample_keys: sample,
        cancelled: cancel.load(Ordering::Acquire),
    })
}

/// Result of a dry-run conflict scan against a dump file.
#[derive(Debug, Clone, Default)]
pub struct ConflictPreview {
    pub total: u64,
    pub conflicting: u64,
    pub free: u64,
    /// First N conflicting key names for the UI list.
    pub sample_keys: Vec<String>,
    pub cancelled: bool,
}

/// Copy a single key's value (and remaining TTL) to another server / db
/// via `DUMP` on the source and `RESTORE` on the target. Source and target
/// may be the same server (e.g. a cross-db copy). Returns `Ok(None)` when
/// the source key no longer exists, otherwise the restore outcome.
pub async fn copy_key(
    source_id: String,
    source_db: usize,
    target_id: String,
    target_db: usize,
    key: String,
    conflict: ConflictMode,
) -> Result<Option<RestoreStatus>> {
    let mut src = super::get_connection_manager()
        .get_connection(&source_id, source_db)
        .await?;
    let entries = dump_keys_chunk(&mut src, std::slice::from_ref(&key)).await?;
    let Some(entry) = entries.into_iter().next() else {
        return Ok(None);
    };
    let mut dst = super::get_connection_manager()
        .get_connection(&target_id, target_db)
        .await?;
    let mut statuses = restore_keys_chunk(&mut dst, std::slice::from_ref(&entry), conflict).await?;
    Ok(statuses.pop())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_header() -> DumpHeader {
        DumpHeader {
            format_version: FORMAT_VERSION,
            source_server_name: "local".to_string(),
            source_redis_version: "7.4.0".to_string(),
            source_db: 0,
            key_count: 2,
            created_at_ms: 1_700_000_000_000,
        }
    }

    fn sample_entries() -> Vec<DumpEntry> {
        vec![
            DumpEntry {
                key: b"hello".to_vec(),
                pttl_ms: -1,
                type_hint: TypeHint::String,
                payload: vec![0x00, 0x05, b'w', b'o', b'r', b'l', b'd'],
            },
            DumpEntry {
                key: vec![0xFF, 0x00, 0x10, 0x80], // non-utf8 raw bytes
                pttl_ms: 60_000,
                type_hint: TypeHint::Hash,
                payload: (0..1024u32).flat_map(|n| n.to_le_bytes()).collect(),
            },
        ]
    }

    fn write_to_buf(header: &DumpHeader, entries: &[DumpEntry]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = DumpWriter::new(&mut buf, header).expect("writer init");
        for e in entries {
            w.write_entry(e).expect("write entry");
        }
        let _ = w.finish().expect("finish");
        buf
    }

    fn read_from_buf(buf: &[u8]) -> (DumpHeader, Vec<DumpEntry>) {
        let mut r = DumpReader::open(Cursor::new(buf)).expect("reader open");
        let header = r.header().clone();
        let mut out = Vec::new();
        while let Some(entry) = r.read_entry().expect("read entry") {
            out.push(entry);
        }
        assert!(r.is_finished());
        (header, out)
    }

    #[test]
    fn roundtrip_basic() {
        let header = sample_header();
        let entries = sample_entries();
        let buf = write_to_buf(&header, &entries);
        let (got_header, got_entries) = read_from_buf(&buf);
        assert_eq!(got_header.format_version, header.format_version);
        assert_eq!(got_header.source_redis_version, header.source_redis_version);
        assert_eq!(got_header.key_count, header.key_count);
        assert_eq!(got_entries.len(), entries.len());
        for (a, b) in got_entries.iter().zip(entries.iter()) {
            assert_eq!(a.key, b.key);
            assert_eq!(a.pttl_ms, b.pttl_ms);
            assert_eq!(a.type_hint, b.type_hint);
            assert_eq!(a.payload, b.payload);
        }
    }

    #[test]
    fn roundtrip_empty() {
        let header = sample_header();
        let buf = write_to_buf(&header, &[]);
        let (_h, got) = read_from_buf(&buf);
        assert!(got.is_empty());
    }

    #[test]
    fn rejects_truncated_file() {
        let buf = write_to_buf(&sample_header(), &sample_entries());
        // Drop the last 8 bytes (CRC + part of magic).
        let truncated = &buf[..buf.len() - 8];
        let mut r = DumpReader::open(Cursor::new(truncated)).expect("header still intact");
        let mut hit_eof = false;
        loop {
            match r.read_entry() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => {
                    hit_eof = true;
                    break;
                }
            }
        }
        assert!(hit_eof, "truncated file must fail before footer is verified");
    }

    #[test]
    fn rejects_corrupted_payload() {
        let mut buf = write_to_buf(&sample_header(), &sample_entries());
        // Flip a byte in the middle (after the header, inside an entry payload).
        let mid = buf.len() / 2;
        buf[mid] ^= 0xFF;
        let mut r = DumpReader::open(Cursor::new(&buf)).expect("header is fine");
        let mut crc_failed = false;
        loop {
            match r.read_entry() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    crc_failed = e.to_string().contains("CRC") || e.to_string().contains("exceeds");
                    break;
                }
            }
        }
        assert!(crc_failed, "corrupted payload must surface as a CRC or bounds error");
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut buf = write_to_buf(&sample_header(), &[]);
        buf[0] = b'X';
        match DumpReader::open(Cursor::new(&buf)) {
            Ok(_) => panic!("bad magic must fail"),
            Err(e) => assert!(e.to_string().to_lowercase().contains("zedis")),
        }
    }

    #[test]
    fn crc32_known_vector() {
        // CRC32-IEEE("123456789") == 0xCBF43926
        let mut c = Crc32::new();
        c.update(b"123456789");
        assert_eq!(c.finalize(), 0xCBF43926);
    }
}

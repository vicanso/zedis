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

//! A minimal ZIP writer (deflate entries, no zip64) for the diagnostics
//! bundle — a few KB of code on top of the `flate2` we already ship, instead
//! of a `zip` dependency. Readers everywhere (Finder, Explorer, `unzip`,
//! GitHub's issue attachments) open what it writes.

use chrono::{Datelike, Local, Timelike};
use flate2::{Compression, Crc, write::DeflateEncoder};
use std::io::Write;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL_DIR: u32 = 0x0605_4b50;
const VERSION_DEFLATE: u16 = 20;
const METHOD_DEFLATE: u16 = 8;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    compressed_size: u32,
    size: u32,
    offset: u32,
    time: u16,
    date: u16,
}

/// Builds an archive in memory; `finish` hands back the bytes to write.
#[derive(Default)]
pub struct ZipWriter {
    bytes: Vec<u8>,
    entries: Vec<Entry>,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one file. `name` uses `/` separators (`logs/zedis.log.2026-08-22`).
    pub fn add(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let compressed = encoder.finish()?;
        let mut crc = Crc::new();
        crc.update(data);
        let (time, date) = dos_time_now();
        let entry = Entry {
            name: name.as_bytes().to_vec(),
            crc: crc.sum(),
            compressed_size: compressed.len() as u32,
            size: data.len() as u32,
            offset: self.bytes.len() as u32,
            time,
            date,
        };
        self.put_u32(LOCAL_HEADER);
        self.put_u16(VERSION_DEFLATE);
        self.put_u16(0); // flags
        self.put_u16(METHOD_DEFLATE);
        self.put_u16(entry.time);
        self.put_u16(entry.date);
        self.put_u32(entry.crc);
        self.put_u32(entry.compressed_size);
        self.put_u32(entry.size);
        self.put_u16(entry.name.len() as u16);
        self.put_u16(0); // extra length
        self.bytes.extend_from_slice(&entry.name);
        self.bytes.extend_from_slice(&compressed);
        self.entries.push(entry);
        Ok(())
    }

    /// Appends the central directory and returns the finished archive.
    pub fn finish(mut self) -> Vec<u8> {
        let central_offset = self.bytes.len() as u32;
        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            self.put_u32(CENTRAL_HEADER);
            self.put_u16(VERSION_DEFLATE); // made by
            self.put_u16(VERSION_DEFLATE); // needed
            self.put_u16(0); // flags
            self.put_u16(METHOD_DEFLATE);
            self.put_u16(entry.time);
            self.put_u16(entry.date);
            self.put_u32(entry.crc);
            self.put_u32(entry.compressed_size);
            self.put_u32(entry.size);
            self.put_u16(entry.name.len() as u16);
            self.put_u16(0); // extra
            self.put_u16(0); // comment
            self.put_u16(0); // disk
            self.put_u16(0); // internal attrs
            self.put_u32(0); // external attrs
            self.put_u32(entry.offset);
            self.bytes.extend_from_slice(&entry.name);
        }
        let central_size = self.bytes.len() as u32 - central_offset;
        self.put_u32(END_OF_CENTRAL_DIR);
        self.put_u16(0); // this disk
        self.put_u16(0); // central dir disk
        self.put_u16(entries.len() as u16);
        self.put_u16(entries.len() as u16);
        self.put_u32(central_size);
        self.put_u32(central_offset);
        self.put_u16(0); // comment
        self.bytes
    }

    fn put_u16(&mut self, v: u16) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn put_u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }
}

/// MS-DOS time/date fields (2-second resolution, years from 1980).
fn dos_time_now() -> (u16, u16) {
    let now = Local::now();
    let time = ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    let year = (now.year().clamp(1980, 2107) - 1980) as u16;
    let date = (year << 9) | ((now.month() as u16) << 5) | now.day() as u16;
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::DeflateDecoder;
    use std::io::Read;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    /// Walks the local headers like a reader would and inflates each entry.
    fn read_entries(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pos = 0;
        while u32_at(archive, pos) == LOCAL_HEADER {
            let crc = u32_at(archive, pos + 14);
            let csize = u32_at(archive, pos + 18) as usize;
            let size = u32_at(archive, pos + 22) as usize;
            let name_len = u16_at(archive, pos + 26) as usize;
            let name = String::from_utf8(archive[pos + 30..pos + 30 + name_len].to_vec()).expect("utf8 name");
            let data_start = pos + 30 + name_len;
            let mut data = Vec::new();
            DeflateDecoder::new(&archive[data_start..data_start + csize])
                .read_to_end(&mut data)
                .expect("inflate");
            assert_eq!(data.len(), size);
            let mut check = Crc::new();
            check.update(&data);
            assert_eq!(check.sum(), crc, "crc of {name}");
            out.push((name, data));
            pos = data_start + csize;
        }
        out
    }

    #[test]
    fn entries_round_trip_and_the_directory_is_consistent() {
        let mut zip = ZipWriter::new();
        let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        zip.add("summary.txt", b"hello").expect("add");
        zip.add("logs/zedis.log.2026-08-22", &big).expect("add");
        zip.add("empty", b"").expect("add");
        let archive = zip.finish();

        let entries = read_entries(&archive);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("summary.txt".to_string(), b"hello".to_vec()));
        assert_eq!(entries[1].0, "logs/zedis.log.2026-08-22");
        assert_eq!(entries[1].1, big);
        assert_eq!(entries[2].1, Vec::<u8>::new());
        // Deflate actually compressed the repetitive payload.
        assert!(archive.len() < big.len() / 2);

        // End-of-central-directory record: count, size and offset agree
        // with where the central directory really starts.
        let eocd = archive.len() - 22;
        assert_eq!(u32_at(&archive, eocd), END_OF_CENTRAL_DIR);
        assert_eq!(u16_at(&archive, eocd + 10), 3);
        let central_offset = u32_at(&archive, eocd + 16) as usize;
        assert_eq!(u32_at(&archive, central_offset), CENTRAL_HEADER);
        let central_size = u32_at(&archive, eocd + 12) as usize;
        assert_eq!(central_offset + central_size, eocd);
    }
}

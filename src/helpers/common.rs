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

use crate::constants::KEY_TREE_MAX_WIDTH;
use crate::constants::KEY_TREE_MIN_WIDTH;
use crate::error::Error;
use gpui::Pixels;
use ruzstd::decoding::StreamingDecoder;
use std::io::Read;

type Result<T, E = Error> = std::result::Result<T, E>;

pub fn get_key_tree_widths(width: Pixels) -> (Pixels, Pixels, Pixels) {
    let min_width = KEY_TREE_MIN_WIDTH;
    let max_width = KEY_TREE_MAX_WIDTH;
    (width.max(min_width), min_width, max_width)
}

pub fn decompress_zstd(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = StreamingDecoder::new(bytes).map_err(|e| Error::Invalid { message: e.to_string() })?;
    let mut decompressed_vec = Vec::with_capacity(bytes.len());
    decoder
        .read_to_end(&mut decompressed_vec)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    Ok(decompressed_vec)
}

/// Compact human form for replication lag in bytes. Drops the unit when zero
/// so healthy replicas don't carry "0 B" noise. (Status bar and Topology.)
pub fn format_lag_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0".into();
    }
    humansize::format_size(bytes as u64, humansize::FormatSizeOptions::default().decimal_places(1))
}

/// Tiny stable hash so element IDs derived from a name compile to `u32`
/// (`ElementId` only accepts primitive tuple seconds).
pub fn djb2_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

/// Split a multi-line KEYS / ARGV field into trimmed non-empty entries.
pub fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_small_shared_helpers_do_what_their_copies_did() {
        assert_eq!(format_lag_bytes(0), "0");
        assert_eq!(format_lag_bytes(-5), "0");
        assert_eq!(format_lag_bytes(1500), "1.5kB");
        assert_eq!(parse_lines(" a \n\n  b\n"), vec!["a".to_string(), "b".to_string()]);
        // Stable across runs and platforms: it names UI elements.
        assert_eq!(djb2_hash(""), 5381);
        assert_eq!(djb2_hash("a"), 5381u32.wrapping_mul(33).wrapping_add(97));
        assert_ne!(djb2_hash("lib-a"), djb2_hash("lib-b"));
    }
}

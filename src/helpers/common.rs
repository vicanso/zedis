// Copyright 2025 Tree xie.
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
use gpui::{Pixels, px};
use ruzstd::decoding::StreamingDecoder;
use std::io::Read;

type Result<T, E = Error> = std::result::Result<T, E>;

pub fn get_key_tree_widths(width: Pixels) -> (Pixels, Pixels, Pixels) {
    let min_width = px(KEY_TREE_MIN_WIDTH);
    let max_width = px(KEY_TREE_MAX_WIDTH);
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

#[inline]
pub fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

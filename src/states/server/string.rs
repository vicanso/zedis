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

use super::value::{DataFormat, RedisBytesValue, detect_format};
use crate::db::{ProtoManager, ScriptManager};
use crate::helpers::decompress_zstd;
use crate::{connection::RedisAsyncConn, error::Error};
use bytes::Bytes;
use chrono::{DateTime, Local};
use flate2::read::GzDecoder;
use gpui::SharedString;
use lz4_flex::block::decompress_size_prepended;
use redis::cmd;
use serde_json::Value;
use snap::read::FrameDecoder;
use std::io::Read;
use tracing::warn;

type Result<T, E = Error> = std::result::Result<T, E>;

fn truncate_long_strings(max_truncate_length: usize, v: &mut Value, truncated: &mut bool) {
    match v {
        Value::String(s) if s.len() > max_truncate_length => {
            let char_count = s.chars().count();
            if char_count > max_truncate_length {
                let mut new_s: String = s.chars().take(max_truncate_length).collect();
                new_s.push_str(&format!("...(Total {} chars, content hidden)", char_count));
                *s = new_s;
                *truncated = true;
            }
        }
        Value::Array(arr) => {
            for item in arr {
                truncate_long_strings(max_truncate_length, item, truncated);
            }
        }
        Value::Object(map) => {
            for val in map.values_mut() {
                truncate_long_strings(max_truncate_length, val, truncated);
            }
        }
        _ => {}
    }
}

/// Attempts to format a string as pretty-printed JSON.
/// Returns None if the string is not valid JSON or doesn't look like JSON.
fn pretty_json(value: &str, max_truncate_length: usize) -> Option<(SharedString, bool)> {
    let trimmed = value.trim();
    if !((trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']'))) {
        return None;
    }
    let mut json_value = serde_json::from_str::<Value>(value).ok()?;
    let mut truncated = false;
    truncate_long_strings(max_truncate_length, &mut json_value, &mut truncated);
    let pretty_str = serde_json::to_string_pretty(&json_value).ok()?;

    Some((pretty_str.into(), truncated))
}

fn format_text(data: &[u8], max_truncate_length: usize) -> Option<(DataFormat, SharedString)> {
    match std::str::from_utf8(data) {
        Ok(s) => {
            if let Some((pretty, truncated)) = pretty_json(s, max_truncate_length) {
                let format = if truncated {
                    DataFormat::Preview
                } else {
                    DataFormat::Json
                };
                Some((format, pretty))
            } else {
                Some((DataFormat::Text, s.to_string().into()))
            }
        }
        Err(_) => None,
    }
}

pub fn detect_and_decode(data: &[u8], max_truncate_length: usize) -> (DataFormat, SharedString) {
    let (initial_format, _) = detect_format(data);
    let process_decompressed = |decompressed: Option<Vec<u8>>| {
        decompressed.and_then(|vec| format_text(&vec, max_truncate_length).map(|(_, text)| (DataFormat::Preview, text)))
    };
    let result = match initial_format {
        DataFormat::MessagePack => rmp_serde::from_slice::<serde_json::Value>(data)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .map(|s| (DataFormat::Preview, SharedString::from(s))),

        DataFormat::Gzip => process_decompressed({
            let mut decoder = GzDecoder::new(data);
            let mut vec = Vec::with_capacity(data.len() * 2);
            decoder.read_to_end(&mut vec).ok().map(|_| vec)
        }),

        DataFormat::Zstd => process_decompressed(decompress_zstd(data).ok()),

        DataFormat::Snappy => process_decompressed({
            let mut decoder = FrameDecoder::new(data);
            let mut vec = Vec::with_capacity(data.len() * 2);
            decoder.read_to_end(&mut vec).ok().map(|_| vec)
        }),

        DataFormat::Svg | DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif => None,

        _ => {
            // NUL bytes mean binary even when every byte is valid
            // UTF-8 — a sparse SETBIT bitmap is mostly 0x00 and must
            // stay `Bytes` so the editor's bitmap heuristics get a
            // chance (mirrors `is_probably_text` in bitmap_editor.rs).
            // The LZ4 sniff also rejects empty output: an all-zero
            // prefix reads as a size-prepended block of length 0 and
            // would "decompress" into an empty preview.
            let has_nul = data.contains(&0);
            let is_utf8 = !has_nul && simdutf8::basic::from_utf8(data).is_ok();
            if !is_utf8
                && let Ok(decompressed) = decompress_size_prepended(data)
                && !decompressed.is_empty()
            {
                process_decompressed(Some(decompressed))
            } else if has_nul {
                None
            } else {
                format_text(data, max_truncate_length)
            }
        }
    };
    if let Some((new_format, text)) = result {
        (new_format, text)
    } else {
        (initial_format, SharedString::new(String::from_utf8_lossy(data)))
    }
}

impl RedisBytesValue {
    pub fn detect_and_update(&mut self, server_id: &str, key: &str, max_truncate_length: usize) {
        let data = self.bytes.as_ref();
        if data.is_empty() {
            return;
        }

        let (initial_format, mime) = detect_format(data);
        self.mime = mime;

        let process_decompressed = |decompressed: Option<Vec<u8>>| {
            decompressed
                .and_then(|vec| format_text(&vec, max_truncate_length).map(|(_, text)| (DataFormat::Preview, text)))
        };

        // A script / proto viewer explicitly configured for this key wins over
        // native format handling. Checked *before* the format match: otherwise a
        // value that also happens to be gzip / zstd / snappy / an image is
        // decoded natively and the configured viewer never runs. A viewer whose
        // decode/execute fails falls through to native handling.
        let result = if let Some(id) = ProtoManager::match_key_to_name(server_id, key)
            && let Ok(decoded) = ProtoManager::decode_data(&id, data)
        {
            Some((DataFormat::Protobuf, SharedString::from(decoded)))
        } else if let Some(id) = ScriptManager::match_key_to_id(server_id, key)
            && let Some(output) = run_script_viewer(&id, key, data)
        {
            Some((DataFormat::Script, SharedString::from(output)))
        } else {
            match initial_format {
                DataFormat::MessagePack => rmp_serde::from_slice::<serde_json::Value>(data)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .map(|s| (DataFormat::Preview, SharedString::from(s))),

                DataFormat::Gzip => process_decompressed({
                    let mut decoder = GzDecoder::new(data);
                    let mut vec = Vec::with_capacity(data.len() * 2);
                    decoder.read_to_end(&mut vec).ok().map(|_| vec)
                }),

                DataFormat::Zstd => process_decompressed(decompress_zstd(data).ok()),

                DataFormat::Snappy => process_decompressed({
                    let mut decoder = FrameDecoder::new(data);
                    let mut vec = Vec::with_capacity(data.len() * 2);
                    decoder.read_to_end(&mut vec).ok().map(|_| vec)
                }),

                DataFormat::Timestamp => format_unix_timestamp(data).map(|text| (DataFormat::Preview, text)),

                DataFormat::Svg | DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif => None,

                _ => {
                    // NUL bytes mean binary even when every byte is valid
                    // UTF-8 — a sparse SETBIT bitmap is mostly 0x00 and must
                    // stay `Bytes` so the editor's bitmap heuristics get a
                    // chance (mirrors `is_probably_text` in bitmap_editor.rs).
                    // The LZ4 sniff also rejects empty output: an all-zero
                    // prefix reads as a size-prepended block of length 0 and
                    // would "decompress" into an empty preview.
                    let has_nul = data.contains(&0);
                    let is_utf8 = !has_nul && simdutf8::basic::from_utf8(data).is_ok();
                    if !is_utf8
                        && let Ok(decompressed) = decompress_size_prepended(data)
                        && !decompressed.is_empty()
                    {
                        process_decompressed(Some(decompressed))
                    } else if has_nul {
                        None
                    } else {
                        format_text(data, max_truncate_length)
                    }
                }
            }
        };

        if let Some((new_format, text)) = result {
            self.format = new_format;
            self.text = Some(text);
        } else {
            self.format = initial_format;
        }
    }
}

/// Render a Unix-timestamp digit string (10 = seconds, 13 = milliseconds,
/// matching [`is_unix_timestamp`]'s detection) as a read-only preview
/// echoing the raw value plus the local and UTC dates.
fn format_unix_timestamp(bytes: &[u8]) -> Option<SharedString> {
    let raw = std::str::from_utf8(bytes).ok()?;
    let value: i64 = raw.parse().ok()?;
    let (dt, unit) = if bytes.len() == 13 {
        (DateTime::from_timestamp_millis(value)?, "milliseconds")
    } else {
        (DateTime::from_timestamp(value, 0)?, "seconds")
    };
    let local = dt.with_timezone(&Local);
    let text = format!(
        "{raw} ({unit})\nLocal: {}\nUTC:   {}",
        local.format("%Y-%m-%d %H:%M:%S %:z"),
        dt.format("%Y-%m-%d %H:%M:%S UTC"),
    );
    Some(SharedString::from(text))
}

pub(crate) async fn get_redis_bytes_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisBytesValue> {
    let value_bytes: Vec<u8> = cmd("GET").arg(key).query_async(conn).await?;
    Ok(RedisBytesValue {
        format: DataFormat::Text,
        bytes: Bytes::from(value_bytes),
        ..Default::default()
    })
}

/// Runs the script viewer configured for this key, or `None` if it failed.
///
/// A failure (missing interpreter, non-zero exit, timeout, output cap) falls
/// back to native format handling, which is the right behaviour but is
/// otherwise indistinguishable from "no viewer matched this key" — so it is
/// logged rather than dropped.
fn run_script_viewer(id: &str, key: &str, data: &[u8]) -> Option<String> {
    match ScriptManager::execute(id, key, data) {
        Ok(output) => Some(output),
        Err(e) => {
            warn!(id, key, error = %e, "script viewer failed, falling back to native formatting");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lz4_flex::block::compress_prepend_size;

    /// The LZ4 sniff in `detect_and_decode`'s fallback arm: a value that does
    /// not read as text but decompresses into some is shown as a decoded
    /// preview. The four-byte size prefix carries NUL bytes for a payload this
    /// size, which is what keeps the block off the plain-text path.
    ///
    /// This is the only coverage the LZ4 branch has, and the decoder behind it
    /// is chosen by a feature flag (`safe-decode`), so the round trip is worth
    /// pinning.
    #[test]
    fn lz4_block_decodes_into_a_preview() {
        let plain = "zedis ".repeat(64);
        let compressed = compress_prepend_size(plain.as_bytes());

        let (format, text) = detect_and_decode(&compressed, 4096);

        assert_eq!(format, DataFormat::Preview);
        assert_eq!(text.as_ref(), plain);
    }
}

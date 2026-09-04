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
use zedis_core::codec::{base64_text, bson, java, jwt, php, pickle, url};

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

/// A decoded document as the pretty JSON the editor shows, its long
/// strings clipped like any other JSON preview.
fn pretty_value(mut value: Value, max_truncate_length: usize) -> Option<SharedString> {
    let mut truncated = false;
    truncate_long_strings(max_truncate_length, &mut value, &mut truncated);
    serde_json::to_string_pretty(&value).ok().map(SharedString::from)
}

/// The signed binary serializations: each decoder is its own detector, so
/// a sniff that lied (a MessagePack-looking pickle) falls through to bytes.
fn decode_binary(format: DataFormat, data: &[u8], max_truncate_length: usize) -> Option<(DataFormat, SharedString)> {
    let value = match format {
        DataFormat::JavaSerialized => java::decode(data)?,
        DataFormat::Pickle => pickle::decode(data)?,
        DataFormat::Bson => bson::decode(data)?,
        _ => return None,
    };
    pretty_value(value, max_truncate_length).map(|text| (format, text))
}

/// A UTF-8 value: JSON and the text encodings before plain text. The
/// encodings are tried from the most to the least self-evident — a JWT's
/// three JSON-bearing segments, PHP's fully-consumed grammar, a percent
/// escape, and last Base64, which any token can resemble and so only
/// counts when what it hides is readable.
fn decode_text(data: &[u8], max_truncate_length: usize) -> Option<(DataFormat, SharedString)> {
    let text = std::str::from_utf8(data).ok()?;
    let trimmed = text.trim();
    let starts_json = trimmed.starts_with('{') || trimmed.starts_with('[');
    if !starts_json && !trimmed.is_empty() {
        if let Some(value) = jwt::decode(trimmed) {
            return pretty_value(value, max_truncate_length).map(|t| (DataFormat::Jwt, t));
        }
        if let Some(value) = php::decode(trimmed.as_bytes()) {
            return pretty_value(value, max_truncate_length).map(|t| (DataFormat::PhpSerialized, t));
        }
        if let Some(value) = url::decode(trimmed) {
            let text = match value {
                Value::String(decoded) => Some(SharedString::from(decoded)),
                other => pretty_value(other, max_truncate_length),
            };
            return text.map(|t| (DataFormat::UrlEncoded, t));
        }
        if let Some(bytes) = base64_text::decode(trimmed)
            && let Some(text) = base64_payload_text(&bytes, max_truncate_length)
        {
            return Some((DataFormat::Base64, text));
        }
    }
    format_text(data, max_truncate_length)
}

/// What Base64 decoded to, if it is worth showing: readable text (JSON
/// pretty-printed), or a binary format the pipeline itself decodes. Noise
/// — which is what most Base64-shaped tokens decode to — is `None`, and
/// the value stays the text it was.
fn base64_payload_text(bytes: &[u8], max_truncate_length: usize) -> Option<SharedString> {
    if bytes.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let readable = !text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'));
        return readable
            .then(|| format_text(bytes, max_truncate_length).map(|(_, t)| t))
            .flatten();
    }
    let (inner, _) = detect_format(bytes);
    if matches!(inner, DataFormat::Bytes | DataFormat::Timestamp) || inner_is_image(inner) {
        return None;
    }
    // A decode that failed hands back the sniffed format with lossy text;
    // only a rendering counts.
    let (decoded, text) = detect_and_decode(bytes, max_truncate_length);
    matches!(
        decoded,
        DataFormat::Preview
            | DataFormat::Json
            | DataFormat::Text
            | DataFormat::Bson
            | DataFormat::Pickle
            | DataFormat::JavaSerialized
    )
    .then_some(text)
}

fn inner_is_image(format: DataFormat) -> bool {
    matches!(
        format,
        DataFormat::Svg | DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif
    )
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

        DataFormat::JavaSerialized | DataFormat::Pickle | DataFormat::Bson => {
            decode_binary(initial_format, data, max_truncate_length)
        }

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
                decode_text(data, max_truncate_length)
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

                DataFormat::JavaSerialized | DataFormat::Pickle | DataFormat::Bson => {
                    decode_binary(initial_format, data, max_truncate_length)
                }

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
                        decode_text(data, max_truncate_length)
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

    /// The text encodings sit between JSON and plain text, and only fire
    /// on a value that really is one: a Base64 token that hides noise, a
    /// digest, a sentence all stay text.
    #[test]
    fn text_encodings_are_decoded_only_when_they_hold_something_readable() {
        let (format, text) = detect_and_decode(b"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2ln", 1000);
        assert_eq!(format, DataFormat::Jwt);
        assert!(text.contains("\"sub\": \"42\""), "{text}");

        let (format, text) = detect_and_decode(br#"a:1:{s:4:"name";s:5:"zedis";}"#, 1000);
        assert_eq!(format, DataFormat::PhpSerialized);
        assert!(text.contains("\"name\": \"zedis\""), "{text}");

        let (format, text) = detect_and_decode(b"name=Zh%C3%A9+Li&page=2", 1000);
        assert_eq!(format, DataFormat::UrlEncoded);
        assert!(text.contains("\"name\": \"Zhé Li\""), "{text}");

        // Base64 of JSON: decoded and pretty-printed.
        let (format, text) = detect_and_decode(b"eyJpZCI6IDcsICJvayI6IHRydWV9", 1000);
        assert_eq!(format, DataFormat::Base64);
        assert!(text.contains("\"id\": 7"), "{text}");

        // Base64-shaped, but a hex digest / a session token: left as text.
        for token in [
            &b"5d41402abc4b2a76b9719d911017c592"[..],
            b"kQ2hvbmcgc2VjcmV0IGtleQ3fa8m1",
            b"just a sentence with spaces",
        ] {
            let (format, text) = detect_and_decode(token, 1000);
            assert_eq!(format, DataFormat::Text, "{}", String::from_utf8_lossy(token));
            assert_eq!(text.as_bytes(), token);
        }

        // JSON keeps winning over anything that starts like it.
        let (format, _) = detect_and_decode(br#"{"a":1}"#, 1000);
        assert_eq!(format, DataFormat::Json);
    }

    #[test]
    fn signed_binary_serializations_are_detected_and_rendered() {
        // A pickle: {"n": 1}
        let pickle: &[u8] = &[0x80, 0x04, b'}', 0x8c, 0x01, b'n', b'K', 0x01, b's', b'.'];
        let (format, text) = detect_and_decode(pickle, 1000);
        assert_eq!(format, DataFormat::Pickle);
        assert!(text.contains("\"n\": 1"), "{text}");

        // A BSON document: {"n": 1}
        let bson: &[u8] = &[0x0c, 0, 0, 0, 0x10, b'n', 0, 1, 0, 0, 0, 0];
        let (format, text) = detect_and_decode(bson, 1000);
        assert_eq!(format, DataFormat::Bson);
        assert!(text.contains("\"n\": 1"), "{text}");

        // A Java stream holding one string.
        let java: &[u8] = &[0xac, 0xed, 0x00, 0x05, 0x74, 0x00, 0x02, b'o', b'k'];
        let (format, text) = detect_and_decode(java, 1000);
        assert_eq!(format, DataFormat::JavaSerialized);
        assert_eq!(text.as_ref(), "\"ok\"");

        // Base64 wrapping a pickle: decoded through the inner format.
        let wrapped = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pickle);
        let (format, text) = detect_and_decode(wrapped.as_bytes(), 1000);
        assert_eq!(format, DataFormat::Base64);
        assert!(text.contains("\"n\": 1"), "{text}");
    }
}

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

use super::value::{DataFormat, RedisBytesValue, ViewMode, ViewerFailure, ViewerKind, detect_format};
use crate::db::ProtoManager;
#[cfg(not(target_family = "wasm"))]
use crate::db::ScriptManager;
use crate::helpers::{configured_time_zone, format_datetime_in, format_datetime_other_zone};
use crate::{
    connection::{ServerDb, string_get, string_range},
    error::Error,
};
use bytes::Bytes;
use chrono::DateTime;
use flate2::read::GzDecoder;
use gpui::SharedString;
use lz4_flex::block::decompress_size_prepended;
use ruzstd::decoding::StreamingDecoder;
use serde_json::Value;
use snap::read::FrameDecoder;
use std::io::Read;
use tracing::warn;
use zedis_core::codec::{base64_text, bson, java, jwt, php, pickle, url, xml, yaml};

type Result<T, E = Error> = std::result::Result<T, E>;

/// The most a compressed value is inflated to for its preview. Past it the
/// preview is the head of the value: a few megabytes of gzip or zstd can
/// inflate to gigabytes, and inflating whole was one allocation that size.
const MAX_INFLATED_PREVIEW: usize = 32 * 1024 * 1024;

/// Read `reader` to its end or to [`MAX_INFLATED_PREVIEW`] bytes, whichever
/// comes first — and a cut head ends on a whole character, so it still
/// reads as text.
fn inflate_capped(reader: impl Read) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(MAX_INFLATED_PREVIEW as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > MAX_INFLATED_PREVIEW {
        out.truncate(MAX_INFLATED_PREVIEW);
        if let Err(e) = std::str::from_utf8(&out)
            && e.error_len().is_none()
        {
            out.truncate(e.valid_up_to());
        }
    }
    Some(out)
}

/// LZ4 with its size prepended — sniffed on any value that is not text, so
/// the four bytes it reads as the inflated size are often not one, and the
/// decoder allocates that size before decoding a byte: up to 4 GiB for an
/// arbitrary binary value. A block inflates at most about 255:1, so a size
/// beyond that, or beyond the preview cap, is not this format.
fn inflate_lz4_prepended(data: &[u8]) -> Option<Vec<u8>> {
    let size_bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    let size = usize::try_from(u32::from_le_bytes(size_bytes)).ok()?;
    let limit = MAX_INFLATED_PREVIEW.min((data.len() - 4).saturating_mul(255));
    if size == 0 || size > limit {
        return None;
    }
    decompress_size_prepended(data).ok()
}

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
        if let Some(pretty) = xml::decode(trimmed) {
            return Some((DataFormat::Xml, SharedString::from(pretty)));
        }
        if yaml::decode(trimmed).is_some() {
            return Some((DataFormat::Yaml, SharedString::from(text.to_string())));
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

        DataFormat::Gzip => process_decompressed(inflate_capped(GzDecoder::new(data))),

        DataFormat::Zstd => process_decompressed(StreamingDecoder::new(data).ok().and_then(inflate_capped)),

        DataFormat::Snappy => process_decompressed(inflate_capped(FrameDecoder::new(data))),

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
            if !is_utf8 && let Some(decompressed) = inflate_lz4_prepended(data) {
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
        let (viewer, viewer_failure) = configured_viewer(server_id, key, data);
        self.viewer_failure = viewer_failure;
        let result = if let Some(viewer) = viewer {
            Some(viewer)
        } else {
            match initial_format {
                DataFormat::MessagePack => rmp_serde::from_slice::<serde_json::Value>(data)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .map(|s| (DataFormat::Preview, SharedString::from(s))),

                DataFormat::Gzip => process_decompressed(inflate_capped(GzDecoder::new(data))),

                DataFormat::Zstd => process_decompressed(StreamingDecoder::new(data).ok().and_then(inflate_capped)),

                DataFormat::Snappy => process_decompressed(inflate_capped(FrameDecoder::new(data))),

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
                    if !is_utf8 && let Some(decompressed) = inflate_lz4_prepended(data) {
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
            self.script_editable = new_format == DataFormat::Script && script_viewer_has_encoder(server_id, key);
        } else {
            self.format = initial_format;
            self.script_editable = false;
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
    // The configured zone + layout first, the other zone under it, so a
    // UTC-minded user and a local-minded one both get their answer on the
    // first line.
    let primary = configured_time_zone();
    let (other_label, other) = format_datetime_other_zone(&dt);
    let text = format!(
        "{raw} ({unit})\n{}: {}\n{other_label}: {other}",
        primary.label(),
        format_datetime_in(&dt, primary),
    );
    Some(SharedString::from(text))
}

/// Up to `len` bytes of a string starting at `offset`, as its preview shows
/// them, plus `(total_len, actual_offset)`. Text when the bytes are text —
/// trimmed past any leading UTF-8 continuation bytes (when `offset > 0` landed
/// inside a multi-byte character) and cut back from any trailing partial
/// character — and hex otherwise.
pub(crate) async fn get_redis_bytes_range(
    at: &ServerDb,
    key: &str,
    offset: u64,
    len: usize,
) -> Result<(RedisBytesValue, u64, u64)> {
    let (chunk, total) = string_range(at, key, offset, len).await?;
    let (val, actual_offset) = preview_bytes_value_at(chunk, offset);
    Ok((val, total, actual_offset))
}

fn preview_bytes_value_at(chunk: Vec<u8>, offset: u64) -> (RedisBytesValue, u64) {
    // When `offset > 0` splits a multi-byte UTF-8 scalar, up to 3 leading
    // continuation bytes (`10xxxxxx`) appear at the head. Only skip them if
    // what remains is valid UTF-8 (apart from a split trailing scalar), so a
    // genuinely binary payload keeps its exact starting byte and hex address.
    let lead_trim = if offset > 0 {
        chunk
            .iter()
            .take(3)
            .take_while(|&&b| (b & 0b1100_0000) == 0b1000_0000)
            .count()
    } else {
        0
    };
    let candidate = &chunk[lead_trim..];
    match std::str::from_utf8(candidate) {
        Ok(_) => (
            RedisBytesValue {
                format: DataFormat::Text,
                bytes: Bytes::copy_from_slice(candidate),
                view_mode: ViewMode::Plain,
                ..Default::default()
            },
            offset + lead_trim as u64,
        ),
        Err(error) if error.error_len().is_none() => (
            RedisBytesValue {
                format: DataFormat::Text,
                bytes: Bytes::copy_from_slice(&candidate[..error.valid_up_to()]),
                view_mode: ViewMode::Plain,
                ..Default::default()
            },
            offset + lead_trim as u64,
        ),
        Err(_) => (
            RedisBytesValue {
                format: DataFormat::Bytes,
                bytes: Bytes::from(chunk),
                view_mode: ViewMode::Hex,
                ..Default::default()
            },
            offset,
        ),
    }
}

pub(crate) async fn get_redis_bytes_value(at: &ServerDb, key: &str) -> Result<RedisBytesValue> {
    let value_bytes = string_get(at, key).await?;
    Ok(RedisBytesValue {
        format: DataFormat::Text,
        bytes: Bytes::from(value_bytes),
        ..Default::default()
    })
}

/// What a configured viewer made of a value: its output, or why there is
/// none.
type ViewerOutcome = (Option<(DataFormat, SharedString)>, Option<ViewerFailure>);

/// The proto / script viewer the user configured for this key, if any: a
/// protobuf descriptor decodes the bytes, or a viewer program is run over
/// them. Both live on disk and the second is a process, so the browser has
/// neither and every key goes straight to the native format handling.
///
/// A viewer that fails — a schema the bytes do not fit, a script whose
/// program is missing, exits non-zero, times out or says too much — falls
/// back to the next one and then to native format handling, which is the
/// right thing to show and looks exactly like "no viewer matched this key".
/// So the failure is handed back with it, for the editor to say: the first
/// one, where both a binding and a script matched and failed.
#[cfg(not(target_family = "wasm"))]
fn configured_viewer(server_id: &str, key: &str, data: &[u8]) -> ViewerOutcome {
    let mut failure = None;
    if let Some(id) = ProtoManager::match_key_to_name(server_id, key) {
        match ProtoManager::decode_data(&id, data) {
            Ok(decoded) => return (Some((DataFormat::Protobuf, SharedString::from(decoded))), None),
            Err(e) => {
                warn!(id, key, error = %e, "proto binding could not decode the value, falling back");
                // The binding as the user named it; its id says nothing to them.
                let rule = ProtoManager::get_proto(&id).map_or(id, |config| config.name);
                failure = Some(viewer_failure(ViewerKind::Proto, rule, &e.to_string()));
            }
        }
    }
    let Some(id) = ScriptManager::match_key_to_id(server_id, key) else {
        return (None, failure);
    };
    match ScriptManager::execute(&id, key, data) {
        Ok(output) => (Some((DataFormat::Script, SharedString::from(output))), failure),
        Err(e) => {
            warn!(id, key, error = %e, "script viewer failed, falling back to native formatting");
            // The rule as the user named it; its id says nothing to them.
            let rule = ScriptManager::get(&id).map_or(id, |config| config.name);
            let script = viewer_failure(ViewerKind::Script, rule, &e.to_string());
            (None, Some(failure.unwrap_or(script)))
        }
    }
}

/// [`configured_viewer`] in a page: the account's proto viewers (kept by
/// the bridge, compiled from their schema text in memory), and no script
/// viewer — that one is a process.
#[cfg(target_family = "wasm")]
fn configured_viewer(server_id: &str, key: &str, data: &[u8]) -> ViewerOutcome {
    let Some(id) = ProtoManager::match_key_to_name(server_id, key) else {
        return (None, None);
    };
    match ProtoManager::decode_data(&id, data) {
        Ok(decoded) => (Some((DataFormat::Protobuf, SharedString::from(decoded))), None),
        Err(e) => {
            warn!(id, key, error = %e, "proto binding could not decode the value, falling back");
            let rule = ProtoManager::get_proto(&id).map_or(id, |config| config.name);
            (None, Some(viewer_failure(ViewerKind::Proto, rule, &e.to_string())))
        }
    }
}

/// Returns `true` when the script viewer matching `(server_id, key)` has a
/// non-empty `encode_command` configured.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn script_viewer_has_encoder(server_id: &str, key: &str) -> bool {
    ScriptManager::has_encoder_for_key(server_id, key)
}

#[cfg(target_family = "wasm")]
pub(crate) fn script_viewer_has_encoder(_server_id: &str, _key: &str) -> bool {
    false
}

/// Runs the `encode_command` of the script viewer matching `(server_id, key)`
/// against `edited_text`, returning the encoded raw bytes plus the freshly
/// decoded display text (if re-decoding succeeds).
#[cfg(not(target_family = "wasm"))]
pub(crate) fn encode_with_script_viewer(
    server_id: &str,
    key: &str,
    edited_text: &str,
) -> Result<(Vec<u8>, Option<SharedString>)> {
    let encoded = ScriptManager::encode_for_key(server_id, key, edited_text)?;
    let redecoded = configured_viewer(server_id, key, &encoded)
        .0
        .and_then(|(fmt, text)| (fmt == DataFormat::Script).then_some(text));
    Ok((encoded, redecoded))
}

#[cfg(target_family = "wasm")]
pub(crate) fn encode_with_script_viewer(
    _server_id: &str,
    _key: &str,
    _edited_text: &str,
) -> Result<(Vec<u8>, Option<SharedString>)> {
    Err(Error::Invalid {
        message: "script viewers are not supported in the browser".to_string(),
    })
}

/// A viewer's failure as the notice words it: the storage layer's
/// "Invalid: " is its own classification, not something the viewer said.
fn viewer_failure(kind: ViewerKind, rule: String, error: &str) -> ViewerFailure {
    ViewerFailure {
        kind,
        rule: rule.into(),
        error: error.strip_prefix("Invalid: ").unwrap_or(error).to_string().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notice names the rule and repeats what the script said, without
    /// the storage layer's own "Invalid: " in front of it.
    #[test]
    fn a_failed_viewer_is_reported_in_the_scripts_own_words() {
        let failure = viewer_failure(
            ViewerKind::Script,
            "fn-hexdump".to_string(),
            "Invalid: script exited with error: sh: xxd: command not found",
        );
        assert_eq!(failure.kind, ViewerKind::Script);
        assert_eq!(failure.rule.as_ref(), "fn-hexdump");
        assert_eq!(
            failure.error.as_ref(),
            "script exited with error: sh: xxd: command not found"
        );
        // Anything else is passed on as it came.
        assert_eq!(
            viewer_failure(ViewerKind::Proto, "r".to_string(), "IO error: denied")
                .error
                .as_ref(),
            "IO error: denied"
        );
    }
    use flate2::{Compression, write::GzEncoder};
    use lz4_flex::block::compress_prepend_size;
    use std::io::Write;

    /// A value that inflates past the cap previews its head instead of
    /// inflating whole.
    #[test]
    fn an_inflated_preview_stops_at_the_cap() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(&vec![b'a'; MAX_INFLATED_PREVIEW + 1024])
            .expect("compress");
        let gzip = encoder.finish().expect("finish");
        let inflated = inflate_capped(GzDecoder::new(gzip.as_slice())).expect("inflate");
        assert_eq!(inflated.len(), MAX_INFLATED_PREVIEW);
        // And a cut through a character ends before it.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut text = vec![b'a'; MAX_INFLATED_PREVIEW - 1];
        text.extend_from_slice("é".repeat(8).as_bytes());
        encoder.write_all(&text).expect("compress");
        let gzip = encoder.finish().expect("finish");
        let inflated = inflate_capped(GzDecoder::new(gzip.as_slice())).expect("inflate");
        assert_eq!(inflated.len(), MAX_INFLATED_PREVIEW - 1);
    }

    /// A preview of text stays text, cut back to its last whole character; a
    /// preview of anything else is hex, whole.
    #[test]
    fn a_preview_is_text_up_to_the_split_character_else_hex() {
        let mut head = b"abc".to_vec();
        head.extend_from_slice(&"é".as_bytes()[..1]);
        let (value, offset) = preview_bytes_value_at(head, 0);
        assert_eq!(offset, 0);
        assert!(matches!(value.format, DataFormat::Text));
        assert!(matches!(value.view_mode, ViewMode::Plain));
        assert_eq!(value.bytes.as_ref(), b"abc");

        // Mid-scalar start at non-zero offset skips leading continuation bytes.
        let mut mid = Vec::new();
        mid.extend_from_slice(&"é".as_bytes()[1..]);
        mid.extend_from_slice(b"hello");
        let (mid_val, actual_offset) = preview_bytes_value_at(mid, 256_000);
        assert_eq!(actual_offset, 256_001);
        assert_eq!(mid_val.bytes.as_ref(), b"hello");

        let binary = vec![0xff, 0x00, b'a', 0xfe];
        let (value, bin_offset) = preview_bytes_value_at(binary.clone(), 0);
        assert_eq!(bin_offset, 0);
        assert!(matches!(value.format, DataFormat::Bytes));
        assert!(matches!(value.view_mode, ViewMode::Hex));
        assert_eq!(value.bytes.as_ref(), binary.as_slice());
    }

    /// Four bytes that only look like an LZ4 size prefix are not taken at
    /// their word: the decoder would allocate what they say first.
    #[test]
    fn an_lz4_size_beyond_what_the_block_could_hold_is_not_lz4() {
        assert!(inflate_lz4_prepended(&[0xff, 0xff, 0xff, 0x7f, 1, 2, 3]).is_none());
        assert!(inflate_lz4_prepended(&[0, 0, 0, 0, 1]).is_none());
        assert!(inflate_lz4_prepended(&[1, 2]).is_none());
        let block = compress_prepend_size(b"hello hello hello hello");
        assert_eq!(
            inflate_lz4_prepended(&block).as_deref(),
            Some(&b"hello hello hello hello"[..])
        );
    }

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

        // XML: pretty-printed and kept editable.
        let (format, text) = detect_and_decode(br#"<user id="1"><name>Alice</name></user>"#, 1000);
        assert_eq!(format, DataFormat::Xml);
        assert!(
            text.contains("<user id=\"1\">\n  <name>Alice</name>\n</user>"),
            "{text}"
        );

        // YAML: multi-line mapping detected as YAML.
        let (format, text) = detect_and_decode(b"server:\n  host: 127.0.0.1\n  port: 6379\n", 1000);
        assert_eq!(format, DataFormat::Yaml);
        assert!(text.contains("host: 127.0.0.1"), "{text}");
    }

    #[test]
    fn script_value_is_editable_only_when_viewer_has_encoder() {
        let decode_only = RedisBytesValue {
            format: DataFormat::Script,
            script_editable: false,
            ..Default::default()
        };
        assert!(!decode_only.is_text_editable());

        let bidirectional = RedisBytesValue {
            format: DataFormat::Script,
            script_editable: true,
            ..Default::default()
        };
        assert!(bidirectional.is_text_editable());
    }

    #[test]
    fn preview_chunk_trims_split_utf8_at_both_boundaries_and_keeps_binary_intact() {
        let full = "你好世界hello".as_bytes(); // each CJK char is 3 bytes
        // Slice starting 1 byte into '你' and ending 2 bytes into '界' (bytes 1..11).
        let (text_chunk, actual_offset) = super::preview_bytes_value_at(full[1..11].to_vec(), 1);
        assert_eq!(text_chunk.format, DataFormat::Text);
        assert_eq!(actual_offset, 3);
        assert_eq!(text_chunk.bytes.as_ref(), "好世".as_bytes());

        // Arbitrary binary bytes starting with 0x80 keep exact offset and Hex mode.
        let (bin_chunk, bin_offset) = super::preview_bytes_value_at(vec![0x80, 0xff, 0x00, 0xfe], 256);
        assert_eq!(bin_chunk.format, DataFormat::Bytes);
        assert_eq!(bin_offset, 256);
        assert_eq!(bin_chunk.bytes.as_ref(), &[0x80, 0xff, 0x00, 0xfe]);
    }
}

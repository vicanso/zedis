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

use super::value::{DataFormat, KeyType, RedisBytesValue, RedisValue, RedisValueData, detect_format};
use crate::helpers::decompress_zstd;
use crate::{connection::RedisAsyncConn, error::Error};
use bytes::Bytes;
use flate2::read::GzDecoder;
use gpui::SharedString;
use lz4_flex::block::decompress_size_prepended;
use redis::cmd;
use serde_json::Value;
use snap::read::FrameDecoder;
use std::io::Read;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

fn truncate_long_strings(max_truncate_length: usize, v: &mut Value, truncated: &mut bool) {
    match v {
        Value::String(s) => {
            if s.len() > max_truncate_length {
                let char_count = s.chars().count();
                if char_count > max_truncate_length {
                    let mut new_s: String = s.chars().take(max_truncate_length).collect();
                    new_s.push_str(&format!("...(Total {} chars, content hidden)", char_count));
                    *s = new_s;
                    *truncated = true;
                }
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

impl RedisBytesValue {
    pub fn detect_and_update(&mut self, max_truncate_length: usize) {
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
                if let Ok(decompressed) = decompress_size_prepended(data) {
                    process_decompressed(Some(decompressed))
                } else {
                    format_text(data, max_truncate_length)
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

/// Fetch a string value from Redis.
/// Returns a RedisValue with the string value and the size.
pub(crate) async fn get_redis_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    max_truncate_length: usize,
) -> Result<RedisValue> {
    let value_bytes: Vec<u8> = cmd("GET").arg(key).query_async(conn).await?;
    let mut data = RedisBytesValue {
        format: DataFormat::Text,
        bytes: Bytes::from(value_bytes),
        ..Default::default()
    };
    data.detect_and_update(max_truncate_length);
    Ok(RedisValue {
        key_type: KeyType::String,
        data: Some(RedisValueData::Bytes(Arc::new(data))),
        ..Default::default()
    })
}

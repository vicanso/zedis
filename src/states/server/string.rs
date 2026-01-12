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

use super::value::{DataFormat, KeyType, RedisBytesValue, RedisValue, RedisValueData, ViewMode, detect_format};
use crate::helpers::decompress_zstd;
use crate::{connection::RedisAsyncConn, error::Error};
use bytes::Bytes;
use flate2::read::GzDecoder;
use gpui::SharedString;
use redis::cmd;
use serde_json::Value;
use std::io::Read;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

const MAX_STRING_LEN: usize = 1024;

fn truncate_long_strings(v: &mut Value, truncated: &mut bool) {
    match v {
        Value::String(s) => {
            if s.len() > MAX_STRING_LEN {
                let char_count = s.chars().count();
                if char_count > MAX_STRING_LEN {
                    let mut new_s: String = s.chars().take(MAX_STRING_LEN).collect();
                    new_s.push_str(&format!("...(Total {} chars, content hidden)", char_count));
                    *s = new_s;
                    *truncated = true;
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                truncate_long_strings(item, truncated);
            }
        }
        Value::Object(map) => {
            for val in map.values_mut() {
                truncate_long_strings(val, truncated);
            }
        }
        _ => {}
    }
}

/// Attempts to format a string as pretty-printed JSON.
/// Returns None if the string is not valid JSON or doesn't look like JSON.
fn pretty_json(value: &str) -> Option<(SharedString, bool)> {
    let trimmed = value.trim();
    if !((trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']'))) {
        return None;
    }
    let mut json_value = serde_json::from_str::<Value>(value).ok()?;
    let mut truncated = false;
    truncate_long_strings(&mut json_value, &mut truncated);
    let pretty_str = serde_json::to_string_pretty(&json_value).ok()?;

    Some((pretty_str.into(), truncated))
}

/// Fetch a string value from Redis.
/// Returns a RedisValue with the string value and the size.
pub(crate) async fn get_redis_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let value_bytes: Vec<u8> = cmd("GET").arg(key).query_async(conn).await?;
    let size = value_bytes.len();
    if value_bytes.is_empty() {
        return Ok(RedisValue {
            key_type: KeyType::String,
            data: Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
                format: DataFormat::Text,
                ..Default::default()
            }))),
            size,
            ..Default::default()
        });
    }
    let bytes = Bytes::from(value_bytes);
    let (mut format, mime) = detect_format(&bytes);
    let text: Option<SharedString> = match format {
        DataFormat::MessagePack => rmp_serde::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .map(SharedString::from),
        DataFormat::Gzip => {
            let mut decoder = GzDecoder::new(bytes.as_ref());
            let mut decompressed_vec = Vec::new();

            if decoder.read_to_end(&mut decompressed_vec).is_ok() {
                match String::from_utf8(decompressed_vec) {
                    Ok(s) => {
                        if let Some((pretty, truncated)) = pretty_json(&s) {
                            if truncated {
                                format = DataFormat::JsonTruncated;
                            } else {
                                format = DataFormat::Json;
                            }
                            Some(pretty)
                        } else {
                            format = DataFormat::Text;
                            Some(s.into())
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        }
        DataFormat::Zstd => {
            if let Ok(decompressed_vec) = decompress_zstd(bytes.as_ref()) {
                match String::from_utf8(decompressed_vec) {
                    Ok(s) => {
                        if let Some((pretty, truncated)) = pretty_json(&s) {
                            if truncated {
                                format = DataFormat::JsonTruncated;
                            } else {
                                format = DataFormat::Json;
                            }
                            Some(pretty)
                        } else {
                            format = DataFormat::Text;
                            Some(s.into())
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        }
        DataFormat::Svg | DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif => None,
        _ => match std::str::from_utf8(&bytes) {
            Ok(s) => {
                if let Some((pretty, truncated)) = pretty_json(s) {
                    if truncated {
                        format = DataFormat::JsonTruncated;
                    } else {
                        format = DataFormat::Json;
                    }
                    Some(pretty)
                } else {
                    format = DataFormat::Text;
                    Some(s.to_string().into())
                }
            }
            Err(_) => None,
        },
    };

    Ok(RedisValue {
        key_type: KeyType::String,
        data: Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
            format,
            mime,
            bytes,
            text,
            view_mode: ViewMode::default(),
        }))),
        size,
        ..Default::default()
    })
}

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

use super::value::KeyType;
use super::value::{RedisValue, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::error::Error;
use gpui::SharedString;
use redis::cmd;
use serde_json::Value;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Attempts to format a string as pretty-printed JSON.
/// Returns None if the string is not valid JSON or doesn't look like JSON.
fn pretty_json(value: &str) -> Option<SharedString> {
    let trimmed = value.trim();
    if !((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
    {
        return None;
    }
    let json_value = serde_json::from_str::<Value>(value).ok()?;
    let pretty_str = serde_json::to_string_pretty(&json_value).ok()?;

    Some(pretty_str.into())
}

/// Fetch a string value from Redis.
/// Returns a RedisValue with the string value and the size.
pub(crate) async fn get_redis_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let value_bytes: Vec<u8> = cmd("GET").arg(key).query_async(conn).await?;
    let size = value_bytes.len();
    if value_bytes.is_empty() {
        return Ok(RedisValue {
            key_type: KeyType::String,
            data: Some(RedisValueData::String(SharedString::default())),
            size,
            ..Default::default()
        });
    }
    let data = match String::from_utf8(value_bytes) {
        Ok(text) => {
            // Check if it's JSON and format it
            if let Some(pretty) = pretty_json(&text) {
                RedisValueData::String(pretty)
            } else {
                // If not JSON, use the original text.
                // Converting String to SharedString is efficient.
                RedisValueData::String(text.into())
            }
        }
        Err(e) => {
            // Conversion failed (invalid UTF-8). Recover the original bytes.
            let raw_bytes = e.into_bytes();
            RedisValueData::Bytes(raw_bytes)
        }
    };
    Ok(RedisValue {
        key_type: KeyType::String,
        data: Some(data),
        size,
        ..Default::default()
    })
}

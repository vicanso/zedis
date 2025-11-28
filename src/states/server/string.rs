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
use redis::cmd;
use serde_json::Value;

type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) async fn get_redis_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let value: Vec<u8> = cmd("GET").arg(key).query_async(conn).await?;
    let size = value.len();
    if value.is_empty() {
        return Ok(RedisValue {
            key_type: KeyType::String,
            data: Some(RedisValueData::String(String::new())),
            size,
            ..Default::default()
        });
    }
    if let Ok(value) = std::str::from_utf8(&value) {
        if let Ok(value) = serde_json::from_str::<Value>(value)
            && let Ok(pretty_value) = serde_json::to_string_pretty(&value)
        {
            return Ok(RedisValue {
                key_type: KeyType::String,
                data: Some(RedisValueData::String(pretty_value)),
                size,
                ..Default::default()
            });
        } else {
            return Ok(RedisValue {
                key_type: KeyType::String,
                data: Some(RedisValueData::String(value.to_string())),
                size,
                ..Default::default()
            });
        }
    }
    Ok(RedisValue {
        key_type: KeyType::String,
        data: Some(RedisValueData::Bytes(value)),
        size,
        ..Default::default()
    })
}

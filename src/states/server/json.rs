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

use super::{
    KeyType, RedisValueData,
    value::{DataFormat, RedisBytesValue, RedisValue},
};
use crate::{connection::RedisAsyncConn, error::Error};
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) async fn get_redis_json_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let value: String = cmd("JSON.GET").arg(key).query_async(conn).await?;
    let value = serde_json::from_str::<serde_json::Value>(&value)?;
    let data = serde_json::to_string_pretty(&value)?;

    Ok(RedisValue {
        key_type: KeyType::Json,
        data: Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
            format: DataFormat::Text,
            text: Some(data.into()),
            ..Default::default()
        }))),
        ..Default::default()
    })
}

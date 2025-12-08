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

use super::value::RedisSetValue;
use super::value::RedisValue;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::error::Error;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

async fn get_redis_set_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: usize,
    count: usize,
) -> Result<(u64, Vec<String>)> {
    let (cursor, value): (u64, Vec<Vec<u8>>) = cmd("SSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg("*")
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;
    if value.is_empty() {
        return Ok((cursor, vec![]));
    }
    let value = value.iter().map(|v| String::from_utf8_lossy(v).to_string()).collect();
    Ok((cursor, value))
}

pub(crate) async fn first_load_set_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("SCARD").arg(key).query_async(conn).await?;
    let (cursor, values) = get_redis_set_value(conn, key, 0, 99).await?;
    Ok(RedisValue {
        key_type: KeyType::Set,
        data: Some(RedisValueData::Set(Arc::new(RedisSetValue {
            cursor,
            size,
            values: values.into_iter().map(|v| v.into()).collect(),
        }))),
        expire_at: None,
        ..Default::default()
    })
}

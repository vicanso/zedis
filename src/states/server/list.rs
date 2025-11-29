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

use super::ZedisServerState;
use super::value::RedisListValue;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::RedisValue;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

async fn get_redis_list_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    start: usize,
    stop: usize,
) -> Result<Vec<String>> {
    let value: Vec<Vec<u8>> = cmd("LRANGE")
        .arg(key)
        .arg(start)
        .arg(stop)
        .query_async(conn)
        .await?;
    if value.is_empty() {
        return Ok(vec![]);
    }
    let value: Vec<String> = value
        .iter()
        .map(|v| String::from_utf8_lossy(v).to_string())
        .collect();
    Ok(value)
}

pub(crate) async fn first_load_list_value(
    conn: &mut RedisAsyncConn,
    key: &str,
) -> Result<RedisValue> {
    let size: usize = cmd("LLEN").arg(key).query_async(conn).await?;
    let values = get_redis_list_value(conn, key, 0, 99).await?;
    Ok(RedisValue {
        key_type: KeyType::List,
        data: Some(RedisValueData::List(Arc::new(RedisListValue {
            size,
            values,
        }))),
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    pub fn update_list_value(
        &mut self,
        index: usize,
        original_value: String,
        new_value: String,
        cx: &mut Context<Self>,
    ) {
        let key = self.key.clone().unwrap_or_default();
        if key.is_empty() {
            return;
        }
        let Some(value) = self.value() else {
            return;
        };
        let Some(data) = value.list_value() else {
            return;
        };
        let data = data.clone();
        let server = self.server.clone();
        let mut value = value.clone();
        self.spawn(
            cx,
            "update_list_value",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;
                let current_value: String = cmd("LINDEX")
                    .arg(&key)
                    .arg(index)
                    .query_async(&mut conn)
                    .await?;
                if current_value != original_value {
                    return Err(Error::Invalid {
                        message: "value is changed, it cannot be updated".to_string(),
                    });
                }
                let _: () = cmd("LSET")
                    .arg(&key)
                    .arg(index)
                    .arg(&new_value)
                    .query_async(&mut conn)
                    .await?;
                let mut values = data.values.clone();
                values[index] = new_value;
                value.data = Some(RedisValueData::List(Arc::new(RedisListValue {
                    size: data.size,
                    values,
                })));
                Ok(value)
            },
            move |this, result, cx| {
                if let Ok(value) = result {
                    this.value = Some(value);
                }
                cx.notify();
            },
        );
    }
    pub fn load_more_list_value(&mut self, cx: &mut Context<Self>) {
        let key = self.key.clone().unwrap_or_default();
        if key.is_empty() {
            return;
        }
        let Some(value) = self.value() else {
            return;
        };
        let Some(data) = value.list_value() else {
            return;
        };
        let data = data.clone();
        let server = self.server.clone();
        let start = data.values.len();
        let stop = start + 99;
        let mut value = value.clone();
        self.spawn(
            cx,
            "load_more_list",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;
                let new_values = get_redis_list_value(&mut conn, &key, start, stop).await?;
                let mut values = data.values.clone();
                values.extend(new_values);
                value.data = Some(RedisValueData::List(Arc::new(RedisListValue {
                    size: data.size,
                    values,
                })));
                Ok(value)
            },
            move |this, result, cx| {
                if let Ok(value) = result {
                    this.value = Some(value);
                }
                cx.notify();
            },
        );
    }
}

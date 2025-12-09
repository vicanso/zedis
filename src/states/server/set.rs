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

use super::ServerTask;
use super::ZedisServerState;
use super::value::RedisSetValue;
use super::value::RedisValue;
use super::value::RedisValueStatus;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::ServerEvent;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use redis::pipe;
use std::sync::Arc;
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

async fn get_redis_set_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<String>)> {
    println!("key: {key} cursor: {cursor} count: {count}");
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

impl ZedisServerState {
    pub fn load_more_set_value(&mut self, cx: &mut Context<Self>) {
        let key = self.key.clone().unwrap_or_default();
        if key.is_empty() {
            return;
        }
        let Some(value) = self.value.as_mut() else {
            return;
        };
        if value.is_busy() {
            return;
        }
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // // Check if we have valid set data
        let cursor = match value.set_value() {
            Some(set) => set.cursor,
            None => return,
        };

        let server_id = self.server_id.clone();
        let current_key = key.clone();
        cx.emit(ServerEvent::LoadMoreValueStart(current_key.clone()));
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                // Fetch only the new items
                let result = get_redis_set_value(&mut conn, &key, cursor, 99).await?;
                Ok(result)
            },
            move |this, result, cx| {
                if let Ok((new_cursor, new_values)) = result
                    && !new_values.is_empty()
                {
                    // Update Local State (UI Thread)
                    // Append new items to the existing list
                    if let Some(RedisValueData::Set(set_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                        let set = Arc::make_mut(set_data);
                        set.cursor = new_cursor;
                        set.values.extend(new_values.into_iter().map(|v| v.into()));
                    }
                }
                cx.emit(ServerEvent::LoadMoreValueFinish(current_key));
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

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
use super::value::RedisValue;
use super::value::RedisValueStatus;
use super::value::RedisZsetValue;
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

async fn get_redis_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    start: usize,
    stop: usize,
) -> Result<Vec<(SharedString, f64)>> {
    let value: Vec<(Vec<u8>, f64)> = cmd("ZRANGE")
        .arg(key)
        .arg(start)
        .arg(stop)
        .arg("WITHSCORES")
        .query_async(conn)
        .await?;
    if value.is_empty() {
        return Ok(vec![]);
    }
    let values: Vec<(SharedString, f64)> = value
        .iter()
        .map(|(name, score)| {
            let name = String::from_utf8_lossy(name).to_string();
            (name.into(), *score)
        })
        .collect();
    Ok(values)
}

pub(crate) async fn first_load_zset_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("ZCARD").arg(key).query_async(conn).await?;
    let values = get_redis_zset_value(conn, key, 0, 99).await?;
    Ok(RedisValue {
        key_type: KeyType::Zset,
        data: Some(RedisValueData::Zset(Arc::new(RedisZsetValue {
            size,
            values,
            ..Default::default()
        }))),
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    pub fn load_more_zset_value(&mut self, cx: &mut Context<Self>) {
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

        // Check if we have valid zset data
        let current_len = match value.zset_value() {
            Some(zset) => zset.values.len(),
            None => return,
        };

        let server_id = self.server_id.clone();
        // Calculate pagination
        let start = current_len;
        let stop = start + 99; // Load 100 items
        cx.emit(ServerEvent::ValuePaginationStarted(key.clone()));
        let key_clone = key.clone();
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                // Fetch only the new items
                let new_values = get_redis_zset_value(&mut conn, &key, start, stop).await?;
                Ok(new_values)
            },
            move |this, result, cx| {
                if let Ok(new_values) = result
                    && !new_values.is_empty()
                {
                    // Update Local State (UI Thread)
                    // Append new items to the existing list
                    if let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                        let zset = Arc::make_mut(zset_data);
                        zset.values.extend(new_values);
                    }
                }
                cx.emit(ServerEvent::ValuePaginationFinished(key_clone));
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

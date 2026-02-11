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
    KeyType, RedisValueData, ServerTask, ZedisServerState,
    value::{RedisListValue, RedisValue, RedisValueStatus},
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
    states::ServerEvent,
};
use gpui::{SharedString, prelude::*};
use redis::{cmd, pipe};
use std::sync::Arc;
use uuid::Uuid;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Fetch a range of elements from a Redis List.
///
/// Returns a vector of strings. Binary data is lossily converted to UTF-8.
async fn get_redis_list_value(conn: &mut RedisAsyncConn, key: &str, start: usize, stop: usize) -> Result<Vec<String>> {
    // Fetch raw bytes to handle binary data safely
    let value: Vec<Vec<u8>> = cmd("LRANGE").arg(key).arg(start).arg(stop).query_async(conn).await?;
    if value.is_empty() {
        return Ok(vec![]);
    }
    let value: Vec<String> = value.iter().map(|v| String::from_utf8_lossy(v).to_string()).collect();
    Ok(value)
}

/// Initial load for a List key.
/// Fetches the total length (LLEN) and the first 100 items.
pub(crate) async fn first_load_list_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("LLEN").arg(key).query_async(conn).await?;
    let values = get_redis_list_value(conn, key, 0, 99).await?;
    Ok(RedisValue {
        key_type: KeyType::List,
        data: Some(RedisValueData::List(Arc::new(RedisListValue {
            size,
            values: values.into_iter().map(|v| v.into()).collect(),
            ..Default::default()
        }))),
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    /// A generic helper to execute Redis List operations with optimistic UI updates and rollback support.
    ///
    /// - `task`: The specific server task type for tracking.
    /// - `optimistic_update`: Logic to modify the local state immediately for better UI responsiveness.
    /// - `redis_op`: The actual async Redis command execution.
    /// - `rollback`: Logic to revert the local state if the Redis command fails.
    fn exec_list_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisListValue),
        redis_op: F,
        rollback: impl FnOnce(&mut RedisListValue) + Send + 'static,
    ) where
        // Corrected: Removed 'mut' keyword from the type definition
        F: FnOnce(String, RedisAsyncConn) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<R>> + Send,
    {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let key_str = key.to_string();

        // Step 1: Set status and perform optimistic UI update
        value.status = RedisValueStatus::Updating;
        if let Some(RedisValueData::List(list_data)) = value.data.as_mut() {
            optimistic_update(Arc::make_mut(list_data));
            cx.emit(ServerEvent::ValueUpdated);
        }
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;

        // Step 2: Spawn background task for Redis operation
        self.spawn(
            task,
            move || async move {
                let conn = get_connection_manager().get_connection(&server_id, db).await?;
                // Pass conn directly; 'mut' is handled inside the closure implementation
                redis_op(key_str, conn).await?;
                Ok(())
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;

                    // Step 3: Handle error by rolling back the local state
                    if result.is_err()
                        && let Some(RedisValueData::List(list_data)) = value.data.as_mut()
                    {
                        rollback(Arc::make_mut(list_data));
                        cx.emit(ServerEvent::ValueUpdated);
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn filter_list_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some((_, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let Some(list_value) = value.list_value() else {
            return;
        };
        let new_list_value = RedisListValue {
            keyword: Some(keyword.clone()),
            size: list_value.size,
            values: list_value.values.clone(),
        };
        value.data = Some(RedisValueData::List(Arc::new(new_list_value)));
        cx.emit(ServerEvent::ValueUpdated);
    }
    /// Removes an item at a specific index using a unique marker to ensure atomicity.
    pub fn remove_list_value(&mut self, index: usize, cx: &mut Context<Self>) {
        // Note: For List removal, rollback requires the original value.
        // In this simplified version, we focus on the shared structure.
        self.exec_list_op(
            ServerTask::RemoveListValue,
            cx,
            |list| {
                list.size -= 1;
                if index < list.values.len() {
                    list.values.remove(index);
                }
            },
            move |key, mut conn| async move {
                let marker = Uuid::new_v4().to_string();
                let _: () = pipe()
                    .atomic()
                    .cmd("LSET")
                    .arg(&key)
                    .arg(index)
                    .arg(&marker)
                    .cmd("LREM")
                    .arg(&key)
                    .arg(1)
                    .arg(&marker)
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |_list| { /* Optional: Re-fetch or re-insert if critical */ },
        );
    }
    /// Pushes a new value to the list (LPUSH or RPUSH).
    pub fn push_list_value(&mut self, new_value: SharedString, mode: SharedString, cx: &mut Context<Self>) {
        let is_lpush = mode == "1";
        let val_clone = new_value.clone();

        self.exec_list_op(
            ServerTask::PushListValue,
            cx,
            move |list| {
                list.size += 1;
                if is_lpush {
                    list.values.insert(0, val_clone);
                } else if list.values.len() + 1 == list.size {
                    list.values.push(val_clone);
                }
            },
            move |key, mut conn| async move {
                let cmd_name = if is_lpush { "LPUSH" } else { "RPUSH" };
                let _: () = cmd(cmd_name)
                    .arg(&key)
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            move |list| {
                list.size -= 1;
                if is_lpush {
                    list.values.remove(0);
                } else {
                    list.values.pop();
                }
            },
        );
    }
    /// Update a specific item in a Redis List.
    ///
    /// Performs an optimistic lock check: verifies if the current value at `index`
    /// matches `original_value` before updating.
    pub fn update_list_value(
        &mut self,
        index: usize,
        original: SharedString,
        new: SharedString,
        cx: &mut Context<Self>,
    ) {
        let new_val = new.clone();
        let old_val = original.clone();

        self.exec_list_op(
            ServerTask::UpdateListValue,
            cx,
            move |list| {
                if index < list.values.len() {
                    list.values[index] = new_val;
                }
            },
            move |key, mut conn| async move {
                // Optimistic check: Ensure value hasn't changed on server
                let current: String = cmd("LINDEX").arg(&key).arg(index).query_async(&mut conn).await?;
                if current != original.as_str() {
                    return Err(Error::Invalid {
                        message: "Value changed on server".into(),
                    });
                }
                let _: () = cmd("LSET")
                    .arg(&key)
                    .arg(index)
                    .arg(new.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            move |list| {
                if index < list.values.len() {
                    list.values[index] = old_val;
                }
            },
        );
    }
    /// Load the next page of items for the current List.
    pub fn load_more_list_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Check if we have valid list data
        let current_len = match value.list_value() {
            Some(list) => list.values.len(),
            None => return,
        };

        let server_id = self.server_id.clone();
        let db = self.db;
        // Calculate pagination
        let start = current_len;
        let stop = start + 99; // Load 100 items
        cx.emit(ServerEvent::ValuePaginationStarted);
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                // Fetch only the new items
                let new_values = get_redis_list_value(&mut conn, &key, start, stop).await?;
                Ok(new_values)
            },
            move |this, result, cx| {
                if let Ok(new_values) = result
                    && !new_values.is_empty()
                {
                    // Update Local State (UI Thread)
                    // Append new items to the existing list
                    if let Some(RedisValueData::List(list_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                        let list = Arc::make_mut(list_data);
                        list.values.extend(new_values.into_iter().map(|v| v.into()));
                    }
                }
                cx.emit(ServerEvent::ValuePaginationFinished);
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

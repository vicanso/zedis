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
use super::value::RedisValue;
use super::value::RedisValueStatus;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Fetch a range of elements from a Redis List.
///
/// Returns a vector of strings. Binary data is lossily converted to UTF-8.
async fn get_redis_list_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    start: usize,
    stop: usize,
) -> Result<Vec<String>> {
    // Fetch raw bytes to handle binary data safely
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

/// Initial load for a List key.
/// Fetches the total length (LLEN) and the first 100 items.
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
            values: values.into_iter().map(|v| v.into()).collect(),
        }))),
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    /// Update a specific item in a Redis List.
    ///
    /// Performs an optimistic lock check: verifies if the current value at `index`
    /// matches `original_value` before updating.
    pub fn update_list_value(
        &mut self,
        index: usize,
        original_value: SharedString,
        new_value: SharedString,
        cx: &mut Context<Self>,
    ) {
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
        value.status = RedisValueStatus::Updating;
        cx.notify();
        // Optimization: We don't clone the entire value here.
        // We only need basic info for the background task.
        let server = self.server.clone();

        // Prepare data for the async block (move ownership)
        let key_clone = key.clone();
        let original_value_clone = original_value.clone();
        let new_value_clone = new_value.clone();

        self.spawn(
            cx,
            "update_list_value",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;

                // 1. Optimistic Lock Check: Get current value
                let current_value: String = cmd("LINDEX")
                    .arg(key_clone.as_str())
                    .arg(index)
                    .query_async(&mut conn)
                    .await?;

                if current_value != original_value_clone {
                    return Err(Error::Invalid {
                        message: format!(
                            "Value changed (expected: '{}', actual: '{}'), update aborted.",
                            original_value_clone, current_value
                        ),
                    });
                }

                // 2. Perform Update
                let _: () = cmd("LSET")
                    .arg(key_clone.as_str())
                    .arg(index)
                    .arg(new_value_clone.as_str())
                    .query_async(&mut conn)
                    .await?;

                // Return the new value so UI thread can update local state
                Ok(new_value_clone)
            },
            move |this, result, cx| {
                if let Ok(updated_val) = result {
                    // 3. Update Local State (UI Thread)
                    // We modify the data here to avoid cloning the whole list in the background task.
                    if let Some(RedisValueData::List(list_data)) =
                        this.value.as_mut().and_then(|v| v.data.as_mut())
                    {
                        // Use Arc::make_mut to get mutable access (Cow behavior)
                        let list = Arc::make_mut(list_data);
                        if index < list.values.len() {
                            list.values[index] = updated_val;
                        }
                    }
                }
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
        );
    }
    /// Load the next page of items for the current List.
    pub fn load_more_list_value(&mut self, cx: &mut Context<Self>) {
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

        // Check if we have valid list data
        let current_len = match value.list_value() {
            Some(list) => list.values.len(),
            None => return,
        };

        let server = self.server.clone();
        // Calculate pagination
        let start = current_len;
        let stop = start + 99; // Load 100 items

        self.spawn(
            cx,
            "load_more_list",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;
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
                    if let Some(RedisValueData::List(list_data)) =
                        this.value.as_mut().and_then(|v| v.data.as_mut())
                    {
                        let list = Arc::make_mut(list_data);
                        list.values.extend(new_values.into_iter().map(|v| v.into()));
                    }
                }
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
        );
    }
}

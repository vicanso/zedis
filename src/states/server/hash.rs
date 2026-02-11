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

//! Redis HASH data type operations module.
//!
//! This module provides functionality for managing Redis HASH operations including:
//! - Loading HASH field-value pairs with pagination support via HSCAN
//! - Adding/updating fields in a HASH (HSET)
//! - Removing fields from a HASH (HDEL)
//! - Filtering HASH fields with pattern matching
//! - Efficient incremental loading for large HASHes

use super::{
    KeyType, RedisValueData, ServerTask, ZedisServerState,
    value::{RedisHashValue, RedisValue, RedisValueStatus},
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
    states::{ServerEvent, i18n_hash_editor},
};
use gpui::{SharedString, prelude::*};
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Type alias for HSCAN result: (cursor, vec of (field, value) pairs as bytes)
type HashScanValue = (u64, Vec<(Vec<u8>, Vec<u8>)>);

/// Retrieves HASH field-value pairs using Redis HSCAN command for cursor-based pagination.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The HASH key to scan
/// * `keyword` - Optional filter keyword for field names (will be wrapped with wildcards)
/// * `cursor` - Current cursor position (0 to start, returned cursor to continue)
/// * `count` - Hint for number of field-value pairs to return per iteration
///
/// # Returns
/// A tuple of (next_cursor, field-value pairs) where next_cursor is 0 when scan is complete
async fn get_redis_hash_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    keyword: Option<SharedString>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<(SharedString, SharedString)>)> {
    // Build pattern: wrap keyword with wildcards or match all fields
    let pattern = keyword
        .as_ref()
        .map(|kw| format!("*{}*", kw))
        .unwrap_or_else(|| "*".to_string());

    // Execute HSCAN with MATCH and COUNT options
    let (next_cursor, raw_values): HashScanValue = cmd("HSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;

    // Early return if no values found
    if raw_values.is_empty() {
        return Ok((next_cursor, vec![]));
    }

    // Convert bytes to UTF-8 strings (lossy conversion for non-UTF8 data)
    let values = raw_values
        .iter()
        .map(|(field, value)| {
            (
                String::from_utf8_lossy(field).to_string().into(),
                String::from_utf8_lossy(value).to_string().into(),
            )
        })
        .collect();

    Ok((next_cursor, values))
}

/// Performs initial load of a Redis HASH value.
///
/// Fetches the total number of fields (HLEN) and loads the first batch of field-value
/// pairs (up to 100). This is called when a HASH key is first opened in the editor.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The HASH key to load
///
/// # Returns
/// A `RedisValue` containing HASH metadata and initial field-value pairs
pub(crate) async fn first_load_hash_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    // Get total number of fields in the HASH
    let size: usize = cmd("HLEN").arg(key).query_async(conn).await?;

    // Load first batch of field-value pairs (up to 100)
    let (cursor, values) = get_redis_hash_value(conn, key, None, 0, 100).await?;

    // If cursor is 0, all values have been loaded in one iteration
    let done = cursor == 0;

    Ok(RedisValue {
        key_type: KeyType::Hash,
        data: Some(RedisValueData::Hash(Arc::new(RedisHashValue {
            cursor,
            size,
            values,
            done,
            ..Default::default()
        }))),
        ..Default::default()
    })
}
impl ZedisServerState {
    /// A generic helper for Redis Hash operations (Add, Remove, Update).
    /// Handles status switching, optimistic UI updates, and background task execution.
    fn exec_hash_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisHashValue),
        redis_op: F,
        on_success: impl FnOnce(&mut Self, R, &mut Context<Self>) + Send + 'static,
    ) where
        F: FnOnce(String, RedisAsyncConn) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<R>> + Send,
        R: Send + 'static,
    {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let key_str = key.to_string();
        value.status = RedisValueStatus::Updating;

        // Step 1: Perform local optimistic update
        if let Some(RedisValueData::Hash(hash_data)) = value.data.as_mut() {
            optimistic_update(Arc::make_mut(hash_data));
            cx.emit(ServerEvent::ValueUpdated);
        }
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;

        // Step 2: Spawn background task
        self.spawn(
            task,
            move || async move {
                let conn = get_connection_manager().get_connection(&server_id, db).await?;
                redis_op(key_str, conn).await
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }

                match result {
                    Ok(data) => on_success(this, data, cx),
                    Err(e) => this.emit_error_notification(e.to_string().into(), cx),
                }
                cx.notify();
            },
            cx,
        );
    }
    /// Adds a field-value pair in the Redis HASH.
    ///
    /// Uses HSET command which creates a new field or updates an existing one.
    /// Updates the UI state and shows appropriate notifications based on whether
    /// it was a new field (count=1) or an update (count=0).
    ///
    /// # Arguments
    /// * `field` - The field name to add
    /// * `value` - The value to set for the field
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn add_hash_value(&mut self, field: SharedString, value: SharedString, cx: &mut Context<Self>) {
        let field_clone = field.clone();
        let value_clone = value.clone();

        self.exec_hash_op(
            ServerTask::AddHashField,
            cx,
            |_| {}, // Wait for server confirmation to avoid duplicate UI entries during scan
            move |key, mut conn| async move {
                let count: usize = cmd("HSET")
                    .arg(&key)
                    .arg(field.as_str())
                    .arg(value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, count, cx| {
                if let Some(RedisValueData::Hash(hash_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let hash = Arc::make_mut(hash_data);
                    hash.size += count;
                    // Optimistically append if we are at the end of the scan
                    if hash.done && !hash.values.iter().any(|(f, _)| f == &field_clone) {
                        hash.values.push((field_clone, value_clone));
                    }
                }
                cx.emit(ServerEvent::ValueAdded);
                this.emit_success_notification(
                    i18n_hash_editor(cx, "add_value_success_tips"),
                    i18n_hash_editor(cx, "add_value_success"),
                    cx,
                );
            },
        );
    }
    /// Updates a field-value pair in the Redis HASH.
    ///
    /// Uses HSET command to update the value of the specified field.
    ///
    /// # Arguments
    /// * `old_field` - The old field name
    /// * `new_field` - The field name to update
    /// * `new_value` - The value to set for the field
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn update_hash_value(
        &mut self,
        old_field: SharedString,
        new_field: SharedString,
        new_value: SharedString,
        cx: &mut Context<Self>,
    ) {
        let old_field_clone = old_field.clone();
        let new_field_clone = new_field.clone();
        let new_value_clone = new_value.clone();
        let is_rename = old_field != new_field;

        self.exec_hash_op(
            ServerTask::UpdateHashField,
            cx,
            move |hash| {
                // Optimistic UI update: Replace old entry with new entry
                if let Some(pos) = hash.values.iter().position(|(f, _)| f == &old_field_clone) {
                    hash.values[pos] = (new_field_clone, new_value_clone);
                }
            },
            move |key, mut conn| async move {
                if is_rename {
                    // Pipeline: Insert new field then delete old field
                    let _: () = redis::pipe()
                        .atomic()
                        .cmd("HSET")
                        .arg(&key)
                        .arg(new_field.as_str())
                        .arg(new_value.as_str())
                        .cmd("HDEL")
                        .arg(&key)
                        .arg(old_field.as_str())
                        .query_async(&mut conn)
                        .await?;
                } else {
                    let _: () = cmd("HSET")
                        .arg(&key)
                        .arg(new_field.as_str())
                        .arg(new_value.as_str())
                        .query_async(&mut conn)
                        .await?;
                }
                Ok(())
            },
            |this, _, cx| {
                this.emit_info_notification(i18n_hash_editor(cx, "update_exist_field_value_success_tips"), cx);
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
        // self.add_or_update_hash_value(new_field, new_value, cx);
    }
    /// Applies a filter to HASH fields by resetting the scan state with a keyword.
    ///
    /// Creates a new HASH value state with the filter keyword and triggers a load.
    /// This allows users to search for specific fields matching a pattern.
    ///
    /// # Arguments
    /// * `keyword` - The search keyword to filter field names (will be wrapped with wildcards)
    /// * `cx` - GPUI context for UI updates
    pub fn filter_hash_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let Some(hash) = value.hash_value() else {
            return;
        };

        // Create new HASH state with filter keyword, reset cursor to start fresh scan
        let new_hash = RedisHashValue {
            keyword: Some(keyword),
            size: hash.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Hash(Arc::new(new_hash)));

        // Trigger load with the new filter
        self.load_more_hash_value(cx);
    }
    /// Removes a field from the Redis HASH.
    ///
    /// Uses HDEL command to delete the specified field and updates both the
    /// Redis field count and the local UI state.
    ///
    /// # Arguments
    /// * `remove_field` - The field name to remove from the HASH
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn remove_hash_value(&mut self, remove_field: SharedString, cx: &mut Context<Self>) {
        let remove_field_clone = remove_field.clone();
        self.exec_hash_op(
            ServerTask::RemoveHashField,
            cx,
            move |hash| {
                hash.size = hash.size.saturating_sub(1);
                hash.values.retain(|(f, _)| f != &remove_field_clone);
            },
            move |key, mut conn| async move {
                let count: usize = cmd("HDEL")
                    .arg(&key)
                    .arg(remove_field.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            |_, _, cx| {
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
    /// Loads the next batch of HASH field-value pairs using cursor-based pagination.
    ///
    /// Uses HSCAN to incrementally load field-value pairs without blocking on large HASHes.
    /// When filtering is active, uses larger batch sizes (1000) for better performance.
    ///
    /// # Arguments
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn load_more_hash_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI to show loading state
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Extract current cursor and filter keyword from HASH state
        let (cursor, keyword) = match value.hash_value() {
            Some(hash) => (hash.cursor, hash.keyword.clone()),
            None => return,
        };

        let server_id = self.server_id.clone();
        let db = self.db;
        cx.emit(ServerEvent::ValuePaginationStarted);

        self.spawn(
            ServerTask::LoadMoreValue,
            // Async operation: fetch next batch using HSCAN
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

                // Use larger batch size when filtering to reduce round trips
                let count = if keyword.is_some() { 1000 } else { 100 };

                get_redis_hash_value(&mut conn, &key, keyword, cursor, count).await
            },
            // UI callback: merge results into local state
            move |this, result, cx| {
                let mut should_load_more = false;
                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Hash(hash_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let hash = Arc::make_mut(hash_data);
                    hash.cursor = new_cursor;

                    // Mark as done when cursor returns to 0 (scan complete)
                    if new_cursor == 0 {
                        hash.done = true;
                    }

                    // Append new field-value pairs to existing list
                    if !new_values.is_empty() {
                        hash.values.extend(new_values);
                    }
                    if !hash.done && hash.values.len() < 50 {
                        should_load_more = true;
                    }
                }

                cx.emit(ServerEvent::ValuePaginationFinished);

                // Reset status to idle
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
                if should_load_more {
                    this.load_more_hash_value(cx);
                }
            },
            cx,
        );
    }
}

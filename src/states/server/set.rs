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
    value::{RedisSetValue, RedisValue, RedisValueStatus},
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
    states::{ServerEvent, i18n_set_editor},
};
use gpui::{SharedString, prelude::*};
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Retrieves SET members using Redis SSCAN command for cursor-based pagination.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The SET key to scan
/// * `keyword` - Optional filter keyword (will be wrapped with wildcards for pattern matching)
/// * `cursor` - Current cursor position (0 to start, returned cursor to continue)
/// * `count` - Hint for number of items to return per iteration
///
/// # Returns
/// A tuple of (next_cursor, values) where next_cursor is 0 when scan is complete
async fn get_redis_set_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    keyword: Option<SharedString>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<String>)> {
    // Build pattern: wrap keyword with wildcards or match all
    let pattern = keyword
        .as_ref()
        .map(|kw| format!("*{}*", kw))
        .unwrap_or_else(|| "*".to_string());

    // Execute SSCAN with MATCH and COUNT options
    let (next_cursor, raw_values): (u64, Vec<Vec<u8>>) = cmd("SSCAN")
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
        .map(|v| String::from_utf8_lossy(v).to_string())
        .collect();

    Ok((next_cursor, values))
}

/// Performs initial load of a Redis SET value.
///
/// Fetches the total cardinality (SCARD) and loads the first batch of members (up to 100).
/// This is called when a SET key is first opened in the editor.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The SET key to load
///
/// # Returns
/// A `RedisValue` containing SET metadata and initial member values
pub(crate) async fn first_load_set_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    // Get total number of members in the SET
    let size: usize = cmd("SCARD").arg(key).query_async(conn).await?;

    // Load first batch of values (up to 100 members)
    let (cursor, values) = get_redis_set_value(conn, key, None, 0, 100).await?;

    // If cursor is 0, all values have been loaded in one iteration
    let done = cursor == 0;

    Ok(RedisValue {
        key_type: KeyType::Set,
        data: Some(RedisValueData::Set(Arc::new(RedisSetValue {
            cursor,
            size,
            values: values.into_iter().map(|v| v.into()).collect(),
            done,
            ..Default::default()
        }))),
        ..Default::default()
    })
}

impl ZedisServerState {
    /// A generic helper for Redis Set operations (Add, Remove, Update).
    /// Handles status switching, optimistic UI updates, and background task execution.
    fn exec_set_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisSetValue),
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
        if let Some(RedisValueData::Set(set_data)) = value.data.as_mut() {
            optimistic_update(Arc::make_mut(set_data));
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
                    Err(e) => {
                        // Handle error (e.g., show notification and potentially rollback if needed)
                        this.emit_error_notification(e.to_string().into(), cx);
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn update_set_value(&mut self, old_value: SharedString, new_value: SharedString, cx: &mut Context<Self>) {
        let old_value_clone = old_value.clone();
        let new_value_clone = new_value.clone();

        self.exec_set_op(
            ServerTask::UpdateSetValue,
            cx,
            move |set| {
                if let Some(pos) = set.values.iter().position(|v| v == &old_value_clone) {
                    set.values[pos] = new_value_clone;
                }
            },
            move |key, mut conn| async move {
                // Use pipeline for atomic-like sequence of SREM and SADD
                let (_, count): (usize, usize) = redis::pipe()
                    .cmd("SREM")
                    .arg(&key)
                    .arg(old_value.as_str())
                    .cmd("SADD")
                    .arg(&key)
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            |_, _, cx| {
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
    /// Adds a new member to the Redis SET.
    ///
    /// Uses SADD command which only adds the member if it doesn't already exist.
    /// Updates the UI state and shows appropriate notifications based on the result.
    ///
    /// # Arguments
    /// * `new_value` - The member value to add to the SET
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn add_set_value(&mut self, new_value: SharedString, cx: &mut Context<Self>) {
        let val_clone = new_value.clone();

        self.exec_set_op(
            ServerTask::AddSetValue,
            cx,
            |_| {}, // No optimistic update for add to prevent duplicate UI entries before confirmation
            move |key, mut conn| async move {
                let count: usize = cmd("SADD")
                    .arg(&key)
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, count, cx| {
                if count == 0 {
                    this.emit_warning_notification(i18n_set_editor(cx, "add_value_exists_tips"), cx);
                } else if let Some(RedisValueData::Set(set_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let set = Arc::make_mut(set_data);
                    set.size += count;
                    // Only append to UI if scan is complete to maintain consistency
                    if set.done && !set.values.contains(&val_clone) {
                        set.values.push(val_clone);
                    }
                    this.emit_success_notification(
                        i18n_set_editor(cx, "add_value_success_tips"),
                        i18n_set_editor(cx, "add_value_success"),
                        cx,
                    );
                }
                cx.emit(ServerEvent::ValueAdded);
            },
        );
    }
    /// Applies a filter to SET members by resetting the scan state with a keyword.
    ///
    /// Creates a new SET value state with the filter keyword and triggers a load.
    /// This allows users to search for specific members matching a pattern.
    ///
    /// # Arguments
    /// * `keyword` - The search keyword to filter members (will be wrapped with wildcards)
    /// * `cx` - GPUI context for UI updates
    pub fn filter_set_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let Some(set) = value.set_value() else {
            return;
        };

        // Create new SET state with filter keyword, reset cursor to start fresh scan
        let new_set = RedisSetValue {
            keyword: Some(keyword.clone()),
            size: set.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Set(Arc::new(new_set)));

        // Trigger load with the new filter
        self.load_more_set_value(cx);
    }
    /// Loads the next batch of SET members using cursor-based pagination.
    ///
    /// Uses SSCAN to incrementally load members without blocking on large SETs.
    /// When filtering is active, uses larger batch sizes (1000) and automatically
    /// loads more batches until at least 50 items are collected or scan is complete.
    ///
    /// # Arguments
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn load_more_set_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI to show loading state
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Extract current cursor and filter keyword from SET state
        let (cursor, keyword) = match value.set_value() {
            Some(set) => (set.cursor, set.keyword.clone()),
            None => return,
        };

        let server_id = self.server_id.clone();
        let db = self.db;
        cx.emit(ServerEvent::ValuePaginationStarted);

        let keyword_clone = keyword.clone().unwrap_or_default();

        self.spawn(
            ServerTask::LoadMoreValue,
            // Async operation: fetch next batch using SSCAN
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

                // Use larger batch size when filtering to reduce round trips
                let count = if keyword.is_some() { 1000 } else { 100 };

                get_redis_set_value(&mut conn, &key, keyword, cursor, count).await
            },
            // UI callback: merge results and handle auto-loading for filters
            move |this, result, cx| {
                let mut should_load_more = false;

                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Set(set_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let set = Arc::make_mut(set_data);
                    set.cursor = new_cursor;

                    // Mark as done when cursor returns to 0 (scan complete)
                    if new_cursor == 0 {
                        set.done = true;
                    }

                    // Append new members to existing list
                    if !new_values.is_empty() {
                        set.values.extend(new_values.into_iter().map(|v| v.into()));
                    }

                    // Auto-load more batches when filtering until we have enough results
                    // This provides better UX by showing meaningful results immediately
                    if !keyword_clone.is_empty() && !set.done && set.values.len() < 50 {
                        should_load_more = true;
                    }
                }

                cx.emit(ServerEvent::ValuePaginationFinished);

                // Reset status to idle
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();

                // Recursively load more if needed
                if should_load_more {
                    this.load_more_set_value(cx);
                }
            },
            cx,
        );
    }
    /// Removes a member from the Redis SET.
    ///
    /// Uses SREM command to delete the specified member and updates both the
    /// Redis cardinality and the local UI state.
    ///
    /// # Arguments
    /// * `remove_value` - The member value to remove from the SET
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn remove_set_value(&mut self, remove_value: SharedString, cx: &mut Context<Self>) {
        let val_clone = remove_value.clone();

        self.exec_set_op(
            ServerTask::RemoveSetValue,
            cx,
            move |set| {
                set.size -= 1;
                set.values.retain(|v| v != &val_clone);
            },
            move |key, mut conn| async move {
                let count: usize = cmd("SREM")
                    .arg(&key)
                    .arg(remove_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            |_, _, cx| {
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
}

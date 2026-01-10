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
    value::{NotificationAction, RedisSetValue, RedisValue, RedisValueStatus},
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
    /// Adds a new member to the Redis SET.
    ///
    /// Uses SADD command which only adds the member if it doesn't already exist.
    /// Updates the UI state and shows appropriate notifications based on the result.
    ///
    /// # Arguments
    /// * `new_value` - The member value to add to the SET
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn add_set_value(&mut self, new_value: SharedString, cx: &mut Context<Self>) {
        // Early return if no key/value is selected
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI state to show "updating" status
        value.status = RedisValueStatus::Updating;
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;
        let key_clone = key.clone();
        let new_value_clone = new_value.clone();

        self.spawn(
            ServerTask::AddSetValue,
            // Async operation: execute SADD on Redis
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

                // SADD returns number of elements added (0 if already exists, 1 if new)
                let count: usize = cmd("SADD")
                    .arg(key.as_str())
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            // UI callback: handle result and update state
            move |this, result, cx| {
                let Some(value) = this.value.as_mut() else {
                    return;
                };
                value.status = RedisValueStatus::Idle;

                if let Ok(count) = result {
                    if count == 0 {
                        // Value already exists in SET
                        let msg = i18n_set_editor(cx, "add_value_exists_tips");
                        cx.emit(ServerEvent::Notification(NotificationAction::new_warning(msg)));
                    } else {
                        // Successfully added new value
                        let title = i18n_set_editor(cx, "add_value_success");
                        let msg = i18n_set_editor(cx, "add_value_success_tips");

                        if let Some(RedisValueData::Set(set_data)) = value.data.as_mut() {
                            let set = Arc::make_mut(set_data);
                            set.size += count;

                            // Only add to UI list if scan is complete and value isn't already shown
                            if set.done && !set.values.contains(&new_value_clone) {
                                set.values.push(new_value_clone);
                            }

                            cx.emit(ServerEvent::Notification(
                                NotificationAction::new_success(msg).with_title(title),
                            ));
                        }
                    }
                }

                cx.emit(ServerEvent::ValueAdded(key_clone));
                cx.notify();
            },
            cx,
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
        cx.emit(ServerEvent::ValuePaginationStarted(key.clone()));

        let key_clone = key.clone();
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

                cx.emit(ServerEvent::ValuePaginationFinished(key_clone));

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
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI state to show loading
        value.status = RedisValueStatus::Loading;
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;
        let remove_value_clone = remove_value.clone();
        let key_clone = key.clone();

        self.spawn(
            ServerTask::RemoveSetValue,
            // Async operation: execute SREM on Redis
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

                // SREM returns number of members removed (0 if doesn't exist, 1 if removed)
                let count: usize = cmd("SREM")
                    .arg(key.as_str())
                    .arg(remove_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            // UI callback: update local state to reflect removal
            move |this, result, cx| {
                if let Ok(count) = result
                    && let Some(RedisValueData::Set(set_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let set = Arc::make_mut(set_data);

                    // Decrease SET size by number of removed members
                    set.size -= count;

                    // Remove from local values list
                    set.values.retain(|v| v != &remove_value_clone);
                }

                cx.emit(ServerEvent::ValueUpdated(key_clone));

                // Reset status to idle
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

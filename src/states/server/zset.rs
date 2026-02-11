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

//! Redis ZSET (Sorted Set) data type operations module.
//!
//! This module provides functionality for managing Redis ZSET operations including:
//! - Loading ZSET values with range-based pagination (ZRANGE/ZREVRANGE)
//! - Adding/updating members with scores (ZADD)
//! - Removing members from a ZSET (ZREM)
//! - Filtering ZSET members with pattern matching (ZSCAN)
//! - Support for ascending and descending sort orders
//! - Efficient incremental loading for large ZSETs

use super::{
    KeyType, RedisValueData, ServerTask, ZedisServerState,
    value::{RedisValue, RedisValueStatus, RedisZsetValue, SortOrder},
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
    states::{ServerEvent, i18n_zset_editor},
};
use gpui::{SharedString, prelude::*};
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Retrieves ZSET members using range-based commands (ZRANGE or ZREVRANGE).
///
/// This function is used for non-filtered pagination, loading members by their
/// rank position in the sorted set.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The ZSET key to query
/// * `sort_order` - Ascending (ZRANGE) or Descending (ZREVRANGE)
/// * `start` - Starting rank index (0-based)
/// * `stop` - Ending rank index (inclusive)
///
/// # Returns
/// A vector of (member, score) tuples in the specified sort order
async fn get_redis_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    sort_order: SortOrder,
    start: usize,
    stop: usize,
) -> Result<Vec<(SharedString, f64)>> {
    // Choose command based on sort order
    let cmd_name = if sort_order == SortOrder::Asc {
        "ZRANGE"
    } else {
        "ZREVRANGE"
    };

    // Execute range query with scores
    let raw_values: Vec<(Vec<u8>, f64)> = cmd(cmd_name)
        .arg(key)
        .arg(start)
        .arg(stop)
        .arg("WITHSCORES")
        .query_async(conn)
        .await?;

    // Early return if no values found
    if raw_values.is_empty() {
        return Ok(vec![]);
    }

    // Convert bytes to UTF-8 strings (lossy conversion for non-UTF8 data)
    let values = raw_values
        .iter()
        .map(|(name, score)| {
            let name = String::from_utf8_lossy(name).to_string();
            (name.into(), *score)
        })
        .collect();

    Ok(values)
}

/// Searches ZSET members using cursor-based ZSCAN command with pattern matching.
///
/// This function is used when filtering is active, allowing users to search for
/// members matching a specific pattern.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The ZSET key to scan
/// * `cursor` - Current cursor position (0 to start, returned cursor to continue)
/// * `pattern` - Pattern to match members against (supports wildcards)
/// * `count` - Hint for number of items to return per iteration
///
/// # Returns
/// A tuple of (next_cursor, values) where next_cursor is 0 when scan is complete
async fn search_redis_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: u64,
    pattern: &str,
    count: u64,
) -> Result<(u64, Vec<(SharedString, f64)>)> {
    // Execute ZSCAN with MATCH and COUNT options
    let (next_cursor, raw_values): (u64, Vec<Vec<u8>>) = cmd("ZSCAN")
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

    // ZSCAN returns alternating member/score pairs, process in chunks of 2
    let mut values = Vec::with_capacity(raw_values.len() / 2);
    for chunk in raw_values.chunks(2) {
        let member = &chunk[0];
        let score_bytes = &chunk[1];

        // Parse score from bytes
        let score_str = String::from_utf8_lossy(score_bytes).to_string();
        let score = score_str.parse::<f64>().unwrap_or_default();

        // Convert member to string
        let name = String::from_utf8_lossy(member).to_string();
        values.push((name.into(), score));
    }

    Ok((next_cursor, values))
}

/// Performs initial load of a Redis ZSET value.
///
/// Fetches the total cardinality (ZCARD) and loads the first batch of members (up to 100).
/// This is called when a ZSET key is first opened in the editor.
///
/// # Arguments
/// * `conn` - Redis async connection
/// * `key` - The ZSET key to load
/// * `sort_order` - Initial sort order (Ascending or Descending)
///
/// # Returns
/// A `RedisValue` containing ZSET metadata and initial member/score pairs
pub(crate) async fn first_load_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    sort_order: SortOrder,
) -> Result<RedisValue> {
    // Get total number of members in the ZSET
    let size: usize = cmd("ZCARD").arg(key).query_async(conn).await?;

    // Load first batch (ranks 0-99, i.e., 100 members)
    let values = get_redis_zset_value(conn, key, sort_order, 0, 99).await?;

    Ok(RedisValue {
        key_type: KeyType::Zset,
        data: Some(RedisValueData::Zset(Arc::new(RedisZsetValue {
            size,
            values,
            sort_order,
            ..Default::default()
        }))),
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    /// A generic helper for ZSET operations (Add, Update, Remove).
    /// Handles status updates, UI synchronization, and background task execution.
    fn exec_zset_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisZsetValue),
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

        // Perform optimistic update on local state
        if let Some(RedisValueData::Zset(zset_data)) = value.data.as_mut() {
            optimistic_update(Arc::make_mut(zset_data));
            cx.emit(ServerEvent::ValueUpdated);
        }
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;

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

    /// Adds or updates a member in the Redis ZSET with the specified score.
    ///
    /// Uses ZADD command which updates the score if the member already exists,
    /// or adds it as a new member if it doesn't. The method intelligently updates
    /// the local UI state by either updating existing members or inserting new ones
    /// in the correct sorted position.
    ///
    /// # Arguments
    /// * `new_value` - The member name to add/update
    /// * `score` - The score to assign to the member
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn add_zset_value(&mut self, new_value: SharedString, score: f64, cx: &mut Context<Self>) {
        self.add_or_update_zset_value(new_value, score, None, cx);
    }
    /// Updates a member in the Redis ZSET with the specified score.
    ///
    /// Uses ZADD command to update the score of the specified member.
    ///
    /// # Arguments
    /// * `new_value` - The member name to update
    /// * `score` - The score to assign to the member
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn update_zset_value(
        &mut self,
        old_value: SharedString,
        new_value: SharedString,
        score: f64,
        cx: &mut Context<Self>,
    ) {
        self.add_or_update_zset_value(new_value, score, Some(old_value), cx);
    }
    fn add_or_update_zset_value(
        &mut self,
        new_value: SharedString,
        score: f64,
        old_value: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let new_value_clone = new_value.clone();
        let old_value_clone = old_value.clone();
        let is_removed = old_value.is_some();

        self.exec_zset_op(
            ServerTask::AddZsetValue,
            cx,
            move |zset| {
                let mut exists = false;
                // Update if exists or handle "rename" logic
                if let Some(ref old) = old_value_clone
                    && let Some(item) = zset.values.iter_mut().find(|v| &v.0 == old)
                {
                    *item = (new_value_clone.clone(), score);
                    exists = true;
                }

                if !exists {
                    // Remove old if this is a rename that wasn't found in current visible page
                    if let Some(ref old) = old_value_clone {
                        zset.values.retain(|v| &v.0 != old);
                    }

                    // Insert into correct position using binary search if no filter is active
                    if zset.keyword.is_none() {
                        let idx = zset.values.partition_point(|(_, s)| {
                            if zset.sort_order == SortOrder::Asc {
                                *s < score
                            } else {
                                *s > score
                            }
                        });
                        zset.values.insert(idx, (new_value_clone, score));
                    }
                }
            },
            move |key, mut conn| async move {
                let count: usize = cmd("ZADD")
                    .arg(&key)
                    .arg(score)
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                if let Some(old) = old_value {
                    let _: () = cmd("ZREM").arg(&key).arg(old.as_str()).query_async(&mut conn).await?;
                }
                Ok(count)
            },
            move |this, count, cx| {
                if !is_removed
                    && let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    Arc::make_mut(zset_data).size += count;
                }
                cx.emit(ServerEvent::ValueAdded);
                this.emit_success_notification(
                    i18n_zset_editor(cx, "add_value_success_tips"),
                    i18n_zset_editor(cx, "add_value_success"),
                    cx,
                );
            },
        );
    }
    /// Applies a filter to ZSET members by resetting the scan state with a keyword.
    ///
    /// Creates a new ZSET value state with the filter keyword and triggers a scan-based load.
    /// When filtering, the system switches from range-based (ZRANGE) to scan-based (ZSCAN)
    /// pagination to support pattern matching.
    ///
    /// # Arguments
    /// * `keyword` - The search keyword to filter members (empty to clear filter)
    /// * `cx` - GPUI context for UI updates
    pub fn filter_zset_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some((_, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let Some(zset) = value.zset_value() else {
            return;
        };

        // Convert empty string to None for consistency
        let keyword = if keyword.is_empty() { None } else { Some(keyword) };

        // Create new ZSET state with filter keyword, reset cursor to start fresh scan
        let new_zset = RedisZsetValue {
            keyword,
            size: zset.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Zset(Arc::new(new_zset)));

        // Trigger load with the new filter
        self.load_more_zset_value(cx);
    }
    /// Loads the next batch of ZSET members using appropriate pagination strategy.
    ///
    /// Uses two different strategies based on whether filtering is active:
    /// - **No filter**: Range-based pagination (ZRANGE/ZREVRANGE) for efficient rank access
    /// - **With filter**: Cursor-based ZSCAN for pattern matching support
    ///
    /// When filtering, automatically loads more batches until at least 50 items are
    /// collected or scan is complete.
    ///
    /// # Arguments
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn load_more_zset_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI to show loading state
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Extract current ZSET state
        let Some(zset) = value.zset_value() else {
            return;
        };
        let current_len = zset.values.len();
        let sort_order = zset.sort_order;
        let keyword = zset.keyword.clone().unwrap_or_default();
        let cursor = zset.cursor;

        let server_id = self.server_id.clone();
        let db = self.db;

        // Calculate range for pagination (load 100 items)
        let start = current_len;
        let stop = start + 99;

        cx.emit(ServerEvent::ValuePaginationStarted);
        let keyword_clone = keyword.clone();

        self.spawn(
            ServerTask::LoadMoreValue,
            // Async operation: fetch next batch using appropriate strategy
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;

                if keyword.is_empty() {
                    // No filter: use range-based pagination
                    let values = get_redis_zset_value(&mut conn, &key, sort_order, start, stop).await?;
                    Ok((0, values)) // Cursor is irrelevant for range queries
                } else {
                    // With filter: use scan-based pagination with pattern matching
                    let pattern = format!("*{keyword}*");
                    let result = search_redis_zset_value(&mut conn, &key, cursor, &pattern, 1000).await?;
                    Ok(result)
                }
            },
            // UI callback: merge results and handle auto-loading for filters
            move |this, result, cx| {
                let mut should_load_more = false;

                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let zset = Arc::make_mut(zset_data);

                    // Append new members to existing list
                    if !new_values.is_empty() {
                        zset.values.extend(new_values);
                    }

                    // Handle cursor state for filtered searches
                    if !keyword_clone.is_empty() {
                        zset.cursor = new_cursor;

                        // Mark as done when cursor returns to 0 (scan complete)
                        if new_cursor == 0 {
                            zset.done = true;
                        }

                        // Auto-load more batches when filtering until we have enough results
                        // This provides better UX by showing meaningful results immediately
                        if !zset.done && zset.values.len() < 50 {
                            should_load_more = true;
                        }
                    }
                }

                // Reset status to idle
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }

                cx.emit(ServerEvent::ValuePaginationFinished);
                cx.notify();

                // Recursively load more if needed
                if should_load_more {
                    this.load_more_zset_value(cx);
                }
            },
            cx,
        );
    }
    /// Removes a member from the Redis ZSET.
    ///
    /// Uses ZREM command to delete the specified member and updates the local UI state
    /// by removing it from the values list.
    ///
    /// # Arguments
    /// * `remove_value` - The member name to remove from the ZSET
    /// * `cx` - GPUI context for spawning async tasks and UI updates
    pub fn remove_zset_value(&mut self, remove_value: SharedString, cx: &mut Context<Self>) {
        let remove_value_clone = remove_value.clone();
        self.exec_zset_op(
            ServerTask::RemoveZsetValue,
            cx,
            move |zset| {
                zset.size = zset.size.saturating_sub(1);
                zset.values.retain(|(name, _)| name != &remove_value_clone);
            },
            move |key, mut conn| async move {
                let _: () = cmd("ZREM")
                    .arg(&key)
                    .arg(remove_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |_, _, cx| {
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
}

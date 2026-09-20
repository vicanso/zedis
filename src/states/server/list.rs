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
    element::KvElement,
    value::{RedisListValue, RedisValue, RedisValueStatus},
};
use crate::helpers::unix_ts;
use crate::{
    connection::{ServerDb, list_len, list_push, list_range, list_set_if_unchanged, remove_list_indexes},
    error::Error,
    states::ServerEvent,
};
use bytes::Bytes;
use gpui::{SharedString, prelude::*};
use std::sync::Arc;
use zedis_core::change_log::ChangeEntry;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Fetch a range of elements from a Redis List, bytes kept as answered.
async fn get_redis_list_value(at: &ServerDb, key: &str, start: usize, stop: usize) -> Result<Vec<KvElement>> {
    let value = list_range(at, key, start, stop).await?;
    Ok(value.into_iter().map(KvElement::from_raw).collect())
}

/// Initial load for a List key.
/// Fetches the total length (LLEN) and the first 100 items.
pub(crate) async fn first_load_list_value(at: &ServerDb, key: &str) -> Result<RedisValue> {
    let size = list_len(at, key).await?;
    let values = get_redis_list_value(at, key, 0, 99).await?;
    Ok(RedisValue {
        key_type: KeyType::List,
        data: Some(RedisValueData::List(Arc::new(RedisListValue {
            size,
            values,
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
        // Runs only when the write succeeded — where the change log records,
        // so a failed write never appears in it.
        on_success: impl FnOnce(&mut Self, &mut Context<Self>) + Send + 'static,
    ) where
        F: FnOnce(String, ServerDb) -> Fut + Send + 'static,
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

        let at = self.at();

        // Step 2: Spawn background task for Redis operation
        self.spawn(
            task,
            move || async move {
                redis_op(key_str, at).await?;
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
                if result.is_ok() {
                    on_success(this, cx);
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
        let log_key = self.key.clone();
        let log_old = self
            .value
            .as_ref()
            .and_then(|v| v.list_value())
            .and_then(|l| l.values.get(index).map(|v| v.text().to_string()));
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
            move |key, at| async move {
                // Redis cannot remove by position; `remove_list_indexes` is
                // the marker workaround, in one MULTI.
                remove_list_indexes(&at, &key, &[index]).await?;
                Ok(())
            },
            |_list| { /* Optional: Re-fetch or re-insert if critical */ },
            move |this, _cx| {
                if let Some(log_key) = log_key {
                    this.record_changes(
                        log_key,
                        vec![ChangeEntry::element(
                            unix_ts(),
                            format!("#{index}"),
                            log_old.as_deref(),
                            None,
                        )],
                    );
                }
            },
        );
    }
    /// Removes several positions in one round trip — the table's
    /// multi-select delete. The renumbering that makes repeated index
    /// deletion wrong is handled by [`remove_list_indexes`].
    pub fn remove_list_values(&mut self, indexes: Vec<usize>, cx: &mut Context<Self>) {
        if indexes.is_empty() {
            return;
        }
        let optimistic = indexes.clone();
        let log_key = self.key.clone();
        let log_removed: Vec<(usize, Option<String>)> = {
            let loaded = self.value.as_ref().and_then(|v| v.list_value());
            indexes
                .iter()
                .map(|i| {
                    (
                        *i,
                        loaded
                            .as_ref()
                            .and_then(|l| l.values.get(*i).map(|v| v.text().to_string())),
                    )
                })
                .collect()
        };
        self.exec_list_op(
            ServerTask::RemoveListValue,
            cx,
            move |list| {
                // Descending, so each removal leaves the lower indexes valid.
                let mut sorted = optimistic;
                sorted.sort_unstable_by(|a, b| b.cmp(a));
                sorted.dedup();
                for index in sorted {
                    if index < list.values.len() {
                        list.values.remove(index);
                        list.size = list.size.saturating_sub(1);
                    }
                }
            },
            move |key, at| async move {
                remove_list_indexes(&at, &key, &indexes).await?;
                Ok(())
            },
            |_list| {},
            move |this, _cx| {
                if let Some(log_key) = log_key {
                    let at = unix_ts();
                    let entries = log_removed
                        .into_iter()
                        .map(|(index, old)| ChangeEntry::element(at, format!("#{index}"), old.as_deref(), None))
                        .collect();
                    this.record_changes(log_key, entries);
                }
            },
        );
    }

    /// Pushes a new value to the list (LPUSH or RPUSH).
    pub fn push_list_value(&mut self, new_value: SharedString, mode: SharedString, cx: &mut Context<Self>) {
        let is_lpush = mode == "1";
        let val_clone = KvElement::from_text(&new_value);
        let log_key = self.key.clone();
        let log_value = new_value.to_string();

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
            move |key, at| async move {
                list_push(&at, &key, new_value.as_bytes(), is_lpush).await?;
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
            move |this, _cx| {
                if let Some(log_key) = log_key {
                    // Named by command: a position would be stale by the next push.
                    let target = if is_lpush { "LPUSH" } else { "RPUSH" };
                    this.record_changes(
                        log_key,
                        vec![ChangeEntry::element(unix_ts(), target, None, Some(log_value.as_str()))],
                    );
                }
            },
        );
    }
    /// Update a specific item in a Redis List.
    ///
    /// Performs an optimistic lock check: the element at `index` must still
    /// hold `original`'s bytes before `new` is written.
    pub fn update_list_value(&mut self, index: usize, original: KvElement, new: Bytes, cx: &mut Context<Self>) {
        let new = KvElement::from_raw(new);
        let new_val = new.clone();
        let old_val = original.clone();
        let log_key = self.key.clone();
        let log_old = original.text().to_string();
        let log_new = new.text().to_string();

        self.exec_list_op(
            ServerTask::UpdateListValue,
            cx,
            move |list| {
                if index < list.values.len() {
                    list.values[index] = new_val;
                }
            },
            move |key, at| async move {
                // Optimistic check: the row is only written while it still
                // holds what the editor loaded.
                if !list_set_if_unchanged(&at, &key, index, original.raw(), new.raw()).await? {
                    return Err(Error::Invalid {
                        message: "Value changed on server".into(),
                    });
                }
                Ok(())
            },
            move |list| {
                if index < list.values.len() {
                    list.values[index] = old_val;
                }
            },
            move |this, _cx| {
                if let Some(log_key) = log_key {
                    this.record_changes(
                        log_key,
                        vec![ChangeEntry::element(
                            unix_ts(),
                            format!("#{index}"),
                            Some(log_old.as_str()),
                            Some(log_new.as_str()),
                        )],
                    );
                }
            },
        );
    }
    /// Load the next page of items for the current List.
    pub fn load_more_list_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Check if we have valid list data
        let current_len = match value.list_value() {
            Some(list) => list.values.len(),
            None => return,
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();

        let at = self.at();
        // Calculate pagination
        let start = current_len;
        let stop = start + 99; // Load 100 items
        cx.emit(ServerEvent::ValuePaginationStarted);
        self.spawn_with_arg(
            ServerTask::LoadMoreValue,
            key.clone(),
            move || async move {
                // Fetch only the new items
                let new_values = get_redis_list_value(&at, &key, start, stop).await?;
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
                        list.values.extend(new_values);
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

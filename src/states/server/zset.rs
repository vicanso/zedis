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
use super::value::SortOrder;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::NotificationAction;
use crate::states::ServerEvent;
use crate::states::i18n_zset_editor;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

async fn get_redis_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    sort_order: SortOrder,
    start: usize,
    stop: usize,
) -> Result<Vec<(SharedString, f64)>> {
    let cmd_name = if sort_order == SortOrder::Asc {
        "ZRANGE"
    } else {
        "ZREVRANGE"
    };

    let value: Vec<(Vec<u8>, f64)> = cmd(cmd_name)
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

async fn search_redis_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: u64,
    pattern: &str,
    count: u64,
) -> Result<(u64, Vec<(SharedString, f64)>)> {
    let (cursor, value): (u64, Vec<Vec<u8>>) = cmd("ZSCAN")
        .arg(key)
        .arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;
    if value.is_empty() {
        return Ok((cursor, vec![]));
    }
    let mut values = Vec::with_capacity(value.len() / 2);
    for chunk in value.chunks(2) {
        let member = &chunk[0];
        let score_str = String::from_utf8_lossy(&chunk[1]).to_string();
        let name = String::from_utf8_lossy(member).to_string();
        let score = score_str.parse::<f64>().unwrap_or_default();
        values.push((name.into(), score));
    }
    Ok((cursor, values))
}

pub(crate) async fn first_load_zset_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    sort_order: SortOrder,
) -> Result<RedisValue> {
    let size: usize = cmd("ZCARD").arg(key).query_async(conn).await?;
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
    pub fn add_zset_value(&mut self, new_value: SharedString, score: f64, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        cx.notify();
        let server_id = self.server_id.clone();
        let key_clone = key.clone();
        let new_value_clone = new_value.clone();
        self.spawn(
            ServerTask::AddZsetValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                let count: usize = cmd("ZADD")
                    .arg(key.as_str())
                    .arg(score)
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, result, cx| {
                let title = i18n_zset_editor(cx, "add_value_success");
                let msg = i18n_zset_editor(cx, "add_value_success_tips");
                if let Ok(count) = result
                    && let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let zset = Arc::make_mut(zset_data);
                    zset.size += count;

                    let mut inserted = false;
                    let mut exists_value = false;
                    for item in zset.values.iter_mut() {
                        if item.0 == new_value_clone {
                            *item = (new_value_clone.clone(), score);
                            exists_value = true;
                            break;
                        }
                    }
                    // zset is not filtered, so we need to add the value to the correct position
                    if !exists_value && zset.keyword.is_none() {
                        let index = zset.values.partition_point(|(_, value)| {
                            if zset.sort_order == SortOrder::Asc {
                                *value < score
                            } else {
                                *value > score
                            }
                        });
                        if index != zset.values.len() {
                            zset.values.insert(index, (new_value_clone, score));
                            inserted = true;
                        }
                    }
                    cx.emit(ServerEvent::ValueAdded(key_clone));
                    if !inserted {
                        cx.dispatch_action(&NotificationAction::new_success(msg).with_title(title));
                    }
                }
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn filter_zset_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some((_, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let Some(zset) = value.zset_value() else {
            return;
        };
        let keyword = if keyword.is_empty() {
            None
        } else {
            Some(keyword.clone())
        };
        let new_zset = RedisZsetValue {
            keyword,
            size: zset.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Zset(Arc::new(new_zset)));
        self.load_more_zset_value(cx);
    }
    pub fn load_more_zset_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Check if we have valid zset data
        let Some(zset) = value.zset_value() else {
            return;
        };
        let current_len = zset.values.len();
        let sort_order = zset.sort_order;
        let keyword = zset.keyword.clone().unwrap_or_default();
        let cursor = zset.cursor;

        let server_id = self.server_id.clone();
        // Calculate pagination
        let start = current_len;
        let stop = start + 99; // Load 100 items
        cx.emit(ServerEvent::ValuePaginationStarted(key.clone()));
        let key_clone = key.clone();
        let keyword_clone = keyword.clone();
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                // Fetch only the new items
                if keyword.is_empty() {
                    let values = get_redis_zset_value(&mut conn, &key, sort_order, start, stop).await?;
                    Ok((0, values))
                } else {
                    let result =
                        search_redis_zset_value(&mut conn, &key, cursor, format!("*{keyword}*").as_str(), 1000).await?;
                    Ok(result)
                }
            },
            move |this, result, cx| {
                let mut should_load_more = false;
                if let Ok((new_cursor, new_values)) = result {
                    // Update Local State (UI Thread)
                    // Append new items to the existing list
                    if let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                        let zset = Arc::make_mut(zset_data);
                        if !new_values.is_empty() {
                            zset.values.extend(new_values);
                        }
                        if !keyword_clone.is_empty() {
                            zset.cursor = new_cursor;
                            if new_cursor == 0 {
                                zset.done = true;
                            }
                            if !zset.done && zset.values.len() < 50 {
                                should_load_more = true;
                            }
                        }
                    }
                }
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.emit(ServerEvent::ValuePaginationFinished(key_clone));
                cx.notify();
                if should_load_more {
                    this.load_more_zset_value(cx);
                }
            },
            cx,
        );
    }
    pub fn remove_zset_value(&mut self, remove_value: SharedString, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();
        let server_id = self.server_id.clone();
        let remove_value_clone = remove_value.clone();
        let key_clone = key.clone();
        self.spawn(
            ServerTask::RemoveZsetValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                let _: () = cmd("ZREM")
                    .arg(key.as_str())
                    .arg(remove_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result
                    && let Some(RedisValueData::Zset(zset_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let zset = Arc::make_mut(zset_data);
                    zset.values.retain(|(name, _)| name != &remove_value_clone);
                }
                cx.emit(ServerEvent::ValueUpdated(key_clone));
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

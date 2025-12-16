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
use super::value::RedisHashValue;
use super::value::RedisValue;
use super::value::RedisValueStatus;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::NotificationAction;
use crate::states::ServerEvent;
use crate::states::i18n_hash_editor;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

type HashScanValue = (u64, Vec<(Vec<u8>, Vec<u8>)>);

async fn get_redis_hash_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    keyword: Option<SharedString>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<(SharedString, SharedString)>)> {
    let pattern = if let Some(keyword) = keyword {
        format!("*{}*", keyword)
    } else {
        "*".to_string()
    };
    let (cursor, value): HashScanValue = cmd("HSCAN")
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
    let value = value
        .iter()
        .map(|(field, value)| {
            (
                String::from_utf8_lossy(field).to_string().into(),
                String::from_utf8_lossy(value).to_string().into(),
            )
        })
        .collect();
    Ok((cursor, value))
}

pub(crate) async fn first_load_hash_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("HLEN").arg(key).query_async(conn).await?;
    let (cursor, values) = get_redis_hash_value(conn, key, None, 0, 100).await?;
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
    pub fn add_hash_value(&mut self, new_field: SharedString, new_value: SharedString, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        cx.notify();
        let server_id = self.server_id.clone();
        let key_clone = key.clone();
        self.spawn(
            ServerTask::AddSetValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;

                let count: usize = cmd("HSET")
                    .arg(key.as_str())
                    .arg(new_field.as_str())
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, result, cx| {
                let title = i18n_hash_editor(cx, "add_value_success");
                let msg = i18n_hash_editor(cx, "add_value_success_tips");
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                    if let Ok(count) = result
                        && let Some(RedisValueData::Hash(hash_data)) = value.data.as_mut()
                    {
                        let hash = Arc::make_mut(hash_data);
                        hash.size += count;
                        cx.dispatch_action(&NotificationAction::new_success(msg).with_title(title));
                        cx.emit(ServerEvent::ValueAdded(key_clone));
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn filter_hash_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let Some(hash) = value.hash_value() else {
            return;
        };
        let new_hash = RedisHashValue {
            keyword: Some(keyword.clone()),
            size: hash.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Hash(Arc::new(new_hash)));
        self.load_more_hash_value(cx);
    }
    pub fn remove_hash_value(&mut self, remove_field: SharedString, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();
        let server_id = self.server_id.clone();
        let remove_field_clone = remove_field.clone();
        let key_clone = key.clone();
        self.spawn(
            ServerTask::RemoveHashValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                let count: usize = cmd("HDEL")
                    .arg(key.as_str())
                    .arg(remove_field.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, result, cx| {
                if let Ok(count) = result {
                    if count != 0
                        && let Some(RedisValueData::Hash(hash_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                    {
                        let hash = Arc::make_mut(hash_data);
                        hash.values.retain(|(field, _)| field != &remove_field_clone);
                        hash.size -= count;
                    }
                    cx.emit(ServerEvent::ValueUpdated(key_clone));
                    if let Some(value) = this.value.as_mut() {
                        value.status = RedisValueStatus::Idle;
                    }
                    cx.notify();
                }
            },
            cx,
        );
    }
    pub fn load_more_hash_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };
        value.status = RedisValueStatus::Loading;
        cx.notify();

        // Check if we have valid hash data
        let (cursor, keyword) = match value.hash_value() {
            Some(hash) => (hash.cursor, hash.keyword.clone()),
            None => return,
        };

        let server_id = self.server_id.clone();
        cx.emit(ServerEvent::ValuePaginationStarted(key.clone()));
        let key_clone = key.clone();
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                // Fetch only the new items
                let count = if keyword.is_some() { 1000 } else { 100 };
                let result = get_redis_hash_value(&mut conn, &key, keyword, cursor, count).await?;
                Ok(result)
            },
            move |this, result, cx| {
                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Hash(hash_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let hash = Arc::make_mut(hash_data);
                    hash.cursor = new_cursor;
                    if new_cursor == 0 {
                        hash.done = true;
                    }

                    if !new_values.is_empty() {
                        // Append new items to the existing list
                        hash.values.extend(new_values);
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

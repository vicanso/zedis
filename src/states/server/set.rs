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
use super::value::NotificationAction;
use super::value::RedisSetValue;
use super::value::RedisValue;
use super::value::RedisValueStatus;
use super::{KeyType, RedisValueData};
use crate::connection::RedisAsyncConn;
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::ServerEvent;
use crate::states::i18n_set_editor;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

async fn get_redis_set_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    keyword: Option<SharedString>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<String>)> {
    let pattern = if let Some(keyword) = keyword {
        format!("*{}*", keyword)
    } else {
        "*".to_string()
    };
    let (cursor, value): (u64, Vec<Vec<u8>>) = cmd("SSCAN")
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
    let value = value.iter().map(|v| String::from_utf8_lossy(v).to_string()).collect();
    Ok((cursor, value))
}

pub(crate) async fn first_load_set_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("SCARD").arg(key).query_async(conn).await?;
    let (cursor, values) = get_redis_set_value(conn, key, None, 0, 100).await?;
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
        expire_at: None,
        ..Default::default()
    })
}

impl ZedisServerState {
    pub fn add_set_value(&mut self, new_value: SharedString, cx: &mut Context<Self>) {
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
        let server_id = self.server_id.clone();
        let current_key = key.clone();
        self.spawn(
            ServerTask::AddSetValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;

                let count: usize = cmd("SADD")
                    .arg(key.as_str())
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(count)
            },
            move |this, result, cx| {
                let title = i18n_set_editor(cx, "add_value_success");
                let msg = i18n_set_editor(cx, "add_value_success_tips");
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                    if let Ok(count) = result
                        && let Some(RedisValueData::Set(set_data)) = value.data.as_mut()
                    {
                        let set = Arc::make_mut(set_data);
                        set.size += count;
                        cx.emit(ServerEvent::ValueAdded(current_key));

                        cx.dispatch_action(&NotificationAction::new_success(msg).with_title(title));
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn filter_set_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let Some(set) = value.set_value() else {
            return;
        };
        let new_set = RedisSetValue {
            keyword: Some(keyword.clone()),
            size: set.size,
            ..Default::default()
        };
        value.data = Some(RedisValueData::Set(Arc::new(new_set)));
        self.load_more_set_value(cx);
    }
    pub fn load_more_set_value(&mut self, cx: &mut Context<Self>) {
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

        // Check if we have valid set data
        let (cursor, keyword) = match value.set_value() {
            Some(set) => (set.cursor, set.keyword.clone()),
            None => return,
        };

        let server_id = self.server_id.clone();
        let current_key = key.clone();
        cx.emit(ServerEvent::ValuePaginationStarted(current_key.clone()));
        self.spawn(
            ServerTask::LoadMoreValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                // Fetch only the new items
                let count = if keyword.is_some() { 1000 } else { 100 };
                let result = get_redis_set_value(&mut conn, &key, keyword, cursor, count).await?;
                Ok(result)
            },
            move |this, result, cx| {
                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Set(set_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let set = Arc::make_mut(set_data);
                    set.cursor = new_cursor;
                    if new_cursor == 0 {
                        set.done = true;
                    }

                    if !new_values.is_empty() {
                        // Append new items to the existing list
                        set.values.extend(new_values.into_iter().map(|v| v.into()));
                    }
                }
                cx.emit(ServerEvent::ValuePaginationFinished(current_key));
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }
}

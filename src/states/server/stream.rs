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
    KeyType, RedisValueData, ServerEvent, ServerTask, ZedisServerState,
    value::{RedisStreamEntry, RedisStreamValue, RedisValue, RedisValueStatus},
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
};
use gpui::{SharedString, prelude::*};
use redis::cmd;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

type RawStreamData = Vec<(String, Vec<String>)>;

async fn get_redis_stream_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: Option<String>,
    count: usize,
) -> Result<(String, Vec<RedisStreamEntry>)> {
    let cursor = if let Some(cursor) = cursor {
        format!("({cursor}")
    } else {
        "-".to_string()
    };
    let entries: RawStreamData = cmd("XRANGE")
        .arg(key)
        .arg(cursor)
        .arg("+")
        .arg("COUNT")
        .arg(count)
        .query_async(conn)
        .await?;

    let done = entries.len() < count;

    let values: Vec<RedisStreamEntry> = entries
        .into_iter()
        .map(|(id, flat_fields)| {
            let mut field_values = Vec::with_capacity(flat_fields.len() / 2);
            let mut iter = flat_fields.into_iter();

            while let Some(key) = iter.next() {
                if let Some(val) = iter.next() {
                    field_values.push((key.into(), val.into()));
                }
            }

            (id.into(), field_values)
        })
        .collect();
    let mut cursor = values.last().map(|(id, _)| id.to_string()).unwrap_or_default();
    if done {
        cursor = "".to_string();
    }

    Ok((cursor, values))
}

pub(crate) async fn first_load_stream_value(conn: &mut RedisAsyncConn, key: &str) -> Result<RedisValue> {
    let size: usize = cmd("XLEN").arg(key).query_async(conn).await?;
    let (cursor, values) = get_redis_stream_value(conn, key, None, 100).await?;
    let done = cursor.is_empty();

    Ok(RedisValue {
        key_type: KeyType::Stream,
        data: Some(RedisValueData::Stream(Arc::new(RedisStreamValue {
            keyword: None,
            cursor,
            size,
            done,
            values,
        }))),
        ..Default::default()
    })
}

impl ZedisServerState {
    fn exec_stream_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisStreamValue),
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
        if let Some(RedisValueData::Stream(stream_data)) = value.data.as_mut() {
            optimistic_update(Arc::make_mut(stream_data));
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
    /// Applies a keyword filter to stream entries (client-side filtering).
    pub fn filter_stream_value(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        let Some((_, value)) = self.try_get_mut_key_value() else {
            return;
        };
        let Some(stream_value) = value.stream_value() else {
            return;
        };
        let new_stream_value = RedisStreamValue {
            keyword: Some(keyword.clone()),
            cursor: stream_value.cursor.clone(),
            size: stream_value.size,
            done: stream_value.done,
            values: stream_value.values.clone(),
        };
        value.data = Some(RedisValueData::Stream(Arc::new(new_stream_value)));
        cx.emit(ServerEvent::ValueUpdated);
    }

    pub fn load_more_stream_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        // Update UI to show loading state
        value.status = RedisValueStatus::Loading;
        cx.notify();

        let cursor = match value.stream_value() {
            Some(stream) => stream.cursor.clone(),
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
                get_redis_stream_value(&mut conn, key.as_str(), Some(cursor), 100).await
            },
            // UI callback: merge results into local state
            move |this, result, cx| {
                let mut should_load_more = false;
                if let Ok((new_cursor, new_values)) = result
                    && let Some(RedisValueData::Stream(stream_data)) = this.value.as_mut().and_then(|v| v.data.as_mut())
                {
                    let stream = Arc::make_mut(stream_data);
                    // Mark as done when cursor returns to 0 (scan complete)
                    if new_cursor.is_empty() {
                        stream.done = true;
                    }

                    stream.cursor = new_cursor;

                    // Append new field-value pairs to existing list
                    if !new_values.is_empty() {
                        stream.values.extend(new_values);
                    }
                    if !stream.done && stream.values.len() < 50 {
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

    pub fn add_stream_value(
        &mut self,
        entry_id: Option<SharedString>,
        values: Vec<(SharedString, SharedString)>,
        cx: &mut Context<Self>,
    ) {
        let values_clone = values.clone();
        let id = entry_id.unwrap_or("*".into());

        self.exec_stream_op(
            ServerTask::AddStreamEntry,
            cx,
            |_| {},
            move |key, mut conn| async move {
                let mut currend_cmd = cmd("XADD");
                let mut current_cmd = currend_cmd.arg(&key).arg(id.as_str());
                for (field, value) in values {
                    current_cmd = current_cmd.arg(field.as_str()).arg(value.as_str());
                }
                let id: String = current_cmd.query_async(&mut conn).await?;
                Ok(id)
            },
            |this, id, cx| {
                if let Some(RedisValueData::Stream(stream_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let stream = Arc::make_mut(stream_data);
                    stream.size += 1;
                    if stream.done {
                        stream.values.push((id.into(), values_clone));
                    }
                }
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
    pub fn remove_stream_value(&mut self, entry_id: SharedString, cx: &mut Context<Self>) {
        let entry_id_clone = entry_id.clone();
        self.exec_stream_op(
            ServerTask::RemoveStreamEntry,
            cx,
            move |stream| {
                stream.values.retain(|(id, _)| id != &entry_id);
            },
            move |key, mut conn| async move {
                let _: () = cmd("XDEL")
                    .arg(&key)
                    .arg(entry_id_clone.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |this, _, cx| {
                if let Some(RedisValueData::Stream(stream_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let stream = Arc::make_mut(stream_data);
                    stream.size -= 1;
                }
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
}

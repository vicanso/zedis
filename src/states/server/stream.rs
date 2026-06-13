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
    value::{
        RedisStreamEntry, RedisStreamValue, RedisValue, RedisValueStatus, StreamConsumerDetail, StreamGroupDetail,
        StreamInfoData, StreamPendingEntry, StreamSummary,
    },
};
use crate::{
    connection::{RedisAsyncConn, get_connection_manager},
    error::Error,
};
use gpui::{SharedString, prelude::*};
use redis::aio::MultiplexedConnection;
use redis::cmd;
use std::collections::HashMap;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

type RawStreamData = Vec<(String, Vec<String>)>;

// ── XINFO / XPENDING parsing helpers ─────────────────────────────────────────

/// Converts a flat alternating-key/value Redis array into a map.
fn xinfo_flat_to_map(arr: &[redis::Value]) -> HashMap<String, &redis::Value> {
    let mut map = HashMap::with_capacity(arr.len() / 2);
    let mut i = 0;
    while i + 1 < arr.len() {
        let key = match &arr[i] {
            redis::Value::BulkString(b) => String::from_utf8_lossy(b).to_string(),
            redis::Value::SimpleString(s) => s.clone(),
            _ => {
                i += 2;
                continue;
            }
        };
        map.insert(key, &arr[i + 1]);
        i += 2;
    }
    map
}

fn redis_to_string(v: &redis::Value) -> SharedString {
    match v {
        redis::Value::BulkString(b) => String::from_utf8_lossy(b).to_string().into(),
        redis::Value::SimpleString(s) => s.clone().into(),
        redis::Value::Int(n) => n.to_string().into(),
        _ => SharedString::default(),
    }
}

fn redis_to_i64(v: &redis::Value) -> i64 {
    match v {
        redis::Value::Int(n) => *n,
        redis::Value::BulkString(b) => String::from_utf8_lossy(b).parse().unwrap_or(0),
        _ => 0,
    }
}

fn redis_to_usize(v: &redis::Value) -> usize {
    redis_to_i64(v).max(0) as usize
}

fn map_get_string(map: &HashMap<String, &redis::Value>, key: &str) -> SharedString {
    map.get(key).map(|v| redis_to_string(v)).unwrap_or_default()
}

fn map_get_usize(map: &HashMap<String, &redis::Value>, key: &str) -> usize {
    map.get(key).map(|v| redis_to_usize(v)).unwrap_or(0)
}

fn map_get_i64(map: &HashMap<String, &redis::Value>, key: &str) -> i64 {
    map.get(key).map(|v| redis_to_i64(v)).unwrap_or(0)
}

// ── Async fetch ──────────────────────────────────────────────────────────────

/// Extracts the entry ID from the first element of an XINFO first/last-entry array.
fn extract_entry_id(v: &redis::Value) -> SharedString {
    match v {
        redis::Value::Array(arr) if !arr.is_empty() => redis_to_string(&arr[0]),
        _ => SharedString::default(),
    }
}

/// Fetches XINFO STREAM, XINFO GROUPS, XINFO CONSUMERS, and XPENDING for every group.
async fn load_stream_info_data(conn: &mut RedisAsyncConn, key: &str) -> Result<StreamInfoData> {
    // ── XINFO STREAM ──────────────────────────────────────────────────────────
    let stream_raw: redis::Value = cmd("XINFO")
        .arg("STREAM")
        .arg(key)
        .query_async(conn)
        .await
        .unwrap_or(redis::Value::Array(vec![]));

    let summary = if let redis::Value::Array(arr) = stream_raw {
        let map = xinfo_flat_to_map(&arr);
        Some(StreamSummary {
            groups_count: map_get_usize(&map, "groups"),
            first_entry_id: map.get("first-entry").map(|v| extract_entry_id(v)).unwrap_or_default(),
            last_entry_id: map.get("last-entry").map(|v| extract_entry_id(v)).unwrap_or_default(),
            radix_tree_keys: map_get_usize(&map, "radix-tree-keys"),
            radix_tree_nodes: map_get_usize(&map, "radix-tree-nodes"),
        })
    } else {
        None
    };

    // ── XINFO GROUPS ─────────────────────────────────────────────────────────
    let groups_raw: redis::Value = cmd("XINFO").arg("GROUPS").arg(key).query_async(conn).await?;

    let mut groups = Vec::new();

    let group_entries = match &groups_raw {
        redis::Value::Array(v) => v.clone(),
        _ => vec![],
    };

    for group_entry in group_entries {
        let fields = match group_entry {
            redis::Value::Array(v) => v,
            _ => continue,
        };
        let map = xinfo_flat_to_map(&fields);
        let name = map_get_string(&map, "name");
        let consumers_count = map_get_usize(&map, "consumers");
        let pending_count = map_get_usize(&map, "pending");
        let last_delivered_id = map_get_string(&map, "last-delivered-id");
        let lag = map_get_i64(&map, "lag");

        // XINFO CONSUMERS key group
        let consumers = {
            let raw: redis::Value = cmd("XINFO")
                .arg("CONSUMERS")
                .arg(key)
                .arg(name.as_ref())
                .query_async(conn)
                .await
                .unwrap_or(redis::Value::Array(vec![]));
            let mut list = Vec::new();
            if let redis::Value::Array(entries) = raw {
                for entry in entries {
                    if let redis::Value::Array(f) = entry {
                        let m = xinfo_flat_to_map(&f);
                        list.push(StreamConsumerDetail {
                            name: map_get_string(&m, "name"),
                            pending: map_get_usize(&m, "pending"),
                            idle_ms: map_get_i64(&m, "idle"),
                        });
                    }
                }
            }
            list
        };

        // XPENDING key group - + 100
        let pending_entries = {
            let raw: redis::Value = cmd("XPENDING")
                .arg(key)
                .arg(name.as_ref())
                .arg("-")
                .arg("+")
                .arg(100usize)
                .query_async(conn)
                .await
                .unwrap_or(redis::Value::Array(vec![]));
            let mut list = Vec::new();
            if let redis::Value::Array(entries) = raw {
                for entry in entries {
                    if let redis::Value::Array(f) = entry
                        && f.len() >= 4
                    {
                        list.push(StreamPendingEntry {
                            id: redis_to_string(&f[0]),
                            consumer: redis_to_string(&f[1]),
                            idle_ms: redis_to_i64(&f[2]),
                            delivery_count: redis_to_i64(&f[3]),
                        });
                    }
                }
            }
            list
        };

        groups.push(StreamGroupDetail {
            name,
            consumers_count,
            pending_count,
            last_delivered_id,
            lag,
            consumers,
            pending_entries,
        });
    }

    Ok(StreamInfoData { summary, groups })
}

/// Fetches a page of stream entries using XRANGE (ascending) or XREVRANGE (descending).
///
/// `cursor` is the exclusive lower/upper bound ID from the previous page; `None`
/// starts from the beginning of the requested direction.  Returns the next cursor
/// (empty string when the end of the stream has been reached) and the loaded entries.
async fn get_redis_stream_value(
    conn: &mut RedisAsyncConn,
    key: &str,
    cursor: Option<String>,
    count: usize,
    reverse: bool,
) -> Result<(String, Vec<RedisStreamEntry>)> {
    // XRANGE  key start end   COUNT n  (oldest → newest, cursor = last seen high ID)
    // XREVRANGE key end start COUNT n  (newest → oldest, cursor = last seen low ID)
    let entries: RawStreamData = if reverse {
        let end = cursor.map_or_else(|| "+".to_string(), |c| format!("({c}"));
        cmd("XREVRANGE")
            .arg(key)
            .arg(&end)
            .arg("-")
            .arg("COUNT")
            .arg(count)
            .query_async(conn)
            .await?
    } else {
        let start = cursor.map_or_else(|| "-".to_string(), |c| format!("({c}"));
        cmd("XRANGE")
            .arg(key)
            .arg(&start)
            .arg("+")
            .arg("COUNT")
            .arg(count)
            .query_async(conn)
            .await?
    };

    let done = entries.len() < count;

    let values: Vec<RedisStreamEntry> = entries
        .into_iter()
        .map(|(id, flat_fields)| {
            let mut field_values = Vec::with_capacity(flat_fields.len() / 2);
            let mut iter = flat_fields.into_iter();
            while let Some(field) = iter.next() {
                if let Some(val) = iter.next() {
                    field_values.push((field.into(), val.into()));
                }
            }
            (id.into(), field_values)
        })
        .collect();

    let cursor = if done {
        String::new()
    } else {
        values.last().map(|(id, _)| id.to_string()).unwrap_or_default()
    };

    Ok((cursor, values))
}

pub(crate) async fn first_load_stream_value(conn: &mut RedisAsyncConn, key: &str, reverse: bool) -> Result<RedisValue> {
    let size: usize = cmd("XLEN").arg(key).query_async(conn).await?;
    let (cursor, values) = get_redis_stream_value(conn, key, None, 100, reverse).await?;
    let done = cursor.is_empty();

    Ok(RedisValue {
        key_type: KeyType::Stream,
        data: Some(RedisValueData::Stream(Arc::new(RedisStreamValue {
            keyword: None,
            cursor,
            size,
            done,
            values,
            reverse,
            info: None,
        }))),
        ..Default::default()
    })
}

/// One `XREAD COUNT n BLOCK ms STREAMS key last_id` round on a
/// dedicated connection. Returns `(new_last_id, entries)`. On block
/// timeout the server replies nil → `(last_id unchanged, [])`.
///
/// Parsed by hand from `redis::Value` because the `redis` crate's
/// `streams` feature is not enabled (keeps the dep surface lean).
/// Reply shape: `[ [ stream_name, [ [id, [f, v, f, v, …]], … ] ], … ]`.
pub(crate) async fn tail_read(
    conn: &mut MultiplexedConnection,
    key: &str,
    last_id: &str,
    block_ms: u64,
    count: usize,
) -> Result<(String, Vec<RedisStreamEntry>)> {
    let reply: redis::Value = cmd("XREAD")
        .arg("COUNT")
        .arg(count)
        .arg("BLOCK")
        .arg(block_ms)
        .arg("STREAMS")
        .arg(key)
        .arg(last_id)
        .query_async(conn)
        .await?;

    let mut new_last = last_id.to_string();
    let mut out: Vec<RedisStreamEntry> = Vec::new();

    let redis::Value::Array(streams) = reply else {
        // Nil (block timeout) or unexpected — nothing new.
        return Ok((new_last, out));
    };
    for stream in streams {
        let redis::Value::Array(name_and_entries) = stream else {
            continue;
        };
        let Some(redis::Value::Array(entries)) = name_and_entries.get(1) else {
            continue;
        };
        for entry in entries {
            let redis::Value::Array(id_and_fields) = entry else {
                continue;
            };
            let Some(id_val) = id_and_fields.first() else {
                continue;
            };
            let id = redis_to_string(id_val);
            let mut fields: Vec<(SharedString, SharedString)> = Vec::new();
            if let Some(redis::Value::Array(flat)) = id_and_fields.get(1) {
                let mut it = flat.iter();
                while let (Some(f), Some(v)) = (it.next(), it.next()) {
                    fields.push((redis_to_string(f), redis_to_string(v)));
                }
            }
            new_last = id.to_string();
            out.push((id, fields));
        }
    }
    Ok((new_last, out))
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
    /// Fetches XINFO GROUPS / XINFO CONSUMERS / XPENDING for the current key and
    /// stores the result in `RedisStreamValue::info`.  Emits `ValueUpdated` on
    /// completion so the stream editor can re-render.
    pub fn fetch_stream_info(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        let server_id = self.server_id.clone();
        let db = self.db;
        let guard_key = key.clone();

        self.spawn_with_arg(
            ServerTask::FetchStreamInfo,
            key.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                load_stream_info_data(&mut conn, key.as_str()).await
            },
            move |this, result, cx| {
                // Drop a result that arrived after the user switched keys — it
                // would otherwise be written into the newly selected key.
                if this.key.as_ref() != Some(&guard_key) {
                    return;
                }
                match result {
                    Ok(info) => {
                        if let Some(RedisValueData::Stream(stream_data)) =
                            this.value.as_mut().and_then(|v| v.data.as_mut())
                        {
                            Arc::make_mut(stream_data).info = Some(Arc::new(info));
                        }
                        cx.emit(ServerEvent::ValueUpdated);
                        cx.notify();
                    }
                    Err(e) => this.emit_error_notification(e.to_string().into(), cx),
                }
            },
            cx,
        );
    }

    /// Clears the current stream data and reloads with the given sort order.
    ///
    /// Unlike `get_value`, this skips the TYPE/TTL round-trip and calls
    /// `first_load_stream_value` directly, since the key type is already known.
    pub fn reload_stream_value(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else { return };
        let server_id = self.server_id.clone();
        let db = self.db;
        let guard_key = key.clone();

        if let Some(value) = self.value.as_mut() {
            value.status = RedisValueStatus::Loading;
        }
        cx.notify();

        self.spawn_with_arg(
            ServerTask::ReloadValue,
            key.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                first_load_stream_value(&mut conn, key.as_str(), reverse).await
            },
            move |this, result, cx| {
                // Drop a result that arrived after the user switched keys — it
                // would otherwise overwrite the newly selected key's value.
                if this.key.as_ref() != Some(&guard_key) {
                    return;
                }
                match result {
                    Ok(new_value) => {
                        if let Some(value) = this.value.as_mut() {
                            value.data = new_value.data;
                            value.status = RedisValueStatus::Idle;
                        }
                        cx.emit(ServerEvent::ValueLoaded);
                        cx.notify();
                    }
                    Err(e) => this.emit_error_notification(e.to_string().into(), cx),
                }
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
            reverse: stream_value.reverse,
            info: stream_value.info.clone(),
        };
        value.data = Some(RedisValueData::Stream(Arc::new(new_stream_value)));
        cx.emit(ServerEvent::ValueUpdated);
    }

    pub fn load_more_stream_value(&mut self, cx: &mut Context<Self>) {
        let Some((key, value)) = self.try_get_mut_key_value() else {
            return;
        };

        let (cursor, reverse) = match value.stream_value() {
            Some(stream) => (stream.cursor.clone(), stream.reverse),
            None => return,
        };

        // Update UI to show loading state
        value.status = RedisValueStatus::Loading;
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;
        let guard_key = key.clone();
        cx.emit(ServerEvent::ValuePaginationStarted);

        self.spawn_with_arg(
            ServerTask::LoadMoreValue,
            key.clone(),
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                get_redis_stream_value(&mut conn, key.as_str(), Some(cursor), 100, reverse).await
            },
            // UI callback: merge results into local state
            move |this, result, cx| {
                // Drop results for a key the user already navigated away from —
                // appending them would corrupt the newly selected stream.
                if this.key.as_ref() != Some(&guard_key) {
                    return;
                }
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
                    this.load_more_stream_value(cx);
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

    /// XGROUP CREATE key group id. `start_id` is `$` (only new
    /// entries), `0` (from the beginning), or an explicit entry ID.
    /// The stream already exists (we're editing it) so MKSTREAM is
    /// unnecessary. Refreshes XINFO on success so the groups table
    /// reflects the new group.
    pub fn create_stream_group(&mut self, group: SharedString, start_id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::CreateStreamGroup,
            cx,
            |_| {},
            move |key, mut conn| async move {
                let _: () = cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&key)
                    .arg(group.as_str())
                    .arg(start_id.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// XGROUP SETID key group id — reposition the group's
    /// last-delivered-id (e.g. `$` to skip backlog, `0` to replay).
    pub fn set_stream_group_id(&mut self, group: SharedString, id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::SetStreamGroupId,
            cx,
            |_| {},
            move |key, mut conn| async move {
                let _: () = cmd("XGROUP")
                    .arg("SETID")
                    .arg(&key)
                    .arg(group.as_str())
                    .arg(id.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// Append entries received from a live-tail `XREAD` into the
    /// current stream value, ring-trimmed to `cap` so a hot stream
    /// can't grow memory unbounded. Guarded by `key` — if the user
    /// switched keys while the tail loop was in flight, the stale
    /// batch is dropped instead of polluting the new key's view.
    pub fn append_tail_entries(
        &mut self,
        key: &str,
        entries: Vec<RedisStreamEntry>,
        cap: usize,
        cx: &mut Context<Self>,
    ) {
        if entries.is_empty() {
            return;
        }
        if self.key.as_ref().map(|k| k.as_str()) != Some(key) {
            return;
        }
        if let Some(RedisValueData::Stream(stream_data)) = self.value.as_mut().and_then(|v| v.data.as_mut()) {
            let stream = Arc::make_mut(stream_data);
            let added = entries.len();
            if stream.reverse {
                // Newest-first display: newer entries go to the front,
                // preserving received order among the batch.
                for entry in entries.into_iter().rev() {
                    stream.values.insert(0, entry);
                }
                stream.values.truncate(cap);
            } else {
                stream.values.extend(entries);
                if stream.values.len() > cap {
                    let overflow = stream.values.len() - cap;
                    stream.values.drain(0..overflow);
                }
            }
            stream.size += added;
            cx.emit(ServerEvent::ValueUpdated);
            cx.notify();
        }
    }

    /// XGROUP DESTROY key group — drops the group and its entire
    /// pending list. Destructive; the caller is expected to confirm.
    pub fn destroy_stream_group(&mut self, group: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::DestroyStreamGroup,
            cx,
            |_| {},
            move |key, mut conn| async move {
                let _: () = cmd("XGROUP")
                    .arg("DESTROY")
                    .arg(&key)
                    .arg(group.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }
}

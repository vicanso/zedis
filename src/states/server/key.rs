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
    ServerEvent, ServerTask, ZedisServerState,
    hash::first_load_hash_value,
    list::first_load_list_value,
    set::first_load_set_value,
    string::get_redis_bytes_value,
    value::{KeyType, RedisValue, RedisValueData, RedisValueStatus, SortOrder},
    zset::first_load_zset_value,
};
use crate::db::add_normalize_history;
use crate::states::{QueryMode, ZedisGlobalStore};
use crate::{
    connection::get_connection_manager,
    error::Error,
    helpers::{parse_duration, unix_ts},
};
use futures::future::try_join_all;
use futures::{StreamExt, stream};
use gpui::{SharedString, prelude::*};
use redis::{cmd, pipe};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;
use uuid::Uuid;

const DEFAULT_SCAN_RESULT_MAX: usize = 1_000;

impl ZedisServerState {
    /// Fills the type of keys that are currently loaded but have an unknown type.
    ///
    /// This is typically used when expanding a directory in the key tree view.
    /// It filters keys based on the prefix and ensures we only query keys at the current level.
    fn fill_key_types(&mut self, prefix: Option<SharedString>, cx: &mut Context<Self>) {
        // Filter keys that need type resolution
        let binding = prefix.unwrap_or_default();
        let prefix = binding.as_str();
        let count = self.keys.len();
        let binding = cx.global::<ZedisGlobalStore>().value(cx);
        let separator = binding.key_separator();
        let mut keys = self
            .keys
            .iter()
            .filter_map(|(key, value)| {
                if *value != KeyType::Unknown {
                    return None;
                }
                if prefix.is_empty() {
                    // if no prefix, only fill keys that are not in a subdirectory
                    // or if the count is less than 1000
                    if count < 1000 || !key.contains(separator) {
                        return Some(key.clone());
                    }
                    return None;
                };
                let suffix = key.strip_prefix(prefix)?;
                // Skip if the key is in a deeper subdirectory (contains delimiter)
                if suffix.contains(separator) {
                    return None;
                }
                Some(key.clone())
            })
            .take(2000)
            .collect::<Vec<SharedString>>();
        debug!(prefix, size = keys.len(), "fill key types");
        if keys.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        keys.sort_unstable();
        // Spawn a background task to fetch types concurrently
        self.spawn(
            ServerTask::FillKeyTypes,
            move || async move {
                let conn = get_connection_manager().get_connection(&server_id, db).await?;
                // Use a stream to execute commands concurrently with backpressure
                let types: Vec<(SharedString, String)> = stream::iter(keys.iter().cloned())
                    .map(|key| {
                        let mut conn_clone = conn.clone();
                        let key = key.clone();
                        async move {
                            let t: String = cmd("TYPE")
                                .arg(key.as_str())
                                .query_async(&mut conn_clone)
                                .await
                                .unwrap_or_default();
                            (key, t)
                        }
                    })
                    .buffer_unordered(100) // Limit concurrency to 100
                    .collect::<Vec<_>>()
                    .await;
                Ok(types)
            },
            move |this, result, cx| {
                if let Ok(types) = result {
                    // Update local state with fetched types
                    for (key, value) in types {
                        if let Some(k) = this.keys.get_mut(&key) {
                            *k = KeyType::from(value.as_str());
                        }
                    }
                    // Trigger UI update by changing the tree ID
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.notify();
            },
            cx,
        );
    }
    /// Internal function to scan keys from Redis.
    ///
    /// It handles pagination via cursors and recursive calls to fetch more data
    /// if the result set is too small.
    pub(crate) fn scan_keys(&mut self, server_id: SharedString, keyword: SharedString, cx: &mut Context<Self>) {
        // Guard clause: ignore if the context has changed (e.g., switched server)
        if self.server_id != server_id || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        // Calculate max limit based on scan times to prevent infinite scrolling from loading too much
        let max = (self.scan_times + 1) * DEFAULT_SCAN_RESULT_MAX;

        let processing_server = server_id.clone();
        let processing_keyword = keyword.clone();
        let key_scan_count = cx.global::<ZedisGlobalStore>().read(cx).key_scan_count() as u64;
        let db = self.db;
        self.spawn(
            ServerTask::ScanKeys,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let pattern = if keyword.is_empty() {
                    "*".to_string()
                } else {
                    format!("*{}*", keyword)
                };
                // Adjust count based on keyword specificity
                let count = if keyword.is_empty() {
                    (key_scan_count / 5).max(1000)
                } else {
                    key_scan_count
                };
                if let Some(cursors) = cursors {
                    client.scan(cursors, &pattern, count).await
                } else {
                    client.first_scan(&pattern, count).await
                }
            },
            move |this, result, cx| {
                match result {
                    Ok((cursors, keys)) => {
                        debug!("cursors: {cursors:?}, keys count: {}", keys.len());
                        // Check if scan is complete (all cursors returned to 0)
                        if cursors.iter().sum::<u64>() == 0 {
                            this.scan_completed = true;
                            cx.emit(ServerEvent::KeyScanFinished);
                            this.cursors = None;
                        } else {
                            this.cursors = Some(cursors);
                        }
                        this.extend_keys(keys);
                    }
                    Err(_) => {
                        this.cursors = None;
                    }
                };
                if this.cursors.is_some() {
                    cx.emit(ServerEvent::KeyScanPaged);
                }
                // Automatically load more if we haven't reached the limit and scan isn't done
                if this.cursors.is_some() && this.keys.len() < max {
                    // run again
                    this.scan_keys(processing_server, processing_keyword, cx);
                    return cx.notify();
                }
                this.scanning = false;
                cx.notify();
                if this.keys.len() == 1
                    && let Some(key) = this.keys.keys().next()
                {
                    this.select_key(key.clone(), cx);
                } else {
                    this.fill_key_types(None, cx);
                }
            },
            cx,
        );
    }
    pub fn handle_filter(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan();
        if !keyword.is_empty() {
            add_normalize_history(&mut self.search_history, keyword.clone());
        }
        match self.query_mode {
            QueryMode::Prefix => self.scan_prefix(keyword, cx),
            QueryMode::Exact => self.select_key(keyword, cx),
            _ => self.scan(keyword, cx),
        }
    }
    /// Collapse all keys
    pub fn collapse_all_keys(&mut self, cx: &mut Context<Self>) {
        cx.emit(ServerEvent::KeyCollapseAll);
    }
    /// Initiates a new scan for keys matching the keyword.
    pub fn scan(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan();
        self.scanning = true;
        self.keyword = keyword.clone();
        cx.emit(ServerEvent::KeyScanStarted);
        cx.notify();
        self.scan_keys(self.server_id.clone(), keyword, cx);
    }
    /// Loads the next batch of keys (pagination).
    pub fn scan_next(&mut self, cx: &mut Context<Self>) {
        if self.scan_completed {
            return;
        }
        self.scan_times += 1;
        self.scan_keys(self.server_id.clone(), self.keyword.clone(), cx);
        cx.notify();
    }
    /// Scans keys matching a specific prefix.
    ///
    /// Optimized for populating directory-like structures in the key view.
    pub fn scan_prefix(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Avoid reloading if already loaded
        let mut key_type_full_loaded = false;
        let mut key_full_loaded = false;
        for key in self.loaded_prefixes.iter() {
            if prefix.as_str() == key.as_str() {
                key_type_full_loaded = true;
                break;
            }
            if prefix.as_str().starts_with(key.as_str()) {
                key_full_loaded = true;
            }
        }
        if key_type_full_loaded {
            return;
        }
        if key_full_loaded {
            self.loaded_prefixes.insert(prefix.clone());
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        // If global scan is complete, we might just need to resolve types
        if self.scan_completed {
            self.fill_key_types(Some(prefix), cx);
            return;
        }
        cx.emit(ServerEvent::KeyScanStarted);

        let server_id = self.server_id.clone();
        let db = self.db;
        let pattern = format!("{}*", prefix);
        let key_scan_count = cx.global::<ZedisGlobalStore>().read(cx).key_scan_count() as u64;
        self.spawn(
            ServerTask::ScanPrefix,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut cursors: Option<Vec<u64>> = None;
                let mut result_keys = vec![];
                let mut done = false;
                // Attempt to fetch keys in a loop (up to 20 iterations)
                // to gather a sufficient amount without blocking for too long.
                for _ in 0..20 {
                    let (new_cursor, keys) = if let Some(cursors) = cursors.clone() {
                        client.scan(cursors, &pattern, key_scan_count).await?
                    } else {
                        client.first_scan(&pattern, key_scan_count).await?
                    };
                    result_keys.extend(keys);
                    // Break if scan cycle finishes
                    if new_cursor.iter().sum::<u64>() == 0 {
                        done = true;
                        break;
                    }
                    cursors = Some(new_cursor);
                }

                Ok((result_keys, done))
            },
            move |this, result, cx| {
                if let Ok((keys, done)) = result {
                    debug!(
                        prefix = prefix.as_str(),
                        count = keys.len(),
                        done,
                        "scan prefix success"
                    );
                    if done {
                        this.loaded_prefixes.insert(prefix.clone());
                    }
                    this.extend_keys(keys);
                }
                cx.notify();
                // Resolve types for the keys under this prefix
                if this.keys.len() == 1
                    && let Some(key) = this.keys.keys().next()
                {
                    this.select_key(key.clone(), cx);
                } else {
                    this.fill_key_types(Some(prefix.clone()), cx);
                }
                cx.emit(ServerEvent::KeyScanFinished);
            },
            cx,
        );
    }

    /// Selects a key and fetches its details (Type, TTL, Value).
    pub fn select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.key = Some(key.clone());
        if key.is_empty() {
            return;
        }
        self.terminal = false;
        // only set loading status if the value exists for better performance
        // prevent editor flickering
        if let Some(value) = self.value.as_mut() {
            value.status = RedisValueStatus::Loading;
        } else {
            self.value = Some(RedisValue {
                status: RedisValueStatus::Loading,
                ..Default::default()
            });
        }
        cx.emit(ServerEvent::KeySelected);
        cx.notify();

        let server_id = self.server_id.clone();
        let db = self.db;
        let current_key = key.clone();
        let max_truncate_length = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();

        self.spawn(
            ServerTask::Selectkey,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let (t, ttl): (String, i64) = pipe()
                    .cmd("TYPE")
                    .arg(key.as_str())
                    .cmd("TTL")
                    .arg(key.as_str())
                    .query_async(&mut conn)
                    .await?;
                // the key does not exist
                if ttl == -2 {
                    return Ok(RedisValue {
                        expire_at: Some(-2),
                        ..Default::default()
                    });
                }
                // Calculate absolute expiration timestamp
                let expire_at = match ttl {
                    -1 => Some(-1), // Persistent
                    t if t >= 0 => Some(unix_ts() + t),
                    _ => None,
                };

                let key_type = KeyType::from(t.as_str());
                let mut redis_value = match key_type {
                    KeyType::String => {
                        let mut data = get_redis_bytes_value(&mut conn, &key).await?;
                        data.detect_and_update(server_id.as_str(), key.as_str(), max_truncate_length);
                        Ok(RedisValue {
                            key_type: KeyType::String,
                            data: Some(RedisValueData::Bytes(Arc::new(data))),
                            ..Default::default()
                        })
                    }
                    KeyType::List => first_load_list_value(&mut conn, &key).await,
                    KeyType::Set => first_load_set_value(&mut conn, &key).await,
                    KeyType::Zset => first_load_zset_value(&mut conn, &key, SortOrder::Asc).await,
                    KeyType::Hash => first_load_hash_value(&mut conn, &key).await,
                    _ => Err(Error::Invalid {
                        message: "unsupported key type".to_string(),
                    }),
                }?;
                if let Ok(memory_usage) = cmd("MEMORY")
                    .arg("USAGE")
                    .arg(key.as_str())
                    .query_async::<u64>(&mut conn)
                    .await
                {
                    redis_value.size = memory_usage;
                }

                redis_value.expire_at = expire_at;

                Ok(redis_value)
            },
            move |this, result, cx| {
                // if the key is not the same as the selected key, return
                if this.key != Some(current_key.clone()) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if !value.is_expired()
                            && let Some(key) = this.key.as_ref()
                        {
                            let mut should_refresh_key_tree = false;
                            if let Some(k) = this.keys.get_mut(key) {
                                if *k != value.key_type {
                                    should_refresh_key_tree = true;
                                    *k = value.key_type();
                                }
                            } else {
                                should_refresh_key_tree = true;
                                this.keys.insert(key.clone(), value.key_type());
                            }
                            if should_refresh_key_tree {
                                this.key_tree_id = Uuid::now_v7().to_string().into();
                            }
                        }
                        this.value = Some(value);
                    }
                    Err(_) => {
                        this.key = None;
                        this.value = None;
                    }
                };
                cx.emit(ServerEvent::ValueLoaded);
                cx.notify();
            },
            cx,
        );
    }
    pub fn delete_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let remove_key = key.clone();
        self.spawn(
            ServerTask::DeleteKey,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("DEL").arg(key.as_str()).query_async(&mut conn).await?;
                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.remove(&remove_key);
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                    // Deselect if the deleted key was selected
                    if this.key == Some(remove_key) {
                        this.key = None;
                        this.value = None;
                    }
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn delete_folder(&mut self, folder: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let separator = cx.global::<ZedisGlobalStore>().value(cx).key_separator().to_string();
        let prefix = format!("{folder}{separator}");
        let pattern = format!("{prefix}*");
        self.spawn(
            ServerTask::DeleteKeys,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let count = 10_000;
                let mut cursors: Option<Vec<u64>> = None;
                // Attempt to fetch keys in a loop (up to 20 iterations)
                // to gather a sufficient amount without blocking for too long.
                for _ in 0..20 {
                    let (new_cursor, keys) = if let Some(cursors) = cursors.clone() {
                        client.scan(cursors, &pattern, count).await?
                    } else {
                        client.first_scan(&pattern, count).await?
                    };
                    if !keys.is_empty() {
                        let mut pipe = redis::pipe();
                        for key in keys {
                            pipe.cmd("UNLINK").arg(key.as_str());
                        }
                        let mut conn = client.connection();
                        let _: () = pipe.query_async(&mut conn).await?;
                    }

                    // Break if scan cycle finishes
                    if new_cursor.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursor);
                }

                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.retain(|key, _| !key.starts_with(prefix.as_str()));
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.notify();
            },
            cx,
        );
    }
    pub fn unlink_key(&mut self, keys: Vec<SharedString>, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let remove_keys = keys.clone();
        self.spawn(
            ServerTask::DeleteKeys,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                if !client.is_cluster() {
                    let mut conn = client.connection();
                    let mut pipe = redis::pipe();
                    for key in keys {
                        pipe.cmd("UNLINK").arg(key.as_str());
                    }
                    let _: () = pipe.query_async(&mut conn).await?;
                    return Ok(());
                }

                // cluster mode
                let conn = client.connection();
                for chunk in keys.chunks(1000) {
                    let futures = chunk.iter().map(|key| {
                        let mut conn_clone = conn.clone();
                        async move {
                            let _: () = cmd("UNLINK").arg(key.as_str()).query_async(&mut conn_clone).await?;
                            Ok::<(), Error>(())
                        }
                    });
                    let _: Vec<()> = try_join_all(futures).await?;
                }
                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.retain(|key, _| !remove_keys.contains(key));
                    // Force refresh of the key tree view
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                }
                cx.notify();
            },
            cx,
        );
    }
    /// Deletes a specified key.
    pub fn delete_select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        cx.notify();
        self.delete_key(key, cx);
    }
    /// Updates the TTL (expiration) for a key.
    pub fn update_key_ttl(&mut self, key: SharedString, ttl: SharedString, cx: &mut Context<Self>) {
        if ttl.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let db = self.db;
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;

        let mut new_ttl = Duration::ZERO;
        let mut parse_fail_error = "".to_string();
        match parse_duration(&ttl) {
            Ok(ttl) => new_ttl = ttl,
            Err(err) => {
                parse_fail_error = err.to_string();
            }
        }

        if !new_ttl.is_zero() {
            value.expire_at = Some(unix_ts() + new_ttl.as_secs() as i64);
        }
        cx.notify();
        self.spawn(
            ServerTask::UpdateKeyTtl,
            move || async move {
                if !parse_fail_error.is_empty() {
                    return Err(Error::Invalid {
                        message: parse_fail_error,
                    });
                }
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let _: () = cmd("EXPIRE")
                    .arg(key.as_str())
                    .arg(new_ttl.as_secs())
                    .query_async(&mut conn)
                    .await?;
                Ok(ttl)
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    if result.is_err() {
                        value.expire_at = original_ttl;
                    }
                    value.status = RedisValueStatus::Idle;
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn add_key(&mut self, category: SharedString, key: SharedString, ttl: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let key_type = KeyType::from(category.to_lowercase().as_str());
        let key_clone = key.clone();
        self.spawn(
            ServerTask::AddKey,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id, db).await?;
                let exists: bool = cmd("EXISTS").arg(key.as_str()).query_async(&mut conn).await?;
                let ttl_duration = if ttl.is_empty() {
                    None
                } else if let Ok(secs) = ttl.parse::<u64>() {
                    Some(Duration::from_secs(secs))
                } else {
                    let ttl = humantime::parse_duration(&ttl).map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Some(ttl)
                };

                if exists {
                    return Err(Error::Invalid {
                        message: "Key already exists".to_string(),
                    });
                }
                match key_type {
                    KeyType::String => {
                        let _: () = cmd("SET").arg(key.as_str()).arg("").query_async(&mut conn).await?;
                    }
                    KeyType::List => {
                        let _: () = cmd("LPUSH")
                            .arg(key.as_str())
                            .arg("list item 1")
                            .query_async(&mut conn)
                            .await?;
                    }
                    KeyType::Set => {
                        let _: () = cmd("SADD")
                            .arg(key.as_str())
                            .arg("set item 1")
                            .query_async(&mut conn)
                            .await?;
                    }
                    KeyType::Zset => {
                        let _: () = cmd("ZADD")
                            .arg(key.as_str())
                            .arg(1.0)
                            .arg("zset item 1")
                            .query_async(&mut conn)
                            .await?;
                    }
                    KeyType::Hash => {
                        let _: () = cmd("HSET")
                            .arg(key.as_str())
                            .arg("field1")
                            .arg("value1")
                            .query_async(&mut conn)
                            .await?;
                    }
                    _ => {
                        return Err(Error::Invalid {
                            message: "Invalid key type".to_string(),
                        });
                    }
                };
                if let Some(ttl_duration) = ttl_duration {
                    let _: () = cmd("EXPIRE")
                        .arg(key.as_str())
                        .arg(ttl_duration.as_secs())
                        .query_async(&mut conn)
                        .await?;
                }

                Ok(())
            },
            move |this, result, cx| {
                if result.is_ok() {
                    this.keys.insert(key_clone.clone(), key_type);
                    this.key_tree_id = Uuid::now_v7().to_string().into();
                    this.select_key(key_clone, cx);
                }
                cx.notify();
            },
            cx,
        );
    }
}

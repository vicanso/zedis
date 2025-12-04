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

use super::ServerEvent;
use super::ServerTask;
use super::ZedisServerState;
use super::list::first_load_list_value;
use super::string::get_redis_value;
use super::value::{KeyType, RedisValue, RedisValueStatus};
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::helpers::unix_ts;
use crate::states::app::QueryMode;
use futures::{StreamExt, stream};
use gpui::SharedString;
use gpui::prelude::*;
use redis::{cmd, pipe};
use std::time::Duration;
use tracing::debug;
use uuid::Uuid;
const DEFAULT_SCAN_RESULT_MAX: usize = 1_000;

impl ZedisServerState {
    /// Fills the type of keys that are currently loaded but have an unknown type.
    ///
    /// This is typically used when expanding a directory in the key tree view.
    /// It filters keys based on the prefix and ensures we only query keys at the current level.
    fn fill_key_types(&mut self, prefix: SharedString, cx: &mut Context<Self>) {
        // Filter keys that need type resolution
        let mut keys = self
            .keys
            .iter()
            .filter_map(|(key, value)| {
                if *value != KeyType::Unknown {
                    return None;
                }
                let suffix = key.strip_prefix(prefix.as_str())?;
                // Skip if the key is in a deeper subdirectory (contains delimiter)
                if suffix.contains(":") {
                    return None;
                }
                Some(key.clone())
            })
            .collect::<Vec<SharedString>>();
        if keys.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        keys.sort_unstable();
        // Spawn a background task to fetch types concurrently
        self.spawn(
            ServerTask::FillKeyTypes,
            move || async move {
                let conn = get_connection_manager().get_connection(&server_id).await?;
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
    pub(crate) fn scan_keys(
        &mut self,
        server_id: SharedString,
        keyword: SharedString,
        cx: &mut Context<Self>,
    ) {
        // Guard clause: ignore if the context has changed (e.g., switched server)
        if self.server_id != server_id || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        // Calculate max limit based on scan times to prevent infinite scrolling from loading too much
        let max = (self.scan_times + 1) * DEFAULT_SCAN_RESULT_MAX;

        let processing_server = server_id.clone();
        let processing_keyword = keyword.clone();
        self.spawn(
            ServerTask::ScanKeys,
            move || async move {
                let client = get_connection_manager().get_client(&server_id).await?;
                let pattern = if keyword.is_empty() {
                    "*".to_string()
                } else {
                    format!("*{}*", keyword)
                };
                // Adjust count based on keyword specificity
                let count = if keyword.is_empty() { 2_000 } else { 10_000 };
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
                            cx.emit(ServerEvent::ScanFinish(processing_keyword.clone()));
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
                    cx.emit(ServerEvent::ScanNext(processing_keyword.clone()));
                }
                // Automatically load more if we haven't reached the limit and scan isn't done
                if this.cursors.is_some() && this.keys.len() < max {
                    // run again
                    this.scan_keys(processing_server, processing_keyword, cx);
                    return cx.notify();
                }
                this.scaning = false;
                cx.notify();
                this.fill_key_types("".into(), cx);
            },
            cx,
        );
    }
    pub fn handle_filter(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan();
        match self.query_mode {
            QueryMode::Prefix => self.scan_prefix(keyword, cx),
            QueryMode::Exact => self.select_key(keyword, cx),
            _ => self.scan(keyword, cx),
        }
    }
    /// Initiates a new scan for keys matching the keyword.
    pub fn scan(&mut self, keyword: SharedString, cx: &mut Context<Self>) {
        self.reset_scan();
        self.scaning = true;
        self.keyword = keyword.clone();
        cx.emit(ServerEvent::ScanStart(keyword.clone()));
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
        if self.loaded_prefixes.contains(&prefix) {
            return;
        }
        // If global scan is complete, we might just need to resolve types
        if self.scan_completed {
            self.fill_key_types(prefix, cx);
            return;
        }
        cx.emit(ServerEvent::ScanStart(prefix.clone()));

        let server_id = self.server_id.clone();
        let pattern = format!("{}*", prefix);
        self.spawn(
            ServerTask::ScanPrefix,
            move || async move {
                let client = get_connection_manager().get_client(&server_id).await?;
                let count = 10_000;
                // let mut cursors: Option<Vec<u64>>,
                let mut cursors: Option<Vec<u64>> = None;
                let mut result_keys = vec![];
                // Attempt to fetch keys in a loop (up to 20 iterations)
                // to gather a sufficient amount without blocking for too long.
                for _ in 0..20 {
                    let (new_cursor, keys) = if let Some(cursors) = cursors.clone() {
                        client.scan(cursors, &pattern, count).await?
                    } else {
                        client.first_scan(&pattern, count).await?
                    };
                    result_keys.extend(keys);
                    // Break if scan cycle finishes
                    if new_cursor.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursor);
                }

                Ok(result_keys)
            },
            move |this, result, cx| {
                if let Ok(keys) = result {
                    debug!(
                        prefix = prefix.as_str(),
                        count = keys.len(),
                        "scan prefix success"
                    );
                    this.loaded_prefixes.insert(prefix.clone());
                    this.extend_keys(keys);
                }
                cx.notify();
                // Resolve types for the keys under this prefix
                this.fill_key_types(prefix.clone(), cx);
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
        cx.emit(ServerEvent::Selectkey(key.clone()));
        cx.notify();

        let server_id = self.server_id.clone();
        let current_key = key.clone();

        self.spawn(
            ServerTask::Selectkey,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
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
                    KeyType::String => get_redis_value(&mut conn, &key).await,
                    KeyType::List => first_load_list_value(&mut conn, &key).await,
                    _ => Err(Error::Invalid {
                        message: "unsupported key type".to_string(),
                    }),
                }?;
                redis_value.expire_at = expire_at;

                Ok(redis_value)
            },
            move |this, result, cx| {
                // if the key is not the same as the selected key, return
                if this.key != Some(current_key) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if !value.is_expired()
                            && let Some(key) = this.key.as_ref()
                            && !this.keys.contains_key(key)
                        {
                            this.keys.insert(key.clone(), value.key_type());
                            this.key_tree_id = Uuid::now_v7().to_string().into();
                        }
                        this.value = Some(value);
                    }
                    Err(_) => {
                        this.value = None;
                    }
                };
                cx.notify();
            },
            cx,
        );
    }
    /// Deletes a specified key.
    pub fn delete_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        cx.notify();
        let remove_key = key.clone();
        self.spawn(
            ServerTask::DeleteKey,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
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
    /// Updates the TTL (expiration) for a key.
    pub fn update_key_ttl(&mut self, key: SharedString, ttl: SharedString, cx: &mut Context<Self>) {
        if ttl.is_empty() {
            return;
        }
        let server_id = self.server_id.clone();
        let Some(value) = self.value.as_mut() else {
            return;
        };
        value.status = RedisValueStatus::Updating;
        let original_ttl = value.expire_at;

        let mut new_ttl = Duration::ZERO;
        let mut parse_fail_error = "".to_string();
        if let Ok(secs) = ttl.parse::<u64>() {
            new_ttl = Duration::from_secs(secs);
        } else {
            match humantime::parse_duration(&ttl) {
                Ok(ttl) => new_ttl = ttl,
                Err(err) => {
                    parse_fail_error = err.to_string();
                }
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
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
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
}

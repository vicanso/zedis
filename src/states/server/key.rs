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

use super::ZedisServerState;
use super::list::first_load_list_value;
use super::string::get_redis_value;
use super::value::{KeyType, RedisValue};
use crate::connection::get_connection_manager;
use crate::error::Error;
use chrono::Local;
use futures::{StreamExt, stream};
use gpui::prelude::*;
use redis::{cmd, pipe};
use tracing::debug;
use uuid::Uuid;

const DEFAULT_SCAN_RESULT_MAX: usize = 1_000;

fn unix_ts() -> i64 {
    Local::now().timestamp()
}

impl ZedisServerState {
    fn fill_key_types(&mut self, cx: &mut Context<Self>, prefix: String) {
        let mut keys = self
            .keys
            .iter()
            .filter_map(|(key, value)| {
                if *value != KeyType::Unknown {
                    return None;
                }
                let suffix = key.strip_prefix(&prefix)?;
                if suffix.contains(":") {
                    return None;
                }
                Some(key.clone())
            })
            .collect::<Vec<String>>();
        if keys.is_empty() {
            return;
        }
        let server = self.server.clone();
        keys.sort_unstable();
        self.spawn(
            cx,
            "fill_key_types",
            move || async move {
                let conn = get_connection_manager().get_connection(&server).await?;
                // run task stream
                let types: Vec<(String, String)> = stream::iter(keys.iter().cloned())
                    .map(|key| {
                        let mut conn_clone = conn.clone();
                        let key = key.clone();
                        async move {
                            let t: String = cmd("TYPE")
                                .arg(&key)
                                .query_async(&mut conn_clone)
                                .await
                                .unwrap_or_default();
                            (key, t.to_string())
                        }
                    })
                    .buffer_unordered(100)
                    .collect::<Vec<_>>()
                    .await;
                Ok(types)
            },
            move |this, result, cx| {
                if let Ok(types) = result {
                    for (key, value) in types.iter() {
                        if let Some(k) = this.keys.get_mut(key) {
                            *k = KeyType::from(value.as_str());
                        }
                    }
                    this.key_tree_id = Uuid::now_v7().to_string();
                }
                cx.notify();
            },
        );
    }
    pub(crate) fn scan_keys(&mut self, cx: &mut Context<Self>, server: String, keyword: String) {
        if self.server != server || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        let max = (self.scan_times + 1) * DEFAULT_SCAN_RESULT_MAX;

        let processing_server = server.clone();
        let processing_keyword = keyword.clone();
        self.spawn(
            cx,
            "scan_keys",
            move || async move {
                let client = get_connection_manager().get_client(&server).await?;
                let pattern = format!("*{}*", keyword);
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
                        if cursors.iter().sum::<u64>() == 0 {
                            this.scan_completed = true;
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
                if this.cursors.is_some() && this.keys.len() < max {
                    // run again
                    this.scan_keys(cx, processing_server, processing_keyword);
                    return cx.notify();
                }
                this.scaning = false;
                cx.notify();
                this.fill_key_types(cx, "".to_string());
            },
        );
    }
    pub fn scan(&mut self, cx: &mut Context<Self>, keyword: String) {
        self.reset_scan();
        self.scaning = true;
        self.keyword = keyword.clone();
        cx.notify();
        self.scan_keys(cx, self.server.clone(), keyword);
    }
    pub fn scan_next(&mut self, cx: &mut Context<Self>) {
        if self.scan_completed {
            return;
        }
        self.scan_times += 1;
        self.scan_keys(cx, self.server.clone(), self.keyword.clone());
        cx.notify();
    }
    pub fn scan_prefix(&mut self, cx: &mut Context<Self>, prefix: String) {
        if self.loaded_prefixes.contains(&prefix) {
            return;
        }
        if self.scan_completed {
            self.fill_key_types(cx, prefix);
            return;
        }

        let server = self.server.clone();
        self.last_operated_at = unix_ts();
        let pattern = format!("{}*", prefix);
        self.spawn(
            cx,
            "scan_prefix",
            move || async move {
                let client = get_connection_manager().get_client(&server).await?;
                let count = 10_000;
                // let mut cursors: Option<Vec<u64>>,
                let mut cursors: Option<Vec<u64>> = None;
                let mut result_keys = vec![];
                // 最多执行x次
                for _ in 0..20 {
                    let (new_cursor, keys) = if let Some(cursors) = cursors.clone() {
                        client.scan(cursors, &pattern, count).await?
                    } else {
                        client.first_scan(&pattern, count).await?
                    };
                    result_keys.extend(keys);
                    if new_cursor.iter().sum::<u64>() == 0 {
                        break;
                    }
                    cursors = Some(new_cursor);
                }

                Ok(result_keys)
            },
            move |this, result, cx| {
                if let Ok(keys) = result {
                    debug!(prefix, count = keys.len(), "scan prefix success");
                    this.loaded_prefixes.insert(prefix.clone());
                    this.extend_keys(keys);
                }
                cx.notify();
                this.fill_key_types(cx, prefix);
            },
        );
    }

    pub fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        if self.key.clone().unwrap_or_default() != key {
            self.key = Some(key.clone());
            cx.notify();
            if key.is_empty() {
                return;
            }
            let server = self.server.clone();
            self.last_operated_at = unix_ts();

            self.spawn(
                cx,
                "select_key",
                move || async move {
                    let mut conn = get_connection_manager().get_connection(&server).await?;
                    let (t, ttl): (String, i64) = pipe()
                        .cmd("TYPE")
                        .arg(&key)
                        .cmd("TTL")
                        .arg(&key)
                        .query_async(&mut conn)
                        .await?;
                    // the key does not exist
                    if ttl == -2 {
                        return Ok(RedisValue {
                            expire_at: Some(-2),
                            ..Default::default()
                        });
                    }
                    let expire_at = if ttl == -1 {
                        Some(-1)
                    } else if ttl >= 0 {
                        Some(unix_ts() + ttl)
                    } else {
                        None
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
                    match result {
                        Ok(value) => {
                            this.value = Some(value);
                        }
                        Err(_) => {
                            this.value = None;
                        }
                    };
                    cx.notify();
                },
            );
        }
    }
    pub fn delete_key(&mut self, key: String, cx: &mut Context<Self>) {
        let server = self.server.clone();
        self.deleting = true;
        cx.notify();
        self.last_operated_at = unix_ts();
        let remove_key = key.clone();
        self.spawn(
            cx,
            "delete_key",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;
                let _: () = cmd("DEL").arg(&key).query_async(&mut conn).await?;
                Ok(())
            },
            move |this, result, cx| {
                if let Ok(()) = result {
                    this.keys.remove(&remove_key);
                    this.key_tree_id = Uuid::now_v7().to_string();
                    this.key = None;
                }
                this.deleting = false;
                cx.notify();
            },
        );
    }
}

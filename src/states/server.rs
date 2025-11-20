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

use crate::connection::RedisServer;
use crate::connection::get_connection_manager;
use crate::connection::{get_servers, save_servers};
use crate::error::Error;
use chrono::Local;
use gpui::prelude::*;
use pretty_hex::{HexConfig, config_hex};
use redis::cmd;
use redis::pipe;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;
use tracing::error;

type Result<T, E = Error> = std::result::Result<T, E>;

// string, list, set, zset, hash, stream, and vectorset.
#[derive(Debug, Clone, Default)]
enum KeyType {
    #[default]
    String,
    List,
    Set,
    Zset,
    Hash,
    Stream,
    Vectorset,
}

fn unix_ts() -> u64 {
    if let Ok(value) = SystemTime::now().duration_since(UNIX_EPOCH) {
        value.as_secs()
    } else {
        0
    }
}

#[derive(Debug, Clone, Default)]
pub struct RedisValue {
    key_type: KeyType,
    data: Option<String>,
    expire_at: Option<u64>,
    size: usize,
}

impl RedisValue {
    pub fn data(&self) -> Option<&String> {
        self.data.as_ref()
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn ttl(&self) -> Option<Duration> {
        self.expire_at
            .map(|expire_at| Duration::from_secs(expire_at - unix_ts()))
    }
}

impl From<String> for KeyType {
    fn from(value: String) -> Self {
        match value.as_str() {
            "list" => KeyType::List,
            "set" => KeyType::Set,
            "zset" => KeyType::Zset,
            "hash" => KeyType::Hash,
            "stream" => KeyType::Stream,
            "vectorset" => KeyType::Vectorset,
            _ => KeyType::String,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ZedisServerState {
    server: String,
    dbsize: Option<u64>,
    latency: Option<Duration>,
    servers: Option<Vec<RedisServer>>,
    key: Option<String>,
    value: Option<RedisValue>,
}

impl ZedisServerState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            ..Default::default()
        }
    }
    fn reset(&mut self) {
        self.server = "".to_string();
        self.dbsize = None;
        self.latency = None;
        self.key = None;
    }
    pub fn dbsize(&self) -> Option<u64> {
        self.dbsize
    }
    pub fn scan_count(&self) -> Option<u64> {
        // TODO
        Some(10)
    }
    pub fn latency(&self) -> Option<Duration> {
        self.latency
    }
    pub fn server(&self) -> &str {
        &self.server
    }
    pub fn servers(&self) -> Option<&[RedisServer]> {
        self.servers.as_deref()
    }
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
    pub fn value(&self) -> Option<&RedisValue> {
        self.value.as_ref()
    }
    pub fn remove_server(&mut self, server: &str, cx: &mut Context<Self>) {
        let mut servers = self.servers.clone().unwrap_or_default();
        servers.retain(|s| s.name != server);
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                save_servers(servers.clone())?;

                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            match result {
                Ok(servers) => handle.update(cx, |this, cx| {
                    this.servers = Some(servers);
                    cx.notify();
                }),
                Err(e) => {
                    // TODO
                    println!("error: {e:?}");
                    Ok(())
                }
            }
        })
        .detach();
    }
    pub fn update_or_insrt_server(&mut self, cx: &mut Context<Self>, mut server: RedisServer) {
        let mut servers = self.servers.clone().unwrap_or_default();
        server.updated_at = Some(Local::now().to_rfc3339());
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                if let Some(existing_server) = servers.iter_mut().find(|s| s.name == server.name) {
                    *existing_server = server;
                } else {
                    servers.push(server);
                }
                save_servers(servers.clone())?;

                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            match result {
                Ok(servers) => handle.update(cx, |this, cx| {
                    this.servers = Some(servers);
                    cx.notify();
                }),
                Err(e) => {
                    // TODO
                    println!("error: {e:?}");
                    Ok(())
                }
            }
        })
        .detach();
    }
    pub fn fetch_servers(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |handle, cx| {
            let task = cx.background_spawn(async move {
                let servers = get_servers()?;
                Ok(servers)
            });
            let result: Result<Vec<RedisServer>> = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok(servers) => {
                        this.servers = Some(servers);
                    }
                    Err(e) => {
                        println!("error: {e:?}");
                    }
                };
                cx.notify();
            })
        })
        .detach();
    }
    pub fn select(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.server != server {
            self.reset();
            self.server = server.to_string();
            debug!(server = self.server, "select server");
            cx.notify();
            if self.server.is_empty() {
                return;
            }
            let server_clone = server.to_string();
            cx.spawn(async move |handle, cx| {
                let counting_server = server_clone.clone();
                let task = cx.background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_clone)?;
                    let dbsize = client.dbsize()?;
                    let start = Instant::now();
                    client.ping()?;
                    Ok((dbsize, start.elapsed()))
                });
                let result: Result<(u64, Duration)> = task.await;
                handle.update(cx, move |this, cx| {
                    if this.server != counting_server {
                        return;
                    }
                    match result {
                        Ok((dbsize, latency)) => {
                            this.latency = Some(latency);
                            this.dbsize = Some(dbsize);
                        }
                        Err(e) => {
                            // TODO 出错的处理
                            error!(error = %e, "get redis info fail");
                            this.dbsize = None;
                            this.latency = None;
                        }
                    };
                    cx.notify();
                })
            })
            .detach();
        }
    }
    pub fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        if self.key.clone().unwrap_or_default() != key {
            self.key = Some(key.clone());
            cx.notify();
            if key.is_empty() {
                return;
            }
            let server = self.server.clone();
            cx.spawn(async move |handle, cx| {
                // TODO判断key的类型
                let task = cx.background_spawn(async move {
                    let client = get_connection_manager().get_client(&server)?;
                    let mut conn = client.get_connection()?;
                    let t: String = cmd("TYPE").arg(&key).query(&mut conn)?;
                    let mut redis_value = RedisValue {
                        key_type: KeyType::from(t),
                        ..Default::default()
                    };
                    let (value, ttl): (Vec<u8>, i64) =
                        pipe().get(&key).ttl(&key).query(&mut conn)?;
                    if ttl >= 0 {
                        redis_value.expire_at = Some(unix_ts() + ttl as u64);
                    }
                    redis_value.size = value.len();
                    if value.is_empty() {
                        return Ok(redis_value);
                    }
                    if let Ok(value) = std::str::from_utf8(&value) {
                        if let Ok(value) = serde_json::from_str::<Value>(value)
                            && let Ok(pretty_value) = serde_json::to_string_pretty(&value)
                        {
                            redis_value.data = Some(pretty_value);
                        } else {
                            redis_value.data = Some(value.to_string());
                        }
                    } else {
                        // TODO 根据窗口宽度使用width:16/32
                        let cfg = HexConfig {
                            title: false,
                            width: 32,
                            group: 0,
                            ..HexConfig::default()
                        };

                        redis_value.data = Some(config_hex(&value, cfg));
                    }
                    Ok(redis_value)
                });
                let result: Result<RedisValue, Error> = task.await;
                handle.update(cx, move |this, cx| {
                    match result {
                        Ok(value) => {
                            this.value = Some(value);
                        }
                        Err(e) => {
                            // TODO 出错的处理
                            this.value = None;
                            error!(error = %e, "get redis info fail");
                        }
                    };
                    cx.notify();
                })
            })
            .detach();
        }
    }
}

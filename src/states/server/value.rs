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
use crate::connection::get_connection_manager;
use crate::helpers::unix_ts;
use chrono::Local;
use gpui::Hsla;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum RedisValueData {
    String(SharedString),
    Bytes(Vec<u8>),
    List(Arc<RedisListValue>),
}

#[derive(Debug, Clone, Default)]
pub struct RedisListValue {
    pub size: usize,
    pub values: Vec<SharedString>,
}

impl RedisValue {
    pub fn list_value(&self) -> Option<&Arc<RedisListValue>> {
        if let Some(RedisValueData::List(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }
}
// string, list, set, zset, hash, stream, and vectorset.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum KeyType {
    #[default]
    Unknown,
    String,
    List,
    Set,
    Zset,
    Hash,
    Stream,
    Vectorset,
}
impl KeyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyType::String => "STR",
            KeyType::List => "LIST",
            KeyType::Hash => "HASH",
            KeyType::Set => "SET",
            KeyType::Zset => "ZSET",
            KeyType::Stream => "STRM",
            KeyType::Vectorset => "VEC",
            KeyType::Unknown => "",
        }
    }

    pub fn color(&self) -> Hsla {
        match self {
            KeyType::String => gpui::hsla(0.6, 0.5, 0.5, 1.0), // 蓝色系
            KeyType::List => gpui::hsla(0.8, 0.5, 0.5, 1.0),   // 紫色系
            KeyType::Hash => gpui::hsla(0.1, 0.6, 0.5, 1.0),   // 橙色系
            KeyType::Set => gpui::hsla(0.5, 0.5, 0.5, 1.0),    // 青色系
            KeyType::Zset => gpui::hsla(0.0, 0.6, 0.55, 1.0),  // 红色系
            KeyType::Stream => gpui::hsla(0.3, 0.5, 0.4, 1.0), // 绿色系
            KeyType::Vectorset => gpui::hsla(0.9, 0.5, 0.5, 1.0), // 粉色系
            KeyType::Unknown => gpui::hsla(0.0, 0.0, 0.4, 1.0), // 灰色
        }
    }
}

#[derive(Clone, PartialEq, Default, Debug)]
pub enum RedisValueStatus {
    #[default]
    Idle,
    Loading,
    Updating,
}

#[derive(Debug, Clone, Default)]
pub struct RedisValue {
    pub(crate) status: RedisValueStatus,
    pub(crate) key_type: KeyType,
    pub(crate) data: Option<RedisValueData>,
    pub(crate) expire_at: Option<i64>,
    pub(crate) size: usize,
}

impl RedisValue {
    pub fn is_busy(&self) -> bool {
        self.status != RedisValueStatus::Idle
    }
    pub fn string_value(&self) -> Option<SharedString> {
        if let Some(RedisValueData::String(value)) = self.data.as_ref() {
            return Some(value.clone());
        }
        None
    }
    pub fn bytes_value(&self) -> Option<&[u8]> {
        if let Some(RedisValueData::Bytes(value)) = self.data.as_ref() {
            return Some(value);
        }
        None
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn ttl(&self) -> Option<chrono::Duration> {
        self.expire_at.map(|expire_at| {
            if expire_at < 0 {
                chrono::Duration::seconds(expire_at)
            } else {
                let now = Local::now().timestamp();
                let seconds = expire_at.saturating_sub(now);
                if seconds < 0 {
                    chrono::Duration::seconds(-2)
                } else {
                    chrono::Duration::seconds(seconds)
                }
            }
        })
    }
    pub fn key_type(&self) -> KeyType {
        self.key_type
    }
}

impl From<&str> for KeyType {
    fn from(value: &str) -> Self {
        match value {
            "list" => KeyType::List,
            "set" => KeyType::Set,
            "zset" => KeyType::Zset,
            "hash" => KeyType::Hash,
            "stream" => KeyType::Stream,
            "vectorset" => KeyType::Vectorset,
            "string" => KeyType::String,
            _ => KeyType::Unknown,
        }
    }
}

impl ZedisServerState {
    pub fn save_value(&mut self, key: String, value: String, cx: &mut Context<Self>) {
        let server = self.server.clone();
        self.updating = true;
        cx.notify();
        self.last_operated_at = unix_ts();
        self.spawn(
            cx,
            "save_value",
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server).await?;
                let _: () = cmd("SET")
                    .arg(&key)
                    .arg(&value)
                    .query_async(&mut conn)
                    .await?;
                Ok(value)
            },
            move |this, result, cx| {
                if let Ok(update_value) = result
                    && let Some(value) = this.value.as_mut()
                {
                    value.size = update_value.len();
                    value.data = Some(RedisValueData::String(update_value.into()));
                }
                this.updating = false;
                cx.notify();
            },
        );
    }
}

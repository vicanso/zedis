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
use crate::connection::get_connection_manager;
use bytes::Bytes;
use chrono::Local;
use gpui::Action;
use gpui::Hsla;
use gpui::SharedString;
use gpui::prelude::*;
use redis::cmd;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Default)]
pub enum NotificationCategory {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action, Default)]
pub struct NotificationAction {
    pub title: Option<SharedString>,
    pub category: NotificationCategory,
    pub message: SharedString,
}

impl NotificationAction {
    pub fn new_info(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Info,
            message,
            ..Default::default()
        }
    }
    pub fn new_success(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Success,
            message,
            ..Default::default()
        }
    }
    pub fn new_warning(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Warning,
            message,
            ..Default::default()
        }
    }
    pub fn new_error(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Error,
            message,
            ..Default::default()
        }
    }
    pub fn with_title(mut self, title: SharedString) -> Self {
        self.title = Some(title);
        self
    }
}

#[derive(Debug, Clone)]
pub enum RedisValueData {
    String(SharedString),
    Bytes(Bytes),
    List(Arc<RedisListValue>),
    Set(Arc<RedisSetValue>),
}

#[derive(Debug, Clone, Default)]
pub struct RedisSetValue {
    pub keyword: Option<SharedString>,
    pub cursor: u64,
    pub size: usize,
    pub values: Vec<SharedString>,
    pub done: bool,
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
    pub fn set_value(&self) -> Option<&Arc<RedisSetValue>> {
        if let Some(RedisValueData::Set(data)) = self.data.as_ref() {
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
            KeyType::String => gpui::hsla(0.6, 0.5, 0.5, 1.0),    // 蓝色系
            KeyType::List => gpui::hsla(0.8, 0.5, 0.5, 1.0),      // 紫色系
            KeyType::Hash => gpui::hsla(0.1, 0.6, 0.5, 1.0),      // 橙色系
            KeyType::Set => gpui::hsla(0.5, 0.5, 0.5, 1.0),       // 青色系
            KeyType::Zset => gpui::hsla(0.0, 0.6, 0.55, 1.0),     // 红色系
            KeyType::Stream => gpui::hsla(0.3, 0.5, 0.4, 1.0),    // 绿色系
            KeyType::Vectorset => gpui::hsla(0.9, 0.5, 0.5, 1.0), // 粉色系
            KeyType::Unknown => gpui::hsla(0.0, 0.0, 0.4, 1.0),   // 灰色
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
        !matches!(self.status, RedisValueStatus::Idle)
    }
    pub fn is_loading(&self) -> bool {
        matches!(self.status, RedisValueStatus::Loading)
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
        let expire_at = self.expire_at?;

        // Handle special Redis TTL codes
        if expire_at < 0 {
            return Some(chrono::Duration::seconds(expire_at));
        }

        // Calculate remaining time
        let now = Local::now().timestamp();
        let remaining = expire_at.saturating_sub(now);

        // If calculated remaining is 0 but it wasn't a special code, it means expired just now.
        // We can treat it as expired (-2) or just 0.
        // Keeping consistent with original logic: if < 0 (should imply logic error if saturating_sub used, but kept for safety)
        Some(chrono::Duration::seconds(remaining))
    }
    pub fn key_type(&self) -> KeyType {
        self.key_type
    }
    pub fn is_expired(&self) -> bool {
        self.expire_at.is_some_and(|expire_at| expire_at == -2)
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
    pub fn save_value(&mut self, key: SharedString, new_value: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let original_value = value.string_value().unwrap_or_default();
        value.status = RedisValueStatus::Updating;
        value.size = new_value.len();
        value.data = Some(RedisValueData::String(new_value.clone()));
        let current_key = key.clone();

        cx.notify();
        self.spawn(
            ServerTask::SaveValue,
            move || async move {
                let mut conn = get_connection_manager().get_connection(&server_id).await?;
                let _: () = cmd("SET")
                    .arg(key.as_str())
                    .arg(new_value.as_str())
                    .query_async(&mut conn)
                    .await?;
                Ok(new_value)
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                    // recover
                    if result.is_err() {
                        value.size = original_value.len();
                        value.data = Some(RedisValueData::String(original_value));
                    }
                    cx.emit(ServerEvent::ValueUpdated(current_key));
                }

                cx.notify();
            },
            cx,
        );
    }
}

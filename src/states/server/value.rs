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

use super::{ServerEvent, ServerTask, ZedisServerState};
use crate::connection::get_connection_manager;
use bytes::Bytes;
use chrono::Local;
use gpui::{Action, Hsla, SharedString, prelude::*};
use redis::cmd;
use schemars::JsonSchema;
use serde::Deserialize;
use std::io::Cursor;
use std::sync::Arc;

/// Notification category for user feedback
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Default)]
pub enum NotificationCategory {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// Notification action that can be triggered in the UI
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action, Default)]
pub struct NotificationAction {
    pub title: Option<SharedString>,
    pub category: NotificationCategory,
    pub message: SharedString,
}

impl NotificationAction {
    /// Creates a new info notification
    pub fn new_info(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Info,
            message,
            ..Default::default()
        }
    }

    /// Creates a new success notification
    pub fn new_success(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Success,
            message,
            ..Default::default()
        }
    }

    /// Creates a new warning notification
    pub fn new_warning(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Warning,
            message,
            ..Default::default()
        }
    }

    /// Creates a new error notification
    pub fn new_error(message: SharedString) -> Self {
        Self {
            category: NotificationCategory::Error,
            message,
            ..Default::default()
        }
    }

    /// Sets the title for the notification
    pub fn with_title(mut self, title: SharedString) -> Self {
        self.title = Some(title);
        self
    }
}

#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub enum DataFormat {
    #[default]
    Bytes,
    Json,
    Preview,
    Text,
    Svg,
    Jpeg,
    Png,
    Webp,
    Gif,
    Gzip,
    Zstd,
    Snappy,
    MessagePack,
}

impl DataFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            DataFormat::Bytes => "bytes",
            DataFormat::Json => "json",
            DataFormat::Preview => "preview",
            DataFormat::Text => "text",
            DataFormat::Svg => "svg",
            DataFormat::Jpeg => "jpeg",
            DataFormat::Png => "png",
            DataFormat::Webp => "webp",
            DataFormat::Gif => "gif",
            DataFormat::Gzip => "gzip",
            DataFormat::Snappy => "snappy",
            DataFormat::Zstd => "zstd",
            DataFormat::MessagePack => "messagepack",
        }
    }
}

fn is_valid_messagepack(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    let first_byte = bytes[0];

    let is_container =
        // FixMap (0x80 - 0x8F)
        (0x80..=0x8f).contains(&first_byte)||
        // FixArray (0x90 - 0x9F)
        (0x90..=0x9f).contains(&first_byte) ||
        // Array16 (0xdc), Array32 (0xdd)
        first_byte == 0xdc || first_byte == 0xdd ||
        // Map16 (0xde), Map32 (0xdf)
        first_byte == 0xde || first_byte == 0xdf;

    if !is_container {
        return false;
    }

    let mut deserializer = rmp_serde::decode::Deserializer::new(Cursor::new(bytes));
    match serde::de::IgnoredAny::deserialize(&mut deserializer) {
        Ok(_) => deserializer.get_ref().position() == bytes.len() as u64,
        Err(_) => false,
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    // only check 4kb
    let check_len = std::cmp::min(bytes.len(), 4096);
    let Ok(header_str) = std::str::from_utf8(&bytes[0..check_len]) else {
        return false;
    };

    let trimmed = header_str.trim();

    // starts with <svg
    // starts with <?xml
    // starts with <!DOCTYPE

    let has_xml_header = trimmed.starts_with("<?xml");
    let has_doctype = trimmed.starts_with("<!DOCTYPE");
    let starts_with_svg_tag = trimmed.starts_with("<svg");

    if starts_with_svg_tag {
        return true;
    }

    if (has_xml_header || has_doctype) && trimmed.contains("<svg") {
        return true;
    }

    false
}

fn is_snappy_framed(bytes: &[u8]) -> bool {
    if bytes.len() < 10 {
        return false;
    }
    bytes.starts_with(&[0xFF, 0x06, 0x00, 0x00, 0x73, 0x4E, 0x61, 0x50, 0x70, 0x59])
}

pub fn detect_format(bytes: &[u8]) -> (DataFormat, Option<SharedString>) {
    if bytes.is_empty() {
        return (DataFormat::Bytes, None);
    }
    let Some(kind) = infer::get(bytes) else {
        return if is_snappy_framed(bytes) {
            (DataFormat::Snappy, Some("application/snappy".to_string().into()))
        } else if is_svg(bytes) {
            (DataFormat::Svg, Some("image/svg+xml".to_string().into()))
        } else if is_valid_messagepack(bytes) {
            (DataFormat::MessagePack, None)
        } else {
            (DataFormat::Bytes, None)
        };
    };
    let mime = kind.mime_type();
    let format = match mime {
        "application/gzip" => DataFormat::Gzip,
        "application/zstd" => DataFormat::Zstd,
        "image/jpeg" => DataFormat::Jpeg,
        "image/png" => DataFormat::Png,
        "image/webp" => DataFormat::Webp,
        "image/gif" => DataFormat::Gif,
        _ => DataFormat::Bytes,
    };
    (format, Some(mime.to_string().into()))
}

/// Redis value data variants for different data types
#[derive(Debug, Clone)]
pub enum RedisValueData {
    Bytes(Arc<RedisBytesValue>),
    List(Arc<RedisListValue>),
    Set(Arc<RedisSetValue>),
    Zset(Arc<RedisZsetValue>),
    Hash(Arc<RedisHashValue>),
}

/// Redis Set value structure with pagination support
#[derive(Debug, Clone, Default)]
pub struct RedisSetValue {
    pub keyword: Option<SharedString>,
    pub cursor: u64,
    pub size: usize,
    pub values: Vec<SharedString>,
    pub done: bool,
}

/// Sort order for sorted sets
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum SortOrder {
    #[default]
    Asc, // Ascending order (default)
    Desc, // Descending order
}

/// Redis Sorted Set value structure with pagination and sorting support
#[derive(Debug, Clone, Default)]
pub struct RedisZsetValue {
    pub keyword: Option<SharedString>,
    pub cursor: u64,
    pub size: usize,
    pub values: Vec<(SharedString, f64)>,
    pub done: bool,
    pub sort_order: SortOrder,
}

/// Redis Hash value structure with pagination support
#[derive(Debug, Clone, Default)]
pub struct RedisHashValue {
    pub cursor: u64,
    pub keyword: Option<SharedString>,
    pub size: usize,
    pub done: bool,
    pub values: Vec<(SharedString, SharedString)>,
}

/// Redis List value structure
#[derive(Debug, Clone, Default)]
pub struct RedisListValue {
    pub keyword: Option<SharedString>,
    pub size: usize,
    pub values: Vec<SharedString>,
}
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ViewMode {
    #[default]
    Auto,
    Plain,
    Hex,
}

impl ViewMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewMode::Auto => "Auto",
            ViewMode::Plain => "Plain",
            ViewMode::Hex => "Hex",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "Plain" => ViewMode::Plain,
            "Hex" => ViewMode::Hex,
            _ => ViewMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RedisBytesValue {
    pub format: DataFormat,
    pub bytes: Bytes,
    pub mime: Option<SharedString>,
    pub text: Option<SharedString>,
    pub view_mode: ViewMode,
}

impl RedisBytesValue {
    pub fn is_image(&self) -> bool {
        matches!(
            self.format,
            DataFormat::Jpeg | DataFormat::Png | DataFormat::Webp | DataFormat::Gif | DataFormat::Svg
        )
    }
    pub fn is_utf8_text(&self) -> bool {
        matches!(self.format, DataFormat::Text | DataFormat::Json)
    }
}

impl RedisValue {
    /// Returns the list value if the data is a List type
    pub fn list_value(&self) -> Option<&Arc<RedisListValue>> {
        if let Some(RedisValueData::List(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }

    /// Returns the set value if the data is a Set type
    pub fn set_value(&self) -> Option<&Arc<RedisSetValue>> {
        if let Some(RedisValueData::Set(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }

    /// Returns the sorted set value if the data is a Zset type
    pub fn zset_value(&self) -> Option<&Arc<RedisZsetValue>> {
        if let Some(RedisValueData::Zset(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }

    /// Returns the hash value if the data is a Hash type
    pub fn hash_value(&self) -> Option<&Arc<RedisHashValue>> {
        if let Some(RedisValueData::Hash(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }
}

/// Redis key types: string, list, set, zset, hash, stream, and vectorset
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
    /// Returns the abbreviated string representation of the key type
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

    /// Returns the color associated with this key type for UI display
    pub fn color(&self) -> Hsla {
        match self {
            KeyType::String => gpui::hsla(0.6, 0.5, 0.5, 1.0),    // Blue
            KeyType::List => gpui::hsla(0.8, 0.5, 0.5, 1.0),      // Purple
            KeyType::Hash => gpui::hsla(0.1, 0.6, 0.5, 1.0),      // Orange
            KeyType::Set => gpui::hsla(0.5, 0.5, 0.5, 1.0),       // Cyan
            KeyType::Zset => gpui::hsla(0.0, 0.6, 0.55, 1.0),     // Red
            KeyType::Stream => gpui::hsla(0.3, 0.5, 0.4, 1.0),    // Green
            KeyType::Vectorset => gpui::hsla(0.9, 0.5, 0.5, 1.0), // Pink
            KeyType::Unknown => gpui::hsla(0.0, 0.0, 0.4, 1.0),   // Gray
        }
    }
}

/// Status of a Redis value operation
#[derive(Clone, PartialEq, Default, Debug)]
pub enum RedisValueStatus {
    #[default]
    Idle,
    Loading,
    Updating,
}

/// Redis value with metadata including type, data, expiration, and status
#[derive(Debug, Clone, Default)]
pub struct RedisValue {
    pub(crate) status: RedisValueStatus,
    pub(crate) key_type: KeyType,
    pub(crate) data: Option<RedisValueData>,
    pub(crate) expire_at: Option<i64>,
    pub(crate) size: u64,
}

impl RedisValue {
    /// Checks if the value is currently being loaded or updated
    pub fn is_busy(&self) -> bool {
        !matches!(self.status, RedisValueStatus::Idle)
    }

    /// Checks if the value is currently loading
    pub fn is_loading(&self) -> bool {
        matches!(self.status, RedisValueStatus::Loading)
    }

    /// Returns the string value if the data is a String type
    pub fn bytes_string_value(&self) -> Option<SharedString> {
        if let Some(value) = self.bytes_value()
            && value.is_utf8_text()
        {
            return value.text.clone();
        }
        None
    }

    /// Returns the bytes value if the data is a Bytes type
    pub fn bytes_value(&self) -> Option<Arc<RedisBytesValue>> {
        if let Some(RedisValueData::Bytes(value)) = self.data.as_ref() {
            return Some(value.clone());
        }
        None
    }

    /// Returns the size of the value in bytes
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the time-to-live duration for this key
    ///
    /// Returns None if no expiration is set.
    /// Special Redis TTL codes:
    /// - -1: No expiration set
    /// - -2: Key does not exist or is expired
    pub fn ttl(&self) -> Option<chrono::Duration> {
        let expire_at = self.expire_at?;

        // Handle special Redis TTL codes
        if expire_at < 0 {
            return Some(chrono::Duration::seconds(expire_at));
        }

        // Calculate remaining time
        let now = Local::now().timestamp();
        let remaining = expire_at.saturating_sub(now);
        // if the remaining time is less than 0, return expired
        if remaining < 0 {
            return Some(chrono::Duration::seconds(-2));
        }

        Some(chrono::Duration::seconds(remaining))
    }

    /// Returns the key type
    pub fn key_type(&self) -> KeyType {
        self.key_type
    }

    /// Checks if the key is expired (TTL = -2)
    pub fn is_expired(&self) -> bool {
        self.expire_at.is_some_and(|expire_at| expire_at == -2)
    }
}

/// Converts a string representation to a KeyType
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
    /// Saves a new value for a Redis string key
    ///
    /// This method updates the UI immediately with the new value and then
    /// asynchronously persists it to Redis. If the save fails, the original
    /// value is restored.
    pub fn save_value(&mut self, key: SharedString, new_value: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;
        let Some(value) = self.value.as_mut() else {
            return;
        };

        let Some(original_bytes_value) = value.bytes_value() else {
            return;
        };
        let format = original_bytes_value.format;
        let original_size = value.size;

        value.status = RedisValueStatus::Updating;
        value.data = Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
            bytes: Bytes::from(new_value.clone().to_string().into_bytes()),
            text: Some(new_value.clone()),
            format,
            ..Default::default()
        })));
        let current_key = key.clone();
        let ttl = value.ttl().map(|ttl| ttl.num_milliseconds()).unwrap_or_default();

        cx.notify();
        self.spawn(
            ServerTask::SaveValue,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut conn = client.connection();
                let mut binding = cmd("SET");
                let mut new_cmd = binding.arg(key.as_str()).arg(new_value.as_str());
                // keep ttl if the version is at least 6.0.0
                new_cmd = if client.is_at_least_version("6.0.0") {
                    new_cmd.arg("KEEPTTL")
                } else if ttl > 0 {
                    new_cmd.arg("PX").arg(ttl)
                } else {
                    new_cmd
                };
                let _: () = new_cmd.query_async(&mut conn).await?;

                let mut size = None;
                if let Ok(memory_usage) = cmd("MEMORY")
                    .arg("USAGE")
                    .arg(key.as_str())
                    .query_async::<u64>(&mut conn)
                    .await
                {
                    size = Some(memory_usage);
                }

                Ok(size)
            },
            move |this, result, cx| {
                if let Some(value) = this.value.as_mut() {
                    value.status = RedisValueStatus::Idle;
                    if let Ok(result_size) = result {
                        if let Some(size) = result_size {
                            value.size = size;
                        }
                    } else {
                        // Recover original value if save failed
                        value.size = original_size;
                        value.data = Some(RedisValueData::Bytes(original_bytes_value.clone()));
                    }
                    cx.emit(ServerEvent::ValueUpdated(current_key));
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn update_bytes_value_view_mode(&mut self, view_mode: SharedString, cx: &mut Context<Self>) {
        let Some(value) = self.value.as_mut() else {
            return;
        };
        let view_mode = ViewMode::from_str(view_mode.as_str());
        let key = self.key.clone().unwrap_or_default();
        // Directly modify the data in place
        if let Some(RedisValueData::Bytes(bytes_value)) = &mut value.data {
            let bytes_value = Arc::make_mut(bytes_value);
            bytes_value.view_mode = view_mode;
            cx.emit(ServerEvent::ValueModeViewUpdated(key));
            cx.notify();
        }
    }
}

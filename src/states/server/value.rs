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
use gpui::{Hsla, SharedString, prelude::*};
use redis::cmd;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

pub(crate) const SUCCESS_NOTIFY_THRESHOLD: usize = 10;

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
    Protobuf,
    MessagePack,
    Script,
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
            DataFormat::Protobuf => "protobuf",
            DataFormat::MessagePack => "messagepack",
            DataFormat::Script => "script",
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
#[derive(Debug, Clone, PartialEq)]
pub enum RedisValueData {
    Bytes(Arc<RedisBytesValue>),
    List(Arc<RedisListValue>),
    Set(Arc<RedisSetValue>),
    Zset(Arc<RedisZsetValue>),
    Hash(Arc<RedisHashValue>),
    Stream(Arc<RedisStreamValue>),
}

/// Redis Set value structure with pagination support
#[derive(Debug, Clone, Default, PartialEq)]
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
}

/// Redis Sorted Set value structure with pagination and sorting support
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedisZsetValue {
    pub keyword: Option<SharedString>,
    pub cursor: u64,
    pub size: usize,
    pub values: Vec<(SharedString, f64)>,
    pub done: bool,
    pub sort_order: SortOrder,
}

/// Redis Hash value structure with pagination support
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedisHashValue {
    pub cursor: u64,
    pub keyword: Option<SharedString>,
    pub size: usize,
    pub done: bool,
    pub values: Vec<(SharedString, SharedString)>,
    /// Per-field TTL in seconds (only populated on Redis 7.4+).
    /// A field absent from this map has no expiry.
    pub field_ttls: HashMap<SharedString, i64>,
}

/// Redis List value structure
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedisListValue {
    pub keyword: Option<SharedString>,
    pub size: usize,
    pub values: Vec<SharedString>,
}

/// Structure: (Message ID, Vec<(Field, Value)>)
pub type RedisStreamEntry = (SharedString, Vec<(SharedString, SharedString)>);
/// A single consumer within a stream group (from XINFO CONSUMERS).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamConsumerDetail {
    pub name: SharedString,
    /// Number of messages pending delivery to this consumer.
    pub pending: usize,
    /// Milliseconds since the consumer last interacted with the server.
    pub idle_ms: i64,
}

/// A pending-message entry (from XPENDING key group - + count).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamPendingEntry {
    pub id: SharedString,
    pub consumer: SharedString,
    /// Milliseconds elapsed since the message was delivered.
    pub idle_ms: i64,
    /// Number of times the message has been delivered.
    pub delivery_count: i64,
}

/// Full details for one consumer group (from XINFO GROUPS + XINFO CONSUMERS + XPENDING).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamGroupDetail {
    pub name: SharedString,
    pub consumers_count: usize,
    pub pending_count: usize,
    pub last_delivered_id: SharedString,
    /// Number of entries not yet delivered to any consumer (0 = no lag).
    pub lag: i64,
    pub consumers: Vec<StreamConsumerDetail>,
    pub pending_entries: Vec<StreamPendingEntry>,
}

/// Macro-level stream metrics from XINFO STREAM.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamSummary {
    /// Number of consumer groups subscribed to this stream.
    pub groups_count: usize,
    /// ID of the first (oldest) entry in the stream.
    pub first_entry_id: SharedString,
    /// ID of the last (newest) entry in the stream.
    pub last_entry_id: SharedString,
    /// Number of internal radix-tree keys (structural).
    pub radix_tree_keys: usize,
    /// Number of radix-tree nodes — proxy for memory footprint.
    pub radix_tree_nodes: usize,
}

/// Aggregated stream statistics fetched on demand (XINFO + XPENDING).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamInfoData {
    /// Macro-level stream metrics (XINFO STREAM).
    pub summary: Option<StreamSummary>,
    pub groups: Vec<StreamGroupDetail>,
}

/// Redis Stream value structure with pagination support
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedisStreamValue {
    /// Optional keyword filter for searching stream entries.
    pub keyword: Option<SharedString>,

    /// The ID of the last entry loaded, used as cursor for the next page.
    /// For ascending (XRANGE) this is the highest ID seen; for descending
    /// (XREVRANGE) this is the lowest ID seen.
    pub cursor: String,

    /// Total count of items in the stream (XLEN).
    pub size: usize,

    /// Whether we have reached the end of the stream (or loaded all requested).
    pub done: bool,

    /// The stream entries.
    pub values: Vec<RedisStreamEntry>,

    /// When `true` entries are loaded newest-first via XREVRANGE; otherwise
    /// oldest-first via XRANGE.
    pub reverse: bool,

    /// Group/consumer/pending statistics loaded on demand via `fetch_stream_info`.
    /// `None` until explicitly fetched.
    pub info: Option<Arc<StreamInfoData>>,
}

impl RedisStreamValue {
    pub fn fields(&self) -> Vec<SharedString> {
        let mut seen = HashSet::new();
        self.values
            .iter()
            .flat_map(|(_, fields)| fields.iter().map(|(field, _)| field.clone()))
            .filter(|field| seen.insert(field.clone()))
            .collect()
    }
    pub fn get_entry_id(&self, index: usize) -> Option<SharedString> {
        self.values.get(index).map(|(id, _)| id.clone())
    }
    pub fn get_field_value(&self, index: usize, field: &SharedString) -> Option<SharedString> {
        let (_, fields) = self.values.get(index)?;
        let values: Vec<SharedString> = fields
            .iter()
            .filter(|(f, _)| f == field)
            .map(|(_, value)| value.clone())
            .collect();

        if values.is_empty() {
            return None;
        }

        Some(values.join("; ").into())
    }
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

#[derive(Debug, Clone, Default, PartialEq)]
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

    /// Returns the stream value if the data is a Stream type
    pub fn stream_value(&self) -> Option<&Arc<RedisStreamValue>> {
        if let Some(RedisValueData::Stream(data)) = self.data.as_ref() {
            return Some(data);
        }
        None
    }
    pub fn stream_fields(&self) -> Vec<SharedString> {
        let mut fields = if let Some(stream_value) = self.stream_value() {
            stream_value.fields()
        } else {
            vec![]
        };
        fields.insert(0, "Entry Id".to_string().into());
        fields
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
    Channel,
    Json,
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
            KeyType::Channel => "CHANNEL",
            KeyType::Json => "JSON",
            KeyType::Unknown => "",
        }
    }

    /// Returns the Redis command used to create a key of this type.
    pub fn create_command(&self) -> &'static str {
        match self {
            KeyType::String => "SET",
            KeyType::List => "LPUSH",
            KeyType::Set => "SADD",
            KeyType::Zset => "ZADD",
            KeyType::Hash => "HSET",
            KeyType::Stream => "XADD",
            KeyType::Json => "JSON.SET",
            _ => "",
        }
    }

    /// Returns the minimal seed arguments required to create a key of this type.
    pub fn seed_args(&self) -> Vec<&'static str> {
        match self {
            KeyType::String => vec![""],
            KeyType::List => vec!["item"],
            KeyType::Set => vec!["member"],
            KeyType::Zset => vec!["1", "member"],
            KeyType::Hash => vec!["field", "value"],
            KeyType::Stream => vec!["*", "field", "value"],
            KeyType::Json => vec!["$", "{}"],
            _ => vec![],
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
            _ => gpui::hsla(0.0, 0.0, 0.4, 1.0),                  // Gray
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
#[derive(Debug, Clone, Default, PartialEq)]
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

    /// Checks if the value is a Redis JSON type
    pub fn is_redis_json(&self) -> bool {
        matches!(self.key_type, KeyType::Json)
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
            "ReJSON-RL" => KeyType::Json,
            "json" => KeyType::Json,
            _ => KeyType::Unknown,
        }
    }
}

/// Compute a RFC 7396 JSON Merge Patch from `old` to `new`.
///
/// - Changed/added fields appear with their new value.
/// - Deleted fields appear as `null`.
/// - Returns `None` when both values are identical (no patch needed).
///
/// Crate-visible so the value-diff view can render the same patch
/// document the Save path sends to Redis as `JSON.MERGE`, giving users
/// a one-to-one preview of what their change will do server-side.
pub(crate) fn json_merge_diff(old: &JsonValue, new: &JsonValue) -> Option<JsonValue> {
    if old == new {
        return None;
    }
    match (old, new) {
        (JsonValue::Object(old_map), JsonValue::Object(new_map)) => {
            let mut patch = serde_json::Map::new();
            // Detect changed and added keys
            for (k, new_v) in new_map {
                if let Some(old_v) = old_map.get(k) {
                    if let Some(sub_patch) = json_merge_diff(old_v, new_v) {
                        patch.insert(k.clone(), sub_patch);
                    }
                } else {
                    // New key
                    patch.insert(k.clone(), new_v.clone());
                }
            }
            // Detect deleted keys
            for k in old_map.keys() {
                if !new_map.contains_key(k) {
                    patch.insert(k.clone(), JsonValue::Null);
                }
            }
            if patch.is_empty() {
                None
            } else {
                Some(JsonValue::Object(patch))
            }
        }
        // For non-object types (arrays, primitives), return the new value directly
        _ => Some(new.clone()),
    }
}

impl ZedisServerState {
    /// Updates a new value for a Redis string key
    ///
    /// This method updates the UI immediately with the new value and then
    /// asynchronously persists it to Redis. If the save fails, the original
    /// value is restored.
    pub fn update_value(&mut self, key: SharedString, new_value: SharedString, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;

        // Inspection phase: pull everything we need out of `self.value`
        // before mutating any state, so we can also call
        // `push_value_history` (which needs &mut self) without
        // overlapping borrows.
        let (format, original_size, original_bytes_value, is_redis_json, ttl) = {
            let Some(value) = self.value.as_ref() else { return };
            let Some(bv) = value.bytes_value() else { return };
            let ttl = value.ttl().map(|t| t.num_milliseconds()).unwrap_or_default();
            (bv.format, value.size, bv, value.is_redis_json(), ttl)
        };

        // Snapshot the pre-overwrite bytes for the rollback history.
        // Captured optimistically — if the SET later fails we leave the
        // entry in place (it still reflects what the user saw on
        // screen), and `push_history` collapses identical retries.
        self.push_value_history(key.clone(), original_bytes_value.bytes.clone());

        // For JSON type, compute a merge patch (RFC 7396) between old and new values.
        // Three possible outcomes:
        // - Some(Some(patch)): fields changed, use JSON.MERGE with the diff
        // - Some(None): no changes, skip the write entirely
        // - None: parse failed or non-JSON, fall back to JSON.SET
        let json_merge_patch: Option<Option<String>> = if is_redis_json {
            original_bytes_value.text.as_deref().and_then(|old_text| {
                let old_json = serde_json::from_str::<JsonValue>(old_text).ok()?;
                let new_json = serde_json::from_str::<JsonValue>(new_value.as_ref()).ok()?;
                let patch = json_merge_diff(&old_json, &new_json);
                Some(patch.and_then(|p| serde_json::to_string(&p).ok()))
            })
        } else {
            None
        };

        let Some(value) = self.value.as_mut() else { return };
        value.status = RedisValueStatus::Updating;
        value.data = Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
            bytes: Bytes::from(new_value.clone().to_string().into_bytes()),
            text: Some(new_value.clone()),
            format,
            ..Default::default()
        })));

        cx.notify();
        self.spawn(
            ServerTask::SaveValue,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut conn = client.connection();
                if is_redis_json {
                    match json_merge_patch {
                        Some(Some(patch)) => {
                            // Partial update: only send changed fields
                            let _: () = cmd("JSON.MERGE")
                                .arg(key.as_str())
                                .arg("$")
                                .arg(patch.as_str())
                                .query_async(&mut conn)
                                .await?;
                        }
                        Some(None) => {
                            // No changes, skip write
                        }
                        None => {
                            // Parse failed or root type changed, full replace
                            let _: () = cmd("JSON.SET")
                                .arg(key.as_str())
                                .arg("$")
                                .arg(new_value.as_str())
                                .query_async(&mut conn)
                                .await?;
                        }
                    }
                } else {
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
                }

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
                    cx.emit(ServerEvent::ValueUpdated);
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Save arbitrary bytes back to a string key. Unlike `update_value`, this
    /// path doesn't try the JSON merge-patch optimization — it always
    /// performs a full `SET` with the raw byte payload. Used by the bytes
    /// editor's hex write mode where the value isn't guaranteed UTF-8.
    pub fn update_value_bytes(&mut self, key: SharedString, new_bytes: Vec<u8>, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let db = self.db;

        // See update_value for the borrow-split rationale.
        let (format, original_size, original_bytes_value, ttl) = {
            let Some(value) = self.value.as_ref() else { return };
            let Some(bv) = value.bytes_value() else { return };
            let ttl = value.ttl().map(|t| t.num_milliseconds()).unwrap_or_default();
            (bv.format, value.size, bv, ttl)
        };

        self.push_value_history(key.clone(), original_bytes_value.bytes.clone());

        let new_bytes_arc = Bytes::from(new_bytes.clone());

        let Some(value) = self.value.as_mut() else { return };
        value.status = RedisValueStatus::Updating;
        value.data = Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
            bytes: new_bytes_arc.clone(),
            // Best-effort UTF-8 decode for in-app preview; non-utf8 bytes
            // fall through and the renderer treats it as binary.
            text: std::str::from_utf8(&new_bytes_arc)
                .ok()
                .map(|s| SharedString::from(s.to_string())),
            format,
            ..Default::default()
        })));

        cx.notify();
        self.spawn(
            ServerTask::SaveValue,
            move || async move {
                let client = get_connection_manager().get_client(&server_id, db).await?;
                let mut conn = client.connection();
                let mut binding = cmd("SET");
                let mut new_cmd = binding.arg(key.as_str()).arg(new_bytes.as_slice());
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
                        value.size = original_size;
                        value.data = Some(RedisValueData::Bytes(original_bytes_value.clone()));
                    }
                    cx.emit(ServerEvent::ValueUpdated);
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
        // Directly modify the data in place
        if let Some(RedisValueData::Bytes(bytes_value)) = &mut value.data {
            let bytes_value = Arc::make_mut(bytes_value);
            bytes_value.view_mode = view_mode;
            cx.emit(ServerEvent::ValueModeViewUpdated);
            cx.notify();
        }
    }
}

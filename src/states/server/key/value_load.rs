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

//! Key selection, value loading (with size-gate/preview/module DUMP handling),
//! header refresh, Pub/Sub publish, and single-key operations on [`ZedisServerState`].

use super::super::{
    hash::first_load_hash_value,
    json::get_redis_json_value,
    list::first_load_list_value,
    set::first_load_set_value,
    stream::first_load_stream_value,
    string::{get_redis_bytes_range, get_redis_bytes_value},
    value::{
        MAX_INLINE_VALUE_SIZE, MAX_MODULE_DUMP_BYTES, ModuleTypeId, RedisBytesValue, RedisValueData, SortOrder,
        VALUE_PREVIEW_BYTES, ViewMode,
    },
    zset::first_load_zset_value,
};
use super::*;
use crate::connection::{
    HeatMetric, KeyOp, KeyOpOutcome, dump_key, floors, get_server_features, get_server_heat_probe, key_memory_usage,
    key_object_meta, key_type_and_ttl, publish, run_key_op, server_supports,
};
use bytes::Bytes;
use futures::future::join;
use rust_i18n::t;
use zedis_core::change_log::ChangeEntry;

impl ZedisServerState {
    pub(super) fn get_value(&mut self, key: SharedString, task: ServerTask, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        let at = self.at();
        let server_id = self.server_id.clone();
        let current_key = key.clone();
        let max_truncate_length = cx.global::<ZedisGlobalStore>().read(cx).max_truncate_length();
        let bypass_size_gate = self.open.size_gate_bypassed.as_ref() == Some(&key);
        let preview_offset = self
            .open
            .size_gate_preview
            .as_ref()
            .filter(|(k, _)| k == &key)
            .map(|(_, offset)| *offset);
        // Header decorations, both resolved from what the connect-time probe
        // already learned: whether `OBJECT ENCODING` answers here at all, and
        // which of FREQ / IDLETIME this server's eviction policy makes
        // meaningful. A server that has neither simply gets no chip.
        let with_encoding = self.facts.feature_matrix().is_usable(ServerCommand::ObjectEncoding);
        let heat_probe = get_server_heat_probe(&server_id);

        self.spawn(
            task,
            move || async move {
                let (t, ttl) = key_type_and_ttl(&at, key.as_str()).await?;
                if ttl == -2 {
                    return Ok(RedisValue {
                        expire_at: Some(-2),
                        ..Default::default()
                    });
                }
                let expire_at = match ttl {
                    -1 => Some(-1),
                    t if t >= 0 => Some(unix_ts() + t),
                    _ => None,
                };

                let key_type = KeyType::from(t.as_str());
                // Size gate: String / RedisJSON pull the whole payload in one
                // command, so probe the size first (STRLEN / MEMORY USAGE —
                // both cheap) and stop before fetching something that would
                // stall the UI and spike memory. Collections are exempt —
                // their first loads are already paginated. On a failed probe
                // the gate is skipped rather than blocking the load.
                if !bypass_size_gate
                    && matches!(key_type, KeyType::String | KeyType::Json)
                    && let Ok(size) = key_memory_usage(&at, key.as_str(), key_type.as_str()).await
                    && size > MAX_INLINE_VALUE_SIZE
                {
                    // A String can be read in part (`GETRANGE`), so its
                    // preview is a bounded chunk; RedisJSON has no such read.
                    if let Some(offset) = preview_offset
                        && key_type == KeyType::String
                    {
                        let (chunk, (encoding, heat)) = join(
                            get_redis_bytes_range(&at, &key, offset, VALUE_PREVIEW_BYTES),
                            key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                        )
                        .await;
                        let (chunk, total, actual_offset) = chunk?;
                        return Ok(RedisValue {
                            key_type,
                            data: Some(RedisValueData::Bytes(Arc::new(chunk))),
                            size,
                            expire_at,
                            encoding: encoding.into(),
                            heat,
                            preview_of: Some(total),
                            preview_offset: actual_offset,
                            ..Default::default()
                        });
                    }
                    return Ok(RedisValue {
                        key_type,
                        status: RedisValueStatus::TooLarge(size),
                        size,
                        expire_at,
                        ..Default::default()
                    });
                }
                // Boxed: this match inlines every type's first-load future
                // into one state machine, and each of those awaits a
                // connection of its own. Unboxed in a debug build the frame
                // outgrew a background thread's 512 KB stack and selecting a
                // key crashed with a stack-guard fault.
                let mut redis_value = Box::pin(async {
                    match key_type {
                        KeyType::String => {
                            let mut data = get_redis_bytes_value(&at, &key).await?;
                            data.detect_and_update(server_id.as_str(), key.as_str(), max_truncate_length);
                            Ok(RedisValue {
                                key_type: KeyType::String,
                                data: Some(RedisValueData::Bytes(Arc::new(data))),
                                ..Default::default()
                            })
                        }
                        KeyType::List => first_load_list_value(&at, &key).await,
                        KeyType::Set => first_load_set_value(&at, &key).await,
                        KeyType::Zset => first_load_zset_value(&at, &key, SortOrder::Asc).await,
                        KeyType::Hash => {
                            let field_ttl = server_supports(&at, floors::HASH_FIELD_TTL).await?;
                            first_load_hash_value(&at, &key, field_ttl).await
                        }
                        KeyType::Stream => first_load_stream_value(&at, &key, true).await,
                        KeyType::Json => get_redis_json_value(&at, &key).await,
                        // The chart + metadata are loaded lazily by
                        // ZedisTimeSeriesEditor (it drives its own TS.INFO /
                        // TS.RANGE with range controls), so here we only need
                        // to classify the key so the editor dispatch routes
                        // to that viewer.
                        KeyType::TimeSeries => Ok(RedisValue {
                            key_type: KeyType::TimeSeries,
                            ..Default::default()
                        }),
                        // RedisBloom structures (Bloom / Cuckoo / CMS / Top-K
                        // / t-digest) — classify only; ZedisProbabilisticEditor
                        // fetches its own *.INFO + extras. `key_type` keeps the
                        // ProbKind so the dispatch knows which one.
                        KeyType::Probabilistic(_) => Ok(RedisValue {
                            key_type,
                            ..Default::default()
                        }),
                        // Redis 8 Vector Set — classify only; the viewer drives
                        // its own VINFO / VCARD / VDIM / VRANDMEMBER / VSIM.
                        KeyType::Vectorset => Ok(RedisValue {
                            key_type: KeyType::Vectorset,
                            ..Default::default()
                        }),
                        // A module type without a viewer: its DUMP bytes,
                        // behind the size gates — see `load_module_value`.
                        KeyType::Module(_) => {
                            let size = key_memory_usage(&at, key.as_str(), key_type.as_str()).await.ok();
                            load_module_value(&at, server_id.as_str(), key.as_str(), key_type, size, bypass_size_gate)
                                .await
                        }
                        KeyType::Unknown | KeyType::Channel => Err(Error::Invalid {
                            message: format!("unsupported key type: {}", key_type.as_str()),
                        }),
                    }
                })
                .await?;
                // Size and the OBJECT header decorations are both O(1) tail
                // calls on the same multiplexed connection, so they go out
                // together — the chip costs no round trip of its own.
                let (memory_usage, (encoding, heat)) = join(
                    key_memory_usage(&at, key.as_str(), key_type.as_str()),
                    key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                )
                .await;
                if let Ok(memory_usage) = memory_usage {
                    redis_value.size = memory_usage;
                }
                redis_value.encoding = encoding.into();
                redis_value.heat = heat;
                redis_value.expire_at = expire_at;
                Ok(redis_value)
            },
            move |this, result, cx| {
                if this.open.key.as_ref() != Some(&current_key) {
                    return;
                }
                // A "load anyway" the user cancelled: the gate is back, and
                // this reply is not wanted any more.
                if bypass_size_gate && this.open.size_gate_bypassed.as_ref() != Some(&current_key) {
                    return;
                }
                this.open.size_gate_pending = None;
                match result {
                    Ok(value) => {
                        if this.open.value.as_ref() == Some(&value) {
                            return;
                        }
                        let mut tree_changed = false;
                        if !value.is_expired() {
                            tree_changed = if let Some(k) = this.scan.keys.get_mut(&current_key) {
                                if *k != value.key_type {
                                    *k = value.key_type();
                                    true
                                } else {
                                    false
                                }
                            } else {
                                this.scan.keys.insert(current_key.clone(), value.key_type());
                                true
                            };
                        }
                        if let Some(ttl_secs) = value.ttl_secs() {
                            tree_changed |= this.sync_tree_ttl(&current_key, ttl_secs);
                        }
                        if tree_changed {
                            this.scan.renew_tree_id();
                            cx.emit(ServerEvent::KeyTreeUpdated);
                        }
                        this.open.next_epoch();
                        this.open.value = Some(value);
                    }
                    Err(e) => {
                        // Keep the key selected and surface the failure in the
                        // value panel (Failed status) instead of silently
                        // deselecting — a transient toast was otherwise the
                        // only signal, and a blank panel looks identical to
                        // "no key selected". The editor renders this inline
                        // with a retry button.
                        let message: SharedString = e.to_string().into();
                        this.open.next_epoch();
                        match this.open.value.as_mut() {
                            Some(value) => {
                                value.status = RedisValueStatus::Failed(message);
                                value.data = None;
                            }
                            None => {
                                this.open.value = Some(RedisValue {
                                    status: RedisValueStatus::Failed(message),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                };
                cx.emit(ServerEvent::ValueLoaded);
                cx.notify();
            },
            cx,
        );
    }

    /// Re-read what the key bar says about the selected key after a write
    /// to it: the size, and the `OBJECT` decorations beside it — the
    /// encoding, which a write can change (`int` to `embstr`, `listpack` to
    /// `quicklist`), and the heat, which the write itself resets. All three
    /// are taken at load, and after a save they went on describing the value
    /// as it was: a string set from `12` to `ABC` still said `int`. Member
    /// counts already move locally. Does not flip `Updating` — the write
    /// already finished.
    pub(crate) fn refresh_value_header(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.open.key.clone() else {
            return;
        };
        if key.is_empty() {
            return;
        }
        let key_type = self
            .open
            .value
            .as_ref()
            .map(|value| value.key_type().as_str().to_string())
            .unwrap_or_default();
        let epoch = self.open.value_epoch;
        let at = self.at();
        // The same two questions the load asks, answered the same way.
        let with_encoding = self.facts.feature_matrix().is_usable(ServerCommand::ObjectEncoding);
        let heat_probe = get_server_heat_probe(self.server_id.as_str());
        self.spawn(
            ServerTask::RefreshValueHeader,
            move || async move {
                let (size, (encoding, heat)) = join(
                    key_memory_usage(&at, key.as_str(), &key_type),
                    key_object_meta(&at, key.as_str(), with_encoding, heat_probe),
                )
                .await;
                Ok((size.ok(), encoding, heat))
            },
            move |this, result, cx| {
                if this.open.value_epoch != epoch {
                    return;
                }
                let Ok((size, encoding, heat)) = result else {
                    return;
                };
                let Some(value) = this.open.value.as_mut() else {
                    return;
                };
                if Self::apply_refreshed_header(value, size, encoding.into(), heat) {
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// Put a re-read header on `value`; true when the key bar would change.
    /// A size the server would not give leaves the one shown.
    pub(super) fn apply_refreshed_header(
        value: &mut RedisValue,
        size: Option<u64>,
        encoding: SharedString,
        heat: HeatMetric,
    ) -> bool {
        let mut changed = size.is_some_and(|size| Self::apply_refreshed_memory_size(value, size));
        if value.encoding != encoding {
            value.encoding = encoding;
            changed = true;
        }
        if value.heat != heat {
            value.heat = heat;
            changed = true;
        }
        changed
    }

    /// True when the header size chip would change.
    pub(super) fn apply_refreshed_memory_size(value: &mut RedisValue, size: u64) -> bool {
        if value.size == size {
            return false;
        }
        value.size = size;
        true
    }

    /// Runs one type-native operation against the selected key, then
    /// reloads the value.
    ///
    /// A full reload rather than an optimistic edit: `LTRIM` and the pops
    /// change an unbounded number of elements, and a half-applied local
    /// guess is worse than the round trip. The outcome becomes one toast —
    /// the new counter, or what was taken out — because these are the
    /// operations whose *result* is the point.
    pub fn run_key_operation(&mut self, key: SharedString, op: KeyOp, cx: &mut Context<Self>) {
        let at = self.at();
        let key_arg = key.clone();
        // Taken before `op` moves into the task. `None` for the String
        // operations, whose history is before-and-after snapshots instead.
        let description = op.describe();
        // That snapshot: the value this operation is about to change, kept
        // like a save keeps the value it overwrites, so a path write to a
        // document can be diffed and restored.
        // Not a preview's: its bytes are the value's beginning only, and
        // restoring them would cut the key down to that.
        let snapshot = self
            .open
            .value
            .as_ref()
            .filter(|value| value.preview_of().is_none())
            .and_then(|value| value.bytes_value())
            .map(|bytes| bytes.bytes.clone())
            .filter(|bytes| !bytes.is_empty());
        if let Some(bytes) = snapshot {
            self.push_value_history(key.clone(), bytes);
        }
        self.spawn_with_arg(
            ServerTask::RunKeyOperation,
            key.clone(),
            move || async move { Ok(run_key_op(&at, key_arg.as_str(), op).await?) },
            move |this, result, cx| {
                let Ok(outcome) = result else {
                    // The error already surfaced through `spawn_with_arg`.
                    return;
                };
                // Logged as an operation: its effect on individual elements
                // was not read back, so it stays out of the net diff.
                if let Some(description) = description {
                    this.record_changes(key.clone(), vec![ChangeEntry::operation(unix_ts(), description)]);
                }
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let message: SharedString = match outcome {
                    KeyOpOutcome::Number(value) => t!("key_ops.result_number", value = value, locale = locale)
                        .to_string()
                        .into(),
                    KeyOpOutcome::Removed(items) if items.is_empty() => {
                        t!("key_ops.result_nothing", locale = locale).to_string().into()
                    }
                    KeyOpOutcome::Removed(items) => t!(
                        "key_ops.result_removed",
                        count = items.len(),
                        items = items.join(", "),
                        locale = locale
                    )
                    .to_string()
                    .into(),
                    KeyOpOutcome::Count(count) => t!("key_ops.result_count", count = count, locale = locale)
                        .to_string()
                        .into(),
                    KeyOpOutcome::Done => t!("key_ops.result_done", locale = locale).to_string().into(),
                };
                this.emit_info_notification(message, cx);
                this.reload_value(key.clone(), cx);
            },
            cx,
        );
    }

    /// Reloads the value for a selected key.
    pub fn reload_value(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.get_value(key, ServerTask::ReloadValue, cx);
    }

    /// The module types among the loaded keys, for the type-filter menu.
    pub fn module_types_seen(&self) -> Vec<ModuleTypeId> {
        let mut ids: Vec<ModuleTypeId> = self
            .scan
            .keys
            .values()
            .filter_map(|t| match t {
                KeyType::Module(id) => Some(*id),
                _ => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Reloads the value for a key with the oversized-value gate disabled
    /// ("Load anyway" on the too-large panel, "Load whole value" on a
    /// preview). The bypass is remembered for the key so a later refresh
    /// doesn't bounce back to the gate panel.
    pub fn load_value_ignore_size_limit(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.open.size_gate_bypassed = Some(key.clone());
        self.open.size_gate_preview = None;
        // Busy while the (large) payload downloads, and carrying the size
        // the probe found: the editor says "Loading 22.5 MB…" with a Cancel
        // instead of going blank, and the header keeps the real size.
        let replaced = self.open.value.take();
        let size = replaced.as_ref().map_or(0, |value| value.size);
        self.open.size_gate_pending = replaced.map(|value| (key.clone(), value));
        self.open.next_epoch();
        self.open.value = Some(RedisValue {
            status: RedisValueStatus::Loading,
            size,
            ..Default::default()
        });
        self.get_value(key, ServerTask::ReloadValue, cx);
    }

    /// Gives up a "load anyway" that has not landed: what it replaced — the
    /// too-large panel, or the preview — comes back as it was, the key is
    /// gated again, and the reply still on its way is dropped when it
    /// arrives (`get_value`). The download itself is not interrupted: the
    /// reply is already on the shared connection.
    pub fn cancel_large_load(&mut self, cx: &mut Context<Self>) {
        let Some((key, replaced)) = self.open.size_gate_pending.take() else {
            return;
        };
        if self.open.key.as_ref() != Some(&key) {
            return;
        }
        self.open.size_gate_bypassed = None;
        if replaced.preview_of().is_some() {
            let offset = replaced.preview_offset();
            self.open.size_gate_preview = Some((key, offset));
        }
        self.open.next_epoch();
        self.open.value = Some(replaced);
        cx.emit(ServerEvent::ValueLoaded);
        cx.notify();
    }

    /// Loads the first [`VALUE_PREVIEW_BYTES`] of a String too large to load
    /// whole ("Preview" on the too-large panel), as a read-only value.
    pub fn load_value_preview(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.load_value_range(key, 0, cx);
    }

    /// Loads a [`VALUE_PREVIEW_BYTES`] chunk starting at `offset` (`GETRANGE`)
    /// of an oversized String, keeping existing preview data visible while the
    /// next chunk arrives so stepping pages does not flash a blank skeleton.
    pub fn load_value_range(&mut self, key: SharedString, offset: u64, cx: &mut Context<Self>) {
        self.open.size_gate_preview = Some((key.clone(), offset));
        self.open.next_epoch();
        if let Some(current) = self.open.value.as_mut()
            && current.preview_of().is_some()
        {
            current.status = RedisValueStatus::Loading;
        } else {
            self.open.value = Some(RedisValue {
                status: RedisValueStatus::Loading,
                ..Default::default()
            });
        }
        self.get_value(key, ServerTask::ReloadValue, cx);
    }

    pub fn is_channel_mode(&self) -> bool {
        self.open.value.as_ref().is_some_and(|v| v.key_type == KeyType::Channel)
    }

    /// Sets the channel mode for current server.
    ///
    /// The Pub/Sub panel is the editor's, and the terminal is drawn in the
    /// editor's place: opened from a menu while the terminal was up, the
    /// mode changed underneath and the menu item appeared to do nothing
    /// (#191). Like selecting a key, it takes the terminal off.
    pub fn change_channel_mode(&mut self, cx: &mut Context<Self>) {
        self.open.next_epoch();
        self.open.value = Some(RedisValue {
            key_type: KeyType::Channel,
            ..Default::default()
        });
        self.open.key = None;
        if self.terminal {
            self.terminal = false;
            cx.emit(ServerEvent::TerminalToggled(false));
        }
        cx.notify();
    }

    /// Publishes a message to a Redis channel. `sharded` switches to
    /// `SPUBLISH` (Redis 7+) — on clusters the message is then routed by
    /// the channel's hash slot instead of being broadcast to every node.
    pub fn publish_message(
        &mut self,
        channel: SharedString,
        message: SharedString,
        sharded: bool,
        cx: &mut Context<Self>,
    ) {
        let at = self.at();
        self.spawn_with_arg(
            ServerTask::PublishMessage,
            channel.clone(),
            move || async move {
                publish(&at, channel.as_str(), message.as_str(), sharded).await?;
                Ok(())
            },
            |_this, _result, cx| {
                cx.emit(ServerEvent::PubsubMessagePublished);
                cx.notify();
            },
            cx,
        );
    }

    /// Selects a key and fetches its details (Type, TTL, Value).
    pub fn select_key(&mut self, key: SharedString, cx: &mut Context<Self>) {
        let same_key = self.open.key.as_ref() == Some(&key);
        self.open.next_epoch();
        self.open.key = Some(key.clone());
        if key.is_empty() {
            return;
        }
        // Opening a key counts as using its history: the budget evicts the
        // key used least recently, and one you keep returning to should stay.
        self.open.histories.touch(&key, unix_ts());
        self.terminal = false;
        // A different key drops the previous payload (type + rows) so the
        // editor cannot paint them under this name. The tree already knows
        // the type. Re-selecting the same key keeps its rows.
        let tree_type = self.scan.keys.get(&key).copied().unwrap_or(KeyType::Unknown);
        self.open.value = Some(RedisValue::for_select(same_key, self.open.value.take(), tree_type));
        if !self.scan.keys.contains_key(&key) {
            self.scan.keys.insert(key.clone(), KeyType::Unknown);
        }
        // MRU: most-recent first, capped per (server, db). Best-effort —
        // a full disk/db error must not block opening the key.
        let scope = recent_keys_scope(self.server_id.as_str(), self.db);
        let key_for_mru = key.to_string();
        cx.background_spawn(async move {
            let _ = get_recent_keys_manager().add_record(&scope, &key_for_mru);
        })
        .detach();
        cx.emit(ServerEvent::KeySelected(key.clone()));
        cx.notify();

        self.get_value(key, ServerTask::Selectkey, cx);
    }
}

/// A key of a type Zedis has no viewer for. The value is shown as its
/// serialized form (`DUMP`), read-only, behind two gates: the auto-load cap
/// the string editor uses (the "load anyway" path lifts it), and a hard cap
/// above which DUMP is never sent — it serializes the whole value on the
/// server's main thread with no way to page, so a huge module value is
/// read from the terminal, not clicked on. `size` is what `MEMORY USAGE`
/// said: a module reports it through its own `mem_usage` callback, and a
/// missing one answers 0 or an error — unknown, which is not "small", and
/// unknown never gets a DUMP. A value the card cannot show still carries
/// its type, size and TTL, so rename / TTL / delete / copy keep working.
async fn load_module_value(
    at: &ServerDb,
    server_id: &str,
    key: &str,
    key_type: KeyType,
    size: Option<u64>,
    bypass_size_gate: bool,
) -> Result<RedisValue, Error> {
    let size = size.filter(|s| *s > 0);
    let mut value = RedisValue {
        key_type,
        size: size.unwrap_or(0),
        ..Default::default()
    };
    let Some(size) = size else {
        return Ok(value);
    };
    if size > MAX_MODULE_DUMP_BYTES {
        return Ok(value);
    }
    if !bypass_size_gate && size > MAX_INLINE_VALUE_SIZE {
        value.status = RedisValueStatus::TooLarge(size);
        return Ok(value);
    }
    if !get_server_features(server_id).is_usable(ServerCommand::Dump) {
        return Ok(value);
    }
    let payload = dump_key(at, key).await?;
    value.data = Some(RedisValueData::Bytes(Arc::new(RedisBytesValue {
        bytes: Bytes::from(payload),
        view_mode: ViewMode::Hex,
        ..Default::default()
    })));
    Ok(value)
}

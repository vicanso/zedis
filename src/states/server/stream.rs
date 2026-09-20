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
        StreamIdmpInfo, StreamInfoData, StreamPendingEntry, StreamRefPolicy, StreamSummary, StreamTrim,
    },
};
use crate::states::ZedisGlobalStore;
use crate::states::i18n_stream_editor;
use crate::{
    connection::{
        PENDING_PAGE, ServerDb, StreamGroup, StreamInfo, StreamPending, consumer_create, consumer_delete, group_create,
        group_destroy, group_set_id, next_stream_id, pending_page, stream_ack, stream_ack_delete, stream_add,
        stream_autoclaim, stream_claim, stream_delete, stream_info, stream_len, stream_nack, stream_page,
        stream_set_id, stream_trim,
    },
    error::Error,
};
use gpui::{SharedString, prelude::*};
use rust_i18n::t;
use std::collections::HashSet;
use std::sync::Arc;

type Result<T, E = Error> = std::result::Result<T, E>;

/// `zedis-connection`'s stream structs, in the `SharedString` the views draw
/// from. The connection crate has no gpui, so the two shapes are kept apart
/// and converted here — one place, on the way in (ADR 10).
fn project_info(info: StreamInfo) -> StreamInfoData {
    StreamInfoData {
        summary: info.summary.map(|summary| StreamSummary {
            groups_count: summary.groups_count,
            first_entry_id: summary.first_entry_id.into(),
            last_entry_id: summary.last_entry_id.into(),
            last_generated_id: summary.last_generated_id.into(),
            radix_tree_keys: summary.radix_tree_keys,
            radix_tree_nodes: summary.radix_tree_nodes,
            idmp: summary.idmp.map(|idmp| StreamIdmpInfo {
                pids_tracked: idmp.pids_tracked,
                iids_tracked: idmp.iids_tracked,
                iids_added: idmp.iids_added,
                iids_duplicates: idmp.iids_duplicates,
            }),
        }),
        groups: info.groups.into_iter().map(project_group).collect(),
    }
}

fn project_group(group: StreamGroup) -> StreamGroupDetail {
    StreamGroupDetail {
        name: group.name.into(),
        consumers_count: group.consumers_count,
        pending_count: group.pending_count,
        last_delivered_id: group.last_delivered_id.into(),
        lag: group.lag,
        consumers: group
            .consumers
            .into_iter()
            .map(|consumer| StreamConsumerDetail {
                name: consumer.name.into(),
                pending: consumer.pending,
                idle_ms: consumer.idle_ms,
            })
            .collect(),
        pending_entries: group.pending_entries.into_iter().map(project_pending).collect(),
        pending_done: group.pending_done,
    }
}

fn project_pending(entry: StreamPending) -> StreamPendingEntry {
    StreamPendingEntry {
        id: entry.id.into(),
        consumer: entry.consumer.into(),
        idle_ms: entry.idle_ms,
        delivery_count: entry.delivery_count,
    }
}

fn project_entries(entries: Vec<(String, Vec<(String, String)>)>) -> Vec<RedisStreamEntry> {
    entries
        .into_iter()
        .map(|(id, fields)| {
            let fields = fields.into_iter().map(|(f, v)| (f.into(), v.into())).collect();
            (id.into(), fields)
        })
        .collect()
}

/// A page of entries, oldest-first (`XRANGE`) or newest-first (`XREVRANGE`).
async fn get_redis_stream_value(
    at: &ServerDb,
    key: &str,
    cursor: Option<String>,
    count: usize,
    reverse: bool,
) -> Result<(String, Vec<RedisStreamEntry>)> {
    let (cursor, entries) = stream_page(at, key, cursor.as_deref(), count, reverse).await?;
    Ok((cursor, project_entries(entries)))
}

pub(crate) async fn first_load_stream_value(at: &ServerDb, key: &str, reverse: bool) -> Result<RedisValue> {
    let size = stream_len(at, key).await?;
    let (cursor, values) = get_redis_stream_value(at, key, None, 100, reverse).await?;
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

impl ZedisServerState {
    fn exec_stream_op<F, Fut, R>(
        &mut self,
        task: ServerTask,
        cx: &mut Context<Self>,
        optimistic_update: impl FnOnce(&mut RedisStreamValue),
        redis_op: F,
        on_success: impl FnOnce(&mut Self, R, &mut Context<Self>) + Send + 'static,
    ) where
        F: FnOnce(String, ServerDb) -> Fut + Send + 'static,
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

        let at = self.at();

        self.spawn(
            task,
            move || async move { redis_op(key_str, at).await },
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
        let at = self.at();
        let guard_key = key.clone();

        self.spawn_with_arg(
            ServerTask::FetchStreamInfo,
            key.clone(),
            move || async move { Ok(project_info(stream_info(&at, key.as_str()).await?)) },
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
        let at = self.at();
        let guard_key = key.clone();

        if let Some(value) = self.value.as_mut() {
            value.status = RedisValueStatus::Loading;
        }
        cx.notify();

        self.spawn_with_arg(
            ServerTask::ReloadValue,
            key.clone(),
            move || async move { first_load_stream_value(&at, key.as_str(), reverse).await },
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

        let at = self.at();
        let guard_key = key.clone();
        cx.emit(ServerEvent::ValuePaginationStarted);

        self.spawn_with_arg(
            ServerTask::LoadMoreValue,
            key.clone(),
            move || async move { get_redis_stream_value(&at, key.as_str(), Some(cursor), 100, reverse).await },
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
            move |key, at| async move {
                let fields: Vec<(String, String)> = values
                    .into_iter()
                    .map(|(field, value)| (field.to_string(), value.to_string()))
                    .collect();
                Ok(stream_add(&at, &key, id.as_str(), &fields).await?)
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
            move |key, at| async move {
                stream_delete(&at, &key, &[entry_id_clone.as_str()]).await?;
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

    /// Removes several entries in one `XDEL` — the table's multi-select
    /// delete. The server reports how many it actually removed, which is
    /// what the size is adjusted by: an id already trimmed away by MAXLEN
    /// between the load and the click must not shrink the count twice.
    pub fn remove_stream_values(&mut self, entry_ids: Vec<SharedString>, cx: &mut Context<Self>) {
        if entry_ids.is_empty() {
            return;
        }
        let gone: HashSet<SharedString> = entry_ids.iter().cloned().collect();
        self.exec_stream_op(
            ServerTask::RemoveStreamEntry,
            cx,
            move |stream| {
                stream.values.retain(|(id, _)| !gone.contains(id));
            },
            move |key, at| async move {
                let ids: Vec<&str> = entry_ids.iter().map(|id| id.as_str()).collect();
                Ok(stream_delete(&at, &key, &ids).await?)
            },
            |this, removed, cx| {
                if let Some(RedisValueData::Stream(stream_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let stream = Arc::make_mut(stream_data);
                    stream.size = stream.size.saturating_sub(removed as usize);
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
            move |key, at| async move {
                group_create(&at, &key, group.as_str(), start_id.as_str()).await?;
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
            move |key, at| async move {
                group_set_id(&at, &key, group.as_str(), id.as_str()).await?;
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
            move |key, at| async move {
                group_destroy(&at, &key, group.as_str()).await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// XACK key group id — acknowledge one pending entry. The refreshed
    /// XINFO is the feedback: the row leaves the pending table.
    pub fn ack_stream_entry(&mut self, group: SharedString, entry_id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::AckStreamEntry,
            cx,
            |_| {},
            move |key, at| async move {
                stream_ack(&at, &key, group.as_str(), entry_id.as_str()).await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// XACKDEL key group KEEPREF IDS 1 id — acknowledge one pending entry
    /// *and* delete it from the stream in one atomic step (Redis 8.2+).
    /// KEEPREF matches classic XDEL semantics: other groups' PEL
    /// references stay. Entries are gone from the value view too, so the
    /// success path reloads it alongside the info refresh.
    pub fn ackdel_stream_entry(&mut self, group: SharedString, entry_id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::AckDelStreamEntry,
            cx,
            |_| {},
            move |key, at| async move {
                stream_ack_delete(&at, &key, group.as_str(), entry_id.as_str()).await?;
                Ok(())
            },
            |this, _, cx| {
                let reverse = this
                    .value
                    .as_ref()
                    .and_then(|v| v.stream_value())
                    .map(|s| s.reverse)
                    .unwrap_or_default();
                this.reload_stream_value(reverse, cx);
                this.fetch_stream_info(cx);
            },
        );
    }

    /// XNACK key group FAIL IDS 1 id — release one pending entry back to
    /// the group PEL without acking (Redis 8.8+): its consumer is cleared
    /// and it moves to the head of the idle order, claimable at once. FAIL
    /// keeps the delivery counter, so a retry policy still sees the failed
    /// attempt (SILENT would undo it, FATAL would poison the entry).
    pub fn nack_stream_entry(&mut self, group: SharedString, entry_id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::NackStreamEntry,
            cx,
            |_| {},
            move |key, at| async move {
                stream_nack(&at, &key, group.as_str(), entry_id.as_str()).await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// XGROUP CREATECONSUMER key group consumer (Redis 6.2+) — an empty
    /// consumer, so it shows up in XINFO before its first read. The server
    /// answers 0 when the name already exists.
    pub fn create_stream_consumer(&mut self, group: SharedString, consumer: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::CreateStreamConsumer,
            cx,
            |_| {},
            move |key, at| async move { Ok(consumer_create(&at, &key, group.as_str(), consumer.as_str()).await?) },
            |this, created, cx| {
                if !created {
                    this.emit_warning_notification(i18n_stream_editor(cx, "consumer_exists"), cx);
                }
                this.fetch_stream_info(cx);
            },
        );
    }

    /// XGROUP DELCONSUMER key group consumer — the counterpart to
    /// CREATECONSUMER above.
    ///
    /// The reply is how many *pending* entries went with the consumer, and
    /// that is the number worth reporting: those messages were delivered,
    /// never acknowledged, and are now unreachable through this group. The
    /// UI shows the count before the click too, so the decision is made with
    /// it rather than told about it afterwards.
    pub fn delete_stream_consumer(&mut self, group: SharedString, consumer: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::DeleteStreamConsumer,
            cx,
            |_| {},
            move |key, at| async move { Ok(consumer_delete(&at, &key, group.as_str(), consumer.as_str()).await?) },
            |this, pending, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let message = t!("stream_editor.consumer_deleted", count = pending, locale = locale);
                this.emit_info_notification(message.to_string().into(), cx);
                this.fetch_stream_info(cx);
            },
        );
    }

    /// XSETID key id — the *stream's* last-generated id, not a group's
    /// position (that is `XGROUP SETID`, above).
    ///
    /// Lowering it lets `XADD` mint ids that already existed, which is why
    /// this is a recovery tool and not an everyday one: the UI puts it
    /// behind a confirmation that says so. `ENTRIESADDED` /
    /// `MAXDELETEDID` are deliberately not exposed — they are replication
    /// bookkeeping, and getting them wrong is worse than leaving them.
    pub fn set_stream_id(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::SetStreamId,
            cx,
            |_| {},
            move |key, at| async move {
                stream_set_id(&at, &key, id.as_str()).await?;
                Ok(())
            },
            |this, _, cx| {
                this.fetch_stream_info(cx);
            },
        );
    }

    /// XCLAIM key group consumer 0 id JUSTID — force-reassign one
    /// pending entry (min-idle-time 0, so it always claims; JUSTID keeps
    /// the delivery counter untouched).
    pub fn claim_stream_entry(
        &mut self,
        group: SharedString,
        consumer: SharedString,
        entry_id: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.exec_stream_op(
            ServerTask::ClaimStreamEntry,
            cx,
            |_| {},
            move |key, at| async move {
                stream_claim(&at, &key, group.as_str(), consumer.as_str(), entry_id.as_str()).await?;
                Ok(())
            },
            |this, _, cx| this.fetch_stream_info(cx),
        );
    }

    /// XAUTOCLAIM key group consumer min-idle 0-0 COUNT n JUSTID —
    /// batch-claim up to `count` entries idle for at least `min_idle_ms`.
    /// Notifies with how many were claimed (Redis ≥ 6.2; older servers
    /// surface the unknown-command error like any other op).
    pub fn autoclaim_stream_entries(
        &mut self,
        group: SharedString,
        consumer: SharedString,
        min_idle_ms: u64,
        count: usize,
        cx: &mut Context<Self>,
    ) {
        self.exec_stream_op(
            ServerTask::AutoclaimStreamEntries,
            cx,
            |_| {},
            move |key, at| async move {
                let claimed =
                    stream_autoclaim(&at, &key, group.as_str(), consumer.as_str(), min_idle_ms, count).await?;
                Ok(claimed)
            },
            |this, claimed, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let message: SharedString = t!("stream_editor.autoclaim_done", count = claimed, locale = locale)
                    .to_string()
                    .into();
                let title: SharedString = t!("stream_editor.autoclaim_title", locale = locale).to_string().into();
                this.emit_success_notification(message, title, cx);
                this.fetch_stream_info(cx);
            },
        );
    }

    /// XTRIM key MAXLEN n / MINID id — cut the stream. Destructive; the
    /// caller confirms first. Reloads entries + info on success (loaded
    /// rows may have been trimmed away).
    ///
    /// `policy` (Redis 8.2+) controls what happens to consumer-group PEL
    /// references of trimmed entries; the caller passes `None` on servers
    /// without the option words.
    pub fn trim_stream(&mut self, trim: StreamTrim, policy: Option<StreamRefPolicy>, cx: &mut Context<Self>) {
        self.exec_stream_op(
            ServerTask::TrimStream,
            cx,
            |_| {},
            move |key, at| async move { Ok(stream_trim(&at, &key, &trim, policy).await?) },
            |this, removed, cx| {
                let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                let message: SharedString = t!("stream_editor.trim_done", count = removed, locale = locale)
                    .to_string()
                    .into();
                let title: SharedString = t!("stream_editor.trim_title", locale = locale).to_string().into();
                this.emit_success_notification(message, title, cx);
                let reverse = this
                    .value
                    .as_ref()
                    .and_then(|v| v.stream_value())
                    .map(|s| s.reverse)
                    .unwrap_or_default();
                this.reload_stream_value(reverse, cx);
                this.fetch_stream_info(cx);
            },
        );
    }

    /// Next XPENDING page for `group`, appended after the last loaded
    /// entry (portable ms-seq stepping — no exclusive ranges needed).
    pub fn load_more_stream_pending(&mut self, group: SharedString, cx: &mut Context<Self>) {
        let start = self
            .value
            .as_ref()
            .and_then(|v| v.stream_value())
            .and_then(|s| s.info.as_ref())
            .and_then(|info| info.groups.iter().find(|g| g.name == group))
            .and_then(|g| g.pending_entries.last())
            .and_then(|entry| next_stream_id(entry.id.as_ref()))
            .unwrap_or_else(|| "-".to_string());
        let group_for_merge = group.clone();
        self.exec_stream_op(
            ServerTask::LoadStreamPending,
            cx,
            |_| {},
            move |key, at| async move {
                let entries = pending_page(&at, &key, group.as_ref(), &start).await?;
                Ok(entries.into_iter().map(project_pending).collect::<Vec<_>>())
            },
            move |this, entries: Vec<StreamPendingEntry>, cx| {
                if let Some(RedisValueData::Stream(stream_data)) = this.value.as_mut().and_then(|v| v.data.as_mut()) {
                    let stream = Arc::make_mut(stream_data);
                    if let Some(info) = stream.info.as_mut() {
                        let info = Arc::make_mut(info);
                        if let Some(g) = info.groups.iter_mut().find(|g| g.name == group_for_merge) {
                            g.pending_done = entries.len() < PENDING_PAGE;
                            g.pending_entries.extend(entries);
                        }
                    }
                }
                cx.emit(ServerEvent::ValueUpdated);
            },
        );
    }
}

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

use crate::helpers::EditorAction;
use crate::states::{ErrorMessage, GlobalEvent, NotificationAction, ZedisGlobalStore, ZedisServerState};
use gpui::prelude::*;
use gpui::{EventEmitter, SharedString};

/// Background task types for Redis operations
///
/// Each variant represents a specific async operation that runs in the background
#[derive(Clone, PartialEq, Debug)]
pub enum ServerTask {
    /// Refresh the Redis server info
    RefreshRedisInfo,

    /// Auto refresh keys
    AutoRefresh,

    /// Connect to and load metadata from a server
    SelectServer,

    /// Fill in key types for unknown keys
    FillKeyTypes,

    /// Load value data for a selected key
    Selectkey,

    /// Delete a key from Redis
    DeleteKey,

    /// Reload value data for a selected key
    ReloadValue,

    /// Delete multiple keys from Redis
    DeleteKeys,

    /// Scan for keys matching pattern
    ScanKeys,

    /// Scan keys with a specific prefix (for lazy folder loading)
    ScanPrefix,

    /// Add a new key
    AddKey,
    /// Update TTL (time-to-live) for a key
    UpdateKeyTtl,

    /// Delete an item from a list
    RemoveListValue,

    /// Update a value in a list
    UpdateListValue,

    /// Push a value to a list
    PushListValue,

    /// Load more items
    LoadMoreValue,

    /// Add a value to a set
    AddSetValue,
    /// Update a value in a set
    UpdateSetValue,
    /// Remove a value from a set
    RemoveSetValue,

    /// Add a value to a zset
    AddZsetValue,
    /// Remove a value from a zset
    RemoveZsetValue,

    /// Add a field-value pair to a hash
    AddHashField,
    /// Update a field-value pair in a hash
    UpdateHashField,
    /// Remove a value from a hash
    RemoveHashField,

    /// Add a stream entry
    AddStreamEntry,
    /// Remove a stream entry
    RemoveStreamEntry,

    /// Save edited value back to Redis
    SaveValue,
}

impl ServerTask {
    /// Get string representation of task (for logging and error messages)
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerTask::RefreshRedisInfo => "refresh_redis_info",
            ServerTask::AutoRefresh => "auto_refresh",
            ServerTask::SelectServer => "select_server",
            ServerTask::FillKeyTypes => "fill_key_types",
            ServerTask::Selectkey => "select_key",
            ServerTask::DeleteKey => "delete_key",
            ServerTask::ReloadValue => "reload_value",
            ServerTask::DeleteKeys => "delete_keys",
            ServerTask::ScanKeys => "scan_keys",
            ServerTask::ScanPrefix => "scan_prefix",
            ServerTask::AddKey => "add_key",
            ServerTask::UpdateKeyTtl => "update_key_ttl",
            ServerTask::RemoveListValue => "remove_list_value",
            ServerTask::UpdateListValue => "update_list_value",
            ServerTask::LoadMoreValue => "load_more_value",
            ServerTask::SaveValue => "save_value",
            ServerTask::PushListValue => "push_list_value",
            ServerTask::AddSetValue => "add_set_value",
            ServerTask::UpdateSetValue => "update_set_value",
            ServerTask::RemoveSetValue => "remove_set_value",
            ServerTask::AddZsetValue => "add_zset_value",
            ServerTask::RemoveZsetValue => "remove_zset_value",
            ServerTask::AddHashField => "add_hash_field",
            ServerTask::UpdateHashField => "update_hash_field",
            ServerTask::RemoveHashField => "remove_hash_field",
            ServerTask::AddStreamEntry => "add_stream_entry",
            ServerTask::RemoveStreamEntry => "remove_stream_entry",
        }
    }
}

/// Events emitted by server state for reactive UI updates
#[derive(Debug)]
pub enum ServerEvent {
    /// A new background task has started.
    TaskStarted(ServerTask),

    /// Terminal toggled
    TerminalToggled(bool),

    /// A key has been selected for viewing/editing
    KeySelected,
    /// Key scan operation has started
    KeyScanStarted,
    /// Key scan found a new batch of keys.
    KeyScanPaged,
    /// Key scan operation has fully completed.
    KeyScanFinished,
    /// Key collapse all
    KeyCollapseAll,

    /// A key's value has been fetched (initial load).
    ValueLoaded,
    /// A key's value has been updated
    ValueUpdated,
    /// A key's value view mode has been updated
    ValueModeViewUpdated,
    /// Load more value
    ValuePaginationStarted,
    /// Load more value
    ValuePaginationFinished,
    /// Add a value to a set、list、hash、zset
    ValueAdded,

    /// User selected a different server
    ServerSelected(SharedString),
    /// Server metadata (info/dbsize) has been refreshed.
    ServerInfoUpdated,
    /// Periodic redis info updated.
    ServerRedisInfoUpdated,

    /// Soft wrap changed
    SoftWrapToggled(bool),
    /// An error occurred.
    ErrorOccurred(ErrorMessage),

    /// Trigger Action
    EditionActionTriggered(EditorAction),
}

impl EventEmitter<ServerEvent> for ZedisServerState {}

impl ZedisServerState {
    pub fn emit_editor_action(&self, event: EditorAction, cx: &mut Context<Self>) {
        let readonly = self.readonly();
        if readonly
            && matches!(
                event,
                EditorAction::Create | EditorAction::Save | EditorAction::UpdateTtl
            )
        {
            return;
        }
        cx.emit(ServerEvent::EditionActionTriggered(event));
    }
    pub fn emit_info_notification(&self, message: SharedString, cx: &mut Context<Self>) {
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_state, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_info(message)));
        });
    }
    pub fn emit_success_notification(&self, message: SharedString, title: SharedString, cx: &mut Context<Self>) {
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_state, cx| {
            cx.emit(GlobalEvent::Notification(
                NotificationAction::new_success(message).with_title(title),
            ));
        });
    }
    pub fn emit_warning_notification(&self, message: SharedString, cx: &mut Context<Self>) {
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_state, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_warning(message)));
        });
    }
    pub fn emit_error_notification(&self, message: SharedString, cx: &mut Context<Self>) {
        cx.global::<ZedisGlobalStore>().clone().update(cx, |_state, cx| {
            cx.emit(GlobalEvent::Notification(NotificationAction::new_error(message)));
        });
    }
}

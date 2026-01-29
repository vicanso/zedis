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
use crate::states::{ErrorMessage, NotificationAction, ZedisServerState};
use gpui::prelude::*;
use gpui::{EventEmitter, SharedString};

/// Background task types for Redis operations
///
/// Each variant represents a specific async operation that runs in the background
#[derive(Clone, PartialEq, Debug)]
pub enum ServerTask {
    /// Refresh the Redis server info
    RefreshRedisInfo,

    /// Connect to and load metadata from a server
    SelectServer,

    /// Remove a server from configuration
    RemoveServer,

    /// Update the server query mode
    UpdateServerQueryMode,

    /// Update the server soft wrap
    UpdateServerSoftWrap,

    /// Add new server or update existing server configuration
    UpdateOrInsertServer,

    /// Fill in key types for unknown keys
    FillKeyTypes,

    /// Load value data for a selected key
    Selectkey,

    /// Delete a key from Redis
    DeleteKey,

    /// Delete a folder from Redis
    DeleteFolder,

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
    /// Remove a value from a set
    RemoveSetValue,

    /// Add a value to a zset
    AddZsetValue,
    /// Remove a value from a zset
    RemoveZsetValue,

    /// Remove a value from a hash
    RemoveHashValue,

    /// Save edited value back to Redis
    SaveValue,
}

impl ServerTask {
    /// Get string representation of task (for logging and error messages)
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerTask::RefreshRedisInfo => "refresh_redis_info",
            ServerTask::SelectServer => "select_server",
            ServerTask::RemoveServer => "remove_server",
            ServerTask::UpdateOrInsertServer => "update_or_insert_server",
            ServerTask::FillKeyTypes => "fill_key_types",
            ServerTask::Selectkey => "select_key",
            ServerTask::DeleteKey => "delete_key",
            ServerTask::DeleteKeys => "delete_keys",
            ServerTask::DeleteFolder => "delete_folder",
            ServerTask::ScanKeys => "scan_keys",
            ServerTask::ScanPrefix => "scan_prefix",
            ServerTask::AddKey => "add_key",
            ServerTask::UpdateKeyTtl => "update_key_ttl",
            ServerTask::RemoveListValue => "remove_list_value",
            ServerTask::UpdateListValue => "update_list_value",
            ServerTask::LoadMoreValue => "load_more_value",
            ServerTask::SaveValue => "save_value",
            ServerTask::UpdateServerQueryMode => "update_server_query_mode",
            ServerTask::UpdateServerSoftWrap => "update_server_soft_wrap",
            ServerTask::PushListValue => "push_list_value",
            ServerTask::AddSetValue => "add_set_value",
            ServerTask::RemoveSetValue => "remove_set_value",
            ServerTask::AddZsetValue => "add_zset_value",
            ServerTask::RemoveZsetValue => "remove_zset_value",
            ServerTask::RemoveHashValue => "remove_hash_value",
        }
    }
}

/// Events emitted by server state for reactive UI updates
#[derive(Debug)]
pub enum ServerEvent {
    /// A new background task has started.
    TaskStarted(ServerTask),
    /// A background task has completed.
    TaskFinished(SharedString),

    /// Terminal toggled
    TerminalToggled(bool),

    /// A key has been selected for viewing/editing
    KeySelected(SharedString),
    /// Key scan operation has started
    KeyScanStarted(SharedString),
    /// Key scan found a new batch of keys.
    KeyScanPaged(SharedString),
    /// Key scan operation has fully completed.
    KeyScanFinished(SharedString),
    /// Key collapse all
    KeyCollapseAll,

    /// A key's value has been fetched (initial load).
    ValueLoaded(SharedString),
    /// A key's value has been updated
    ValueUpdated(SharedString),
    /// A key's value view mode has been updated
    ValueModeViewUpdated(SharedString),
    /// Load more value
    ValuePaginationStarted(SharedString),
    /// Load more value
    ValuePaginationFinished(SharedString),
    /// Add a value to a set、list、hash、zset
    ValueAdded(SharedString),

    /// User selected a different server
    ServerSelected(SharedString, usize),
    /// Server list config has been modified (add/remove/edit).
    ServerListUpdated,
    /// Server metadata (info/dbsize) has been refreshed.
    ServerInfoUpdated(SharedString),
    /// Periodic redis info updated.
    ServerRedisInfoUpdated(SharedString),

    /// Soft wrap changed
    SoftWrapToggled(bool),
    /// An error occurred.
    ErrorOccurred(ErrorMessage),
    /// A notification has been emitted.
    Notification(NotificationAction),

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
}

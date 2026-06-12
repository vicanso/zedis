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

//! Shared "export bytes to a file" flow, reused by every export action
//! (value file export, key-tree CSV, Slow Log CSV/JSON, value-search CSV).

use super::dirs_default_directory;
use crate::states::ZedisServerState;
use gpui::{Context, Entity, SharedString, prelude::*};

/// Prompt for a save path, write `bytes` to it off the UI thread, and emit
/// a success / error notification through the server state.
///
/// `success_title` and `error_label` are passed **pre-resolved** so each
/// caller keeps its own i18n section (the value editor uses `editor.*`,
/// the others `common.*`). The success notification shows the written path
/// as its body and `success_title` as its title (matching
/// `emit_success_notification`'s `(message, title)` order); on failure the
/// message is `"<error_label>: <io error>"`. Cancelling the dialog is a
/// no-op. Fire-and-forget: the task detaches and survives the call.
pub(crate) fn export_to_file<V: 'static>(
    cx: &mut Context<V>,
    server_state: Entity<ZedisServerState>,
    bytes: Vec<u8>,
    suggested_name: &str,
    success_title: SharedString,
    error_label: SharedString,
) {
    let receiver = cx.prompt_for_new_path(&dirs_default_directory(), Some(suggested_name));
    cx.spawn(async move |_view, cx| {
        let Ok(Ok(Some(path))) = receiver.await else {
            return;
        };
        let result = cx
            .background_spawn(async move { std::fs::write(&path, &bytes).map(|_| path) })
            .await;
        server_state.update(cx, |state, cx| match &result {
            Ok(path) => state.emit_success_notification(path.display().to_string().into(), success_title, cx),
            Err(e) => state.emit_error_notification(format!("{error_label}: {e}").into(), cx),
        });
    })
    .detach();
}

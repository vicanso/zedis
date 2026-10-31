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
//! (value file export, key-tree CSV, Slow Log CSV/JSON, value-search CSV),
//! and its other half, "pick a file and read it". Each is a pair: the
//! desktop's panels, and in a page the browser's download and file picker
//! (`helpers::web_files`), so a caller is written once and asks
//! `saves_files()` / `opens_files()` whether to offer itself.

use crate::connection::ConflictMode;
use crate::helpers::{PickFailure, PickedFile};
#[cfg(not(target_family = "wasm"))]
use crate::helpers::{get_download_dir, get_home_dir};
#[cfg(target_family = "wasm")]
use crate::helpers::{media_type_for, web_download, web_upload};
use crate::states::{GlobalEvent, NotificationAction, ZedisGlobalStore, ZedisServerState};
use chrono::Local;
use gpui::{App, Context, Entity, SharedString, Task, Window};
#[cfg(not(target_family = "wasm"))]
use gpui::{PathPromptOptions, prelude::*};
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

/// Where an export's key list came from. Only the caller knows whether the
/// user hand-picked the keys or handed over "whatever the tree has loaded",
/// and the difference decides whether the SCAN-subset caveat is useful
/// information or pure noise.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExportSource {
    /// Keys the user explicitly selected — exactly what they asked for.
    Selection,
    /// Everything loaded under one folder prefix, or the whole tree: a
    /// SCAN-paginated subset of the database.
    Loaded,
}

/// A copy target the export / copy view opens on, when the caller already
/// knows it — the compare view hands over what the target lacks.
#[derive(Clone)]
pub struct CopyPreset {
    pub target_id: SharedString,
    pub target_db: usize,
    pub conflict: ConflictMode,
}

/// Export `keys` of one server and database: the migration window on the
/// desktop — binary, JSON or CSV, to a file or to another server…
#[cfg(not(target_family = "wasm"))]
pub fn open_key_export(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    source: ExportSource,
    _window: &mut Window,
    cx: &mut App,
) {
    super::open_migration_export_window(server_id, server_name, db, keys, source, cx);
}

/// …and in a page, which has neither a second window nor a file to stream
/// into, a dialog that builds the JSON or CSV in memory and downloads it, or
/// copies the keys to another server through the bridge.
#[cfg(target_family = "wasm")]
pub fn open_key_export(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    source: ExportSource,
    window: &mut Window,
    cx: &mut App,
) {
    super::key_export::open_key_export_dialog(server_id, server_name, db, keys, source, window, cx);
}

/// Open the export view already set to copy `keys` to `preset`'s server / db:
/// the migration window on the desktop…
#[cfg(not(target_family = "wasm"))]
pub fn open_key_copy(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    preset: CopyPreset,
    _window: &mut Window,
    cx: &mut App,
) {
    super::open_migration_copy_window(server_id, server_name, db, keys, preset, cx);
}

/// …and in a page, the key export dialog opened on its server-to-server copy
/// destination.
#[cfg(target_family = "wasm")]
pub fn open_key_copy(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    keys: Vec<SharedString>,
    preset: CopyPreset,
    window: &mut Window,
    cx: &mut App,
) {
    super::key_export::open_key_copy_dialog(server_id, server_name, db, keys, preset, window, cx);
}

/// Open the key import view targeting `server_id` / `db`: the migration
/// window on the desktop (`.zedis-dump`, JSON or CSV)…
#[cfg(not(target_family = "wasm"))]
pub fn open_key_import(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    _window: &mut Window,
    cx: &mut App,
) {
    super::open_migration_import_window(server_id, server_name, db, cx);
}

/// …and in a page, a dialog that picks a JSON or CSV export file and writes
/// its entries through the bridge.
#[cfg(target_family = "wasm")]
pub fn open_key_import(
    server_id: SharedString,
    server_name: SharedString,
    db: usize,
    window: &mut Window,
    cx: &mut App,
) {
    super::key_export::open_key_import_dialog(server_id, server_name, db, window, cx);
}

/// Where a save dialog opens: the platform's Downloads directory when that
/// folder exists, else home, else wherever the process is. A configured
/// Downloads path that is not on disk is skipped so the dialog does not
/// open in a missing folder. A page has no save dialog to open
/// (`export_to_file` downloads there).
#[cfg(not(target_family = "wasm"))]
pub(crate) fn dirs_default_directory() -> PathBuf {
    get_download_dir()
        .filter(|p| p.is_dir())
        .or_else(get_home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Prompt for a save path, write `bytes` to it off the UI thread, and emit
/// a success / error notification through the server state.
///
/// `success_title` and `error_label` are passed **pre-resolved** so each
/// caller keeps its own i18n section (the value editor uses `editor.*`,
/// the others `common.*`). The success notification shows the written path
/// as its body and `success_title` as its title (matching
/// `emit_success_notification`'s `(message, title)` order); on failure the
/// message is `"<error_label>: <io error>"`. Cancelling the dialog is a
/// no-op. Fire-and-forget: the task detaches and survives the call. The
/// dialog opens in [`dirs_default_directory`].
#[cfg(not(target_family = "wasm"))]
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

/// [`export_to_file`] in a page: the browser downloads the file, named as
/// the save dialog would have suggested, and the notice names it.
#[cfg(target_family = "wasm")]
pub(crate) fn export_to_file<V: 'static>(
    cx: &mut Context<V>,
    server_state: Entity<ZedisServerState>,
    bytes: Vec<u8>,
    suggested_name: &str,
    success_title: SharedString,
    error_label: SharedString,
) {
    let result = download(suggested_name, &bytes);
    let name = SharedString::from(suggested_name.to_string());
    server_state.update(cx, |state, cx| match result {
        Ok(()) => state.emit_success_notification(name, success_title, cx),
        Err(e) => state.emit_error_notification(format!("{error_label}: {e}").into(), cx),
    });
}

/// Hand the browser `bytes` to download as `name`.
#[cfg(target_family = "wasm")]
fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let download = web_download().ok_or_else(|| "no download in this page".to_string())?;
    download(name, media_type_for(name), bytes)
}

/// Like [`export_to_file`] but for views without a [`ZedisServerState`] (e.g.
/// the server list): notifications are surfaced through the global store
/// instead. Prompts for a save path (see [`dirs_default_directory`]), writes
/// `bytes` off the UI thread, then emits a success / error notification.
/// Cancelling the dialog is a no-op; fire-and-forget.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn export_to_file_global(
    cx: &mut App,
    bytes: Vec<u8>,
    suggested_name: &str,
    success_label: SharedString,
    error_label: SharedString,
) {
    let receiver = cx.prompt_for_new_path(&dirs_default_directory(), Some(suggested_name));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = receiver.await else {
            return;
        };
        let result = cx
            .background_spawn(async move { std::fs::write(&path, &bytes).map(|_| path) })
            .await;
        cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
            store.update(cx, |_state, cx| match &result {
                Ok(path) => cx.emit(GlobalEvent::Notification(NotificationAction::new_success(
                    format!("{success_label}: {}", path.display()).into(),
                ))),
                Err(e) => cx.emit(GlobalEvent::Notification(NotificationAction::new_error(
                    format!("{error_label}: {e}").into(),
                ))),
            });
        });
    })
    .detach();
}

/// [`export_to_file_global`] in a page: a download, and the notice names it.
#[cfg(target_family = "wasm")]
pub(crate) fn export_to_file_global(
    cx: &mut App,
    bytes: Vec<u8>,
    suggested_name: &str,
    success_label: SharedString,
    error_label: SharedString,
) {
    let notification = match download(suggested_name, &bytes) {
        Ok(()) => NotificationAction::new_success(format!("{success_label}: {suggested_name}").into()),
        Err(e) => NotificationAction::new_error(format!("{error_label}: {e}").into()),
    };
    cx.global::<ZedisGlobalStore>()
        .clone()
        .update(cx, |_state, cx| cx.emit(GlobalEvent::Notification(notification)));
}

/// Ask for one file and read it whole — refused before it is read when it
/// is larger than `max_bytes`. `None` when the person cancels. `accept`
/// narrows the browser's picker (`".json,.yaml"`); the desktop's open panel
/// takes no filter, as it never did.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn open_file(cx: &mut App, _accept: &str, max_bytes: u64) -> Task<Option<Result<PickedFile, PickFailure>>> {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return None;
        };
        let path = paths.into_iter().next()?;
        Some(cx.background_spawn(async move { read_picked(&path, max_bytes) }).await)
    })
}

/// A picked path, read whole unless it is over `max_bytes`.
#[cfg(not(target_family = "wasm"))]
fn read_picked(path: &std::path::Path, max_bytes: u64) -> Result<PickedFile, PickFailure> {
    let size = std::fs::metadata(path)
        .map_err(|e| PickFailure::Unreadable(e.to_string()))?
        .len();
    if size > max_bytes {
        return Err(PickFailure::TooLarge { size });
    }
    let bytes = std::fs::read(path).map_err(|e| PickFailure::Unreadable(e.to_string()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PickedFile { name, bytes })
}

/// [`open_file`] in a page: the browser's file picker. A picker the person
/// closes without choosing answers nothing, which reads as cancelled.
#[cfg(target_family = "wasm")]
pub(crate) fn open_file(cx: &mut App, accept: &str, max_bytes: u64) -> Task<Option<Result<PickedFile, PickFailure>>> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    if let Some(upload) = web_upload() {
        upload(
            accept,
            max_bytes,
            Box::new(move |picked| {
                let _ = sender.send(picked);
            }),
        );
    }
    cx.spawn(async move |_| receiver.await.ok())
}

/// Build a unique, filesystem-safe export filename `zedis-<slug>-<ts>.json`.
/// The `YYYYMMDD-HHMMSS` timestamp keeps repeated exports from overwriting
/// each other; `stem` is sanitized to an ASCII-alphanumeric slug.
pub(crate) fn export_filename(stem: &str) -> String {
    export_filename_with(stem, "json")
}

/// [`export_filename`] with another extension: `zedis-<slug>-<ts>.<extension>`.
pub(crate) fn export_filename_with(stem: &str, extension: &str) -> String {
    let slug: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    let ts = Local::now().format("%Y%m%d-%H%M%S");
    if slug.is_empty() {
        format!("zedis-{ts}.{extension}")
    } else {
        format!("zedis-{slug}-{ts}.{extension}")
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    /// A picked file over the caller's limit is refused by its size, before
    /// a byte of it is read; one within it comes back whole, named.
    #[test]
    fn a_picked_file_over_the_limit_is_refused_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("zedis-open-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("value.bin");
        std::fs::write(&path, [7u8; 16]).expect("written");

        assert_eq!(read_picked(&path, 15), Err(PickFailure::TooLarge { size: 16 }));
        assert_eq!(
            read_picked(&path, 16),
            Ok(PickedFile {
                name: "value.bin".to_string(),
                bytes: vec![7; 16],
            })
        );
        assert!(matches!(
            read_picked(&dir.join("missing.bin"), 16),
            Err(PickFailure::Unreadable(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

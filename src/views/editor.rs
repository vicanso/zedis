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

use crate::{
    assets::CustomIconName,
    components::KeyTypeBadge,
    connection::{ConflictMode, RestoreStatus, copy_key, get_connection_manager, get_server, get_servers},
    constants::EDITOR_KEY_BAR_HEIGHT,
    db::get_favorites_manager,
    helpers::{EditorAction, format_duration, get_mono_font_family, humanize_keystroke, unix_ts, validate_ttl},
    states::{
        DataFormat, KeyType, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props,
        escalate_dangerous_body, i18n_bitmap, i18n_common, i18n_copy, i18n_editor, i18n_geo_map,
    },
    views::{
        BitmapEvent, DiffCloseCallback, GeoMapEvent, ZedisBitmapEditor, ZedisBytesEditor, ZedisCopyKeyDialog,
        ZedisGeoMap, ZedisHashEditor, ZedisHllEditor, ZedisListEditor, ZedisProbabilisticEditor, ZedisPubsubEditor,
        ZedisSetEditor, ZedisStreamEditor, ZedisTimeSeriesEditor, ZedisValueDiff, ZedisVectorSetEditor,
        ZedisZsetEditor, bitmap_eligible, export_to_file, looks_like_bitmap, looks_like_hll, zset_looks_geo,
    },
};
use gpui::{ClipboardItem, Entity, PathPromptOptions, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::DropdownMenu,
    notification::Notification,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use rust_i18n::t;
use std::time::{Duration, Instant};
use tracing::{debug, info};
use zedis_ui::ZedisDialog;

// Constants
const RECENTLY_SELECTED_THRESHOLD_MS: u64 = 300;
const TTL_INPUT_MAX_WIDTH: f32 = 120.0;
/// Redis caps a string value at 512 MB; refuse to import anything bigger.
const MAX_IMPORT_VALUE_BYTES: usize = 512 * 1024 * 1024;

/// Active side-by-side diff session — captured snapshot of both panes
/// so the diff view stays stable while the user reads it, even if the
/// underlying history or live value mutates in the background.
///
/// Both sides are stored as raw bytes (not the rendered SharedString)
/// because the diff view re-renders them via the same formatting paths
/// the editor uses, so binary-safe round-trips work for non-UTF8 keys.
#[derive(Clone, Debug)]
pub(crate) struct DiffSession {
    /// History index used to look up the reference version. Kept so the
    /// view can render the same "vN (3 min ago)" label the toolbar
    /// dropdown shows.
    pub history_idx: u32,
    /// Bytes from `value_history_for(key)[history_idx]` — left pane.
    pub reference_bytes: bytes::Bytes,
    /// Unix-seconds capture time of the reference version, for the
    /// "(3 min ago)" relative label.
    pub reference_at: i64,
    /// Bytes from the bytes editor at the moment the diff was opened —
    /// right pane. Snapshotted instead of read live so that an in-flight
    /// reload doesn't yank the pane out from under the user.
    pub current_bytes: bytes::Bytes,
    /// True when the current key was detected as RedisJSON / JSON-format
    /// at session-open. Drives whether we also render the RFC 7396
    /// merge-patch block below the side-by-side panes.
    pub is_json: bool,
    /// Custom left-pane / title label. `None` for a history-version diff
    /// (which labels the reference as "vN (3 min ago)"); `Some` for a
    /// cross-server diff, where it carries the other server's "name / dbN".
    pub reference_label: Option<SharedString>,
}

/// Main editor component for displaying and editing Redis key values
/// Supports different key types (String, List, etc.) with type-specific editors
pub struct ZedisEditor {
    /// Reference to the server state containing Redis connection and data
    server_state: Entity<ZedisServerState>,

    /// Type-specific editors for different Redis data types
    list_editor: Option<Entity<ZedisListEditor>>,
    bytes_editor: Option<Entity<ZedisBytesEditor>>,
    /// Dedicated read-only HyperLogLog card, shown in place of the bytes
    /// editor when a string key's value carries the HLL magic.
    hll_editor: Option<Entity<ZedisHllEditor>>,
    /// Bit-grid viewer for the current string, created lazily when the
    /// string editor enters Bitmap mode.
    bitmap_editor: Option<Entity<ZedisBitmapEditor>>,
    /// Explicit Raw/Bitmap choice for the current string. `None` defers to
    /// the small-binary heuristic; `Some(true/false)` is a user override.
    /// Reset to `None` on key change so each key re-decides.
    bitmap_override: Option<bool>,
    set_editor: Option<Entity<ZedisSetEditor>>,
    zset_editor: Option<Entity<ZedisZsetEditor>>,
    /// Geo "radar" map for the current sorted set, created lazily when
    /// the user flips the ZSET editor into Map mode.
    geo_map: Option<Entity<ZedisGeoMap>>,
    /// Whether the ZSET editor is showing the geo map instead of the table.
    zset_map_mode: bool,
    /// Whether the current sorted set looks like GEO data (probed via
    /// `GEOPOS` on the first members). `None` until probed; the Map toggle
    /// only appears when `Some(true)`.
    zset_is_geo: Option<bool>,
    /// In-flight GEOPOS probe task for the current sorted set.
    geo_probe_task: Option<Task<()>>,
    hash_editor: Option<Entity<ZedisHashEditor>>,
    stream_editor: Option<Entity<ZedisStreamEditor>>,
    pubsub_editor: Option<Entity<ZedisPubsubEditor>>,
    timeseries_editor: Option<Entity<ZedisTimeSeriesEditor>>,
    probabilistic_editor: Option<Entity<ZedisProbabilisticEditor>>,
    vector_set_editor: Option<Entity<ZedisVectorSetEditor>>,

    /// TTL editing state
    should_enter_ttl_edit_mode: Option<bool>,
    ttl_edit_mode: bool,
    ttl_input_state: Entity<InputState>,

    /// Rename dialog input — holds the proposed new key name.
    rename_input_state: Entity<InputState>,
    /// Pending overwrite confirmation: `Some((old, new))` after a RENAMENX
    /// found the destination occupied. Consumed on the next render (which
    /// has a `Window`) to open the confirm dialog.
    pending_overwrite_confirm: Option<(SharedString, SharedString)>,

    /// Track when a key was selected to handle loading states smoothly
    selected_key_at: Option<Instant>,

    readonly: bool,

    auto_refresh_task: Option<Task<()>>,
    auto_refresh_interval_sec: u64,

    /// Active side-by-side diff session, when the user opened the
    /// "Diff vs version" view. `Some` swaps the body region from the
    /// bytes editor to the diff view until the user dismisses it.
    diff_session: Option<DiffSession>,
    /// Cached diff view entity — kept alive while `diff_session` is
    /// `Some` so re-renders don't tear it down. Both are taken in
    /// `close_diff_session` so we don't leak the view past the
    /// session it was built for.
    diff_view: Option<Entity<ZedisValueDiff>>,

    /// Event subscriptions for reactive updates
    _subscriptions: Vec<Subscription>,
}

fn format_ttl_string(ttl: &str) -> String {
    let trimmed = ttl.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    if trimmed.ends_with('.') {
        return format!("{}0s", trimmed);
    }

    let ends_with_digit = trimmed.chars().last().is_some_and(|c| c.is_ascii_digit());

    if ends_with_digit {
        return format!("{}s", trimmed);
    }
    trimmed.to_string()
}

/// File extension suggested when exporting a value, from its detected format.
fn value_export_extension(format: DataFormat) -> &'static str {
    match format {
        DataFormat::Json => "json",
        DataFormat::Svg => "svg",
        DataFormat::Jpeg => "jpg",
        DataFormat::Png => "png",
        DataFormat::Webp => "webp",
        DataFormat::Gif => "gif",
        DataFormat::Gzip => "gz",
        DataFormat::Zstd => "zst",
        DataFormat::Snappy => "snappy",
        DataFormat::MessagePack => "msgpack",
        DataFormat::Protobuf => "pb",
        DataFormat::Text => "txt",
        _ => "bin",
    }
}

/// Sanitized `<key>.<ext>` suggestion for the export save dialog (same
/// character policy as the migration window's dump filenames).
fn suggested_value_filename(key: &str, format: DataFormat) -> String {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.{}", value_export_extension(format))
}

/// Map a copy failure into a user-facing message: a cross-version
/// `DUMP` / `RESTORE` incompatibility (the target is an older Redis than the
/// source) gets the explanatory note; everything else shows the raw error.
fn copy_failure_message(cx: &gpui::App, raw: &str) -> SharedString {
    if raw.contains("version or checksum") || raw.contains("Bad data format") {
        i18n_copy(cx, "version_note")
    } else {
        format!("{}: {raw}", i18n_copy(cx, "failed")).into()
    }
}

/// Parameters for a cross-server key copy, bundled to keep `run_copy`
/// under the argument-count limit.
struct CopyRequest {
    source_id: String,
    source_db: usize,
    target_id: SharedString,
    target_db: usize,
    key: SharedString,
    conflict: ConflictMode,
}

impl ZedisEditor {
    /// Create a new editor instance with event subscriptions
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![];

        // Initialize TTL input field with placeholder
        let ttl_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .validate(|s, _cx| {
                    if s.is_empty() {
                        return true;
                    }
                    validate_ttl(&format_ttl_string(s))
                })
                .clean_on_escape()
                .placeholder(i18n_common(cx, "ttl_placeholder"))
        });

        let rename_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_editor(cx, "rename_placeholder"))
        });

        // Subscribe to server events to track when keys are selected
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::KeySelected(_) => {
                    this.selected_key_at = Some(Instant::now());
                    this.start_auto_refresh(None, cx);
                    // A new key re-decides Raw vs Bitmap via the heuristic,
                    // dropping any override the previous key carried.
                    this.bitmap_override = None;
                    // Auto-close any open diff — the reference bytes
                    // belong to the previous key and would be nonsense
                    // alongside the new key's editor.
                    this.close_diff_session(cx);
                }
                ServerEvent::ValueLoaded => {
                    // stream editor is different of each key, so we need to destroy it
                    this.stream_editor.take();
                    // Same for the time series editor — it snapshots the
                    // key at construction and drives its own per-key
                    // TS.INFO / TS.RANGE, so a fresh load must recreate it.
                    this.timeseries_editor.take();
                    // Likewise the probabilistic (RedisBloom) editor.
                    this.probabilistic_editor.take();
                    // ...and the vector set editor.
                    this.vector_set_editor.take();
                    // The HLL card and bitmap grid likewise snapshot the
                    // key + bytes at construction, so recreate them on a
                    // fresh load (else a key switch shows stale counts/bits).
                    this.hll_editor.take();
                    this.bitmap_editor.take();
                    // The geo map snapshots the key at construction and
                    // self-fetches GEOPOS, so recreate it on a fresh load
                    // and fall back to the table view.
                    this.geo_map.take();
                    this.zset_map_mode = false;
                    // Re-probe whether the freshly loaded value is GEO data
                    // so the Map toggle only shows for geospatial sorted sets.
                    this.zset_is_geo = None;
                    this.probe_zset_geo(cx);
                    // A fresh value load (reload, type change, server
                    // switch) invalidates the diff snapshot.
                    this.close_diff_session(cx);
                }
                ServerEvent::ValueUpdated => {
                    // After a successful Save the right-pane snapshot
                    // is stale (the bytes we captured are now an
                    // ex-version). Dismiss the diff so the user lands
                    // back on the live editor.
                    this.close_diff_session(cx);
                }
                ServerEvent::RenameTargetExists(old, new) => {
                    // RENAMENX hit an existing destination — stash it so the
                    // next render (which has a Window) opens the overwrite
                    // confirm dialog.
                    this.pending_overwrite_confirm = Some((old.clone(), new.clone()));
                    cx.notify();
                }
                ServerEvent::ServerInfoUpdated => {
                    this.readonly = server_state.read(cx).readonly();
                }
                ServerEvent::EditionActionTriggered(action) => match action {
                    EditorAction::UpdateTtl => {
                        this.should_enter_ttl_edit_mode = Some(true);
                        cx.notify();
                    }
                    EditorAction::Reload => {
                        this.reload(cx);
                    }
                    _ => {}
                },
                _ => {}
            }),
        );

        // Subscribe to TTL input events for Enter key and blur
        subscriptions.push(cx.subscribe_in(
            &ttl_input_state,
            window,
            |view, _state, event, window, cx| match &event {
                InputEvent::PressEnter { .. } => {
                    view.handle_update_ttl(window, cx);
                }
                InputEvent::Blur => {
                    view.ttl_edit_mode = false;
                    cx.notify();
                }
                _ => {}
            },
        ));

        let readonly = server_state.read(cx).readonly();
        info!("Creating new editor view");

        Self {
            auto_refresh_task: None,
            auto_refresh_interval_sec: 0,
            server_state,
            list_editor: None,
            bytes_editor: None,
            hll_editor: None,
            bitmap_editor: None,
            bitmap_override: None,
            set_editor: None,
            zset_editor: None,
            geo_map: None,
            zset_map_mode: false,
            zset_is_geo: None,
            geo_probe_task: None,
            hash_editor: None,
            stream_editor: None,
            pubsub_editor: None,
            timeseries_editor: None,
            probabilistic_editor: None,
            vector_set_editor: None,
            readonly,
            ttl_edit_mode: false,
            ttl_input_state,
            rename_input_state,
            pending_overwrite_confirm: None,
            should_enter_ttl_edit_mode: None,
            _subscriptions: subscriptions,
            selected_key_at: None,
            diff_session: None,
            diff_view: None,
        }
    }

    /// Open a diff session against the history version at `idx`. No-op
    /// if the index is out of bounds, the current key has no bytes
    /// editor, or the bytes editor is still loading — better to silently
    /// skip than show an empty diff that confuses the user.
    fn open_diff_session(&mut self, idx: u32, cx: &mut Context<Self>) {
        let Some(bytes_editor) = self.bytes_editor.clone() else {
            return;
        };
        let (reference_bytes, reference_at, is_redis_json) = {
            let server_state = self.server_state.read(cx);
            let Some(key) = server_state.key() else { return };
            let entry = server_state
                .value_history_for(&key)
                .and_then(|history| history.get(idx as usize))
                .cloned();
            let Some(entry) = entry else { return };
            // Detect RedisJSON keys explicitly — for plain String keys
            // whose payload happens to be valid JSON, the diff view
            // re-checks by parsing the bytes itself, so we don't need
            // the bytes-editor's private `is_json_value` flag here.
            let is_redis_json = server_state.value().map(|v| v.is_redis_json()).unwrap_or(false);
            (entry.bytes, entry.at, is_redis_json)
        };

        // Snapshot the current editor bytes — going through
        // `value_bytes_for_save` would also pull pending hex-edit
        // changes, which is what we want for the right pane.
        let current_bytes: bytes::Bytes = bytes_editor.update(cx, |state, cx| {
            match state.value_bytes_for_save(cx) {
                Some(Ok(b)) => bytes::Bytes::from(b),
                // Fall back to the rendered text on hex-parse errors —
                // the diff is still informative, just textually shaped.
                Some(Err(_)) | None => bytes::Bytes::from(state.value(cx).to_string()),
            }
        });

        let session = DiffSession {
            history_idx: idx,
            reference_bytes,
            reference_at,
            current_bytes,
            is_json: is_redis_json,
            reference_label: None,
        };

        // Build the view eagerly so the close-callback can capture a
        // WeakEntity of ZedisEditor (closure must outlive this fn).
        let editor_weak = cx.entity().downgrade();
        let on_close: DiffCloseCallback = std::sync::Arc::new(move |_w, cx| {
            if let Some(editor) = editor_weak.upgrade() {
                editor.update(cx, |this, cx| this.close_diff_session(cx));
            }
        });
        let view = cx.new(|cx| ZedisValueDiff::new(session.clone(), on_close, cx));

        self.diff_session = Some(session);
        self.diff_view = Some(view);
        cx.notify();
    }

    /// Close any active diff session. Safe to call when none is open.
    /// Triggered by the Close button in the diff view, by a save, and
    /// by key / server switches (via the existing subscriptions).
    fn close_diff_session(&mut self, cx: &mut Context<Self>) {
        let had_session = self.diff_session.take().is_some();
        let had_view = self.diff_view.take().is_some();
        if had_session || had_view {
            cx.notify();
        }
    }
    fn start_auto_refresh(&mut self, auto_refresh_interval_sec: Option<u64>, cx: &mut Context<Self>) {
        let auto_refresh_interval_sec = auto_refresh_interval_sec.unwrap_or(0);
        self.auto_refresh_interval_sec = auto_refresh_interval_sec;
        if auto_refresh_interval_sec == 0 {
            self.auto_refresh_task = None;
            return;
        }
        let server_state = self.server_state.clone();
        self.auto_refresh_task = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(auto_refresh_interval_sec))
                    .await;
                server_state.update(cx, move |state, cx| {
                    let key = state.key().unwrap_or_default();
                    if key.is_empty() {
                        return;
                    }
                    info!(key = key.as_str(), "auto refresh value");
                    state.reload_value(key, cx);
                });
            }
        }));
    }

    /// Check if a key was selected recently (within threshold)
    /// Used to prevent showing loading indicator immediately after selection
    fn is_selected_key_recently(&self) -> bool {
        self.selected_key_at
            .map(|t| t.elapsed() < Duration::from_millis(RECENTLY_SELECTED_THRESHOLD_MS))
            .unwrap_or(false)
    }
    /// Handle TTL update when user submits new value
    fn handle_update_ttl(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let key = self.server_state.clone().read(cx).key().unwrap_or_default();
        if key.is_empty() {
            return;
        }

        self.ttl_edit_mode = false;
        let ttl = format_ttl_string(&self.ttl_input_state.read(cx).value());

        self.server_state.update(cx, move |state, cx| {
            state.update_key_ttl(key, ttl.into(), cx);
        });
        cx.notify();
    }

    /// Delete the currently selected key with confirmation dialog
    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };

        let server_state = self.server_state.clone();
        let server_id = self.server_state.read(cx).server_id().to_string();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("editor.delete_key_prompt", key = key, locale = locale).to_string();
        let message = escalate_dangerous_body(cx, &server_id, message);

        ZedisDialog::new_alert(i18n_editor(cx, "delete_key_title"), message)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let key = key.clone();
                server_state.update(cx, move |state, cx| {
                    state.delete_select_key(key, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        self.server_state.update(cx, move |state, cx| {
            state.reload_value(key, cx);
        });
    }

    /// Pull entry `idx` out of the current key's history and push its bytes
    /// into the bytes editor. The user then reviews and Saves as usual —
    /// we don't auto-SET so binary or sensitive rollbacks stay deliberate.
    fn load_history_entry(&mut self, idx: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bytes_editor) = self.bytes_editor.clone() else {
            return;
        };
        let bytes = {
            let server_state = self.server_state.read(cx);
            let Some(key) = server_state.key() else { return };
            server_state
                .value_history_for(&key)
                .and_then(|history| history.get(idx as usize))
                .map(|entry| entry.bytes.clone())
        };
        let Some(bytes) = bytes else { return };
        bytes_editor.update(cx, |state, cx| {
            state.load_bytes_into_editor(bytes, window, cx);
        });
    }
    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // The value editor is no longer `.disabled()` in read-only mode (kept
        // legible), so the read-only lock has to be enforced here instead — this
        // hard-blocks every save path (Save button, cmd-s, …) at the source. The
        // button is disabled, so cmd-s is what reaches here; surface a toast so
        // the otherwise-silent no-op is explained.
        if self.readonly {
            self.server_state.update(cx, |state, cx| {
                state.emit_warning_notification(i18n_common(cx, "disable_in_readonly"), cx);
            });
            return;
        }
        let server_state = self.server_state.read(cx);
        let is_busy = server_state.value().map(|v| v.is_busy()).unwrap_or(false);
        if is_busy {
            return;
        }
        let Some(key) = server_state.key() else {
            return;
        };
        let Some(editor) = self.bytes_editor.as_ref() else {
            return;
        };
        editor.clone().update(cx, move |state, cx| {
            // We only reach here when the server isn't read-only (that's caught
            // above), so `is_readonly()` here means the *value* can't be saved
            // as-is: a binary / truncated / decompressed preview whose editor
            // text isn't the raw stored bytes. Explain that at save time rather
            // than with a banner (editor space is tight).
            if state.is_readonly() {
                self.server_state.update(cx, |s, cx| {
                    s.emit_warning_notification(i18n_editor(cx, "value_readonly"), cx);
                });
                return;
            }
            // Hex view mode round-trips through hex text — decode back to
            // raw bytes here and use the bytes save path so we can write
            // arbitrary binary data, not just UTF-8 strings.
            match state.value_bytes_for_save(cx) {
                Some(Ok(bytes)) => {
                    self.server_state.update(cx, move |state, cx| {
                        state.update_value_bytes(key, bytes, cx);
                    });
                }
                Some(Err(msg)) => {
                    self.server_state.update(cx, |state, cx| {
                        state.emit_error_notification(format!("Hex parse error: {msg}").into(), cx);
                    });
                }
                None => {
                    let value = state.value(cx);
                    self.server_state.update(cx, move |state, cx| {
                        state.update_value(key, value, cx);
                    });
                }
            }
        });
    }
    fn enter_ttl_edit_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let Some(value) = server_state.value() else {
            return;
        };
        let is_busy = value.is_busy();
        if is_busy {
            return;
        }
        let ttl: SharedString = value.ttl().unwrap_or_default().to_string().into();
        self.ttl_edit_mode = true;
        self.ttl_input_state.update(cx, move |state, cx| {
            // Clear value if permanent, otherwise use current TTL
            let value = if humantime::parse_duration(&ttl).is_err() {
                SharedString::default()
            } else {
                ttl.clone()
            };
            state.set_value(value, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }
    /// Save the current string value's raw bytes to a user-chosen file.
    /// Binary-safe: writes exactly what `GET` returned, no decoding.
    fn export_value_to_file(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let Some(key) = state.key() else {
            return;
        };
        let Some(bytes_value) = state.value().and_then(|v| v.bytes_value()) else {
            return;
        };
        let bytes = bytes_value.bytes.to_vec();
        let suggested = suggested_value_filename(key.as_ref(), bytes_value.format);
        let server_state = self.server_state.clone();
        let success = i18n_editor(cx, "value_exported");
        let error = i18n_editor(cx, "export_value_failed");
        export_to_file(cx, server_state, bytes, &suggested, success, error);
    }

    /// Pick a file and overwrite the current string value with its bytes.
    /// Reuses the regular save path (`SET … KEEPTTL` + local write history),
    /// so a failed write rolls back and the change is diffable/undoable.
    fn import_value_from_file(&mut self, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx.background_spawn(async move { std::fs::read(&path) }).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(bytes) => {
                    if bytes.len() > MAX_IMPORT_VALUE_BYTES {
                        this.server_state.update(cx, |state, cx| {
                            state.emit_error_notification(i18n_editor(cx, "import_value_too_large"), cx);
                        });
                        return;
                    }
                    let Some(key) = this.server_state.read(cx).key() else {
                        return;
                    };
                    this.server_state
                        .update(cx, |state, cx| state.update_value_bytes(key, bytes, cx));
                }
                Err(e) => {
                    this.server_state.update(cx, |state, cx| {
                        state.emit_error_notification(
                            format!("{}: {e}", i18n_editor(cx, "import_value_failed")).into(),
                            cx,
                        );
                    });
                }
            });
        })
        .detach();
    }

    /// Open the rename dialog, prefilled with the current key name. OK
    /// fires a `RENAMENX`; a destination collision comes back via
    /// `ServerEvent::RenameTargetExists` and routes to the overwrite confirm.
    fn open_rename_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        self.rename_input_state.update(cx, |state, cx| {
            state.set_value(key.clone(), window, cx);
            state.focus(window, cx);
        });
        let input_child = self.rename_input_state.clone();
        let input_ok = self.rename_input_state.clone();
        let server_state = self.server_state.clone();
        let old = key.clone();
        ZedisDialog::new(i18n_editor(cx, "rename_title"))
            .w(px(420.))
            .ok_text(i18n_common(cx, "confirm"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_common(cx, "confirm"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || Input::new(&input_child))
            .on_ok(move |_, _window, cx| {
                let new = input_ok.read(cx).value().trim().to_string();
                if new.is_empty() || new.as_str() == old.as_ref() {
                    return true;
                }
                let old = old.clone();
                let new: SharedString = new.into();
                server_state.update(cx, move |state, cx| {
                    state.rename_key(old, new, false, cx);
                });
                true
            })
            .open(window, cx);
    }

    /// Confirm dialog shown when a rename would overwrite an existing key;
    /// proceeding issues a clobbering `RENAME`.
    fn open_overwrite_confirm(
        &mut self,
        old: SharedString,
        new: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server_state = self.server_state.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("editor.rename_overwrite_prompt", key = new.as_ref(), locale = locale).to_string();
        ZedisDialog::new_alert(i18n_editor(cx, "rename_overwrite_title"), message)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, window, cx| {
                let (old, new) = (old.clone(), new.clone());
                server_state.update(cx, move |state, cx| {
                    state.rename_key(old, new, true, cx);
                });
                window.close_dialog(cx);
                true
            })
            .open(window, cx);
    }

    /// Open the cross-server "copy to…" dialog for the selected key. On OK
    /// the chosen target server / db (and overwrite flag) drive `run_copy`.
    fn open_copy_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let source_id = state.server_id().to_string();
        let source_db = state.db();
        let Some(key) = state.key() else {
            return;
        };
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.clone().into(), source_db, true, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        let source_id_ok = source_id.clone();
        let key_ok = key.clone();
        ZedisDialog::new(i18n_copy(cx, "title"))
            .w(px(460.))
            .ok_text(i18n_copy(cx, "copy"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_copy(cx, "copy"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                let conflict = view_ok.read(cx).conflict();
                if let Some(editor) = editor.upgrade() {
                    let req = CopyRequest {
                        source_id: source_id_ok.clone(),
                        source_db,
                        target_id,
                        target_db,
                        key: key_ok.clone(),
                        conflict,
                    };
                    editor.update(cx, move |this, cx| this.run_copy(req, cx));
                }
                true
            })
            .open(window, cx);
    }

    /// Open the cross-server diff picker (the copy dialog reused as a pure
    /// server / db picker). On OK, diff this server's value of the key against
    /// the same key on the chosen server.
    fn open_diff_with_server_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        let source_id = state.server_id().to_string();
        let source_db = state.db();
        let Some(key) = state.key() else {
            return;
        };
        if get_servers().map(|s| s.is_empty()).unwrap_or(true) {
            return;
        }
        let view = cx.new(|cx| ZedisCopyKeyDialog::new(source_id.into(), source_db, false, window, cx));
        let view_child = view.clone();
        let view_ok = view.clone();
        let editor = cx.entity().downgrade();
        let key_ok = key.clone();
        ZedisDialog::new(i18n_editor(cx, "diff_with_server"))
            .w(px(460.))
            .ok_text(i18n_editor(cx, "diff_with_server"))
            .cancel_text(i18n_common(cx, "cancel"))
            .button_props(
                dialog_button_props(cx)
                    .ok_text(i18n_editor(cx, "diff_with_server"))
                    .cancel_text(i18n_common(cx, "cancel")),
            )
            .child(move || view_child.clone())
            .on_ok(move |_, _window, cx| {
                let Some(target_id) = view_ok.read(cx).target_server_id() else {
                    return false;
                };
                let target_db = view_ok.read(cx).target_db(cx);
                if let Some(editor) = editor.upgrade() {
                    let key = key_ok.clone();
                    editor.update(cx, move |this, cx| {
                        this.run_cross_server_diff(target_id, target_db, key, cx)
                    });
                }
                true
            })
            .open(window, cx);
    }

    /// Fetch the same key's value from `target_id`/`target_db` and open the
    /// diff view: the other server's value (left) vs this server's (right).
    /// String keys only — non-string keys have no bytes editor to diff.
    fn run_cross_server_diff(
        &mut self,
        target_id: SharedString,
        target_db: usize,
        key: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(bytes_editor) = self.bytes_editor.clone() else {
            self.server_state.update(cx, |s, cx| {
                s.emit_warning_notification(i18n_editor(cx, "diff_string_only"), cx);
            });
            return;
        };
        // Snapshot this server's current bytes for the right pane.
        let current_bytes: bytes::Bytes = bytes_editor.update(cx, |state, cx| match state.value_bytes_for_save(cx) {
            Some(Ok(b)) => bytes::Bytes::from(b),
            Some(Err(_)) | None => bytes::Bytes::from(state.value(cx).to_string()),
        });
        let is_json = self
            .server_state
            .read(cx)
            .value()
            .map(|v| v.is_redis_json())
            .unwrap_or(false);
        let target_name: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        let label: SharedString = format!("{target_name} / db{target_db}").into();
        cx.spawn(async move |this, cx| {
            let fetched = async {
                let client = get_connection_manager().get_client(&target_id, target_db).await?;
                client.get_key_bytes(&key).await
            }
            .await;
            let _ = this.update(cx, move |this, cx| match fetched {
                Ok(other_bytes) => {
                    let session = DiffSession {
                        history_idx: 0,
                        reference_bytes: bytes::Bytes::from(other_bytes),
                        reference_at: 0,
                        current_bytes,
                        is_json,
                        reference_label: Some(label),
                    };
                    let editor_weak = cx.entity().downgrade();
                    let on_close: DiffCloseCallback = std::sync::Arc::new(move |_w, cx| {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.update(cx, |this, cx| this.close_diff_session(cx));
                        }
                    });
                    let view = cx.new(|cx| ZedisValueDiff::new(session.clone(), on_close, cx));
                    this.diff_session = Some(session);
                    this.diff_view = Some(view);
                    cx.notify();
                }
                Err(e) => this.server_state.update(cx, |s, cx| {
                    s.emit_error_notification(format!("{}: {e}", i18n_editor(cx, "diff_with_server")).into(), cx);
                }),
            });
        })
        .detach();
    }

    /// Run the cross-server copy (`DUMP` + `RESTORE`) in the background and
    /// report the outcome via a notification.
    fn run_copy(&mut self, req: CopyRequest, cx: &mut Context<Self>) {
        let CopyRequest {
            source_id,
            source_db,
            target_id,
            target_db,
            key,
            conflict,
        } = req;
        let target_name: SharedString = get_server(&target_id)
            .map(|s| s.name.into())
            .unwrap_or_else(|_| target_id.clone());
        cx.spawn(async move |this, cx| {
            let result = copy_key(source_id, source_db, target_id.to_string(), target_db, key, conflict).await;
            let _ = this.update(cx, move |this, cx| {
                this.server_state.update(cx, |state, cx| match result {
                    Ok(Some(RestoreStatus::Written)) => state.emit_success_notification(
                        format!("{target_name} / db{target_db}").into(),
                        i18n_copy(cx, "done"),
                        cx,
                    ),
                    Ok(Some(RestoreStatus::Skipped)) => state.emit_warning_notification(i18n_copy(cx, "skipped"), cx),
                    Ok(None) => state.emit_warning_notification(i18n_copy(cx, "key_gone"), cx),
                    Ok(Some(RestoreStatus::Failed(msg))) => {
                        state.emit_error_notification(copy_failure_message(cx, &msg), cx)
                    }
                    Err(e) => state.emit_error_notification(copy_failure_message(cx, &e.to_string()), cx),
                });
            });
        })
        .detach();
    }

    /// Render the key information bar with actions (copy, save, TTL, delete)
    fn render_select_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(key) = server_state.key() else {
            return h_flex();
        };

        let mut is_busy = false;
        let mut btns = vec![];
        let mut ttl = SharedString::default();
        let mut size = SharedString::default();
        let mut bitmap_candidate = false;
        let mut bitmap_view = false;
        let mut has_bytes_value = false;

        let mut key_type = KeyType::Unknown;
        // Extract value information if available
        if let Some(value) = server_state.value() {
            is_busy = value.is_busy();
            key_type = value.key_type();

            // Format TTL display
            ttl = if let Some(ttl) = value.ttl() {
                let seconds = ttl.num_seconds();
                if seconds == -2 {
                    i18n_common(cx, "expired")
                } else if seconds < 0 {
                    i18n_common(cx, "permanent")
                } else {
                    format_duration(Duration::from_secs(seconds as u64)).into()
                }
            } else {
                "--".into()
            };

            size = format_size(value.size(), DECIMAL).into();
            // The Bitmap toggle only makes sense for genuinely opaque binary —
            // anything the format pipeline decoded (Protobuf, MessagePack,
            // JSON, timestamps, compressed, images, text) keeps its own viewer,
            // so we require the detected format to be the raw `Bytes` fallback.
            // `infer` can't recognise Protobuf/MessagePack, so the byte
            // heuristic alone would wrongly grab them.
            bitmap_candidate = value.key_type() == KeyType::String
                && value.bytes_value().is_some_and(|b| {
                    matches!(b.format, DataFormat::Bytes)
                        && !looks_like_hll(b.bytes.as_ref())
                        && bitmap_eligible(b.bytes.as_ref())
                });
            bitmap_view = bitmap_candidate
                && self
                    .bitmap_override
                    .unwrap_or_else(|| value.bytes_value().is_some_and(|b| looks_like_bitmap(b.bytes.as_ref())));
            has_bytes_value = value.bytes_value().is_some();
        }

        // Show loading only if busy and not recently selected (avoid flashing)
        let should_show_loading = is_busy && !self.is_selected_key_recently();
        // Size display, rendered just after the key name (per the design): the
        // value alone, prefixed with a lock glyph only when the value is
        // read-only (read-only connection or non-editable binary). Built here,
        // placed in the header row below.
        let size_el = (!size.is_empty()).then(|| {
            let muted = cx.theme().muted_foreground;
            let value_readonly = self.readonly
                || self
                    .bytes_editor
                    .as_ref()
                    .map(|editor| editor.read(cx).is_readonly())
                    .unwrap_or(false);
            let mut row = h_flex().flex_none().items_center().gap_1();
            if value_readonly {
                row = row.child(Icon::new(CustomIconName::Lock).xsmall().text_color(muted));
            }
            row.child(
                Label::new(size)
                    .text_sm()
                    .font_family(get_mono_font_family())
                    .text_color(muted),
            )
            .into_any_element()
        });

        // Add save button for string editor if value is modified
        if let Some(bytes_editor) = &self.bytes_editor {
            let state = bytes_editor.read(cx);
            let value_modified = state.is_value_modified();
            let readonly = state.is_readonly();
            let tooltip = if self.readonly {
                i18n_common(cx, "disable_in_readonly")
            } else if readonly {
                i18n_editor(cx, "can_not_edit_value")
            } else {
                format!(
                    "{} ({})",
                    i18n_editor(cx, "save_data_tooltip"),
                    humanize_keystroke("cmd-s")
                )
                .into()
            };

            btns.push(
                Button::new("zedis-editor-save-key")
                    .disabled(self.readonly || !value_modified || should_show_loading)
                    .primary()
                    .label(i18n_common(cx, "save"))
                    .tooltip(tooltip)
                    .icon(CustomIconName::Save)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.save(window, cx);
                    }))
                    .into_any_element(),
            );
        }

        // Add TTL button (or input field when in edit mode)
        if !ttl.is_empty() {
            let ttl_btn = if self.ttl_edit_mode {
                // Show input field with confirmation button
                Input::new(&self.ttl_input_state)
                    .max_w(px(TTL_INPUT_MAX_WIDTH))
                    .suffix(
                        Button::new("zedis-editor-ttl-update-btn")
                            .icon(Icon::new(IconName::Check))
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                this.handle_update_ttl(window, cx);
                            })),
                    )
                    .into_any_element()
            } else {
                // Show TTL button that switches to edit mode on click
                let ttl_tooltip: SharedString = if self.readonly {
                    i18n_common(cx, "disable_in_readonly")
                } else {
                    format!(
                        "{} ({})",
                        i18n_editor(cx, "update_ttl_tooltip"),
                        humanize_keystroke("cmd-t")
                    )
                    .into()
                };
                Button::new("zedis-editor-ttl-btn")
                    .outline()
                    .font_family(get_mono_font_family())
                    .disabled(self.readonly || should_show_loading)
                    .tooltip(ttl_tooltip)
                    .label(ttl.clone())
                    .icon(CustomIconName::Clock3)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.enter_ttl_edit_mode(window, cx);
                    }))
                    .into_any_element()
            };
            btns.push(ttl_btn);
        }

        let reload_tooltip: SharedString = format!(
            "{} ({})",
            i18n_editor(cx, "reload_key_tooltip"),
            humanize_keystroke("cmd-shift-r")
        )
        .into();
        // reload
        let auto_refresh_interval_sec = self.auto_refresh_interval_sec;
        btns.push(
            DropdownButton::new("zedis-editor-reload-key")
                .button(
                    Button::new("zedis-editor-reload-now")
                        .ghost()
                        .disabled(should_show_loading)
                        .when(auto_refresh_interval_sec > 0, |this| {
                            this.label(format!("{}s", auto_refresh_interval_sec))
                        })
                        .tooltip(reload_tooltip)
                        .icon(CustomIconName::RotateCw)
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.reload(cx);
                        })),
                )
                .dropdown_menu(move |menu, _, cx| {
                    let mut menu = menu;
                    for interval in [0, 1, 5, 10, 30, 60] {
                        let label = if interval == 0 {
                            i18n_editor(cx, "disable_auto_refresh")
                        } else {
                            format!("{}s", interval).into()
                        };
                        menu = menu.menu_element_with_check(
                            auto_refresh_interval_sec == interval,
                            Box::new(EditorAction::AutoRefresh(interval as u32)),
                            move |_, _cx| Label::new(label.clone()),
                        );
                    }
                    menu
                })
                .into_any_element(),
        );

        // Lower-frequency actions live behind one "…" menu so the bar
        // stays compact: bitmap view, value file export / import, diff,
        // delete.
        let bitmap_item = bitmap_candidate && !bitmap_view;
        let export_item = has_bytes_value;
        let diff_with_server_item = has_bytes_value;
        let import_item = has_bytes_value && !self.readonly;
        let rename_item = !self.readonly;
        // Cross-server copy reads the source and writes a (possibly
        // different, writable) target, so it stays available even when the
        // source connection is read-only.
        let copy_item = true;
        let delete_item = !self.readonly;
        // Diff submenu: editable string value with at least one saved
        // version to compare the live value against.
        let diff_editable = self
            .bytes_editor
            .as_ref()
            .map(|e| !e.read(cx).is_readonly())
            .unwrap_or(false)
            && !self.readonly;
        let diff_history: Vec<(i64, usize)> = if diff_editable {
            server_state
                .value_history_for(&key)
                .map(|deque| deque.iter().map(|e| (e.at, e.size())).collect())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let diff_item = !diff_history.is_empty();
        if rename_item || copy_item || bitmap_item || export_item || import_item || diff_item || delete_item {
            btns.push(
                Button::new("zedis-editor-more")
                    .ghost()
                    .disabled(should_show_loading)
                    .tooltip(i18n_editor(cx, "more_actions"))
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, window, cx| {
                        let mut menu = menu;
                        if bitmap_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Binary,
                                Box::new(EditorAction::ViewBitmap),
                                move |_, cx| Label::new(i18n_bitmap(cx, "bitmap")),
                            );
                        }
                        if export_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Download,
                                Box::new(EditorAction::ExportValue),
                                move |_, cx| Label::new(i18n_editor(cx, "export_value_tooltip")),
                            );
                        }
                        if import_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::Upload,
                                Box::new(EditorAction::ImportValue),
                                move |_, cx| Label::new(i18n_editor(cx, "import_value_tooltip")),
                            );
                        }
                        // Restore submenu: pull any saved version back into the
                        // editor (the value-history dropdown, moved off the
                        // toolbar into "more actions"). Same version data as the
                        // diff submenu below, different action.
                        if diff_item {
                            let snap = diff_history.clone();
                            menu = menu.submenu_with_icon(
                                Some(Icon::new(IconName::Undo)),
                                i18n_editor(cx, "history_label"),
                                window,
                                cx,
                                move |submenu, _window, _cx| {
                                    let mut submenu = submenu;
                                    let now = unix_ts();
                                    for (idx, (at, size)) in snap.iter().enumerate() {
                                        let secs_ago = (now - at).max(0) as u64;
                                        let rel = format_duration(Duration::from_secs(secs_ago));
                                        let size_str = format_size(*size as u64, DECIMAL);
                                        let label = format!("v{} • {} • {}", idx + 1, rel, size_str);
                                        let idx_u32 = idx as u32;
                                        submenu = submenu.menu_element(
                                            Box::new(EditorAction::LoadHistory(idx_u32)),
                                            move |_w, _cx| Label::new(label.clone()),
                                        );
                                    }
                                    submenu
                                },
                            );
                        }
                        // Diff submenu: pick any saved version to compare the
                        // live value against (v1 = most recent, the common case).
                        if diff_item {
                            let snap = diff_history.clone();
                            menu = menu.submenu_with_icon(
                                Some(Icon::new(CustomIconName::GitCompareArrows)),
                                i18n_editor(cx, "diff_button"),
                                window,
                                cx,
                                move |submenu, _window, _cx| {
                                    let mut submenu = submenu;
                                    let now = unix_ts();
                                    for (idx, (at, size)) in snap.iter().enumerate() {
                                        let secs_ago = (now - at).max(0) as u64;
                                        let rel = format_duration(Duration::from_secs(secs_ago));
                                        let size_str = format_size(*size as u64, DECIMAL);
                                        let label = format!("v{} • {} • {}", idx + 1, rel, size_str);
                                        let idx_u32 = idx as u32;
                                        submenu = submenu.menu_element(
                                            Box::new(EditorAction::DiffHistory(idx_u32)),
                                            move |_w, _cx| Label::new(label.clone()),
                                        );
                                    }
                                    submenu
                                },
                            );
                        }
                        // Key-level ops (rename / copy / delete) sit below a
                        // separator from the value-view actions above.
                        if (rename_item || copy_item || delete_item || diff_with_server_item)
                            && (bitmap_item || export_item || import_item || diff_item)
                        {
                            menu = menu.separator();
                        }
                        if rename_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::FilePenLine,
                                Box::new(EditorAction::Rename),
                                move |_, cx| Label::new(i18n_editor(cx, "rename")),
                            );
                        }
                        if copy_item {
                            menu = menu.menu_element_with_icon(
                                IconName::Copy,
                                Box::new(EditorAction::CopyTo),
                                move |_, cx| Label::new(i18n_copy(cx, "copy_to")),
                            );
                        }
                        if diff_with_server_item {
                            menu = menu.menu_element_with_icon(
                                CustomIconName::GitCompareArrows,
                                Box::new(EditorAction::DiffWithServer),
                                move |_, cx| Label::new(i18n_editor(cx, "diff_with_server")),
                            );
                        }
                        if delete_item {
                            menu = menu.menu_element_with_icon(
                                IconName::CircleX,
                                Box::new(EditorAction::Delete),
                                move |_, cx| Label::new(i18n_editor(cx, "delete_key_tooltip")),
                            );
                        }
                        menu
                    })
                    .into_any_element(),
            );
        }

        let content = key.clone();
        let server_id = server_state.server_id().to_string();
        let is_favorited = get_favorites_manager()
            .records(&server_id)
            .unwrap_or_default()
            .iter()
            .any(|k| k.as_ref() == key.as_ref());
        let favorite_icon = if is_favorited {
            IconName::StarFill
        } else {
            IconName::Star
        };
        let favorite_tooltip = if is_favorited {
            i18n_editor(cx, "remove_favorite_tooltip")
        } else {
            i18n_editor(cx, "add_favorite_tooltip")
        };
        let favorite_key = key.clone();
        h_flex()
            .px_2()
            .h(EDITOR_KEY_BAR_HEIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .gap_2()
            .w_full()
            .child(
                // Copy + favourite share a tight 2px group so the pair reads as
                // one cluster (matching the design), independent of the
                // toolbar's wider `gap_2` between sections.
                h_flex()
                    .items_center()
                    .gap_0p5()
                    .child(
                        // Copy key button
                        Button::new("zedis-editor-copy-key")
                            .ghost()
                            .tooltip(i18n_editor(cx, "copy_key_tooltip"))
                            .loading(should_show_loading)
                            .icon(IconName::Copy)
                            .on_click(cx.listener(move |_this, _event, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(content.to_string()));
                                window.push_notification(
                                    Notification::info(i18n_editor(cx, "copied_key_to_clipboard")),
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("zedis-editor-favorite-key")
                            .ghost()
                            .tooltip(favorite_tooltip)
                            .icon(favorite_icon)
                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                let server_id = _this.server_state.read(cx).server_id().to_string();
                                let key = favorite_key.clone();
                                let is_favorited = is_favorited;
                                cx.spawn(async move |_, cx| {
                                    let _ = cx
                                        .background_spawn(async move {
                                            let manager = get_favorites_manager();
                                            if is_favorited {
                                                let _ = manager.remove_record(&server_id, key.as_ref());
                                            } else {
                                                let _ = manager.add_record(&server_id, key.as_ref());
                                            }
                                        })
                                        .await;
                                })
                                .detach();
                                cx.notify();
                            })),
                    ),
            )
            .child(KeyTypeBadge::new(key_type).into_any_element())
            .child(
                // Key name — hugs its content and truncates when long (`min_w_0`
                // + ellipsis) instead of growing, so the size can sit right
                // after it; the flex spacer below pushes the actions right.
                div().min_w_0().overflow_hidden().child(
                    Label::new(key)
                        // Monospace so the key reads like the identifier it is.
                        // Bold felt too heavy and Menlo ships no lighter emphasis
                        // face (only Regular/Bold), so we keep the regular weight.
                        .font_family(get_mono_font_family())
                        .text_ellipsis()
                        .whitespace_nowrap(),
                ),
            )
            .children(size_el)
            .child(div().flex_1())
            .children(btns)
    }
    /// Probe whether the current sorted set holds GEO data (via `GEOPOS`
    /// on the first members) so the Map toggle only appears for
    /// geospatial sets. No-op unless the loaded value is a ZSET.
    fn probe_zset_geo(&mut self, cx: &mut Context<Self>) {
        let state = self.server_state.read(cx);
        if state.value().map(|v| v.key_type()) != Some(KeyType::Zset) {
            self.geo_probe_task.take();
            return;
        }
        let server_id = state.server_id().to_string();
        let db = state.db();
        let Some(key) = state.key().map(|k| k.to_string()) else {
            return;
        };
        self.geo_probe_task = Some(cx.spawn(async move |this, cx| {
            let is_geo = zset_looks_geo(server_id, db, key).await;
            let _ = this.update(cx, |this, cx| {
                this.zset_is_geo = Some(is_geo);
                cx.notify();
            });
        }));
    }

    /// Clean up unused editors when switching between key types
    fn reset_editors(&mut self, key_type: KeyType) {
        if key_type != KeyType::String {
            let _ = self.bytes_editor.take();
            let _ = self.hll_editor.take();
            let _ = self.bitmap_editor.take();
            self.bitmap_override = None;
        }
        if key_type != KeyType::List {
            let _ = self.list_editor.take();
        }
        if key_type != KeyType::Set {
            let _ = self.set_editor.take();
        }
        if key_type != KeyType::Zset {
            let _ = self.zset_editor.take();
            let _ = self.geo_map.take();
            self.zset_map_mode = false;
            self.zset_is_geo = None;
            let _ = self.geo_probe_task.take();
        }
        if key_type != KeyType::Hash {
            let _ = self.hash_editor.take();
        }
        if key_type != KeyType::Stream {
            let _ = self.stream_editor.take();
        }
        if key_type != KeyType::Channel {
            let _ = self.pubsub_editor.take();
        }
        if key_type != KeyType::TimeSeries {
            let _ = self.timeseries_editor.take();
        }
        if !matches!(key_type, KeyType::Probabilistic(_)) {
            let _ = self.probabilistic_editor.take();
        }
        if key_type != KeyType::Vectorset {
            let _ = self.vector_set_editor.take();
        }
    }

    /// Render the appropriate editor based on the key type
    /// Inline error panel shown when a value load failed (the key stays
    /// selected). Offers a Retry that re-fetches the current key.
    fn render_load_error(&mut self, message: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let title = i18n_editor(cx, "value_load_failed");
        let retry = i18n_editor(cx, "retry");
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(Label::new(title).text_color(danger))
            .child(Label::new(message).text_sm().text_color(muted))
            .child(
                Button::new("value-reload-retry")
                    .outline()
                    .label(retry)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        let key = this.server_state.read(cx).key();
                        if let Some(key) = key {
                            this.server_state.update(cx, |state, cx| state.reload_value(key, cx));
                        }
                    })),
            )
    }

    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A failed value load keeps the key selected — surface the error
        // inline (with a Retry) instead of a blank/looks-empty sub-editor.
        if let Some(message) = self.server_state.read(cx).value().and_then(|v| v.failure_message()) {
            return self.render_load_error(message, cx).into_any_element();
        }
        let Some(value) = self.server_state.read(cx).value() else {
            self.reset_editors(KeyType::Unknown);
            return div().into_any_element();
        };

        // Don't render anything if key type is unknown and still loading
        if value.key_type == KeyType::Unknown && value.is_busy() {
            return div().into_any_element();
        }

        // HyperLogLog sketches live in string keys; detect them from the
        // already-loaded bytes (the "HYLL" magic) so the dispatch can show
        // the dedicated read-only card instead of the raw bytes editor.
        let is_hll = value.key_type() == KeyType::String
            && value.bytes_value().is_some_and(|b| looks_like_hll(b.bytes.as_ref()));
        // Bitmap view: only for genuinely opaque (`DataFormat::Bytes`) non-HLL
        // string keys — anything the format pipeline decoded (Protobuf,
        // MessagePack, JSON, timestamps, compressed, …) keeps its own viewer.
        // Then the user's explicit override, else the small-binary heuristic.
        let bitmap_view = value.key_type() == KeyType::String
            && !is_hll
            && value.bytes_value().is_some_and(|b| {
                matches!(b.format, DataFormat::Bytes)
                    && self
                        .bitmap_override
                        .unwrap_or_else(|| looks_like_bitmap(b.bytes.as_ref()))
            });

        match value.key_type() {
            KeyType::List => {
                self.reset_editors(KeyType::List);
                let editor = self.list_editor.get_or_insert_with(|| {
                    debug!("Creating new list editor");
                    cx.new(|cx| ZedisListEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Set => {
                self.reset_editors(KeyType::Set);
                let editor = self.set_editor.get_or_insert_with(|| {
                    debug!("Creating new set editor");
                    cx.new(|cx| ZedisSetEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Zset => {
                self.reset_editors(KeyType::Zset);
                // Map mode only when a GEOPOS probe confirmed GEO data.
                let map_mode = self.zset_map_mode && self.zset_is_geo == Some(true);

                if map_mode {
                    let map = self.geo_map.get_or_insert_with(|| {
                        debug!("Creating new geo map");
                        let map = cx.new(|cx| ZedisGeoMap::new(self.server_state.clone(), window, cx));
                        // Switch back to the table when the map's toggle fires.
                        cx.subscribe(&map, |this, _, _event: &GeoMapEvent, cx| {
                            if this.zset_map_mode {
                                this.zset_map_mode = false;
                                cx.notify();
                            }
                        })
                        .detach();
                        map
                    });
                    map.clone().into_any_element()
                } else {
                    let editor = self.zset_editor.get_or_insert_with(|| {
                        debug!("Creating new zset editor");
                        let editor = cx.new(|cx| ZedisZsetEditor::new(self.server_state.clone(), window, cx));
                        // Inject the Table/Map toggle into the table footer, beside
                        // the keyword filter. Self-gates on the GEO probe so it only
                        // appears for geospatial sorted sets.
                        let weak = cx.weak_entity();
                        editor.update(cx, |ze, cx| {
                            ze.set_action_button_factory(
                                Box::new(move |_window, cx| {
                                    let Some(ed) = weak.upgrade() else {
                                        return vec![];
                                    };
                                    if ed.read(cx).zset_is_geo != Some(true) {
                                        return vec![];
                                    }
                                    let w_map = weak.clone();
                                    // Icon-only button matching the footer's
                                    // Add/Bulk style; opens the GEO map view.
                                    vec![
                                        Button::new("zset-view-map")
                                            .icon(IconName::Map)
                                            .tooltip(i18n_geo_map(cx, "map"))
                                            .on_click(move |_, _, cx| {
                                                let _ = w_map.update(cx, |e, cx| {
                                                    if !e.zset_map_mode {
                                                        e.zset_map_mode = true;
                                                        cx.notify();
                                                    }
                                                });
                                            }),
                                    ]
                                }),
                                cx,
                            );
                        });
                        editor
                    });
                    editor.clone().into_any_element()
                }
            }
            KeyType::Hash => {
                self.reset_editors(KeyType::Hash);
                let editor = self.hash_editor.get_or_insert_with(|| {
                    debug!("Creating new hash editor");
                    cx.new(|cx| ZedisHashEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Stream => {
                self.reset_editors(KeyType::Stream);
                let editor = self.stream_editor.get_or_insert_with(|| {
                    debug!("Creating new stream editor");
                    cx.new(|cx| ZedisStreamEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Channel => {
                self.reset_editors(KeyType::Channel);
                let editor = self.pubsub_editor.get_or_insert_with(|| {
                    debug!("Creating new pubsub editor");
                    cx.new(|cx| ZedisPubsubEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::TimeSeries => {
                self.reset_editors(KeyType::TimeSeries);
                let editor = self.timeseries_editor.get_or_insert_with(|| {
                    debug!("Creating new time series editor");
                    cx.new(|cx| ZedisTimeSeriesEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Probabilistic(kind) => {
                self.reset_editors(KeyType::Probabilistic(kind));
                let editor = self.probabilistic_editor.get_or_insert_with(|| {
                    debug!("Creating new probabilistic editor");
                    cx.new(|cx| ZedisProbabilisticEditor::new(self.server_state.clone(), kind, window, cx))
                });
                editor.clone().into_any_element()
            }
            KeyType::Vectorset => {
                self.reset_editors(KeyType::Vectorset);
                let editor = self.vector_set_editor.get_or_insert_with(|| {
                    debug!("Creating new vector set editor");
                    cx.new(|cx| ZedisVectorSetEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
            }
            _ => {
                // Default to bytes editor for String type and other types
                self.reset_editors(KeyType::String);

                // A string key holding an HLL sketch gets the dedicated
                // read-only card instead of the (binary-garbage) bytes view.
                if is_hll {
                    let _ = self.bytes_editor.take();
                    let editor = self.hll_editor.get_or_insert_with(|| {
                        debug!("Creating new HLL editor");
                        cx.new(|cx| ZedisHllEditor::new(self.server_state.clone(), window, cx))
                    });
                    return editor.clone().into_any_element();
                }
                let _ = self.hll_editor.take();

                // Bitmap view: chosen by the heuristic or an explicit toggle
                // (computed above as `bitmap_view`).
                if bitmap_view {
                    let readonly = self.readonly;
                    let editor = self.bitmap_editor.get_or_insert_with(|| {
                        debug!("Creating new bitmap editor");
                        let editor =
                            cx.new(|cx| ZedisBitmapEditor::new(self.server_state.clone(), readonly, window, cx));
                        // The "Raw" toggle pins the raw bytes view for this key.
                        cx.subscribe(&editor, |this, _, _event: &BitmapEvent, cx| {
                            this.bitmap_override = Some(false);
                            cx.notify();
                        })
                        .detach();
                        editor
                    });
                    return editor.clone().into_any_element();
                }
                let _ = self.bitmap_editor.take();

                let editor = self
                    .bytes_editor
                    .get_or_insert_with(|| {
                        debug!("Creating new bytes editor");
                        cx.new(|cx| ZedisBytesEditor::new(self.server_state.clone(), window, cx))
                    })
                    .clone();
                // Swap the bytes editor out for the side-by-side diff
                // view when a session is open. The bytes editor itself
                // stays alive in `self.bytes_editor` so closing the
                // diff returns the user to it without losing pending
                // edits.
                if let Some(diff_view) = self.diff_view.as_ref() {
                    diff_view.clone().into_any_element()
                } else {
                    editor.into_any_element()
                }
            }
        }
    }

    /// Centered empty state for the right panel when no key is selected.
    ///
    /// The editor pane is otherwise a blank rectangle on first connect —
    /// before the user clicks anything in the tree — which reads as dead
    /// space. A calm muted icon + a one-line hint pointing at the key
    /// tree gives the pane a deliberate resting state instead.
    fn render_no_key_selected(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                Icon::new(IconName::Inbox)
                    .with_size(px(44.))
                    .text_color(muted.alpha(0.55)),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_title"))
                    .font_medium()
                    .text_color(cx.theme().foreground),
            )
            .child(
                Label::new(i18n_editor(cx, "no_key_selected_hint"))
                    .text_sm()
                    .text_color(muted),
            )
    }
}

impl Render for ZedisEditor {
    /// Main render method - displays key info bar and appropriate editor
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let is_channel_mode = server_state.is_channel_mode();
        let no_key_selected = !is_channel_mode && server_state.key().is_none();

        // Right after connecting (no key clicked yet) the pane would
        // otherwise be blank — show a centered empty-state hint instead.
        if no_key_selected {
            return self.render_no_key_selected(cx).into_any_element();
        }
        if let Some(true) = self.should_enter_ttl_edit_mode.take() {
            self.enter_ttl_edit_mode(window, cx);
        }
        if let Some((old, new)) = self.pending_overwrite_confirm.take() {
            self.open_overwrite_confirm(old, new, window, cx);
        }

        v_flex()
            .w_full()
            .h_full()
            .when(!is_channel_mode, |this| this.child(self.render_select_key(cx)))
            // The key bar above is fixed-height; the editor body must be a
            // bounded flex item (`flex_1` + `min_h_0`) so children that scroll
            // internally — e.g. the side-by-side diff view's panes — have a
            // definite parent height to shrink against. Without this the diff
            // panes grow to their content height and never show a scrollbar.
            .child(div().flex_1().min_h_0().child(self.render_editor(window, cx)))
            .on_action(cx.listener(move |this, event: &EditorAction, window, cx| match event {
                EditorAction::Save => {
                    this.save(window, cx);
                }
                EditorAction::AutoRefresh(interval) => {
                    this.start_auto_refresh(Some(*interval as u64), cx);
                }
                EditorAction::LoadHistory(idx) => {
                    this.load_history_entry(*idx, window, cx);
                }
                EditorAction::DiffHistory(idx) => {
                    this.open_diff_session(*idx, cx);
                }
                EditorAction::ExportValue => {
                    this.export_value_to_file(cx);
                }
                EditorAction::ImportValue => {
                    this.import_value_from_file(cx);
                }
                EditorAction::ViewBitmap => {
                    this.bitmap_override = Some(true);
                    cx.notify();
                }
                EditorAction::Delete => {
                    this.delete_key(window, cx);
                }
                EditorAction::Rename => {
                    this.open_rename_dialog(window, cx);
                }
                EditorAction::CopyTo => {
                    this.open_copy_dialog(window, cx);
                }
                EditorAction::DiffWithServer => {
                    this.open_diff_with_server_dialog(window, cx);
                }
                _ => {
                    cx.propagate();
                }
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{suggested_value_filename, value_export_extension};
    use crate::states::DataFormat;

    #[test]
    fn export_filename_is_sanitized_with_extension() {
        assert_eq!(
            suggested_value_filename("user:1/avatar", DataFormat::Png),
            "user_1_avatar.png"
        );
        assert_eq!(suggested_value_filename("plain", DataFormat::Json), "plain.json");
    }

    #[test]
    fn export_extension_falls_back_to_bin() {
        assert_eq!(value_export_extension(DataFormat::Bytes), "bin");
        assert_eq!(value_export_extension(DataFormat::Gzip), "gz");
    }
}

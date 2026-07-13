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
        DataFormat, KeyType, MAX_INLINE_VALUE_SIZE, ServerEvent, ZedisGlobalStore, ZedisServerState,
        dialog_button_props, escalate_dangerous_body, i18n_bitmap, i18n_common, i18n_copy, i18n_editor, i18n_geo_map,
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

mod dialogs;
mod render;
mod toolbar;

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
    /// Set by the key tree's right-click "Rename" (the event subscription has
    /// no `Window`); the next render opens the rename dialog and clears it.
    pending_rename_dialog: bool,

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
                    // Read-only toggled — refresh the flag and re-render so the
                    // toolbar (Save / TTL disabled state, the size lock glyph)
                    // reflects it immediately instead of on the next paint. The
                    // bitmap editor caches read-only at construction, so drop it
                    // here to force a rebuild with the new flag on the next
                    // render (the kv-table editors re-read it themselves).
                    this.readonly = server_state.read(cx).readonly();
                    this.bitmap_editor.take();
                    cx.notify();
                }
                ServerEvent::EditionActionTriggered(action) => match action {
                    EditorAction::UpdateTtl => {
                        this.should_enter_ttl_edit_mode = Some(true);
                        cx.notify();
                    }
                    EditorAction::Reload => {
                        this.reload(cx);
                    }
                    // From the key tree's right-click (already gated by
                    // Capability::RenameKey in emit_editor_action). No Window
                    // here — stash and open on the next render.
                    EditorAction::Rename => {
                        this.pending_rename_dialog = true;
                        cx.notify();
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
            pending_rename_dialog: false,
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

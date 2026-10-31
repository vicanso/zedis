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

use crate::views::ZedisPubsubEditor;
use crate::views::placeholders::Placeholders;
// The link panel's Diagnose button opens the desktop's diagnostics window.
use crate::{
    assets::CustomIconName,
    components::KeyTypeBadge,
    connection::HeatMetric,
    connection::{
        ConflictMode, CopyLoss, CopyPreflight, RestoreStatus, ServerDb, copy_key, copy_preflight, floors, get_server,
        get_servers, key_bytes, zset_looks_geo,
    },
    constants::EDITOR_KEY_BAR_HEIGHT,
    db::get_favorites_manager,
    helpers::{
        EditorAction, KeyOpAction, PickFailure, card_background, format_duration, format_duration_units,
        format_unix_secs, get_mono_font_family, hot_key_label, humanize_keystroke, ttl_secs, unix_ts, validate_ttl,
        with_hot_key,
    },
    states::{
        ConnectionErrorKind, DataFormat, HINT_EDITOR_GUIDE, KeyType, LinkProblem, MAX_INLINE_VALUE_SIZE, ServerEvent,
        VALUE_PREVIEW_BYTES, ViewMode, ViewerKind, ZedisGlobalStore, ZedisServerState, dialog_button_props,
        i18n_bitmap, i18n_common, i18n_copy, i18n_editor, i18n_expire_at, i18n_geo_map, i18n_key_ops, i18n_shortcuts,
        i18n_status_bar, update_app_state_and_save_quiet,
    },
    views::{
        BitmapEvent, CopyDialogPurpose, DiffCloseCallback, GeoMapEvent, ZedisBitmapEditor, ZedisBytesEditor,
        ZedisCopyKeyDialog, ZedisExpireAtDialog, ZedisGeoMap, ZedisHashEditor, ZedisHllEditor, ZedisListEditor,
        ZedisProbabilisticEditor, ZedisSetEditor, ZedisStreamEditor, ZedisTimeSeriesEditor, ZedisValueDiff,
        ZedisVectorSetEditor, ZedisZsetEditor, bitmap_eligible, confirm_delete_key, export_to_file,
        json_invalid_message, key_op_title_key, looks_like_bitmap, looks_like_hll, open_change_log_dialog, open_file,
        open_key_op_dialog, open_key_timeline_dialog,
    },
};
use crate::{states::i18n_servers, views::open_connection_diagnostics};
use bytes::Bytes;
use gpui::{
    ClipboardItem, Entity, FocusHandle, Focusable, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt,
    alert::Alert,
    button::{Button, ButtonVariants, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    menu::DropdownMenu,
    notification::Notification,
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use rust_i18n::t;
use std::time::Duration;
use tracing::{debug, info};
use web_time::Instant;
use zedis_core::json::check_json;
use zedis_ui::{ZedisDialog, ZedisSkeletonLoading};

mod dialogs;
mod more_menu;
mod render;
mod toolbar;

// Constants
const RECENTLY_SELECTED_THRESHOLD_MS: u64 = 300;

/// A newly selected key with no payload yet: empty for the 300ms delay so a
/// local GET does not flash grey bars, then the skeleton.
fn pending_load_shows_skeleton(recently_selected: bool) -> bool {
    !recently_selected
}
const TTL_INPUT_MAX_WIDTH: f32 = 120.0;
/// What the empty TTL field shows: a duration as it is typed, in every
/// locale. The sentence around it (`ttl_duration_hint`) does not fit
/// a field this wide and is the field's tooltip instead.
const TTL_DURATION_EXAMPLE: &str = "1h 30m";
/// Redis caps a string value at 512 MB; refuse to import anything bigger.
#[cfg(not(target_family = "wasm"))]
const MAX_IMPORT_VALUE_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(not(target_family = "wasm"))]
const IMPORT_VALUE_TOO_LARGE: &str = "editor.import_value_too_large";
/// A page writes through the bridge, which takes a value only so large in
/// one request (`BRIDGE_MAX_VALUE_BYTES`); a larger file could never be saved.
#[cfg(target_family = "wasm")]
const MAX_IMPORT_VALUE_BYTES: u64 = crate::connection::BRIDGE_MAX_VALUE_BYTES as u64;
#[cfg(target_family = "wasm")]
const IMPORT_VALUE_TOO_LARGE: &str = "editor.import_value_too_large_web";

/// Active side-by-side diff session — captured snapshot of both panes
/// so the diff view stays stable while the user reads it, even if the
/// underlying history or live value mutates in the background.
///
/// Both sides are stored as raw bytes (not the rendered SharedString)
/// because the diff view re-renders them via the same formatting paths
/// the editor uses, so binary-safe round-trips work for non-UTF8 keys.
#[derive(Clone, Debug)]
pub struct DiffSession {
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
    /// The editor's own focus, which stays in the tree on every key: what
    /// the TTL field hands the focus to when it closes with it.
    focus_handle: FocusHandle,

    /// Rename dialog input — holds the proposed new key name.
    rename_input_state: Entity<InputState>,
    /// Re-words the placeholders above after a language change (#198).
    placeholders: Placeholders,
    /// Pending overwrite confirmation: `Some((old, new))` after a RENAMENX
    /// found the destination occupied. Consumed on the next render (which
    /// has a `Window`) to open the confirm dialog.
    pending_overwrite_confirm: Option<(SharedString, SharedString)>,
    /// Pending CAS save conflict: `Some((key, draft))` after a `SET … IFEQ`
    /// was refused. Consumed on the next render to open the overwrite
    /// dialog; `draft` is the exact byte payload the refused save carried.
    pending_save_conflict: Option<(SharedString, Bytes)>,
    /// Set by the key tree's right-click "Rename" (the event subscription has
    /// no `Window`); the next render opens the rename dialog and clears it.
    pending_rename_dialog: bool,

    /// Track when a key was selected to handle loading states smoothly
    selected_key_at: Option<Instant>,
    /// Fires once the 300ms loading delay has elapsed so the skeleton can
    /// appear without waiting for GET. Dropped on `ValueLoaded` or the next
    /// `KeySelected`.
    loading_reveal_task: Option<Task<()>>,

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

/// What the TTL field opens with: the time left in a form the field reads
/// back (`1h 30m 5s`), or nothing for a key without one. `chrono`'s own
/// `Display` is ISO 8601 (`PT5405S`), which the field cannot parse.
///
/// Past a day, humantime spells months (`11months 18days 12hours…`) which
/// this 120px field truncates to `11months 1`. Days and leftover hours
/// (`353d 12h`) still parse and fit.
fn ttl_field_text(ttl: Option<chrono::Duration>) -> SharedString {
    match ttl.and_then(|ttl| u64::try_from(ttl.num_seconds()).ok()) {
        Some(secs) if secs > 0 && secs < 86_400 => {
            humantime::format_duration(Duration::from_secs(secs)).to_string().into()
        }
        Some(secs) if secs > 0 => {
            let days = secs / 86_400;
            let hours = (secs % 86_400) / 3_600;
            if hours == 0 {
                format!("{days}d").into()
            } else {
                format!("{days}d {hours}h").into()
            }
        }
        _ => SharedString::default(),
    }
}

/// The bitmap override a viewer picked on the status bar leaves: Plain or
/// Hex shows the bytes rather than the grid (`Some(false)`, what the grid's
/// ≡ button sets), Auto lets the small-binary heuristic decide again.
fn bitmap_override_for(mode: Option<&ViewMode>) -> Option<bool> {
    match mode {
        Some(ViewMode::Plain | ViewMode::Hex) => Some(false),
        Some(ViewMode::Auto) | None => None,
    }
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
        DataFormat::Text | DataFormat::Base64 | DataFormat::UrlEncoded | DataFormat::PhpSerialized => "txt",
        DataFormat::Jwt => "jwt",
        DataFormat::Bson => "bson",
        DataFormat::Pickle => "pkl",
        DataFormat::JavaSerialized => "ser",
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
#[derive(Clone)]
struct CopyRequest {
    source_id: String,
    source_db: usize,
    target_id: SharedString,
    target_db: usize,
    key: SharedString,
    conflict: ConflictMode,
}

/// Open the connection's entry for editing — the link panel's way to fix a
/// wrong host or password without hunting for the card.
fn edit_connection(server_id: &str, cx: &mut gpui::App) {
    let store = cx.global::<ZedisGlobalStore>().clone();
    store.update(cx, |state, cx| state.edit_server(server_id, cx));
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

        let rename_input_state = cx.new(|cx| InputState::new(window, cx).clean_on_escape());
        let mut placeholders = Placeholders::new(cx);
        placeholders.add(
            &rename_input_state,
            |cx| i18n_editor(cx, "rename_placeholder"),
            window,
            cx,
        );

        // Subscribe to server events to track when keys are selected
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::KeySelected(_) => {
                    this.selected_key_at = Some(Instant::now());
                    this.schedule_loading_reveal(cx);
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
                    this.loading_reveal_task = None;
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
                ServerEvent::ValueModeViewUpdated => {
                    // A viewer picked on the status bar is a request to see
                    // the bytes that way, so Plain or Hex leaves the bitmap
                    // grid the small-binary heuristic opened — as the grid's
                    // own ≡ button does — where it used to change nothing
                    // until that button was found (#224). Auto hands the
                    // choice back to the heuristic.
                    let mode = server_state
                        .read(cx)
                        .value()
                        .and_then(|value| value.bytes_value())
                        .map(|bytes| bytes.view_mode.clone());
                    this.bitmap_override = bitmap_override_for(mode.as_ref());
                    cx.notify();
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
                ServerEvent::ValueSaveConflict { key, draft } => {
                    // The CAS save was refused — same stash-then-render
                    // dance as the rename overwrite; the dialog re-sends
                    // the carried draft, not the (reloaded) editor text.
                    this.pending_save_conflict = Some((key.clone(), draft.clone()));
                    cx.notify();
                }
                // The link went down or came back: the empty state and the
                // link panel trade places.
                ServerEvent::ConnectionHealthChanged => cx.notify(),
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
                    EditorAction::PersistTtl => {
                        this.persist_current_key_ttl(cx);
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
            pending_save_conflict: None,
            ttl_edit_mode: false,
            ttl_input_state,
            focus_handle: cx.focus_handle(),
            rename_input_state,
            placeholders,
            pending_overwrite_confirm: None,
            pending_rename_dialog: false,
            should_enter_ttl_edit_mode: None,
            _subscriptions: subscriptions,
            selected_key_at: None,
            loading_reveal_task: None,
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
                    // Nobody is looking: a background tab, or an app left
                    // unattended. The key tree's refresh, the heartbeat and
                    // the sampling panels wait then too (ADR 5).
                    if state.is_background() {
                        return;
                    }
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

    /// Notify after the 300ms delay so a still-pending first load can show
    /// its skeleton. A faster GET drops this task on `ValueLoaded`.
    fn schedule_loading_reveal(&mut self, cx: &mut Context<Self>) {
        let delay = Duration::from_millis(RECENTLY_SELECTED_THRESHOLD_MS);
        self.loading_reveal_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.loading_reveal_task.take();
                cx.notify();
            });
        }));
    }
    /// Handle TTL update when user submits new value
    fn handle_update_ttl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.server_state.clone().read(cx).key().unwrap_or_default();
        if key.is_empty() {
            return;
        }

        let ttl = format_ttl_string(&self.ttl_input_state.read(cx).value());
        // `0` or `500ms` would reach `EXPIRE key 0`, which deletes the key.
        // Keep the field open with the reason instead of sending that.
        if !ttl.is_empty() && ttl_secs(&ttl).is_none() {
            self.server_state.update(cx, |state, cx| {
                state.emit_error_notification(i18n_common(cx, "ttl_invalid"), cx);
            });
            return;
        }
        self.close_ttl_field(window, cx);

        self.server_state.update(cx, move |state, cx| {
            state.update_key_ttl(key, ttl.into(), cx);
        });
        cx.notify();
    }

    /// Leave TTL edit mode after a TTL was set. The field leaves the tree,
    /// and with it the focus if it held it — after which no shortcut reached
    /// its handler until something was clicked: ⌘N did nothing after a TTL
    /// set with Return. The editor takes the focus then; when the field
    /// closes because the focus went elsewhere (`Blur`), it stays there.
    fn close_ttl_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .ttl_input_state
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            self.focus_handle.focus(window, cx);
        }
        self.ttl_edit_mode = false;
    }

    /// Drop the selected key's expiry (`PERSIST`) from the TTL dropdown.
    fn persist_current_key_ttl(&mut self, cx: &mut Context<Self>) {
        let key = self.server_state.read(cx).key().unwrap_or_default();
        if key.is_empty() {
            return;
        }
        self.ttl_edit_mode = false;
        self.server_state.update(cx, |state, cx| {
            state.persist_key_ttl(key, cx);
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
        let name = key.clone();
        confirm_delete_key(&server_id, &name, window, cx, move |_window, cx| {
            let key = key.clone();
            server_state.update(cx, move |state, cx| {
                state.delete_select_key(key, cx);
            });
        });
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
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        // What the text has to be before it is written: a RedisJSON
        // document must parse; a string that was JSON when it was loaded
        // is asked about, because a string may hold anything.
        let (is_redis_json, is_json_string) = server_state
            .value()
            .map(|value| {
                let json_string = value
                    .bytes_value()
                    .is_some_and(|bytes| bytes.format == DataFormat::Json);
                (value.is_redis_json(), json_string)
            })
            .unwrap_or((false, false));
        let Some(editor) = self.bytes_editor.as_ref() else {
            return;
        };
        editor.clone().update(cx, move |state, cx| {
            // The Hex format's dump is not text to save — ⌘S there would
            // write whatever the hidden editor last held.
            if state.shows_hex_dump() {
                return;
            }
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
                        state.update_value_bytes(key, bytes, false, cx);
                    });
                }
                Some(Err(msg)) => {
                    self.server_state.update(cx, |state, cx| {
                        state.emit_error_notification(format!("Hex parse error: {msg}").into(), cx);
                    });
                }
                None => {
                    let value = state.value(cx);
                    if (is_redis_json || is_json_string)
                        && let Err(error) = check_json(value.as_ref())
                    {
                        let message = json_invalid_message(&error, cx);
                        if is_redis_json {
                            self.server_state
                                .update(cx, |state, cx| state.emit_error_notification(message, cx));
                            return;
                        }
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let body = t!("editor.json_invalid_save_body", error = message, locale = locale).to_string();
                        let server_state = self.server_state.clone();
                        ZedisDialog::new_alert(i18n_editor(cx, "json_invalid_save_title"), body)
                            .button_props(dialog_button_props(cx).ok_text(i18n_editor(cx, "json_invalid_save_anyway")))
                            .on_ok(move |_, window, cx| {
                                let (key, value) = (key.clone(), value.clone());
                                server_state.update(cx, |state, cx| state.update_value(key, value, cx));
                                window.close_dialog(cx);
                                true
                            })
                            .open(window, cx);
                        return;
                    }
                    self.server_state.update(cx, move |state, cx| {
                        state.update_value(key, value, cx);
                    });
                }
            }
        });
    }
    /// Open the inline TTL editor, which always reads a countdown
    /// (`EXPIRE`). An absolute deadline goes through
    /// [`Self::open_expire_at_dialog`] instead — a date and a time do not
    /// fit in a key-bar field, and a calendar beats typing a timestamp.
    fn enter_ttl_edit_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let Some(value) = server_state.value() else {
            return;
        };
        let is_busy = value.is_busy();
        if is_busy {
            return;
        }
        let value = ttl_field_text(value.ttl());
        let placeholder = TTL_DURATION_EXAMPLE;
        self.ttl_edit_mode = true;
        self.ttl_input_state.update(cx, move |state, cx| {
            state.set_placeholder(placeholder, window, cx);
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
        // The key the import was asked for. The picker is not modal on every
        // platform (macOS's panel lets the tree be clicked), and reading the
        // selection when the file came back wrote it to whichever key was
        // open by then — with that key's own value as the CAS baseline, so
        // the write went through.
        let Some(target) = self.server_state.read(cx).key() else {
            return;
        };
        let picked = open_file(cx, "", MAX_IMPORT_VALUE_BYTES);
        cx.spawn(async move |this, cx| {
            let Some(picked) = picked.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| match picked {
                Ok(file) => {
                    if this.server_state.read(cx).key().as_ref() != Some(&target) {
                        this.server_state.update(cx, |state, cx| {
                            state.emit_warning_notification(i18n_editor(cx, "import_value_key_changed"), cx);
                        });
                        return;
                    }
                    // Import-from-file is still a save of this loaded value —
                    // the CAS guard applies the same way as a typed edit.
                    this.server_state
                        .update(cx, |state, cx| state.update_value_bytes(target, file.bytes, false, cx));
                }
                Err(PickFailure::TooLarge { .. }) => {
                    this.server_state.update(cx, |state, cx| {
                        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
                        let limit = format_size(MAX_IMPORT_VALUE_BYTES, DECIMAL);
                        let message = t!(IMPORT_VALUE_TOO_LARGE, limit = limit, locale = locale).to_string();
                        state.emit_error_notification(message.into(), cx);
                    });
                }
                Err(PickFailure::Unreadable(e)) => {
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
            let is_geo = zset_looks_geo(&ServerDb::new(server_id, db), &key).await;
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
    use super::ZedisEditor;
    use super::render::{ACT_HINTS, EMPTY_NEW_KEY_ID, FIND_HINTS};
    use super::{
        bitmap_override_for, format_ttl_string, suggested_value_filename, ttl_field_text, value_export_extension,
    };
    use crate::helpers::hot_key_table;
    use crate::states::ZedisServerState;
    use crate::states::{DataFormat, ViewMode};
    use crate::test_support::{EventLog, init_ui, record_events, take_events};
    use gpui::{AppContext, TestAppContext, px, size};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use std::time::Duration;
    use zedis_core::validate::ttl_secs;

    /// How many times the value was read again, by the editor's own auto
    /// refresh, in the events since `log` was last taken.
    fn reloads(log: &EventLog) -> usize {
        take_events(log)
            .iter()
            .filter(|event| event.as_str() == "TaskStarted(ReloadValue)")
            .count()
    }

    /// A value's auto refresh reads it again on its interval while its tab is
    /// in front, and not while nobody is looking — the key tree's refresh,
    /// the heartbeat and the sampling panels already wait then (ADR 5), and a
    /// server billed per command was still sent a read a second.
    #[gpui::test]
    fn a_values_auto_refresh_waits_while_its_tab_is_in_the_background(cx: &mut TestAppContext) {
        init_ui(cx);
        let server_state = cx.new(|_| ZedisServerState::default());
        server_state.update(cx, |state, cx| {
            state.select("scratch".into(), 0, cx);
            state.select_key("watched".into(), cx);
        });
        let mut editor = None;
        cx.open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx));
            editor = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        let editor = editor.expect("the window built the editor");
        let log = record_events(cx, &server_state);
        editor.update(cx, |editor, cx| editor.start_auto_refresh(Some(1), cx));

        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(reloads(&log), 1, "in front, on its interval");

        server_state.update(cx, |state, _| state.set_background(true));
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(reloads(&log), 0, "not in a background tab");

        server_state.update(cx, |state, _| state.set_background(false));
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(reloads(&log), 1, "and again once it is in front");
    }

    /// Whether the empty editor offers *New key*, with the connection
    /// read-only or not.
    fn offers_new_key(cx: &mut TestAppContext, read_only: bool) -> bool {
        let server_state = cx.new(|_| ZedisServerState::default());
        if read_only {
            server_state.update(cx, |state, cx| state.toggle_readonly(cx));
        }
        let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
            let view = cx.new(|cx| ZedisEditor::new(server_state.clone(), window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find(EMPTY_NEW_KEY_ID).is_some()
        })
        .expect("the window is open")
    }

    /// The empty editor offers *New key* only where a key can be created.
    /// On a read-only connection — a Redis user that may not write, an entry
    /// set read-only, a read-only bridge account — the press did nothing,
    /// while the tree's own button was grey and said why.
    #[gpui::test]
    fn the_empty_editor_offers_new_key_only_where_one_can_be_created(cx: &mut TestAppContext) {
        init_ui(cx);
        assert!(offers_new_key(cx, false), "a connection that may write is offered it");
        assert!(!offers_new_key(cx, true), "a read-only one is not");
    }

    /// A viewer chosen on the status bar decides between the bitmap grid
    /// and the bytes: Plain and Hex leave the grid, Auto gives the choice
    /// back to the heuristic (#224).
    #[test]
    fn a_viewer_picked_on_the_status_bar_leaves_the_bitmap_grid() {
        assert_eq!(bitmap_override_for(Some(&ViewMode::Hex)), Some(false));
        assert_eq!(bitmap_override_for(Some(&ViewMode::Plain)), Some(false));
        assert_eq!(bitmap_override_for(Some(&ViewMode::Auto)), None);
        assert_eq!(bitmap_override_for(None), None);
    }

    /// The no-key screen draws each id's shortcut and, as its description,
    /// the `shortcuts.` entry of the same name — the one the ⌘/ overlay uses.
    #[test]
    fn the_no_key_screen_lists_hot_keys_under_their_own_descriptions() {
        for id in FIND_HINTS.iter().chain(&ACT_HINTS) {
            let description = hot_key_table()
                .iter()
                .find(|hot_key| hot_key.id == *id)
                .and_then(|hot_key| hot_key.reference)
                .map(|(_, description)| description);
            assert_eq!(description, Some(*id), "`{id}` is not a hot key described as itself");
        }
    }

    #[test]
    fn the_ttl_field_opens_with_the_time_left_in_a_form_it_reads_back() {
        let text = ttl_field_text(Some(chrono::Duration::seconds(5405)));
        assert_eq!(text.as_ref(), "1h 30m 5s");
        assert_eq!(ttl_secs(&format_ttl_string(&text)), Some(5405));
        // A year-scale TTL stays compact and parseable — humantime would
        // open as `11months 18days…` and the field would clip it.
        let long = 353 * 86_400 + 12 * 3_600;
        let text = ttl_field_text(Some(chrono::Duration::seconds(long)));
        assert_eq!(text.as_ref(), "353d 12h");
        assert_eq!(ttl_secs(&format_ttl_string(&text)), Some(long as u64));
        // No TTL (-1), a missing key (-2) and none at all open empty.
        assert!(ttl_field_text(Some(chrono::Duration::seconds(-1))).is_empty());
        assert!(ttl_field_text(Some(chrono::Duration::seconds(-2))).is_empty());
        assert!(ttl_field_text(None).is_empty());
    }

    #[test]
    fn pending_load_shows_skeleton_after_the_delay() {
        assert!(!super::pending_load_shows_skeleton(true));
        assert!(super::pending_load_shows_skeleton(false));
    }

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

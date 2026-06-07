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
    constants::EDITOR_KEY_BAR_HEIGHT,
    db::get_favorites_manager,
    helpers::{EditorAction, format_duration, humanize_keystroke, unix_ts, validate_ttl},
    states::{KeyType, ServerEvent, ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_editor},
    views::{
        DiffCloseCallback, ZedisBytesEditor, ZedisHashEditor, ZedisListEditor, ZedisProbabilisticEditor,
        ZedisPubsubEditor, ZedisSetEditor, ZedisStreamEditor, ZedisTimeSeriesEditor, ZedisValueDiff, ZedisZsetEditor,
    },
};
use gpui::{ClipboardItem, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, WindowExt,
    button::{Button, DropdownButton},
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
}

/// Main editor component for displaying and editing Redis key values
/// Supports different key types (String, List, etc.) with type-specific editors
pub struct ZedisEditor {
    /// Reference to the server state containing Redis connection and data
    server_state: Entity<ZedisServerState>,

    /// Type-specific editors for different Redis data types
    list_editor: Option<Entity<ZedisListEditor>>,
    bytes_editor: Option<Entity<ZedisBytesEditor>>,
    set_editor: Option<Entity<ZedisSetEditor>>,
    zset_editor: Option<Entity<ZedisZsetEditor>>,
    hash_editor: Option<Entity<ZedisHashEditor>>,
    stream_editor: Option<Entity<ZedisStreamEditor>>,
    pubsub_editor: Option<Entity<ZedisPubsubEditor>>,
    timeseries_editor: Option<Entity<ZedisTimeSeriesEditor>>,
    probabilistic_editor: Option<Entity<ZedisProbabilisticEditor>>,

    /// TTL editing state
    should_enter_ttl_edit_mode: Option<bool>,
    ttl_edit_mode: bool,
    ttl_input_state: Entity<InputState>,

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

        // Subscribe to server events to track when keys are selected
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::KeySelected(_) => {
                    this.selected_key_at = Some(Instant::now());
                    this.start_auto_refresh(None, cx);
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
            set_editor: None,
            zset_editor: None,
            hash_editor: None,
            stream_editor: None,
            pubsub_editor: None,
            timeseries_editor: None,
            probabilistic_editor: None,
            readonly,
            ttl_edit_mode: false,
            ttl_input_state,
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
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale();
        let message = t!("editor.delete_key_prompt", key = key, locale = locale).to_string();

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

        // Extract value information if available
        if let Some(value) = server_state.value() {
            is_busy = value.is_busy();

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
        }

        // Show loading only if busy and not recently selected (avoid flashing)
        let should_show_loading = is_busy && !self.is_selected_key_recently();
        // Add size label if available
        if !size.is_empty() {
            let size_label = i18n_common(cx, "size");
            btns.push(
                Label::new(format!("{size_label} : {size}"))
                    .text_sm()
                    .into_any_element(),
            );
        }

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
                    .outline()
                    .label(i18n_common(cx, "save"))
                    .tooltip(tooltip)
                    .icon(CustomIconName::FileCheckCorner)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.save(window, cx);
                    }))
                    .into_any_element(),
            );

            // History dropdown: only meaningful for editable string values.
            // Snapshot `(timestamp, size)` tuples here so the menu builder
            // (which is `Fn`, not `FnMut`) can render without re-borrowing
            // server state on each menu open.
            if !readonly && !self.readonly {
                let history_snapshot: Vec<(i64, usize)> = server_state
                    .value_history_for(&key)
                    .map(|deque| deque.iter().map(|e| (e.at, e.size())).collect())
                    .unwrap_or_default();
                let count = history_snapshot.len();
                let label = i18n_editor(cx, "history_label");
                let empty_label = i18n_editor(cx, "history_empty");
                let tooltip = if count == 0 {
                    empty_label.clone()
                } else {
                    format!("{label} ({count})").into()
                };

                // Clone the snapshot before the Restore button takes
                // ownership — the Diff button below needs its own copy
                // to build a parallel dropdown.
                let history_snapshot_for_diff = history_snapshot.clone();

                btns.push(
                    Button::new("zedis-editor-history")
                        .outline()
                        .disabled(count == 0 || should_show_loading)
                        .icon(IconName::Undo)
                        .tooltip(tooltip)
                        .dropdown_menu(move |menu, _window, _cx| {
                            let mut menu = menu;
                            if history_snapshot.is_empty() {
                                return menu;
                            }
                            let now = unix_ts();
                            for (idx, (at, size)) in history_snapshot.iter().enumerate() {
                                let secs_ago = (now - at).max(0) as u64;
                                let rel = format_duration(Duration::from_secs(secs_ago));
                                let size_str = format_size(*size as u64, DECIMAL);
                                let label = format!("{rel} • {size_str}");
                                let idx_u32 = idx as u32;
                                menu = menu
                                    .menu_element(Box::new(EditorAction::LoadHistory(idx_u32)), move |_w, _cx| {
                                        Label::new(label.clone())
                                    });
                            }
                            menu
                        })
                        .into_any_element(),
                );

                // Diff with history version. Split-button: main click
                // diffs against v0 (most recent), dropdown lets the user
                // pick any version. Disabled when there's no history at
                // all — there's nothing to compare against until the
                // user has saved at least once.
                let diff_tooltip: SharedString = if count == 0 {
                    i18n_editor(cx, "diff_no_history")
                } else {
                    i18n_editor(cx, "diff_button_tooltip")
                };
                btns.push(
                    DropdownButton::new("zedis-editor-diff")
                        .button(
                            Button::new("zedis-editor-diff-main")
                                .outline()
                                .disabled(count == 0 || should_show_loading)
                                // No diff-shaped glyph in CustomIconName
                                // yet; the label "Diff" is short and
                                // self-explanatory next to the Undo
                                // restore icon, so we skip the icon
                                // rather than adding a new SVG asset.
                                .label(i18n_editor(cx, "diff_button"))
                                .tooltip(diff_tooltip)
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    // Diff against the most recent
                                    // history entry — the common case.
                                    this.open_diff_session(0, cx);
                                })),
                        )
                        .dropdown_menu(move |menu, _, _cx| {
                            let mut menu = menu;
                            if history_snapshot_for_diff.is_empty() {
                                return menu;
                            }
                            let now = unix_ts();
                            for (idx, (at, size)) in history_snapshot_for_diff.iter().enumerate() {
                                let secs_ago = (now - at).max(0) as u64;
                                let rel = format_duration(Duration::from_secs(secs_ago));
                                let size_str = format_size(*size as u64, DECIMAL);
                                let label = format!("v{} • {} • {}", idx + 1, rel, size_str);
                                let idx_u32 = idx as u32;
                                menu = menu
                                    .menu_element(Box::new(EditorAction::DiffHistory(idx_u32)), move |_w, _cx| {
                                        Label::new(label.clone())
                                    });
                            }
                            menu
                        })
                        .into_any_element(),
                );
            }
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
                    .w(px(TTL_INPUT_MAX_WIDTH))
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
                        .outline()
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

        // Add delete button
        btns.push(
            Button::new("zedis-editor-delete-key")
                .outline()
                .disabled(self.readonly || should_show_loading)
                .tooltip(if self.readonly {
                    i18n_common(cx, "disable_in_readonly")
                } else {
                    i18n_editor(cx, "delete_key_tooltip")
                })
                .icon(IconName::CircleX)
                .on_click(cx.listener(move |this, _event, window, cx| {
                    if is_busy {
                        return;
                    }
                    this.delete_key(window, cx);
                }))
                .into_any_element(),
        );

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
                // Copy key button
                Button::new("zedis-editor-copy-key")
                    .outline()
                    .tooltip(i18n_editor(cx, "copy_key_tooltip"))
                    .loading(should_show_loading)
                    .icon(IconName::Copy)
                    .on_click(cx.listener(move |_this, _event, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(content.to_string()));
                        window.push_notification(Notification::info(i18n_editor(cx, "copied_key_to_clipboard")), cx);
                    })),
            )
            .child(
                Button::new("zedis-editor-favorite-key")
                    .outline()
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
            )
            .child(
                // Key name display - w_0 prevents long keys from breaking layout
                div()
                    .flex_1()
                    .w_0()
                    .overflow_hidden()
                    .child(Label::new(key).text_ellipsis().whitespace_nowrap()),
            )
            .children(btns)
    }
    /// Clean up unused editors when switching between key types
    fn reset_editors(&mut self, key_type: KeyType) {
        if key_type != KeyType::String {
            let _ = self.bytes_editor.take();
        }
        if key_type != KeyType::List {
            let _ = self.list_editor.take();
        }
        if key_type != KeyType::Set {
            let _ = self.set_editor.take();
        }
        if key_type != KeyType::Zset {
            let _ = self.zset_editor.take();
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
    }

    /// Render the appropriate editor based on the key type
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(value) = self.server_state.read(cx).value() else {
            self.reset_editors(KeyType::Unknown);
            return div().into_any_element();
        };

        // Don't render anything if key type is unknown and still loading
        if value.key_type == KeyType::Unknown && value.is_busy() {
            return div().into_any_element();
        }

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
                let editor = self.zset_editor.get_or_insert_with(|| {
                    debug!("Creating new zset editor");
                    cx.new(|cx| ZedisZsetEditor::new(self.server_state.clone(), window, cx))
                });
                editor.clone().into_any_element()
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
            _ => {
                // Default to bytes editor for String type and other types
                self.reset_editors(KeyType::String);

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
}

impl Render for ZedisEditor {
    /// Main render method - displays key info bar and appropriate editor
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let is_channel_mode = server_state.is_channel_mode();

        // Don't render anything if no key is selected
        if !is_channel_mode && server_state.key().is_none() {
            return v_flex().into_any_element();
        }
        if let Some(true) = self.should_enter_ttl_edit_mode.take() {
            self.enter_ttl_edit_mode(window, cx);
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
                _ => {
                    cx.propagate();
                }
            }))
            .into_any_element()
    }
}

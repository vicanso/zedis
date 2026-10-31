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

//! Key Timeline / Watch dialog (`open_key_timeline_dialog`).
//!
//! Watches a single Redis key in real time using both:
//! 1. Redis Keyspace Notifications (`__keyspace@<db>__:<key>`) for immediate
//!    event-driven snapshots (`set`, `hset`, `expire`, `del`, `expired`, etc.)
//!    when `notify-keyspace-events` includes `K`.
//! 2. Periodic change-detection polling (`pacing::KEY_TIMELINE_POLL_INTERVAL`)
//!    so value mutations and expirations are still captured automatically on
//!    servers where `notify-keyspace-events` is disabled or restricted.
//!
//! Keeps a bounded in-memory ring buffer (`MAX_SNAPSHOTS = 50`) and renders a
//! unified line diff between any selected snapshot and its predecessor.

use crate::assets::CustomIconName;
use crate::connection::{
    ChannelSubscription, KeyTimelineProbe, ServerDb, SubscribeKind, capture_key_timeline_snapshot, config_get_one,
};
use crate::error::Error;
use crate::helpers::{
    DiffOp, channel, get_mono_font_family, line_diff, now_clock, pacing::KEY_TIMELINE_POLL_INTERVAL, saves_files,
};
use crate::states::{ZedisGlobalStore, ZedisServerState, dialog_button_props, i18n_common, i18n_editor};
use crate::views::export_to_file;
use gpui::{App, ClipboardItem, Entity, Hsla, SharedString, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    v_flex,
};
use humansize::{DECIMAL, format_size};
use rust_i18n::t;
use std::collections::VecDeque;
use zedis_ui::ZedisDialog;

const DIALOG_WIDTH: f32 = 900.0;
const BODY_HEIGHT: f32 = 430.0;
const SIDEBAR_WIDTH: f32 = 270.0;
const MAX_SNAPSHOTS: usize = 50;
const CONTEXT_LINES: usize = 3;

pub const KEY_TIMELINE_PAUSE_BTN_ID: &str = "key-timeline-pause-btn";
pub const KEY_TIMELINE_SNAP_BTN_ID: &str = "key-timeline-snap-btn";
pub const KEY_TIMELINE_COPY_BTN_ID: &str = "key-timeline-copy-btn";
pub const KEY_TIMELINE_EXPORT_BTN_ID: &str = "key-timeline-export-btn";

#[derive(Clone, Debug)]
pub struct KeyTimelineSnapshot {
    pub id: usize,
    pub timestamp: SharedString,
    pub event: SharedString,
    pub exists: bool,
    pub key_type: SharedString,
    pub ttl_secs: i64,
    pub snapshot_text: String,
}

impl KeyTimelineSnapshot {
    fn ttl_badge(&self) -> String {
        if !self.exists || self.ttl_secs == -2 {
            "expired".to_string()
        } else if self.ttl_secs == -1 {
            "no TTL".to_string()
        } else {
            format!("TTL {}s", self.ttl_secs)
        }
    }

    fn size_badge(&self) -> String {
        if !self.exists {
            "0 B".to_string()
        } else {
            format_size(self.snapshot_text.len() as u64, DECIMAL)
        }
    }
}

/// Whether a periodic poll probe represents a meaningful change compared to
/// the most recent snapshot (ignoring normal countdown ticks of a positive TTL).
pub fn is_meaningful_poll_change(prev: &KeyTimelineSnapshot, probe: &KeyTimelineProbe) -> bool {
    if prev.exists != probe.exists {
        return true;
    }
    if prev.key_type.as_ref() != probe.key_type {
        return true;
    }
    if prev.snapshot_text != probe.snapshot_text {
        return true;
    }
    // Detect when a key switches between permanent (-1) and having an expiry (>= 0).
    let prev_has_ttl = prev.ttl_secs >= 0;
    let curr_has_ttl = probe.ttl_secs >= 0;
    prev_has_ttl != curr_has_ttl
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    Folded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Builds a compact unified diff with `CONTEXT_LINES` of surrounding context
/// around each change and folded markers for longer unchanged runs.
pub fn build_compact_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    if old_text == new_text {
        return Vec::new();
    }
    let old_lines: Vec<&str> = old_text.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new_text.split_inclusive('\n').collect();
    let ops = line_diff(old_text, new_text);

    let mut raw: Vec<(DiffLineKind, String)> = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            DiffOp::Equal(l, _) => {
                let line = old_lines.get(l).copied().unwrap_or("").trim_end_matches(['\r', '\n']);
                raw.push((DiffLineKind::Context, format!("  {line}")));
            }
            DiffOp::Delete(l) => {
                let line = old_lines.get(l).copied().unwrap_or("").trim_end_matches(['\r', '\n']);
                raw.push((DiffLineKind::Removed, format!("- {line}")));
            }
            DiffOp::Insert(r) => {
                let line = new_lines.get(r).copied().unwrap_or("").trim_end_matches(['\r', '\n']);
                raw.push((DiffLineKind::Added, format!("+ {line}")));
            }
        }
    }

    // Mark lines within CONTEXT_LINES of any Added/Removed line.
    let n = raw.len();
    let mut keep = vec![false; n];
    for (idx, (kind, _)) in raw.iter().enumerate() {
        if *kind != DiffLineKind::Context {
            let start = idx.saturating_sub(CONTEXT_LINES);
            let end = (idx + CONTEXT_LINES + 1).min(n);
            for flag in &mut keep[start..end] {
                *flag = true;
            }
        }
    }

    let mut out = Vec::new();
    let mut idx = 0;
    while idx < n {
        if keep[idx] {
            let (kind, text) = raw[idx].clone();
            out.push(DiffLine { kind, text });
            idx += 1;
        } else {
            let start = idx;
            while idx < n && !keep[idx] {
                idx += 1;
            }
            let skipped = idx - start;
            out.push(DiffLine {
                kind: DiffLineKind::Folded,
                text: format!("@@ … {skipped} unchanged lines … @@"),
            });
        }
    }
    out
}

pub struct ZedisKeyTimelineDialog {
    server_state: Entity<ZedisServerState>,
    server_id: String,
    db: usize,
    key: SharedString,
    snapshots: VecDeque<KeyTimelineSnapshot>,
    selected_id: Option<usize>,
    next_id: usize,
    paused: bool,
    push_connected: bool,
    notify_flags: SharedString,
    capturing: bool,
    show_raw: bool,
    pending_notification: Option<Notification>,
    _tasks: Vec<Task<()>>,
}

struct TimelineDialogActions {
    dialog: Entity<ZedisKeyTimelineDialog>,
}

impl Render for TimelineDialogActions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.dialog.read(cx);
        let paused = state.paused;
        let capturing = state.capturing;
        let has_snapshots = !state.snapshots.is_empty();
        let dialog_for_pause = self.dialog.clone();
        let dialog_for_snap = self.dialog.clone();
        let dialog_for_copy = self.dialog.clone();

        let pause_label = if paused {
            i18n_editor(cx, "key_timeline_resume")
        } else {
            i18n_editor(cx, "key_timeline_pause")
        };

        h_flex()
            .items_center()
            .gap_2()
            .child(
                Button::new(KEY_TIMELINE_PAUSE_BTN_ID)
                    .outline()
                    .small()
                    .label(pause_label)
                    .on_click(move |_, _, cx| {
                        dialog_for_pause.update(cx, |this, cx| {
                            this.paused = !this.paused;
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new(KEY_TIMELINE_SNAP_BTN_ID)
                    .outline()
                    .small()
                    .icon(CustomIconName::RotateCw)
                    .loading(capturing)
                    .disabled(capturing)
                    .label(i18n_editor(cx, "key_timeline_snapshot_now"))
                    .on_click(move |_, _, cx| {
                        dialog_for_snap.update(cx, |this, cx| {
                            this.trigger_snapshot("manual", false, cx);
                        });
                    }),
            )
            .child(
                Button::new(KEY_TIMELINE_COPY_BTN_ID)
                    .outline()
                    .small()
                    .icon(IconName::Copy)
                    .disabled(!has_snapshots)
                    .label(i18n_editor(cx, "key_timeline_copy_md"))
                    .on_click(move |_, window, cx| {
                        let md = dialog_for_copy.read(cx).to_markdown();
                        cx.write_to_clipboard(ClipboardItem::new_string(md));
                        window.push_notification(Notification::info(i18n_common(cx, "copied_to_clipboard")), cx);
                    }),
            )
            .when(saves_files(), |el| {
                el.child(
                    Button::new(KEY_TIMELINE_EXPORT_BTN_ID)
                        .outline()
                        .small()
                        .icon(Icon::new(CustomIconName::Download))
                        .disabled(!has_snapshots)
                        .label(i18n_editor(cx, "key_timeline_export_md"))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            let (md, server_state) = {
                                let d = this.dialog.read(cx);
                                (d.to_markdown(), d.server_state.clone())
                            };
                            let success = i18n_common(cx, "csv_exported");
                            let error = i18n_common(cx, "csv_export_failed");
                            export_to_file(cx, server_state, md.into_bytes(), "key-timeline.md", success, error);
                        })),
                )
            })
    }
}

impl ZedisKeyTimelineDialog {
    pub fn new(server_state: Entity<ZedisServerState>, key: SharedString, cx: &mut Context<Self>) -> Self {
        let state = server_state.read(cx);
        let server_id = state.server_id().to_string();
        let db = state.db();

        let mut this = Self {
            server_state,
            server_id,
            db,
            key,
            snapshots: VecDeque::new(),
            selected_id: None,
            next_id: 1,
            paused: false,
            push_connected: false,
            notify_flags: SharedString::default(),
            capturing: false,
            show_raw: false,
            pending_notification: None,
            _tasks: Vec::new(),
        };

        this.fetch_notify_flags(cx);
        this.trigger_snapshot("initial", false, cx);
        this.start_keyspace_subscription(cx);
        this.start_poll_loop(cx);
        this
    }

    fn fetch_notify_flags(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let at = ServerDb::new(self.server_id.clone(), self.db);
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_handle, cx| {
            let res: Result<Option<String>, Error> = cx
                .background_spawn(async move { Ok(config_get_one(&at, "notify-keyspace-events").await?) })
                .await;
            if let Ok(Some(flags)) = res {
                let _ = entity.update(cx, |this, cx| {
                    this.notify_flags = flags.into();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn start_keyspace_subscription(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let at = ServerDb::new(self.server_id.clone(), self.db);
        let channel_pattern = format!("__keyspace@{}__:{}", self.db, self.key);
        let entity = cx.entity().downgrade();

        let task = cx.spawn(async move |_handle, cx| {
            let connect: Result<ChannelSubscription, Error> = cx
                .background_spawn(async move {
                    let patterns = [channel_pattern.as_str()];
                    Ok(ChannelSubscription::open(&at, SubscribeKind::KeyspaceEvents, &patterns).await?)
                })
                .await;

            let Ok(mut pubsub) = connect else {
                return;
            };

            if entity
                .update(cx, |this, cx| {
                    this.push_connected = true;
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let (tx, rx) = channel::unbounded::<String>();
            let reader = cx.background_spawn(async move {
                while let Some(msg) = pubsub.next_message().await {
                    let event_name = String::from_utf8_lossy(&msg.payload).into_owned();
                    if tx.send(event_name).await.is_err() {
                        break;
                    }
                }
            });

            while let Ok(mut last_event) = rx.recv().await {
                // Coalesce burst events that arrived simultaneously on the same key.
                while let Ok(next_event) = rx.try_recv() {
                    last_event = next_event;
                }
                let should_continue = entity
                    .update(cx, |this, cx| {
                        if !this.paused {
                            this.trigger_snapshot(&last_event, false, cx);
                        }
                    })
                    .is_ok();
                if !should_continue {
                    break;
                }
            }

            drop(reader);
            let _ = entity.update(cx, |this, cx| {
                this.push_connected = false;
                cx.notify();
            });
        });

        self._tasks.push(task);
    }

    fn start_poll_loop(&mut self, cx: &mut Context<Self>) {
        if self.server_id.is_empty() {
            return;
        }
        let entity = cx.entity().downgrade();
        let task = cx.spawn(async move |_handle, cx| {
            loop {
                cx.background_executor().timer(KEY_TIMELINE_POLL_INTERVAL).await;
                let ok = entity
                    .update(cx, |this, cx| {
                        if !this.paused && !this.capturing {
                            this.trigger_snapshot("poll", true, cx);
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        });
        self._tasks.push(task);
    }

    fn trigger_snapshot(&mut self, event: &str, only_if_changed: bool, cx: &mut Context<Self>) {
        if self.server_id.is_empty() || self.capturing {
            return;
        }
        self.capturing = true;
        cx.notify();

        let at = ServerDb::new(self.server_id.clone(), self.db);
        let key = self.key.to_string();
        let event_name: SharedString = event.to_string().into();
        let entity = cx.entity().downgrade();

        cx.spawn(async move |_handle, cx| {
            let probe_res: Result<KeyTimelineProbe, Error> = cx
                .background_spawn(async move { Ok(capture_key_timeline_snapshot(&at, &key).await?) })
                .await;

            let _ = entity.update(cx, |this, cx| {
                this.capturing = false;
                match probe_res {
                    Ok(probe) => {
                        this.ingest_probe(event_name, probe, only_if_changed);
                    }
                    Err(e) => {
                        if !only_if_changed {
                            this.pending_notification = Some(Notification::error(e.to_string()));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ingest_probe(&mut self, event: SharedString, probe: KeyTimelineProbe, only_if_changed: bool) {
        if let Some(latest) = self.snapshots.front_mut() {
            if only_if_changed && !is_meaningful_poll_change(latest, &probe) {
                return;
            }
            // If a push event arrives right after a poll already captured the exact same state,
            // upgrade that poll entry's event label instead of duplicating the snapshot.
            if !only_if_changed
                && event.as_ref() != "manual"
                && event.as_ref() != "initial"
                && latest.event.as_ref() == "poll"
                && !is_meaningful_poll_change(latest, &probe)
            {
                latest.event = event;
                latest.ttl_secs = probe.ttl_secs;
                return;
            }
        }

        let was_viewing_latest =
            self.selected_id.is_none() || self.snapshots.front().is_some_and(|s| Some(s.id) == self.selected_id);

        let id = self.next_id;
        self.next_id += 1;

        let snap = KeyTimelineSnapshot {
            id,
            timestamp: now_clock(false).into(),
            event,
            exists: probe.exists,
            key_type: probe.key_type.into(),
            ttl_secs: probe.ttl_secs,
            snapshot_text: probe.snapshot_text,
        };

        self.snapshots.push_front(snap);
        while self.snapshots.len() > MAX_SNAPSHOTS {
            self.snapshots.pop_back();
        }

        if was_viewing_latest || !self.snapshots.iter().any(|s| Some(s.id) == self.selected_id) {
            self.selected_id = Some(id);
        }
    }

    fn selected_and_previous(&self) -> Option<(&KeyTimelineSnapshot, Option<&KeyTimelineSnapshot>)> {
        let selected_idx = self
            .selected_id
            .and_then(|id| self.snapshots.iter().position(|s| s.id == id))
            .unwrap_or(0);
        let current = self.snapshots.get(selected_idx)?;
        // Snapshots are stored newest-first, so the chronological predecessor is at `selected_idx + 1`.
        let previous = self.snapshots.get(selected_idx + 1);
        Some((current, previous))
    }

    pub fn to_markdown(&self) -> String {
        let mut out = format!("# Key Timeline: `{}` (db {})\n\n", self.key, self.db);
        for (idx, snap) in self.snapshots.iter().enumerate() {
            out.push_str(&format!(
                "## #{} · `{}` at {} ({}, {}, {})\n\n",
                snap.id,
                snap.event,
                snap.timestamp,
                snap.key_type,
                snap.size_badge(),
                snap.ttl_badge()
            ));
            if let Some(prev) = self.snapshots.get(idx + 1) {
                let diff = build_compact_diff(&prev.snapshot_text, &snap.snapshot_text);
                if !diff.is_empty() {
                    out.push_str("```diff\n");
                    for line in diff {
                        out.push_str(&line.text);
                        out.push('\n');
                    }
                    out.push_str("```\n\n");
                    continue;
                }
            }
            out.push_str("```text\n");
            out.push_str(&snap.snapshot_text);
            if !snap.snapshot_text.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
        out
    }

    fn event_color(&self, event: &str, cx: &App) -> Hsla {
        let theme = cx.theme();
        match event {
            "del" | "expired" | "evicted" => theme.red,
            "set" | "hset" | "sadd" | "zadd" | "lpush" | "rpush" | "xadd" => theme.green,
            "expire" | "pexpire" | "persist" => theme.yellow,
            "initial" | "manual" => theme.primary,
            _ => theme.muted_foreground,
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let selected_id = self.selected_id.or_else(|| self.snapshots.front().map(|s| s.id));
        let mono = get_mono_font_family();

        let items: Vec<_> = self
            .snapshots
            .iter()
            .map(|snap| {
                let id = snap.id;
                let is_selected = Some(id) == selected_id;
                let event_color = self.event_color(&snap.event, cx);
                let row_bg = if is_selected {
                    theme.list_active
                } else {
                    theme.background
                };
                let meta = format!("{} · {} · {}", snap.key_type, snap.size_badge(), snap.ttl_badge());

                div()
                    .id(("timeline-snap-row", id))
                    .w_full()
                    .px_2p5()
                    .py_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .bg(row_bg)
                    .border_1()
                    .border_color(if is_selected { theme.primary } else { border })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_id = Some(id);
                        cx.notify();
                    }))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        h_flex()
                                            .gap_1p5()
                                            .items_center()
                                            .child(
                                                Label::new(format!("#{id}"))
                                                    .font_family(mono.clone())
                                                    .text_xs()
                                                    .text_color(muted),
                                            )
                                            .child(
                                                Label::new(snap.event.clone())
                                                    .font_family(mono.clone())
                                                    .text_xs()
                                                    .text_color(event_color),
                                            ),
                                    )
                                    .child(
                                        Label::new(snap.timestamp.clone())
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .text_color(muted),
                                    ),
                            )
                            .child(
                                Label::new(meta)
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .text_color(muted)
                                    .text_ellipsis(),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .pr_2()
            .border_r_1()
            .border_color(border)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_1p5()
                    .children(items),
            )
    }

    fn render_detail_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let mono = get_mono_font_family();

        let Some((curr, prev_opt)) = self.selected_and_previous() else {
            return v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .child(
                    Label::new(i18n_editor(cx, "key_timeline_empty"))
                        .text_sm()
                        .text_color(muted),
                )
                .into_any_element();
        };

        let header_text = match prev_opt {
            Some(prev) if !self.show_raw => format!(
                "#{} ({} @ {})  vs  #{} ({} @ {})",
                curr.id, curr.event, curr.timestamp, prev.id, prev.event, prev.timestamp
            ),
            _ => format!(
                "#{} ({} @ {}) · {} · {} · {}",
                curr.id,
                curr.event,
                curr.timestamp,
                curr.key_type,
                curr.size_badge(),
                curr.ttl_badge()
            ),
        };

        let content_el = if !curr.exists {
            v_flex()
                .p_3()
                .gap_1()
                .child(
                    Label::new(i18n_editor(cx, "key_timeline_key_absent"))
                        .text_sm()
                        .text_color(theme.red),
                )
                .into_any_element()
        } else if self.show_raw || prev_opt.is_none() {
            v_flex()
                .p_3()
                .gap_0p5()
                .children(curr.snapshot_text.lines().map(|line| {
                    Label::new(line.to_string())
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(theme.foreground)
                        .into_any_element()
                }))
                .into_any_element()
        } else if let Some(prev) = prev_opt {
            let diff_lines = build_compact_diff(&prev.snapshot_text, &curr.snapshot_text);
            if diff_lines.is_empty() {
                let ttl_note = if prev.ttl_secs != curr.ttl_secs {
                    format!("TTL: {} → {}", prev.ttl_badge(), curr.ttl_badge())
                } else {
                    i18n_editor(cx, "key_timeline_no_value_diff").to_string()
                };
                v_flex()
                    .p_3()
                    .gap_2()
                    .child(
                        Label::new(ttl_note)
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(theme.yellow),
                    )
                    .children(curr.snapshot_text.lines().take(40).map(|line| {
                        Label::new(format!("  {line}"))
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(muted)
                            .into_any_element()
                    }))
                    .into_any_element()
            } else {
                v_flex()
                    .p_3()
                    .gap_0p5()
                    .children(diff_lines.into_iter().map(|line| {
                        let color = match line.kind {
                            DiffLineKind::Added => theme.green,
                            DiffLineKind::Removed => theme.red,
                            DiffLineKind::Folded => theme.primary,
                            DiffLineKind::Context => theme.foreground,
                        };
                        Label::new(line.text)
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(color)
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded(px(4.))
                    .bg(theme.secondary)
                    .child(
                        Label::new(header_text)
                            .font_family(mono)
                            .text_xs()
                            .text_color(muted)
                            .text_ellipsis(),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(border)
                    .overflow_y_scrollbar()
                    .child(content_el),
            )
            .into_any_element()
    }
}

impl Render for ZedisKeyTimelineDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let has_keyspace_flag = self.notify_flags.contains('K') || self.notify_flags.contains('A');
        let mode_text = if self.paused {
            i18n_editor(cx, "key_timeline_status_paused")
        } else if self.push_connected && has_keyspace_flag {
            i18n_editor(cx, "key_timeline_status_push_poll")
        } else {
            i18n_editor(cx, "key_timeline_status_poll_only")
        };
        let status_color = if self.paused { theme.yellow } else { theme.green };

        let count_label = t!(
            "editor.key_timeline_count",
            count = self.snapshots.len(),
            locale = locale
        )
        .to_string();

        let show_raw = self.show_raw;

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(Label::new(mode_text).text_xs().text_color(status_color))
                            .child(Label::new("·").text_xs().text_color(muted))
                            .child(Label::new(count_label).text_xs().text_color(muted)),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Button::new("key-timeline-mode-diff")
                                    .xsmall()
                                    .when(!show_raw, |b| b.primary())
                                    .when(show_raw, |b| b.ghost())
                                    .label(i18n_editor(cx, "key_timeline_view_diff"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_raw = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("key-timeline-mode-raw")
                                    .xsmall()
                                    .when(show_raw, |b| b.primary())
                                    .when(!show_raw, |b| b.ghost())
                                    .label(i18n_editor(cx, "key_timeline_view_snapshot"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_raw = true;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .h(px(BODY_HEIGHT))
                    .gap_3()
                    .child(self.render_sidebar(cx))
                    .child(self.render_detail_pane(cx)),
            )
    }
}

pub fn open_key_timeline_dialog(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut App) {
    let Some(key) = server_state.read(cx).key() else {
        return;
    };
    let title = format!("{} — {key}", i18n_editor(cx, "key_timeline_title"));
    let view = cx.new(|cx| ZedisKeyTimelineDialog::new(server_state, key, cx));
    let actions = cx.new(|_| TimelineDialogActions { dialog: view.clone() });
    let close = i18n_common(cx, "close");

    ZedisDialog::new(title)
        .w(px(DIALOG_WIDTH))
        .ok_text(close.clone())
        .button_props(dialog_button_props(cx).ok_text(close))
        .footer_child(move || actions.clone())
        .child(move || view.clone())
        .on_ok(|_, _, _| true)
        .open(window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_change_ignores_positive_ttl_countdown_and_detects_value_or_expiry_switch() {
        let base = KeyTimelineSnapshot {
            id: 1,
            timestamp: "12:00:00".into(),
            event: "initial".into(),
            exists: true,
            key_type: "string".into(),
            ttl_secs: 120,
            snapshot_text: "{\"count\": 1}".to_string(),
        };

        // Normal TTL countdown from 120s to 118s with identical value is not a mutation.
        let same_value_tick = KeyTimelineProbe {
            exists: true,
            key_type: "string".to_string(),
            ttl_secs: 118,
            snapshot_text: "{\"count\": 1}".to_string(),
        };
        assert!(!is_meaningful_poll_change(&base, &same_value_tick));

        // Value mutation is detected.
        let mutated_value = KeyTimelineProbe {
            exists: true,
            key_type: "string".to_string(),
            ttl_secs: 118,
            snapshot_text: "{\"count\": 2}".to_string(),
        };
        assert!(is_meaningful_poll_change(&base, &mutated_value));

        // PERSIST (TTL switched from >=0 to -1) is detected.
        let persisted = KeyTimelineProbe {
            exists: true,
            key_type: "string".to_string(),
            ttl_secs: -1,
            snapshot_text: "{\"count\": 1}".to_string(),
        };
        assert!(is_meaningful_poll_change(&base, &persisted));
    }

    #[test]
    fn compact_diff_highlights_added_and_removed_lines() {
        let old_text = "a: 1\nb: 2\nc: 3\n";
        let new_text = "a: 1\nb: 20\nc: 3\n";
        let diff = build_compact_diff(old_text, new_text);
        assert!(
            diff.iter()
                .any(|l| l.kind == DiffLineKind::Removed && l.text == "- b: 2")
        );
        assert!(
            diff.iter()
                .any(|l| l.kind == DiffLineKind::Added && l.text == "+ b: 20")
        );
    }
}

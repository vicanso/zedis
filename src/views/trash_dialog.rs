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

//! Recycle-bin dialog for soft-deleted keys.
//!
//! Opened from the status bar's Tools menu. Lists the local trash entries
//! of the active server (all databases) with per-row Restore / Remove.
//! Restore replays the stored `DUMP` payload via `RESTORE`, carrying the
//! TTL the key had when it was deleted; a key that meanwhile exists again
//! on the server fails with a BUSYKEY-specific message.

use crate::connection::get_connection_manager;
use crate::db::{TRASH_RETENTION_MS, TrashMeta, get_trash_entry, list_trash_meta, purge_trash, remove_trash_entry};
use crate::error::Error;
use crate::helpers::{get_mono_font_family, unix_ts_millis};
use crate::states::{GlobalEvent, NotificationAction, ZedisGlobalStore, i18n_common, i18n_trash};
use chrono::{Local, LocalResult, TimeZone};
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    v_flex,
};
use redis::cmd;
use rust_i18n::t;
use zedis_ui::ZedisDialog;

pub struct ZedisTrashDialog {
    server_id: String,
    entries: Vec<TrashMeta>,
    loading: bool,
    /// True while a batch restore runs (disables the restore-all button).
    restoring: bool,
    /// Substring filter over the listed keys.
    filter_state: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

fn format_deleted_at(ts_ms: i64) -> SharedString {
    match Local.timestamp_millis_opt(ts_ms) {
        LocalResult::Single(dt) => dt.format("%m-%d %H:%M").to_string().into(),
        _ => "--".into(),
    }
}

fn emit_notification(notification: NotificationAction, cx: &mut App) {
    cx.update_global::<ZedisGlobalStore, ()>(|store, cx| {
        store.update(cx, |_state, cx| {
            cx.emit(GlobalEvent::Notification(notification));
        });
    });
}

impl ZedisTrashDialog {
    pub fn new(server_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_trash(cx, "filter_placeholder"))
        });
        // Live filtering: re-render on every keystroke.
        let subscription = cx.subscribe(&filter_state, |_this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut this = Self {
            server_id,
            entries: vec![],
            loading: true,
            restoring: false,
            filter_state,
            _subscriptions: vec![subscription],
        };
        this.reload(cx);
        this
    }

    /// Entries matching the current substring filter (all when empty).
    fn filtered_entries(&self, cx: &Context<Self>) -> Vec<TrashMeta> {
        let keyword = self.filter_state.read(cx).value().to_string();
        self.entries
            .iter()
            .filter(|e| keyword.is_empty() || e.key.contains(keyword.as_str()))
            .cloned()
            .collect()
    }

    /// Restore every currently-listed (filtered) entry in one background
    /// task: RESTORE with the stored TTL, drop the trash row on success,
    /// then a single aggregated notification + one reload. A key that
    /// meanwhile exists again (BUSYKEY) is counted as skipped, not failed.
    fn restore_all(&mut self, cx: &mut Context<Self>) {
        if self.restoring {
            return;
        }
        let ids: Vec<String> = self.filtered_entries(cx).iter().map(|e| e.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        self.restoring = true;
        let server_id = self.server_id.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        cx.spawn(async move |this, cx| {
            let sid = server_id.clone();
            let (restored, skipped, failed) = cx
                .background_spawn(async move {
                    let mut restored = 0usize;
                    let mut skipped = 0usize;
                    let mut failed = 0usize;
                    for id in ids {
                        let Ok(Some(entry)) = get_trash_entry(&sid, &id) else {
                            failed += 1;
                            continue;
                        };
                        let result = async {
                            let mut conn = get_connection_manager().get_connection(&sid, entry.db).await?;
                            let _: () = cmd("RESTORE")
                                .arg(entry.key.as_str())
                                .arg(entry.pttl_ms.max(0))
                                .arg(entry.payload.as_slice())
                                .query_async(&mut conn)
                                .await?;
                            Ok::<(), Error>(())
                        }
                        .await;
                        match result {
                            Ok(()) => {
                                let _ = remove_trash_entry(&sid, &id);
                                restored += 1;
                            }
                            Err(e) if e.to_string().contains("BUSYKEY") => skipped += 1,
                            Err(_) => failed += 1,
                        }
                    }
                    (restored, skipped, failed)
                })
                .await;
            let msg = t!(
                "trash.restore_all_result",
                restored = restored,
                skipped = skipped,
                failed = failed,
                locale = &locale
            );
            let notification = if failed == 0 {
                NotificationAction::new_success(msg.into())
            } else {
                NotificationAction::new_error(msg.into())
            };
            cx.update(|cx| emit_notification(notification, cx));
            let _ = this.update(cx, |state, cx| {
                state.restoring = false;
                state.reload(cx);
            });
        })
        .detach();
    }

    /// Refresh the listing off the UI thread; expired rows are purged
    /// first so nothing unrestorable is ever shown.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        let server_id = self.server_id.clone();
        cx.spawn(async move |this, cx| {
            let entries = cx
                .background_spawn(async move {
                    let _ = purge_trash(&server_id, unix_ts_millis() - TRASH_RETENTION_MS);
                    list_trash_meta(&server_id).unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.entries = entries;
                state.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn restore(&mut self, id: String, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        cx.spawn(async move |this, cx| {
            let sid = server_id.clone();
            let row_id = id.clone();
            let entry = cx.background_spawn(async move { get_trash_entry(&sid, &row_id) }).await;
            let notification = match entry {
                Ok(Some(entry)) => {
                    let restored = async {
                        let mut conn = get_connection_manager().get_connection(&server_id, entry.db).await?;
                        // RESTORE ttl is milliseconds; 0 = no expiry. A key
                        // deleted without TTL reports PTTL -1 → clamp to 0.
                        let _: () = cmd("RESTORE")
                            .arg(entry.key.as_str())
                            .arg(entry.pttl_ms.max(0))
                            .arg(entry.payload.as_slice())
                            .query_async(&mut conn)
                            .await?;
                        Ok::<(), Error>(())
                    }
                    .await;
                    match restored {
                        Ok(()) => {
                            let sid = server_id.clone();
                            let row_id = id.clone();
                            let _ = cx
                                .background_spawn(async move { remove_trash_entry(&sid, &row_id) })
                                .await;
                            let msg = t!("trash.restored", key = entry.key, locale = &locale);
                            NotificationAction::new_success(msg.into())
                        }
                        Err(e) => {
                            let message = e.to_string();
                            let msg = if message.contains("BUSYKEY") {
                                t!("trash.restore_exists", key = entry.key, locale = &locale)
                            } else {
                                t!("trash.restore_failed", error = message, locale = &locale)
                            };
                            NotificationAction::new_error(msg.into())
                        }
                    }
                }
                Ok(None) => {
                    let msg = t!("trash.restore_failed", error = "entry not found", locale = &locale);
                    NotificationAction::new_error(msg.into())
                }
                Err(e) => {
                    let msg = t!("trash.restore_failed", error = e.to_string(), locale = &locale);
                    NotificationAction::new_error(msg.into())
                }
            };
            cx.update(|cx| emit_notification(notification, cx));
            let _ = this.update(cx, |state, cx| state.reload(cx));
        })
        .detach();
    }

    /// Drop one entry from the bin permanently.
    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        cx.spawn(async move |this, cx| {
            let _ = cx
                .background_spawn(async move { remove_trash_entry(&server_id, &id) })
                .await;
            let _ = this.update(cx, |state, cx| state.reload(cx));
        })
        .detach();
    }
}

impl Render for ZedisTrashDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let entries = self.filtered_entries(cx);
        let mut list = v_flex().w_full().gap_2();
        if self.loading {
            list = list.child(Label::new(i18n_common(cx, "loading")).text_sm().text_color(muted));
        } else if entries.is_empty() {
            list = list.child(Label::new(i18n_trash(cx, "empty")).text_sm().text_color(muted));
        }
        for (index, entry) in entries.iter().enumerate() {
            let restore_id = entry.id.clone();
            let remove_id = entry.id.clone();
            list = list.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                Label::new(entry.key.clone())
                                    .text_sm()
                                    .font_family(get_mono_font_family()),
                            )
                            .child({
                                let mut meta = format!("DB {} · {}", entry.db, format_deleted_at(entry.deleted_at_ms));
                                // Restore keeps the TTL the key had at deletion.
                                if entry.pttl_ms > 0 {
                                    meta.push_str(&format!(" · TTL {}s", entry.pttl_ms / 1000));
                                }
                                Label::new(meta).text_xs().text_color(muted)
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("trash-restore-{index}")))
                            .xsmall()
                            .outline()
                            .label(i18n_trash(cx, "restore"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.restore(restore_id.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("trash-remove-{index}")))
                            .xsmall()
                            .ghost()
                            .label(i18n_trash(cx, "remove"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.remove(remove_id.clone(), cx);
                            })),
                    ),
            );
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(Label::new(i18n_trash(cx, "note")).text_xs().text_color(muted))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.filter_state).flex_1().cleanable(true).small())
                    .child(
                        Button::new("trash-restore-all")
                            .xsmall()
                            .outline()
                            .label(i18n_trash(cx, "restore_all"))
                            .loading(self.restoring)
                            .disabled(self.restoring || entries.is_empty())
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.restore_all(cx);
                            })),
                    ),
            )
            // Not `max_h`: `Scrollable` keeps a copy of the caller's size styles
            // on the inner content and its forced `h_auto` doesn't reset `max_h`,
            // so the content itself gets clamped and never scrolls (same pitfall
            // as the update dialog's release notes). Fixed-height viewport only
            // when the list is long enough to need one; short lists stay inline.
            .child(if entries.len() > 7 {
                div()
                    .w_full()
                    .h(px(360.))
                    .child(list)
                    .overflow_y_scrollbar()
                    .into_any_element()
            } else {
                div().w_full().child(list).into_any_element()
            })
    }
}

/// Open the recycle bin for the active server; no-op when nothing is
/// connected (the Tools menu is only reachable with a live connection).
pub fn open_trash_dialog(window: &mut Window, cx: &mut App) {
    let Some((server_id, _db)) = cx.global::<ZedisGlobalStore>().read(cx).selected_server().cloned() else {
        return;
    };
    let view = cx.new(|cx| ZedisTrashDialog::new(server_id, window, cx));
    let view_child = view.clone();
    ZedisDialog::new(i18n_trash(cx, "title"))
        .w(px(560.))
        .ok_text(i18n_common(cx, "confirm"))
        .child(move || view_child.clone())
        .open(window, cx);
}

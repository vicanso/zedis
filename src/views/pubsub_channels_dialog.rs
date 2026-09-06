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

//! The Pub/Sub channel browser: a dialog over the Pub/Sub editor listing
//! the channels that have a subscriber right now — `PUBSUB CHANNELS` +
//! `NUMSUB`, or the `SHARD…` pair in sharded mode — with a glob filter
//! and Refresh, so a channel can be picked instead of typed. A row's
//! action hands the name back to the editor, which subscribes to it. The
//! list is a snapshot: Redis forgets a channel the moment its last
//! subscriber leaves, which is what the empty state explains.
//!
//! The body is a view entity (an `Input` inside a rebuilt element tree is
//! the dialog gotcha in CLAUDE.md), opened through
//! [`open_pubsub_channels_dialog`].

use crate::assets::CustomIconName;
use crate::connection::{MAX_PUBSUB_CHANNELS, PubsubChannelsSnapshot, get_connection_manager};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisGlobalStore, ZedisServerState, i18n_common, i18n_pubsub_editor};
use gpui::{App, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{DataTable, TableState},
    v_flex,
};
use rust_i18n::t;
use std::rc::Rc;
use tracing::error;
use zedis_ui::{CellAction, TextColumn, ZedisDialog, ZedisTextTable};

/// What the editor does with a picked channel name.
pub type ChannelPick = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

const DIALOG_WIDTH: f32 = 600.;
const SUBSCRIBERS_WIDTH: f32 = 130.;
/// The dialog's paddings and the cell's hover buttons, taken off the
/// channel column.
const CHANNEL_WIDTH: f32 = DIALOG_WIDTH - SUBSCRIBERS_WIDTH - 70.;
/// A definite height — `Scrollable` + `max_h` clips instead of scrolling
/// (CLAUDE.md).
const LIST_HEIGHT: f32 = 340.;

/// The listing's headline numbers, kept for the summary line.
#[derive(Clone, Copy)]
struct Summary {
    channels: usize,
    pattern_subscriptions: Option<u64>,
    nodes: usize,
    truncated: bool,
}

impl From<&PubsubChannelsSnapshot> for Summary {
    fn from(snapshot: &PubsubChannelsSnapshot) -> Self {
        Self {
            channels: snapshot.channels.len(),
            pattern_subscriptions: snapshot.pattern_subscriptions,
            nodes: snapshot.nodes,
            truncated: snapshot.truncated,
        }
    }
}

pub struct ZedisPubsubChannelsDialog {
    server_state: Entity<ZedisServerState>,
    /// Shard channels (`SHARDCHANNELS` / `SHARDNUMSUB`) instead of the
    /// classic ones — the editor's Sharded toggle at open time.
    sharded: bool,
    pattern_state: Entity<InputState>,
    table_state: Entity<TableState<ZedisTextTable>>,
    loading: bool,
    /// `None` until the first listing came back: the body shows "Loading…"
    /// rather than an empty state that is not yet true.
    summary: Option<Summary>,
    error: Option<SharedString>,
    fetch_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisPubsubChannelsDialog {
    pub fn new(
        server_state: Entity<ZedisServerState>,
        sharded: bool,
        on_pick: ChannelPick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // No `clean_on_escape`: Escape belongs to the dialog.
        let pattern_state = cx.new(|cx| {
            let input = InputState::new(window, cx).placeholder(i18n_pubsub_editor(cx, "channels_pattern_placeholder"));
            input.focus(window, cx);
            input
        });
        let subscription = cx.subscribe_in(&pattern_state, window, |view, _state, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                view.fetch(cx);
            }
        });

        let subscribe_tooltip = i18n_pubsub_editor(cx, "channels_subscribe_tooltip");
        let columns = vec![
            TextColumn::new("channel", i18n_pubsub_editor(cx, "channel"), CHANNEL_WIDTH),
            TextColumn::new(
                "subscribers",
                i18n_pubsub_editor(cx, "channels_subscribers"),
                SUBSCRIBERS_WIDTH,
            )
            .sortable()
            .numeric(),
        ];
        let table = ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
            .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
            .cell_action(Rc::new(move |column, cells| {
                if column != 0 {
                    return None;
                }
                let channel = cells.first()?.clone();
                let on_pick = on_pick.clone();
                Some(CellAction {
                    icon: IconName::Play,
                    tooltip: subscribe_tooltip.clone(),
                    on_click: Rc::new(move |window, cx| on_pick(channel.clone(), window, cx)),
                })
            }));
        let table_state = cx.new(|cx| TableState::new(table, window, cx));

        let mut this = Self {
            server_state,
            sharded,
            pattern_state,
            table_state,
            loading: false,
            summary: None,
            error: None,
            fetch_task: None,
            _subscriptions: vec![subscription],
        };
        this.fetch(cx);
        this
    }

    /// Lists the channels matching the pattern field (all when empty),
    /// replacing the table. One listing in flight at a time — a new one
    /// drops the previous task.
    fn fetch(&mut self, cx: &mut Context<Self>) {
        let server_id = self.server_state.read(cx).server_id().to_string();
        if server_id.is_empty() {
            return;
        }
        let db = self.server_state.read(cx).db();
        let pattern = self.pattern_state.read(cx).value().trim().to_string();
        let sharded = self.sharded;
        self.loading = true;
        self.error = None;
        cx.notify();

        self.fetch_task = Some(cx.spawn(async move |handle, cx| {
            let result: Result<PubsubChannelsSnapshot, Error> = cx
                .background_spawn(async move {
                    let client = get_connection_manager().get_client(&server_id, db).await?;
                    Ok(client.pubsub_channels(&pattern, sharded).await?)
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(snapshot) => {
                        this.summary = Some(Summary::from(&snapshot));
                        let rows: Vec<Vec<SharedString>> = snapshot
                            .channels
                            .into_iter()
                            .map(|channel| vec![channel.name.into(), channel.subscribers.to_string().into()])
                            .collect();
                        this.table_state
                            .update(cx, |state, _| state.delegate_mut().set_rows(rows));
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to list pub/sub channels");
                        // A NOPERM / unknown-subcommand reply degrades the
                        // feature matrix (and explains itself once); anything
                        // else is shown inline.
                        let explained = this
                            .server_state
                            .update(cx, |state, cx| state.note_command_error(&e, cx));
                        this.error = (!explained).then(|| SharedString::from(e.to_string()));
                    }
                }
                cx.notify();
            });
        }));
    }

    /// "N channels · M pattern subscriptions", once a listing is in.
    fn summary_line(&self, locale: &str) -> Option<SharedString> {
        let summary = self.summary?;
        let text = if self.sharded {
            t!(
                "pubsub_editor.channels_summary_sharded",
                count = summary.channels,
                locale = locale
            )
        } else {
            t!(
                "pubsub_editor.channels_summary",
                count = summary.channels,
                patterns = summary.pattern_subscriptions.unwrap_or_default(),
                locale = locale
            )
        };
        Some(text.to_string().into())
    }
}

impl Render for ZedisPubsubChannelsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let locale = cx.global::<ZedisGlobalStore>().read(cx).locale().to_string();
        let row_count = self.table_state.read(cx).delegate().total_len();
        let first_listing = self.loading && self.summary.is_none();
        let summary_line = self.summary_line(&locale);
        let cluster_note: Option<SharedString> = self.summary.filter(|s| s.nodes > 1).map(|s| {
            t!("pubsub_editor.channels_cluster_note", nodes = s.nodes, locale = &locale)
                .to_string()
                .into()
        });
        let truncated_note: Option<SharedString> = self.summary.filter(|s| s.truncated).map(|_| {
            t!(
                "pubsub_editor.channels_truncated",
                max = MAX_PUBSUB_CHANNELS,
                locale = &locale
            )
            .to_string()
            .into()
        });

        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.pattern_state).flex_1())
                    .child(
                        Button::new("pubsub-channels-refresh")
                            .outline()
                            // Spinner replaces the icon while loading (see CLAUDE.md).
                            .icon(Icon::new(CustomIconName::RefreshCw))
                            .label(i18n_pubsub_editor(cx, "channels_refresh"))
                            .loading(self.loading)
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| this.fetch(cx))),
                    ),
            )
            .when_some(summary_line, |this, text| {
                this.child(Label::new(text).text_xs().text_color(muted).whitespace_normal())
            })
            .when_some(cluster_note, |this, text| {
                this.child(Label::new(text).text_xs().text_color(muted).whitespace_normal())
            })
            .when_some(truncated_note, |this, text| {
                this.child(Label::new(text).text_xs().text_color(muted).whitespace_normal())
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(danger).whitespace_normal())
            })
            .child(
                v_flex()
                    .h(px(LIST_HEIGHT))
                    .w_full()
                    .font_family(get_mono_font_family())
                    .when(first_listing, |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Label::new(i18n_common(cx, "loading")).text_color(muted)),
                        )
                    })
                    .when(!first_listing && row_count == 0, |this| {
                        // A column, and the label full-width: text only wraps
                        // inside a definite width, which a centered item in a
                        // row box never gets.
                        this.child(
                            v_flex().size_full().items_center().justify_center().px_6().child(
                                Label::new(i18n_pubsub_editor(cx, "channels_empty"))
                                    .w_full()
                                    .text_sm()
                                    .text_center()
                                    .text_color(muted)
                                    .whitespace_normal(),
                            ),
                        )
                    })
                    .when(!first_listing && row_count > 0, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(true)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
    }
}

/// Opens the browser over the Pub/Sub editor; `on_pick` receives the
/// channel of the row whose action was clicked (closing the dialog is the
/// caller's call, so it can keep it open on failure).
pub fn open_pubsub_channels_dialog(
    server_state: Entity<ZedisServerState>,
    sharded: bool,
    on_pick: ChannelPick,
    window: &mut Window,
    cx: &mut App,
) {
    let title_key = if sharded {
        "channels_sharded_title"
    } else {
        "channels_title"
    };
    let title = i18n_pubsub_editor(cx, title_key);
    let view = cx.new(|cx| ZedisPubsubChannelsDialog::new(server_state, sharded, on_pick, window, cx));
    ZedisDialog::new(title)
        .icon(CustomIconName::Rss)
        .w(px(DIALOG_WIDTH))
        .child(move || view.clone())
        .ok_text(i18n_common(cx, "close"))
        .open(window, cx);
}

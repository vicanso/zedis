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

/// Redis Pub/Sub editor view.
///
/// Provides a UI for subscribing to Redis channels via pattern-based subscriptions
/// and publishing messages. Received messages are displayed in a scrollable table
/// with timestamp, channel, and message columns.
use crate::assets::CustomIconName;
use crate::connection::ServerCommand;
use crate::connection::{Capability, ShardedPubSub, get_connection_manager};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::{ZedisGlobalStore, ZedisServerState, detect_and_decode, i18n_common, i18n_pubsub_editor};
use crate::views::unavailable_chip;
use chrono::Local;
use gpui::{Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{DataTable, TableState},
    v_flex,
};
use redis::aio::PubSub;
use tracing::{error, info};
use zedis_ui::{TextColumn, ZedisTextTable};

/// Cap on retained messages. The list is a ring buffer (like the Monitor
/// view's `MAX_RECORDS`) so subscribing to a hot channel can't grow
/// memory without bound; matches the Keyspace-notifications history cap.
const MAX_MESSAGES: usize = 1_000;

/// A single message received from a Redis Pub/Sub channel.
#[derive(Clone, Debug)]
struct PubsubMessage {
    timestamp: SharedString,
    channel: SharedString,
    message: SharedString,
}

/// The two subscription transports: classic Pub/Sub over the dedicated
/// RESP2 connection (`PSUBSCRIBE`), or sharded Pub/Sub over a RESP3 push
/// connection (`SSUBSCRIBE`, Redis 7+ — slot-routed on clusters instead
/// of broadcast). Both yield [`redis::Msg`], so the reader/drainer path
/// downstream is shared.
enum SubscribeConn {
    Plain(Box<PubSub>),
    Sharded(Box<ShardedPubSub>),
}

/// Decode one incoming message and ferry it to the drainer. `Err` means
/// the receiver (the view) is gone, so the reader loop should stop.
async fn forward_message(
    tx: &smol::channel::Sender<PubsubMessage>,
    msg: &redis::Msg,
) -> Result<(), smol::channel::SendError<PubsubMessage>> {
    let channel: String = msg.get_channel_name().to_string();
    let (_, text) = detect_and_decode(msg.get_payload_bytes(), 1024);
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.send(PubsubMessage {
        timestamp: timestamp.into(),
        channel: channel.into(),
        message: text,
    })
    .await
}

impl PubsubMessage {
    fn cells(&self) -> Vec<SharedString> {
        vec![self.timestamp.clone(), self.channel.clone(), self.message.clone()]
    }
}

/// The message grid: newest first, capped at [`MAX_MESSAGES`]. Fixed
/// widths for timestamp and channel; the message column takes the rest.
fn build_table(window: &mut Window, cx: &mut gpui::App) -> ZedisTextTable {
    // Use the global content width if available; fall back to the full window width.
    let window_width = window.viewport_size().width;
    let content_width = cx
        .global::<ZedisGlobalStore>()
        .read(cx)
        .content_width()
        .unwrap_or(window_width);
    // Timestamp: "2026-08-01 12:34:56" = 19 mono chars + cell padding —
    // 200px clipped the seconds.
    let timestamp_width = 230.;
    let channel_width = 150.;
    let remaining_width = content_width.as_f32() - timestamp_width - channel_width - 10.;
    let columns = vec![
        TextColumn::new("timestamp", i18n_pubsub_editor(cx, "timestamp"), timestamp_width),
        TextColumn::new("channel", i18n_pubsub_editor(cx, "channel"), channel_width),
        TextColumn::new("message", i18n_pubsub_editor(cx, "message"), remaining_width),
    ];
    ZedisTextTable::new(columns, i18n_common(cx, "copied_to_clipboard"))
        .copy_tooltip(i18n_common(cx, "copy_cell_tooltip"))
        .max_rows(MAX_MESSAGES)
}

/// Main Pub/Sub editor component.
///
/// Layout (top to bottom):
///   1. Subscribe bar  – channel pattern input + subscribe/unsubscribe button
///   2. Message table   – live stream of received messages (newest first)
///   3. Publish bar     – channel input + message input + publish button
///
/// The subscription mirrors the Monitor pattern: a dedicated connection is
/// read (and payloads decoded) on a *background* task that ferries parsed
/// messages over a `smol::channel`; a foreground drainer pulls them in
/// batches into the delegate's capped ring buffer with one `notify` per
/// batch. Dropping `subscribe_task` cancels the loop and the connection.
pub struct ZedisPubsubEditor {
    server_state: Entity<ZedisServerState>,

    subscribe_input_state: Entity<InputState>,
    publish_channel_input_state: Entity<InputState>,
    publish_message_input_state: Entity<InputState>,

    table_state: Entity<TableState<ZedisTextTable>>,
    /// Mirror of the delegate's row count, kept by the drainer so
    /// `render` can branch to the empty state without reading the table.
    message_count: usize,

    /// True while the initial subscribe handshake is in progress.
    subscribing: bool,

    /// Sharded mode (`SSUBSCRIBE` / `SPUBLISH`, Redis 7+): exact channel
    /// names instead of patterns, slot-routed on clusters. Toggled by the
    /// checkbox in the subscribe bar (only shown when the server supports it).
    sharded: bool,

    /// Holds the long-running subscription loop; `None` when not subscribed.
    subscribe_task: Option<Task<()>>,
    /// Why the last subscribe failed, shown under the bar — a server that
    /// rejects SUBSCRIBE must not look like an idle, empty stream.
    subscribe_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisPubsubEditor {
    /// Creates a new Pub/Sub editor bound to the given server connection.
    /// The subscribe input is auto-focused so the user can immediately type a channel pattern.
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();

        let subscribe_input_state = cx.new(|cx| {
            let input = InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_pubsub_editor(cx, "subscribe_channel_placeholder"));
            input.focus(window, cx);
            input
        });

        let publish_channel_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_pubsub_editor(cx, "publish_channel_placeholder"))
        });

        let publish_message_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_pubsub_editor(cx, "publish_message_placeholder"))
        });

        // Enter in the subscribe input triggers subscription.
        subscriptions.push(
            cx.subscribe_in(&subscribe_input_state, window, |view, _state, event, window, cx| {
                if let InputEvent::PressEnter { .. } = &event {
                    view.handle_subscribe(window, cx);
                }
            }),
        );

        // Enter in the publish message input sends the message and clears the field.
        subscriptions.push(cx.subscribe_in(
            &publish_message_input_state,
            window,
            |view, _state, event, window, cx| {
                if let InputEvent::PressEnter { .. } = &event {
                    view.handle_publish(window, cx);
                    view.publish_message_input_state.update(cx, |state, cx| {
                        state.set_value(SharedString::default(), window, cx);
                    });
                }
            },
        ));

        let table_state = cx.new(|cx| TableState::new(build_table(window, cx), window, cx));

        info!("Creating new pubsub editor");

        Self {
            server_state,
            subscribe_input_state,
            publish_channel_input_state,
            publish_message_input_state,
            table_state,
            message_count: 0,
            subscribing: false,
            sharded: false,
            subscribe_task: None,
            subscribe_error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Starts a subscription. Classic mode is pattern-based (`PSUBSCRIBE`,
    /// space-separated patterns like "news.* alerts.*"); sharded mode
    /// (`SSUBSCRIBE`, Redis 7+) takes exact channel names instead.
    /// A background task is spawned that opens a dedicated Pub/Sub connection,
    /// subscribes, and then loops forever reading incoming messages until the
    /// stream ends or the entity is dropped.
    fn handle_subscribe(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let channel: SharedString = self.subscribe_input_state.read(cx).value();
        if channel.is_empty() {
            return;
        }

        let server_state = self.server_state.read(cx);
        let server_id = server_state.server_id().to_string();
        let sharded = self.sharded;
        self.subscribing = true;
        cx.notify();

        let entity = cx.entity().downgrade();
        let channel_clone = channel.clone();
        let (tx, rx) = smol::channel::unbounded::<PubsubMessage>();

        self.subscribe_task = Some(cx.spawn(async move |_handle, cx| {
            // Establish a dedicated Pub/Sub connection on a background thread
            // so the UI thread stays responsive during the network handshake.
            let result: Result<SubscribeConn, Error> = cx
                .background_spawn(async move {
                    let channels = channel_clone
                        .split(' ')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<&str>>();
                    if sharded {
                        let mut pubsub = get_connection_manager().get_sharded_pubsub(&server_id).await?;
                        pubsub.ssubscribe(&channels).await?;
                        Ok(SubscribeConn::Sharded(Box::new(pubsub)))
                    } else {
                        let mut pubsub = get_connection_manager().get_pubsub_connection(&server_id).await?;
                        pubsub
                            .psubscribe(channels)
                            .await
                            .map_err(|e| Error::Invalid { message: e.to_string() })?;
                        Ok(SubscribeConn::Plain(Box::new(pubsub)))
                    }
                })
                .await;

            match result {
                Ok(sub) => {
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        cx.notify();
                    });

                    // Read + decode messages on a background task so payload
                    // decoding never lands on the UI thread; parsed entries
                    // are ferried over the channel to the drainer below.
                    let reader = cx.background_spawn(async move {
                        match sub {
                            SubscribeConn::Plain(mut pubsub) => {
                                use futures::StreamExt;
                                let mut stream = pubsub.on_message();
                                while let Some(msg) = stream.next().await {
                                    // Receiver gone (entity dropped) — stop reading.
                                    if forward_message(&tx, &msg).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            SubscribeConn::Sharded(pubsub) => {
                                while let Some(msg) = pubsub.recv().await {
                                    if forward_message(&tx, &msg).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    });

                    // Foreground drainer (Monitor pattern): after the first
                    // recv() wakes us, drain everything pending (capped per
                    // batch) into the ring buffer with a single notify —
                    // a hot channel costs one refresh per batch, not per
                    // message, and memory stays bounded by MAX_MESSAGES.
                    const BATCH_LIMIT: usize = 200;
                    while let Ok(first) = rx.recv().await {
                        let mut batch = Vec::with_capacity(BATCH_LIMIT);
                        batch.push(first);
                        while batch.len() < BATCH_LIMIT {
                            match rx.try_recv() {
                                Ok(entry) => batch.push(entry),
                                Err(_) => break,
                            }
                        }

                        let result = entity.update(cx, |this, cx| {
                            let count = this.table_state.update(cx, |state, _| {
                                let delegate = state.delegate_mut();
                                // Newest first: prepend, the cap trims the tail.
                                for entry in batch {
                                    delegate.push_front(entry.cells());
                                }
                                delegate.total_len()
                            });
                            this.message_count = count;
                            cx.notify();
                        });
                        // Entity was dropped – stop the loop.
                        if result.is_err() {
                            break;
                        }
                    }

                    // Stream ended or entity dropped: cancel the reader (and
                    // with it the dedicated Pub/Sub connection).
                    drop(reader);
                }
                Err(e) => {
                    error!("Pubsub subscribe error: {:?}", e);
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        // A NOPERM / unknown-command reply degrades the feature
                        // matrix (and explains itself once); anything else is
                        // shown inline instead of vanishing into the log.
                        let explained = this
                            .server_state
                            .update(cx, |state, cx| state.note_command_error(&e, cx));
                        this.subscribe_error = (!explained).then(|| SharedString::from(e.to_string()));
                        cx.notify();
                    });
                }
            }
        }));
    }

    /// Cancels the active subscription by dropping the background task,
    /// which in turn drops the Pub/Sub connection.
    fn handle_unsubscribe(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.subscribe_task.take();
        self.subscribing = false;
        cx.notify();
    }

    /// Publishes a message to the specified channel via the server state.
    /// Does nothing if either the channel or message field is empty.
    fn handle_publish(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Defense in depth — the publish bar is hidden without PublishMessage.
        if !self.server_state.read(cx).can(Capability::PublishMessage) {
            return;
        }
        let channel: SharedString = self.publish_channel_input_state.read(cx).value();
        let message: SharedString = self.publish_message_input_state.read(cx).value();
        if channel.is_empty() || message.is_empty() {
            return;
        }

        let sharded = self.sharded;
        self.server_state.update(cx, move |state, cx| {
            state.publish_message(channel, message, sharded, cx);
        });
    }

    /// Renders the top toolbar: a channel pattern input, a Sharded toggle
    /// (Redis 7+ only), and a subscribe/unsubscribe button.
    /// While an active subscription exists the input is disabled and the button switches
    /// to "unsubscribe".
    fn render_subscribe_bar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_subscriptions = self.subscribe_task.is_some();
        let supports_sharded = self.server_state.read(cx).supports_sharded_pubsub();
        let subscribe_btn = if has_subscriptions {
            Button::new("pubsub-unsubscribe-btn")
                .outline()
                .label(i18n_pubsub_editor(cx, "unsubscribe"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.handle_unsubscribe(window, cx);
                }))
        } else {
            Button::new("pubsub-subscribe-btn")
                .outline()
                // Spinner replaces the icon while loading (see CLAUDE.md).
                .icon(Icon::new(CustomIconName::Rss))
                .loading(self.subscribing)
                .disabled(self.subscribing)
                .label(i18n_pubsub_editor(cx, "subscribe"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.handle_subscribe(window, cx);
                }))
        };

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.subscribe_input_state)
                    .w_full()
                    .flex_1()
                    .disabled(has_subscriptions),
            )
            .when(supports_sharded, |this| {
                this.child(
                    Checkbox::new("pubsub-sharded")
                        .label(i18n_pubsub_editor(cx, "sharded"))
                        .tooltip(i18n_pubsub_editor(cx, "sharded_tooltip"))
                        .checked(self.sharded)
                        // Mid-subscription the transport can't change; toggle
                        // applies to the next subscribe (and to publishes).
                        .disabled(has_subscriptions || self.subscribing)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            this.sharded = *checked;
                            // Patterns don't exist in sharded mode — swap the
                            // input hint to exact channel names.
                            let key = if this.sharded {
                                "subscribe_sharded_channel_placeholder"
                            } else {
                                "subscribe_channel_placeholder"
                            };
                            let placeholder = i18n_pubsub_editor(cx, key);
                            this.subscribe_input_state.update(cx, |state, cx| {
                                state.set_placeholder(placeholder, window, cx);
                            });
                            cx.notify();
                        })),
                )
            })
            .child(subscribe_btn)
    }

    /// Renders the bottom toolbar: channel input, message input, and a publish button.
    fn render_publish_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.publish_channel_input_state)
                    .w(px(200.))
                    .flex_shrink_0(),
            )
            .child(Input::new(&self.publish_message_input_state).w_full().flex_1())
            .child(
                Button::new("pubsub-publish-btn")
                    .outline()
                    .label(i18n_pubsub_editor(cx, "publish"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.handle_publish(window, cx);
                    })),
            )
    }
}

impl Render for ZedisPubsubEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.message_count == 0;

        v_flex()
            .size_full()
            .font_family(get_mono_font_family())
            .overflow_hidden()
            .child(self.render_subscribe_bar(window, cx))
            // SUBSCRIBE missing / denied on this server (proxies that don't
            // forward subscriptions, restricted ACL users).
            .when_some(
                self.server_state.read(cx).command_block(ServerCommand::Subscribe),
                |this, status| {
                    this.child(
                        h_flex()
                            .px_3()
                            .py_1()
                            .child(unavailable_chip(cx, ServerCommand::Subscribe, status)),
                    )
                },
            )
            .when_some(self.subscribe_error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .child(Label::new(error).text_xs().text_color(cx.theme().danger)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .when(is_empty, |this| {
                        this.child(div().size_full().flex().items_center().justify_center().child(
                            Label::new(i18n_pubsub_editor(cx, "no_messages")).text_color(cx.theme().muted_foreground),
                        ))
                    })
                    .when(!is_empty, |this| {
                        this.child(
                            DataTable::new(&self.table_state)
                                .stripe(true)
                                .bordered(false)
                                .scrollbar_visible(true, true),
                        )
                    }),
            )
            // PUBLISH mutates server state (Capability::PublishMessage);
            // subscribing stays available read-only (Observe).
            .when(self.server_state.read(cx).can(Capability::PublishMessage), |this| {
                this.child(self.render_publish_bar(cx))
            })
            // PUBLISH the server forbids: name the reason where the bar was.
            .when_some(
                self.server_state.read(cx).blocked_by(Capability::PublishMessage),
                |this, (command, status)| {
                    this.child(h_flex().px_3().py_2().child(unavailable_chip(cx, command, status)))
                },
            )
            .into_any_element()
    }
}

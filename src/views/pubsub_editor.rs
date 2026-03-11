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
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::{ZedisGlobalStore, ZedisServerState, detect_and_decode, i18n_common, i18n_pubsub_editor};
use chrono::Local;
use gpui::{ClipboardItem, Edges, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants;
use gpui_component::notification::Notification;
use gpui_component::{
    ActiveTheme, Disableable, IconName, StyledExt, WindowExt,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};
use std::sync::Arc;
use tracing::{error, info};

/// A single message received from a Redis Pub/Sub channel.
#[derive(Clone, Debug)]
struct PubsubMessage {
    timestamp: SharedString,
    channel: SharedString,
    message: SharedString,
}

/// Table delegate that drives the message list display.
/// Column widths are computed from the available content area so the message
/// column fills whatever space remains after timestamp and channel.
struct PubsubTableDelegate {
    messages: Arc<Vec<PubsubMessage>>,
    columns: Vec<Column>,
}

impl PubsubTableDelegate {
    fn new(messages: Arc<Vec<PubsubMessage>>, window: &mut Window, cx: &mut gpui::App) -> Self {
        // Use the global content width if available; fall back to the full window width.
        let window_width = window.viewport_size().width;
        let content_width = cx
            .global::<ZedisGlobalStore>()
            .read(cx)
            .content_width()
            .unwrap_or(window_width);
        // Fixed widths for timestamp and channel; the message column gets the rest.
        let timestamp_width = 200.;
        let channel_width = 150.;
        let remaining_width = content_width.as_f32() - timestamp_width - channel_width - 10.;
        let columns = vec![
            Column::new("timestamp", i18n_pubsub_editor(cx, "timestamp"))
                .width(timestamp_width)
                .map(|mut col| {
                    col.paddings = Some(Edges {
                        top: px(2.),
                        bottom: px(2.),
                        left: px(10.),
                        right: px(10.),
                    });
                    col
                }),
            Column::new("channel", i18n_pubsub_editor(cx, "channel"))
                .width(channel_width)
                .map(|mut col| {
                    col.paddings = Some(Edges {
                        top: px(2.),
                        bottom: px(2.),
                        left: px(10.),
                        right: px(10.),
                    });
                    col
                }),
            Column::new("message", i18n_pubsub_editor(cx, "message"))
                .width(remaining_width)
                .map(|mut col| {
                    col.paddings = Some(Edges {
                        top: px(2.),
                        bottom: px(2.),
                        left: px(10.),
                        right: px(10.),
                    });
                    col
                }),
        ];
        Self { messages, columns }
    }
}

impl TableDelegate for PubsubTableDelegate {
    fn columns_count(&self, _cx: &gpui::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &gpui::App) -> usize {
        self.messages.len()
    }

    fn column(&self, index: usize, _cx: &gpui::App) -> Column {
        self.columns[index].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        div()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_color(cx.theme().primary)
                    .text_sm(),
            )
    }

    /// Renders a table cell. Each cell shows a copy button on hover that writes
    /// the cell text to the system clipboard.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut gpui::Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let value = if let Some(msg) = self.messages.get(row_ix) {
            match col_ix {
                0 => msg.timestamp.clone(),
                1 => msg.channel.clone(),
                2 => msg.message.clone(),
                _ => "--".into(),
            }
        } else {
            "--".into()
        };

        // Unique group name per cell so hover state is scoped correctly.
        let group_name: SharedString = format!("pubsub-td-{}-{}", row_ix, col_ix).into();
        let copied_message = i18n_common(cx, "copied_to_clipboard");
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .group(group_name.clone())
            .overflow_hidden()
            .child(
                Label::new(value.clone())
                    .text_align(column.align)
                    .text_ellipsis()
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                div()
                    .id(("copy-wrapper", row_ix * 100 + col_ix))
                    .invisible()
                    .group_hover(group_name, |style| style.visible())
                    .flex_none()
                    .on_click(|_, _, cx: &mut gpui::App| cx.stop_propagation())
                    .child(
                        Button::new(("copy-cell", row_ix * 100 + col_ix))
                            .ghost()
                            .icon(IconName::Copy)
                            .on_click(move |_, window, cx: &mut gpui::App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                                window.push_notification(Notification::info(copied_message.clone()), cx);
                            }),
                    ),
            )
    }

    fn has_more(&self, _cx: &gpui::App) -> bool {
        false
    }

    fn load_more_threshold(&self) -> usize {
        0
    }

    fn load_more(&mut self, _window: &mut Window, _cx: &mut gpui::Context<TableState<Self>>) {}
}

/// Main Pub/Sub editor component.
///
/// Layout (top to bottom):
///   1. Subscribe bar  – channel pattern input + subscribe/unsubscribe button
///   2. Message table   – live stream of received messages (newest first)
///   3. Publish bar     – channel input + message input + publish button
///
/// The subscription runs as a background async task (`subscribe_task`) that
/// continuously reads from the Redis Pub/Sub stream and pushes messages into
/// the shared `messages` vec. Dropping the task cancels the subscription.
pub struct ZedisPubsubEditor {
    server_state: Entity<ZedisServerState>,

    subscribe_input_state: Entity<InputState>,
    publish_channel_input_state: Entity<InputState>,
    publish_message_input_state: Entity<InputState>,

    table_state: Entity<TableState<PubsubTableDelegate>>,
    messages: Arc<Vec<PubsubMessage>>,

    /// True while the initial subscribe handshake is in progress.
    subscribing: bool,

    /// Holds the long-running subscription loop; `None` when not subscribed.
    subscribe_task: Option<Task<()>>,
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

        let messages: Arc<Vec<PubsubMessage>> = Arc::new(Vec::new());
        let delegate = PubsubTableDelegate::new(messages.clone(), window, cx);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        info!("Creating new pubsub editor");

        Self {
            server_state,
            subscribe_input_state,
            publish_channel_input_state,
            publish_message_input_state,
            table_state,
            messages,
            subscribing: false,
            subscribe_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Starts a pattern-based subscription (`PSUBSCRIBE`).
    /// The channel input supports space-separated patterns (e.g. "news.* alerts.*").
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
        self.subscribing = true;
        cx.notify();

        let entity = cx.entity().downgrade();
        let channel_clone = channel.clone();

        self.subscribe_task = Some(cx.spawn(async move |_handle, cx| {
            // Establish a dedicated Pub/Sub connection on a background thread
            // so the UI thread stays responsive during the network handshake.
            let result: Result<_, Error> = cx
                .background_spawn(async move {
                    let mut pubsub = get_connection_manager().get_pubsub_connection(&server_id).await?;
                    let channels = channel_clone.split(' ').collect::<Vec<&str>>();
                    pubsub
                        .psubscribe(channels)
                        .await
                        .map_err(|e| Error::Invalid { message: e.to_string() })?;
                    Ok(pubsub)
                })
                .await;

            match result {
                Ok(mut pubsub) => {
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
                        cx.notify();
                    });

                    // Continuously read messages from the subscription stream.
                    // Each incoming message is prepended to the list so the
                    // newest message always appears at the top of the table.
                    use futures::StreamExt;
                    let mut stream = pubsub.on_message();
                    loop {
                        let msg_opt = stream.next().await;

                        match msg_opt {
                            Some(msg) => {
                                let channel: String = msg.get_channel_name().to_string();
                                let payload = msg.get_payload_bytes();
                                let (_, text) = detect_and_decode(payload, 1024);
                                let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                                // Update both the editor's own message list and the
                                // table delegate's reference so the UI stays in sync.
                                let result = entity.update(cx, move |this, cx| {
                                    let mut msgs = (*this.messages).clone();
                                    msgs.insert(
                                        0,
                                        PubsubMessage {
                                            timestamp: timestamp.into(),
                                            channel: channel.into(),
                                            message: text,
                                        },
                                    );
                                    let messages = Arc::new(msgs);
                                    this.messages = messages.clone();
                                    this.table_state.update(cx, |state, _| {
                                        state.delegate_mut().messages = messages;
                                    });
                                    cx.notify();
                                });
                                // Entity was dropped – stop the loop.
                                if result.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
                Err(e) => {
                    error!("Pubsub subscribe error: {:?}", e);
                    let _ = entity.update(cx, |this, cx| {
                        this.subscribing = false;
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
        let channel: SharedString = self.publish_channel_input_state.read(cx).value();
        let message: SharedString = self.publish_message_input_state.read(cx).value();
        if channel.is_empty() || message.is_empty() {
            return;
        }

        self.server_state.update(cx, move |state, cx| {
            state.publish_message(channel, message, cx);
        });
    }

    /// Renders the top toolbar: a channel pattern input and a subscribe/unsubscribe toggle.
    /// While an active subscription exists the input is disabled and the button switches
    /// to "unsubscribe".
    fn render_subscribe_bar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_subscriptions = self.subscribe_task.is_some();
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
        let is_empty = self.messages.is_empty();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(self.render_subscribe_bar(window, cx))
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
            .child(self.render_publish_bar(cx))
            .into_any_element()
    }
}

// Copyright 2025 Tree xie.
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

use std::time::Duration;

use crate::assets::CustomIconName;
use crate::states::{RedisValue, ZedisServerState};
use gpui::AnyWindowHandle;
use gpui::ClipboardItem;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::button::Button;
use gpui_component::h_flex;
use gpui_component::highlighter::Language;
use gpui_component::input::InputEvent;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, IconName};
use gpui_component::{Disableable, WindowExt};
use humansize::{DECIMAL, format_size};
use tracing::debug;

pub struct ZedisEditor {
    server_state: Entity<ZedisServerState>,
    editor: Entity<InputState>,
    value_modified: bool,
    window_handle: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

impl ZedisEditor {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&server_state, |this, model, cx| {
            let value = model.read(cx).value().cloned();
            this.update_editor_value(cx, value);
        }));
        let default_language = Language::from_str("json");
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(default_language.name())
                .line_number(true)
                // TODO 等component完善后，再打开indent_guides
                .indent_guides(false)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .searchable(true)
                .soft_wrap(true)
        });
        subscriptions.push(cx.subscribe(&editor, |this, _, event, cx| {
            if let InputEvent::Change = &event {
                let value = this.editor.read(cx).value();
                let redis_value = this.server_state.read(cx).value();
                let original = redis_value.and_then(|r| r.data()).map_or("", |v| v);

                this.value_modified = original != value.as_str();
                cx.notify();
            }
        }));

        Self {
            server_state,
            editor,
            value_modified: false,
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
        }
    }
    fn update_editor_value(&mut self, cx: &mut Context<Self>, value: Option<RedisValue>) {
        let window_handle = self.window_handle;
        self.value_modified = false;
        let _ = window_handle.update(cx, move |_, window, cx| {
            self.editor.update(cx, move |this, cx| {
                debug!(value = ?value, "update editor value");
                let Some(value) = value else {
                    this.set_value("", window, cx);
                    return;
                };
                if let Some(data) = value.data() {
                    this.set_value(data, window, cx);
                } else {
                    this.set_value("", window, cx);
                }
                cx.notify();
            });
        });
    }
    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        let key = key.to_string();
        let server_state = self.server_state.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let message = format!("Are you sure you want to delete this key: {key}?");
            let server_state = server_state.clone();
            let key = key.clone();
            dialog.confirm().child(message).on_ok(move |_, window, cx| {
                let key = key.clone();
                server_state.update(cx, move |state, cx| {
                    state.delete_key(key, cx);
                });
                window.close_dialog(cx);
                true
            })
        });
    }
    fn render_select_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(key) = server_state.key().map(|key| key.to_string()) else {
            return h_flex();
        };
        let mut labels = vec![];
        if let Some(value) = server_state.value() {
            let ttl = if let Some(ttl) = value.ttl() {
                let seconds = ttl.num_seconds();
                if seconds < 0 {
                    "Perm".to_string()
                } else {
                    humantime::format_duration(Duration::from_secs(seconds as u64)).to_string()
                }
            } else {
                "--".to_string()
            };
            let ttl = ttl
                .split_whitespace()
                .take(2)
                .collect::<Vec<&str>>()
                .join(" ");
            let size = format_size(value.size() as u64, DECIMAL);
            labels.push(Label::new(format!("size : {size}")).mr_2().text_sm());
            labels.push(Label::new(format!("ttl : {ttl}",)).text_sm());
        }
        let content = key.clone();

        h_flex()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .w_full()
            .child(
                Button::new("zedis-editor-copy-key")
                    .outline()
                    .tooltip("Copy key")
                    .icon(IconName::Copy)
                    .on_click(cx.listener(move |_this, _event, window, cx| {
                        let content = content.clone();
                        cx.write_to_clipboard(ClipboardItem::new_string(content));
                        window.push_notification(
                            Notification::info("Copied the key to clipboard"),
                            cx,
                        );
                    })),
            )
            .child(
                div()
                    .flex_1()
                    // 不设置为w_0，宽度会被过长的key撑开，导致布局错乱
                    .w_0()
                    .overflow_hidden()
                    .mx_2()
                    .child(Label::new(key).text_ellipsis().whitespace_nowrap()),
            )
            .children(labels)
            .child(
                Button::new("zedis-editor-save-key")
                    .disabled(!self.value_modified || server_state.updating())
                    .loading(server_state.updating())
                    .outline()
                    .tooltip("Save data")
                    .ml_2()
                    .icon(CustomIconName::FileCheckCorner)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        let Some(key) = this.server_state.read(cx).key().map(|key| key.to_string())
                        else {
                            return;
                        };
                        let value = this.editor.read(cx).value().to_string();
                        this.server_state.update(cx, move |state, cx| {
                            state.save_value(key, value, cx);
                        });
                    })),
            )
            .child(
                Button::new("zedis-editor-delete-key")
                    .outline()
                    .loading(server_state.deleting())
                    .disabled(server_state.deleting())
                    .tooltip("Delete key")
                    .icon(IconName::CircleX)
                    .ml_2()
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.delete_key(window, cx);
                    })),
            )
    }
}

impl Render for ZedisEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .child(self.render_select_key(cx))
            .child(
                Input::new(&self.editor)
                    .flex_1()
                    .bordered(false)
                    .p_0()
                    .w_full()
                    .h_full()
                    .font_family("Monaco")
                    .text_size(px(12.))
                    .focus_bordered(false),
            )
            .into_any_element()
    }
}

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

use crate::assets::CustomIconName;
use crate::states::{RedisValue, ZedisServerState};
use gpui::AnyWindowHandle;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::h_flex;
use gpui_component::highlighter::Language;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use gpui_component::label::Label;
use gpui_component::v_flex;
use humansize::{DECIMAL, format_size};
use tracing::debug;

pub struct ZedisEditor {
    server_state: Entity<ZedisServerState>,
    editor: Entity<InputState>,
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

        Self {
            server_state,
            editor,
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
        }
    }
    fn update_editor_value(&mut self, cx: &mut Context<Self>, value: Option<RedisValue>) {
        let window_handle = self.window_handle;
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
    fn render_select_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        let Some(key) = server_state.key().map(|key| key.to_string()) else {
            return h_flex();
        };
        let mut labels = vec![];
        if let Some(value) = server_state.value() {
            let ttl = if let Some(ttl) = value.ttl() {
                humantime::format_duration(ttl).to_string()
            } else {
                "--".to_string()
            };
            let ttl = ttl
                .split_whitespace()
                .take(2)
                .collect::<Vec<&str>>()
                .join(" ");
            let size = format_size(value.size() as u64, DECIMAL);
            let text_color = cx.theme().muted_foreground;
            labels.push(
                Label::new(format!("SIZE : {size}"))
                    .mr_2()
                    .text_sm()
                    .text_color(text_color),
            );
            labels.push(
                Label::new(format!("TTL : {ttl}",))
                    .text_sm()
                    .text_color(text_color),
            );
        }

        h_flex()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .items_center()
            .w_full()
            .child(Icon::new(CustomIconName::Key).mr_1())
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

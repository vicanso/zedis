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

use crate::states::{RedisValue, ZedisServerState};
use gpui::AnyWindowHandle;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::highlighter::Language;
use gpui_component::input::InputEvent;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use tracing::debug;

pub struct ZedisStringEditor {
    server_state: Entity<ZedisServerState>,
    value_modified: bool,
    editor: Entity<InputState>,
    window_handle: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

impl ZedisStringEditor {
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
        let value = server_state
            .read(cx)
            .value()
            .and_then(|v| v.string_value())
            .map_or(String::new(), |v| v.to_string());

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
                .default_value(value)
        });
        subscriptions.push(cx.subscribe(&editor, |this, _, event, cx| {
            if let InputEvent::Change = &event {
                let value = this.editor.read(cx).value();
                let redis_value = this.server_state.read(cx).value();
                let original = redis_value.and_then(|r| r.string_value()).map_or("", |v| v);

                this.value_modified = original != value.as_str();
                cx.notify();
            }
        }));

        Self {
            value_modified: false,
            editor,
            window_handle: window.window_handle(),
            server_state,
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
                if let Some(data) = value.string_value() {
                    this.set_value(data, window, cx);
                } else {
                    this.set_value("", window, cx);
                }
                cx.notify();
            });
        });
    }
    pub fn is_value_modified(&self) -> bool {
        self.value_modified
    }
    pub fn value(&self, cx: &mut Context<Self>) -> String {
        self.editor.read(cx).value().to_string()
    }
}

impl Render for ZedisStringEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.editor)
            .flex_1()
            .bordered(false)
            .p_0()
            .w_full()
            .h_full()
            .font_family("Monaco")
            .text_size(px(12.))
            .focus_bordered(false)
            .into_any_element()
    }
}

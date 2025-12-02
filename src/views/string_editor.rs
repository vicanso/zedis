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

use crate::helpers::get_font_family;
use crate::states::{RedisValue, ZedisServerState};
use gpui::AnyWindowHandle;
use gpui::Entity;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::highlighter::Language;
use gpui_component::input::InputEvent;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use pretty_hex::HexConfig;
use pretty_hex::config_hex;

pub struct ZedisStringEditor {
    server_state: Entity<ZedisServerState>,
    value_modified: bool,
    editor: Entity<InputState>,
    window_handle: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

fn get_string_value(window: &Window, value: Option<&RedisValue>) -> SharedString {
    let Some(value) = value else {
        return String::new().into();
    };
    let mut string_value = value.string_value().unwrap_or_default();
    if string_value.is_empty()
        && let Some(data) = value.bytes_value()
    {
        let width = window.viewport_size().width;
        let width = match width {
            width if width < px(1400.) => 16,
            _ => 32,
        };
        let cfg = HexConfig {
            title: false,
            width,
            group: 0,
            ..Default::default()
        };
        string_value = config_hex(&data, cfg).into()
    }
    string_value
}

impl ZedisStringEditor {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_editor_value(cx);
        }));
        let value = get_string_value(window, server_state.read(cx).value());

        let default_language = Language::from_str("json");
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(default_language.name())
                .line_number(true)
                // TODO 等component完善后，再打开indent_guides
                .indent_guides(true)
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
                let original = redis_value
                    .and_then(|r| r.string_value())
                    .map_or("".into(), |v| v);

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
    fn update_editor_value(&mut self, cx: &mut Context<Self>) {
        // prevent editor flickering by skipping value updates while loading
        if self
            .server_state
            .read(cx)
            .value()
            .map(|value| value.is_loading())
            .unwrap_or(false)
        {
            return;
        }
        let window_handle = self.window_handle;
        let server_state = self.server_state.clone();
        self.value_modified = false;
        let _ = window_handle.update(cx, move |_, window, cx| {
            self.editor.update(cx, move |this, cx| {
                let value = server_state.read(cx).value();
                this.set_value(get_string_value(window, value), window, cx);
                cx.notify();
            });
        });
    }
    pub fn is_value_modified(&self) -> bool {
        self.value_modified
    }
    pub fn value(&self, cx: &mut Context<Self>) -> SharedString {
        self.editor.read(cx).value()
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
            .font_family(get_font_family())
            .text_size(px(12.))
            .focus_bordered(false)
            .into_any_element()
    }
}

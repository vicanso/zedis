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
use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::ZedisServerState;
use gpui::AnyWindowHandle;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::Icon;
use gpui_component::h_flex;
use gpui_component::highlighter::Language;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};
use gpui_component::label::Label;
use gpui_component::v_flex;
use serde_json::Value;

type Result<T, E = Error> = std::result::Result<T, E>;

pub struct ZedisEditor {
    selected_key: String,
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
            let selected_key = model.read(cx).key().unwrap_or_default();
            if this.selected_key != selected_key {
                this.selected_key = selected_key.to_string();
                this.handle_get_value(cx);
            }
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
            selected_key: "".to_string(),
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
        }
    }
    fn handle_get_value(&mut self, cx: &mut Context<Self>) {
        let window_handle = self.window_handle;
        let server = self.server_state.read(cx).server().to_string();
        let selected_key = self.selected_key.clone();
        if selected_key.is_empty() {
            let _ = window_handle.update(cx, move |_, window, cx| {
                self.editor.update(cx, |this, cx| {
                    this.set_value("", window, cx);
                    cx.notify();
                });
            });
            return;
        }
        cx.spawn(async move |handle, cx| {
            let processing_selected_key = selected_key.clone();
            let task = cx.background_spawn(async move {
                // TODO 根据key的类型判断逻辑
                let client = get_connection_manager().get_client(&server)?;
                let value = client.get::<String>(&selected_key)?.unwrap_or_default();
                if !value.is_empty()
                    && let Ok(value) = serde_json::from_str::<Value>(&value)
                    && let Ok(pretty_value) = serde_json::to_string_pretty(&value)
                {
                    return Ok(pretty_value);
                }
                Ok(value)
            });
            let result: Result<String, Error> = task.await;
            window_handle.update(cx, move |_, window, cx| {
                handle.update(cx, move |this, cx| {
                    // if this.selected_key changed, stop the task
                    if this.selected_key != processing_selected_key {
                        return;
                    }
                    this.editor.update(cx, |this, cx| {
                        let value = result.unwrap_or_else(|e| {
                            // TODO: handle error
                            println!("error: {e:?}");
                            format!("Zedis error: {e:?}")
                        });
                        this.set_value(value, window, cx);
                        cx.notify();
                    });
                })
            })
        })
        .detach();
    }
}

impl Render for ZedisEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .child(
                h_flex()
                    .m_2()
                    .items_center()
                    .child(Icon::new(CustomIconName::Key).mr_1())
                    .child(Label::new(&self.selected_key)),
            )
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

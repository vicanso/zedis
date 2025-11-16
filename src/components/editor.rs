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

use crate::connection::get_connection_manager;
use crate::error::Error;
use crate::states::ZedisServerState;
use gpui::AnyWindowHandle;
use gpui::AppContext;
use gpui::Subscription;
use gpui::px;
use gpui::{Context, Entity, IntoElement, Render, Styled, Window};
use gpui_component::highlighter::Language;
use gpui_component::input::TabSize;
use gpui_component::input::{Input, InputState};

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
            let selected_key = model.read(cx).selected_key.clone().unwrap_or_default();
            if this.selected_key != selected_key {
                this.selected_key = selected_key;
                this.handle_get_value(cx);
            }
        }));
        let default_language = Language::from_str("json");
        let editor = cx.new(|cx| {
            let editor = InputState::new(window, cx)
                .code_editor(default_language.name())
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .soft_wrap(true);
            // .default_value(include_str!("./fixtures/test.rs"))
            // .placeholder("Enter your code here...");

            // let lsp_store = Rc::new(lsp_store.clone());
            // editor.lsp.completion_provider = Some(lsp_store.clone());
            // editor.lsp.code_action_providers = vec![lsp_store.clone(), Rc::new(TextConvertor)];
            // editor.lsp.hover_provider = Some(lsp_store.clone());
            // editor.lsp.definition_provider = Some(lsp_store.clone());
            // editor.lsp.document_color_provider = Some(lsp_store.clone());

            editor
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
        let window_handle = self.window_handle.clone();
        let server = self.server_state.read(cx).server.clone();
        let selected_key = self.selected_key.clone();
        cx.spawn(async move |handle, cx| {
            let processing_selected_key = selected_key.clone();
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let value = client.get(&selected_key)?;
                Ok(value.unwrap_or_default())
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.editor)
            .bordered(false)
            .p_0()
            .h_full()
            .font_family("Monaco")
            .text_size(px(12.))
            .focus_bordered(false)
            .into_any_element()
    }
}

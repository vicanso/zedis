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
use crate::states::ZedisGlobalStore;
use crate::states::i18n_editor;
use crate::states::{KeyType, ZedisServerState};
use crate::views::ZedisListEditor;
use crate::views::ZedisStringEditor;
use gpui::ClipboardItem;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::Icon;
use gpui_component::button::Button;
use gpui_component::h_flex;
use gpui_component::input::Input;
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, IconName};
use gpui_component::{Disableable, WindowExt};
use humansize::{DECIMAL, format_size};
use rust_i18n::t;
use std::time::Duration;
use tracing::debug;

const PERM: &str = "perm";

pub struct ZedisEditor {
    server_state: Entity<ZedisServerState>,

    // editors
    list_editor: Option<Entity<ZedisListEditor>>,
    string_editor: Option<Entity<ZedisStringEditor>>,
    // state
    ttl_edit_mode: bool,
    ttl_input_state: Entity<InputState>,

    _subscriptions: Vec<Subscription>,
}

impl ZedisEditor {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = vec![];
        let input = cx.new(|cx| InputState::new(window, cx).clean_on_escape());

        subscriptions.push(
            cx.subscribe_in(
                &input,
                window,
                |view, _state, event, window, cx| match &event {
                    InputEvent::PressEnter { .. } => {
                        view.handle_update_ttl(window, cx);
                    }
                    InputEvent::Blur => {
                        view.ttl_edit_mode = false;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
        );

        Self {
            server_state,
            list_editor: None,
            string_editor: None,
            ttl_edit_mode: false,
            ttl_input_state: input,
            _subscriptions: subscriptions,
        }
    }
    fn handle_update_ttl(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let key = self.server_state.clone().read(cx).key().unwrap_or_default();
        if key.is_empty() {
            return;
        }
        self.ttl_edit_mode = false;
        let ttl = self.ttl_input_state.read(cx).value();
        self.server_state.update(cx, move |state, cx| {
            state.update_key_ttl(key, ttl, cx);
        });
        cx.notify();
    }

    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.server_state.read(cx).key() else {
            return;
        };
        let server_state = self.server_state.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let locale = cx.global::<ZedisGlobalStore>().locale(cx);
            let message = t!("editor.delete_key_prompt", key = key, locale = locale).to_string();
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
        let Some(key) = server_state.key() else {
            return h_flex();
        };
        let mut btns = vec![];
        let mut ttl = "".to_string();
        let mut size = "".to_string();
        if let Some(value) = server_state.value() {
            ttl = if let Some(ttl) = value.ttl() {
                let seconds = ttl.num_seconds();
                if seconds == -2 {
                    i18n_editor(cx, "expired")
                } else if seconds < 0 {
                    i18n_editor(cx, "perm")
                } else {
                    humantime::format_duration(Duration::from_secs(seconds as u64))
                        .to_string()
                        .into()
                }
            } else {
                "--".into()
            }
            .split_whitespace()
            .take(2)
            .collect::<Vec<&str>>()
            .join(" ");
            size = format_size(value.size() as u64, DECIMAL);
        }
        let size_label = i18n_editor(cx, "size");
        if !size.is_empty() {
            btns.push(
                Label::new(format!("{size_label} : {size}"))
                    .ml_2()
                    .text_sm()
                    .into_any_element(),
            );
        }

        if let Some(string_editor) = &self.string_editor {
            let value_modified = string_editor.read(cx).is_value_modified();
            btns.push(
                Button::new("zedis-editor-save-key")
                    .ml_2()
                    .disabled(!value_modified || server_state.updating())
                    .loading(server_state.updating())
                    .outline()
                    .tooltip(i18n_editor(cx, "save_data_tooltip"))
                    .ml_2()
                    .icon(CustomIconName::FileCheckCorner)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        let Some(key) = this.server_state.read(cx).key() else {
                            return;
                        };
                        let Some(editor) = this.string_editor.as_ref() else {
                            return;
                        };
                        editor.clone().update(cx, move |state, cx| {
                            let value = state.value(cx);
                            this.server_state.update(cx, move |state, cx| {
                                state.save_value(key.to_string(), value, cx);
                            });
                        });
                    }))
                    .into_any_element(),
            );
        }

        if !ttl.is_empty() {
            let ttl_btn = if self.ttl_edit_mode {
                Input::new(&self.ttl_input_state)
                    .ml_2()
                    .max_w(px(150.))
                    .suffix(
                        Button::new("zedis-editor-ttl-update-btn")
                            .icon(Icon::new(IconName::Check))
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                this.handle_update_ttl(window, cx);
                            })),
                    )
                    .into_any_element()
            } else {
                Button::new("zedis-editor-ttl-btn")
                    .ml_2()
                    .label(ttl.clone())
                    .icon(Icon::new(CustomIconName::Clock3))
                    .text_sm()
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        let ttl = ttl.clone();
                        this.ttl_edit_mode = true;
                        this.ttl_input_state.update(cx, move |state, cx| {
                            let value = if ttl == PERM {
                                "".to_string()
                            } else {
                                ttl.clone()
                            };
                            state.set_value(value, window, cx);
                            state.focus(window, cx);
                        });
                        cx.notify();
                    }))
                    .into_any_element()
            };
            btns.push(ttl_btn);
        }

        btns.push(
            Button::new("zedis-editor-delete-key")
                .ml_2()
                .outline()
                .loading(server_state.deleting())
                .disabled(server_state.deleting())
                .tooltip(i18n_editor(cx, "delete_key_tooltip").to_string())
                .icon(IconName::CircleX)
                .ml_2()
                .on_click(cx.listener(move |this, _event, window, cx| {
                    this.delete_key(window, cx);
                }))
                .into_any_element(),
        );

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
                    .tooltip(i18n_editor(cx, "copy_key_tooltip").to_string())
                    .icon(IconName::Copy)
                    .on_click(cx.listener(move |_this, _event, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(content.to_string()));
                        window.push_notification(
                            Notification::info(
                                i18n_editor(cx, "copied_key_to_clipboard").to_string(),
                            ),
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
            .children(btns)
    }
    fn reset_editors(&mut self, key_type: KeyType) {
        if key_type != KeyType::String {
            let _ = self.string_editor.take();
        }
        if key_type != KeyType::List {
            let _ = self.list_editor.take();
        }
    }
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(value) = self.server_state.read(cx).value() else {
            self.reset_editors(KeyType::Unknown);
            return div().into_any_element();
        };
        match value.key_type() {
            KeyType::List => {
                self.reset_editors(KeyType::List);
                let editor = if let Some(list_editor) = &self.list_editor {
                    list_editor.clone()
                } else {
                    debug!("new list editor");
                    let list_editor =
                        cx.new(|cx| ZedisListEditor::new(window, cx, self.server_state.clone()));
                    self.list_editor = Some(list_editor.clone());
                    list_editor
                };
                editor.into_any_element()
            }
            _ => {
                self.reset_editors(KeyType::String);
                let editor = if let Some(string_editor) = &self.string_editor {
                    string_editor.clone()
                } else {
                    debug!("new string editor");
                    let string_editor =
                        cx.new(|cx| ZedisStringEditor::new(window, cx, self.server_state.clone()));
                    self.string_editor = Some(string_editor.clone());
                    let string_editor_entity = string_editor.clone();
                    cx.spawn(async move |_this, cx| {
                        string_editor_entity.update(cx, move |_state, cx| {
                            cx.notify();
                        })
                    })
                    .detach();
                    string_editor
                };
                editor.into_any_element()
            }
        }
    }
}

impl Render for ZedisEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        if server_state.key().is_none() {
            return v_flex().into_any_element();
        }

        v_flex()
            .w_full()
            .h_full()
            .child(self.render_select_key(cx))
            .child(self.render_editor(window, cx))
            .into_any_element()
    }
}

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
use crate::states::{KeyType, ZedisServerState};
use crate::views::ZedisListEditor;
use crate::views::ZedisStringEditor;
use gpui::ClipboardItem;
use gpui::Entity;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui_component::button::Button;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::notification::Notification;
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, IconName};
use gpui_component::{Disableable, WindowExt};
use humansize::{DECIMAL, format_size};
use std::time::Duration;
use tracing::debug;

pub struct ZedisEditor {
    server_state: Entity<ZedisServerState>,

    // editors
    list_editor: Option<Entity<ZedisListEditor>>,
    string_editor: Option<Entity<ZedisStringEditor>>,
}

impl ZedisEditor {
    pub fn new(
        _window: &mut Window,
        _cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        Self {
            server_state,
            list_editor: None,
            string_editor: None,
        }
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

        let save_btn = if let Some(string_editor) = &self.string_editor {
            let value_modified = string_editor.read(cx).is_value_modified();
            Button::new("zedis-editor-save-key")
                .disabled(!value_modified || server_state.updating())
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
                    let Some(editor) = this.string_editor.as_ref() else {
                        return;
                    };
                    editor.clone().update(cx, move |state, cx| {
                        let value = state.value(cx);
                        this.server_state.update(cx, move |state, cx| {
                            state.save_value(key, value, cx);
                        });
                    });
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };

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
            .child(save_btn)
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

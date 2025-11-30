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
use crate::states::i18n_list_editor;
use crate::states::{RedisListValue, ZedisServerState};
use gpui::App;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::{Icon, IconName};
use std::sync::Arc;

const INDEX_WIDTH: f32 = 50.;
const ACTION_WIDTH: f32 = 100.;

#[derive(Debug)]
struct RedisListValues {
    view: Entity<ZedisListEditor>,
    list_value: Arc<RedisListValue>,
    server_state: Entity<ZedisServerState>,
    selected_index: Option<IndexPath>,
    value_state: Entity<InputState>,
    updated_index: Option<IndexPath>,
    done: bool,
}
impl RedisListValues {
    pub fn get_counts(&self) -> (usize, usize) {
        (self.list_value.values.len(), self.list_value.size)
    }
}
impl ListDelegate for RedisListValues {
    type Item = ListItem;
    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.list_value.values.len()
    }
    fn render_item(&self, ix: IndexPath, _window: &mut Window, cx: &mut App) -> Option<Self::Item> {
        let even_bg = cx.theme().background;
        let odd_bg = if cx.theme().is_dark() {
            Hsla::white().alpha(0.1)
        } else {
            Hsla::black().alpha(0.03)
        };
        self.list_value.values.get(ix.row).map(|item| {
            let index = ix.row + 1;
            let bg = if index.is_multiple_of(2) {
                even_bg
            } else {
                odd_bg
            };
            let is_updated = self.updated_index == Some(ix);
            let content = if is_updated {
                div()
                    .mx_2()
                    .child(Input::new(&self.value_state).small())
                    .flex_1()
                    .into_any_element()
            } else {
                Label::new(item)
                    .pl_4()
                    .text_sm()
                    .flex_1()
                    .into_any_element()
            };
            let view = self.view.clone();
            let default_value = item.clone();
            ListItem::new(("zedis-editor-list-item", index))
                .gap(px(0.))
                .bg(bg)
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .child(
                            Label::new((index).to_string())
                                .text_align(TextAlign::Right)
                                .text_sm()
                                .w(px(INDEX_WIDTH)),
                        )
                        .child(content)
                        .child(
                            h_flex()
                                .w(px(ACTION_WIDTH))
                                .child(
                                    Button::new(("zedis-editor-list-action-update-btn", index))
                                        .small()
                                        .ghost()
                                        .mr_2()
                                        .tooltip(i18n_list_editor(cx, "update_tooltip").to_string())
                                        .when(!is_updated, |this| {
                                            this.icon(Icon::new(CustomIconName::FilePenLine))
                                        })
                                        .when(is_updated, |this| {
                                            this.icon(Icon::new(IconName::Check))
                                        })
                                        .on_click(move |_event, _window, cx| {
                                            cx.stop_propagation();
                                            view.update(cx, |this, cx| {
                                                if is_updated {
                                                    this.handle_update_value(
                                                        default_value.clone(),
                                                        ix,
                                                        cx,
                                                    );
                                                } else {
                                                    this.handle_update_index(
                                                        default_value.clone(),
                                                        ix,
                                                        cx,
                                                    );
                                                }
                                            });
                                        }),
                                )
                                .child(
                                    Button::new(("zedis-editor-list-action-delete-btn", index))
                                        .small()
                                        .ghost()
                                        .tooltip(i18n_list_editor(cx, "delete_tooltip").to_string())
                                        .icon(Icon::new(CustomIconName::FilePlusCorner))
                                        .on_click(move |_event, _window, cx| {
                                            cx.stop_propagation();
                                            // 3. 使用外部捕获的 view 句柄来更新
                                            // delete_view.update(cx, |this, cx| {
                                            //     // this.handle_click(ix, cx);
                                            // });
                                        }),
                                ),
                        ),
                )
        })
    }
    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }
    fn load_more(&mut self, _window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if self.done || self.loading(cx) {
            return;
        }
        if self.list_value.values.len() >= self.list_value.size {
            self.done = true;
            return;
        }

        self.server_state.update(cx, |this, cx| {
            this.load_more_list_value(cx);
        });
    }
}

pub struct ZedisListEditor {
    list_state: Entity<ListState<RedisListValues>>,
    server_state: Entity<ZedisServerState>,
    value_state: Entity<InputState>,
    input_default_value: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisListEditor {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_list_values(cx);
        }));
        let value_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_list_editor(cx, "value_placeholder").to_string())
        });
        let view = cx.entity();
        let mut deletage = RedisListValues {
            view,
            server_state: server_state.clone(),
            list_value: Default::default(),
            selected_index: Default::default(),
            value_state: value_state.clone(),
            done: false,
            updated_index: None,
        };
        if let Some(data) = server_state.read(cx).value().and_then(|v| v.list_value()) {
            deletage.list_value = data.clone()
        };

        let list_state = cx.new(|cx| ListState::new(deletage, window, cx));
        Self {
            server_state,
            list_state,
            value_state,
            input_default_value: None,
            _subscriptions: subscriptions,
        }
    }
    fn update_list_values(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let Some(data) = server_state.value().and_then(|v| v.list_value()) else {
            return;
        };
        let items = data.clone();
        self.list_state.update(cx, |this, cx| {
            this.delegate_mut().list_value = items;
            cx.notify();
        });
    }
    fn handle_update_index(&mut self, value: SharedString, ix: IndexPath, cx: &mut Context<Self>) {
        self.input_default_value = Some(value);
        self.list_state.update(cx, |this, _cx| {
            this.delegate_mut().updated_index = Some(ix);
        });
    }
    fn handle_update_value(
        &mut self,
        original_value: SharedString,
        ix: IndexPath,
        cx: &mut Context<Self>,
    ) {
        self.list_state.update(cx, |this, _cx| {
            this.delegate_mut().updated_index = None;
        });
        let value = self.value_state.read(cx).value();
        self.server_state.update(cx, |this, cx| {
            this.update_list_value(ix.row, original_value, value, cx);
        });
    }
}

impl Render for ZedisListEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value_label = i18n_list_editor(cx, "value").to_string();
        let action_label = i18n_list_editor(cx, "action").to_string();
        let list_state = self.list_state.read(cx).delegate();
        let (items_count, total_count) = list_state.get_counts();
        let text_color = cx.theme().muted_foreground;
        if let Some(value) = self.input_default_value.take() {
            self.value_state.update(cx, |this, cx| {
                this.set_value(value, window, cx);
                this.focus(window, cx);
            });
        }

        v_flex()
            .h_full()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .child(
                        Label::new("#")
                            .text_align(TextAlign::Right)
                            .text_sm()
                            .text_color(text_color)
                            .w(px(INDEX_WIDTH + 10.)),
                    )
                    .child(
                        Label::new(value_label)
                            .pl_4()
                            .text_sm()
                            .text_color(text_color)
                            .flex_1(),
                    )
                    .child(
                        Label::new(action_label)
                            .text_sm()
                            .text_color(text_color)
                            .w(px(ACTION_WIDTH + 10.)),
                    ),
            )
            .child(List::new(&self.list_state).flex_1())
            .child(
                h_flex().w_full().p_2().text_align(TextAlign::Right).child(
                    Label::new(format!("{} / {}", items_count, total_count))
                        .text_sm()
                        .text_color(text_color)
                        .flex_1(),
                ),
            )
    }
}

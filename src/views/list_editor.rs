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

use crate::states::i18n_list_editor;
use crate::states::{RedisListValue, ZedisServerState};
use gpui::App;
use gpui::Entity;
use gpui::Hsla;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::IndexPath;
use gpui_component::h_flex;
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::v_flex;
use std::sync::Arc;

const INDEX_WIDTH: f32 = 50.;

#[derive(Debug)]
struct RedisListValues {
    list_value: Arc<RedisListValue>,
    server_state: Entity<ZedisServerState>,
    selected_index: Option<IndexPath>,
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
                        .child(Label::new(item).pl_4().text_sm().flex_1()),
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
        let mut deletage = RedisListValues {
            server_state: server_state.clone(),
            list_value: Default::default(),
            selected_index: Default::default(),
            done: false,
        };
        if let Some(data) = server_state.read(cx).value().and_then(|v| v.list_value()) {
            deletage.list_value = data.clone()
        };
        let list_state = cx.new(|cx| ListState::new(deletage, window, cx));
        Self {
            server_state,
            list_state,
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
}

impl Render for ZedisListEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value_label = i18n_list_editor(cx, "value").to_string();
        let list_state = self.list_state.read(cx).delegate();
        let (items_count, total_count) = list_state.get_counts();
        let text_color = cx.theme().muted_foreground;
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

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

use crate::states::ZedisServerState;
use gpui::AppContext;
use gpui::Entity;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Colorize;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::StyledExt;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::h_flex;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;
use gpui_component::v_flex;
use tracing::debug;

pub struct ZedisKeyTree {
    is_empty: bool,
    server_state: Entity<ZedisServerState>,
    key_tree_id: String,
    tree_state: Entity<TreeState>,

    server: String,
    keyword_state: Entity<InputState>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyTree {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let server = server_state.read(cx).server().to_string();
        subscriptions.push(cx.observe(&server_state, |this, model, cx| {
            let server_state = model.read(cx);
            let server = server_state.server();
            debug!(
                server,
                key_tree_server = this.server,
                "observe server state"
            );
            this.update_key_tree(cx);
        }));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder("Filter keys by keyword")
        });
        subscriptions.push(
            cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = &event {
                    view.handle_filter(cx);
                }
            }),
        );

        debug!(server, "new key tree");

        Self {
            is_empty: false,
            key_tree_id: "".to_string(),

            error: None,
            tree_state,
            server,
            keyword_state,
            server_state,
            _subscriptions: subscriptions,
        }
    }

    fn update_key_tree(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        if self.key_tree_id == server_state.key_tree_id() {
            return;
        }
        self.key_tree_id = server_state.key_tree_id().to_string();
        let items = server_state.key_tree();
        self.is_empty = items.is_empty() && !server_state.scaning();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            cx.notify();
        });
    }
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        if self.server_state.read(cx).scaning() {
            return;
        }
        let keyword = self.keyword_state.read(cx).text().to_string();
        self.server_state.update(cx, move |handle, cx| {
            handle.scan(cx, keyword);
        });
    }

    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_empty || self.error.is_some() {
            let text = self
                .error
                .clone()
                .unwrap_or_else(|| "No keys found".to_string());
            return div()
                .h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pt_5()
                .px_2()
                .child(Icon::new(IconName::Info).text_sm())
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(Label::new(text).text_sm().whitespace_normal()),
                )
                .into_any_element();
        }
        let view = cx.entity();
        tree(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                view.update(cx, |_, cx| {
                    let item = entry.item();
                    let icon = if !entry.is_folder() {
                        IconName::File
                    } else if entry.is_expanded() {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    };
                    let bg = if ix % 2 == 0 {
                        cx.theme().background
                    } else {
                        cx.theme().background.lighten(1.0)
                    };

                    ListItem::new(ix)
                        .w_full()
                        .rounded(cx.theme().radius)
                        .bg(bg)
                        .py_1()
                        .px_2()
                        .pl(px(16.) * entry.depth() + px(8.))
                        .child(h_flex().gap_2().child(icon).child(item.label.clone()))
                        .on_click(cx.listener({
                            let item = item.clone();
                            move |this, _, _window, cx| {
                                if item.is_folder() {
                                    return;
                                }
                                let selected_key = item.id.to_string();
                                this.server_state.update(cx, |state, cx| {
                                    state.select_key(selected_key, cx);
                                });
                            }
                        }))
                })
            },
        )
        .text_sm()
        .p_1()
        .bg(cx.theme().sidebar)
        .text_color(cx.theme().sidebar_foreground)
        .h_full()
        .into_any_element()
    }
    fn render_keyword_input(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let scaning = self.server_state.read(cx).scaning();
        div()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.keyword_state)
                    .suffix(
                        Button::new("key-tree-search-btn")
                            .ghost()
                            .tooltip("Search keys")
                            .loading(scaning)
                            .disabled(scaning)
                            .icon(IconName::Search)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.handle_filter(cx);
                            })),
                    )
                    .cleanable(true),
            )
    }
}

impl Render for ZedisKeyTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w_full()
            .child(self.render_keyword_input(cx))
            .child(self.render_tree(cx))
    }
}

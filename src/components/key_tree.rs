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
use crate::helpers::build_key_tree;
use crate::states::ZedisServerState;
use gpui::AppContext;
use gpui::Subscription;
use gpui::px;
use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::ActiveTheme;
use gpui_component::Colorize;
use gpui_component::Disableable;
use gpui_component::IconName;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::h_flex;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::ListItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;
use gpui_component::v_flex;

pub struct ZedisKeyTree {
    loading: bool,
    keys: Vec<String>,
    keyword: String,
    cursors: Option<Vec<u64>>,
    server: String,
    keyword_state: Entity<InputState>,
    server_state: Entity<ZedisServerState>,
    tree_state: Entity<TreeState>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisKeyTree {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        server_state: Entity<ZedisServerState>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(&server_state, |this, model, cx| {
            let server = model.read(cx).server.clone();
            if this.server != server {
                this.server = server;
                this.reset(cx);
                this.handle_fetch_keys(cx);
            }
        }));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder("Input scan keyword")
        });
        subscriptions.push(
            cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = &event {
                    view.handle_filter(cx);
                }
            }),
        );

        Self {
            loading: false,
            cursors: None,
            tree_state,
            keys: vec![],
            keyword: "".to_string(),
            server: "".to_string(),
            keyword_state,
            server_state,
            _subscriptions: subscriptions,
        }
    }
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.cursors = None;
        self.keys.clear();
        self.keyword = "".to_string();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(vec![], cx);
        });
    }
    fn scan_keys(&mut self, cx: &mut Context<Self>, server: String, keyword: String) {
        // if server or keyword changed, stop the scan
        if self.server != server || self.keyword != keyword {
            return;
        }
        let cursors = self.cursors.clone();
        cx.spawn(async move |handle, cx| {
            let processing_server = server.clone();
            let processing_keyword = keyword.clone();
            let task = cx.background_spawn(async move {
                let client = get_connection_manager().get_client(&server)?;
                let pattern = format!("*{}*", keyword);
                let count = if keyword.is_empty() { 2_000 } else { 10_000 };
                if let Some(cursors) = cursors {
                    client.scan(cursors, &pattern, count)
                } else {
                    client.first_scan(&pattern, count)
                }
            });
            let result = task.await;
            handle.update(cx, move |this, cx| {
                match result {
                    Ok((cursors, keys)) => {
                        if cursors.iter().sum::<u64>() == 0 {
                            this.cursors = None;
                        } else {
                            this.cursors = Some(cursors);
                        }
                        this.extend_key(keys, cx);
                    }
                    Err(e) => {
                        // TODO 出错的处理
                        println!("error: {e:?}");
                        this.cursors = None;
                    }
                };
                if this.cursors.is_some() && this.keys.len() < 1_000 {
                    // run again
                    this.scan_keys(cx, processing_server, processing_keyword);
                    return cx.notify();
                }
                this.loading = false;
                cx.notify();
            })
        })
        .detach();
    }
    fn handle_fetch_keys(&mut self, cx: &mut Context<Self>) {
        let server = self.server.clone();
        if server.is_empty() {
            return;
        }
        self.loading = true;
        cx.notify();
        self.scan_keys(cx, server, self.keyword.clone());
    }
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let value = self.keyword_state.read(cx).text().to_string();
        if value != self.keyword {
            self.reset(cx);
            self.keyword = value;
            self.handle_fetch_keys(cx);
        }
    }
    pub fn extend_key(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        self.keys.extend(keys);
        let items = build_key_tree(&self.keys);

        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }
    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
                        cx.theme().background.lighten(0.8)
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
                                    state.selected_key = Some(selected_key);
                                    cx.notify();
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
    }
    fn render_keyword_input(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.keyword_state)
                    .suffix(
                        Button::new("key-tree-search-btn")
                            .ghost()
                            .loading(self.loading)
                            .disabled(self.loading)
                            .icon(IconName::Search)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.handle_filter(cx);
                                // let value = this.keyword_state.read(cx).text().to_string();
                                // if value != this.keyword {
                                //     this.reset(cx);
                                //     this.keyword = value;
                                //     this.handle_fetch_keys(cx);
                                // }
                            })),
                    )
                    .cleanable(true),
            )
    }
}

impl Render for ZedisKeyTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w_full()
            .child(self.render_keyword_input(cx))
            .child(self.render_tree(cx))
    }
}

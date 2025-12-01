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
use crate::states::KeyType;
use crate::states::QueryMode;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_key_tree;
use crate::states::save_app_state;
use ahash::AHashSet;
use gpui::AppContext;
use gpui::Corner;
use gpui::Entity;
use gpui::Hsla;
use gpui::Subscription;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::ActiveTheme;
use gpui_component::Disableable;
use gpui_component::Icon;
use gpui_component::IconName;
use gpui_component::StyledExt;
use gpui_component::button::ButtonVariants;
use gpui_component::button::{Button, DropdownButton};
use gpui_component::h_flex;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::list::ListItem;
use gpui_component::tree::TreeState;
use gpui_component::tree::tree;
use gpui_component::v_flex;
use tracing::debug;
use tracing::error;
use tracing::info;

pub struct ZedisKeyTree {
    is_empty: bool,
    server_state: Entity<ZedisServerState>,
    key_tree_id: String,
    tree_state: Entity<TreeState>,

    query_mode: QueryMode,

    expanded_items: AHashSet<String>,
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
        subscriptions.push(cx.observe(&server_state, |this, _model, cx| {
            this.update_key_tree(cx);
        }));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_key_tree(cx, "filter_placeholder").to_string())
        });
        subscriptions.push(
            cx.subscribe_in(&keyword_state, window, |view, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = &event {
                    view.handle_filter(cx);
                }
            }),
        );
        let query_mode = cx
            .global::<ZedisGlobalStore>()
            .query_mode(server.as_str(), cx);

        debug!(server, "new key tree");

        let mut this = Self {
            is_empty: false,
            key_tree_id: "".to_string(),

            error: None,
            tree_state,
            keyword_state,
            server_state,
            query_mode,
            expanded_items: AHashSet::with_capacity(10),
            _subscriptions: subscriptions,
        };
        this.update_key_tree(cx);

        this
    }

    fn update_key_tree(&mut self, cx: &mut Context<Self>) {
        let server_state = self.server_state.read(cx);
        let query_mode = cx
            .global::<ZedisGlobalStore>()
            .query_mode(server_state.server(), cx);
        debug!(
            key_tree_server = server_state.server(),
            key_tree_id = server_state.key_tree_id(),
            "observe server state"
        );
        self.query_mode = query_mode;

        if self.key_tree_id == server_state.key_tree_id() {
            return;
        }

        let expand_all = server_state.scan_count() < 20;
        let items = server_state.key_tree(&self.expanded_items, expand_all);
        if items.is_empty() {
            self.expanded_items.clear();
        }
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
        let keyword = self.keyword_state.read(cx).value();
        self.server_state.update(cx, move |handle, cx| {
            handle.handle_filter(keyword, cx);
        });
    }

    fn render_tree(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let server_state = self.server_state.read(cx);
        if !server_state.scaning() && (self.is_empty || self.error.is_some()) {
            if self.query_mode == QueryMode::Exact {
                if let Some(value) = server_state.value()
                    && value.is_expired()
                {
                    return h_flex()
                        .w_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .pt_5()
                        .px_2()
                        .child(Label::new(i18n_key_tree(cx, "key_not_exists")).text_sm())
                        .into_any_element();
                }
                return h_flex().into_any_element();
            }
            let text = self
                .error
                .clone()
                .unwrap_or_else(|| i18n_key_tree(cx, "no_keys_found").to_string());
            return div()
                .h_flex()
                .w_full()
                .items_center()
                .justify_center()
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
        let yellow = cx.theme().colors.yellow;
        let selected_key = server_state.key().unwrap_or_default();
        let server_state = self.server_state.clone();
        let even_bg = cx.theme().background;
        let odd_bg = if cx.theme().is_dark() {
            Hsla::white().alpha(0.1)
        } else {
            Hsla::black().alpha(0.03)
        };
        let list_active_color = cx.theme().list_active;
        let list_active_border_color = cx.theme().list_active_border;
        tree(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                view.update(cx, |_, cx| {
                    let item = entry.item();
                    let icon = if !entry.is_folder() {
                        let key_type = server_state
                            .read(cx)
                            .key_type(&item.id)
                            .unwrap_or(&KeyType::Unknown);
                        if key_type == &KeyType::Unknown {
                            div().into_any_element()
                        } else {
                            let key_type_color = key_type.color();
                            let mut key_type_bg = key_type_color;
                            key_type_bg.fade_out(0.8);
                            let mut key_type_border = key_type_color;
                            key_type_border.fade_out(0.5);
                            Label::new(key_type.as_str())
                                .text_xs()
                                .bg(key_type_bg)
                                .text_color(key_type_color)
                                .border_1()
                                .px_1()
                                .rounded_sm()
                                .border_color(key_type_border)
                                .into_any_element()
                        }
                    } else if entry.is_expanded() {
                        Icon::new(IconName::FolderOpen)
                            .text_color(yellow)
                            .into_any_element()
                    } else {
                        Icon::new(IconName::Folder)
                            .text_color(yellow)
                            .into_any_element()
                    };
                    let bg = if item.id == selected_key {
                        list_active_color
                    } else if ix % 2 == 0 {
                        even_bg
                    } else {
                        odd_bg
                    };
                    let mut count_label = Label::new("");
                    if entry.is_folder() {
                        count_label = Label::new(item.children.len().to_string())
                            .text_sm()
                            .text_color(cx.theme().muted_foreground);
                    }

                    ListItem::new(ix)
                        .w_full()
                        .bg(bg)
                        .py_1()
                        .px_2()
                        .pl(px(16.) * entry.depth() + px(8.))
                        .when(item.id == selected_key, |this| {
                            this.border_r_3().border_color(list_active_border_color)
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .child(icon)
                                .child(div().flex_1().text_ellipsis().child(item.label.clone()))
                                .child(count_label),
                        )
                        .on_click(cx.listener({
                            let item = item.clone();
                            move |this, _, _window, cx| {
                                if item.is_folder() {
                                    let key = item.id.to_string();
                                    if item.is_expanded() {
                                        this.expanded_items.insert(key.clone());
                                        this.server_state.update(cx, |state, cx| {
                                            state.scan_prefix(
                                                format!("{}:", key.as_str()).into(),
                                                cx,
                                            );
                                        });
                                    } else {
                                        this.expanded_items.remove(&key);
                                    }
                                    return;
                                }
                                let selected_key = item.id.clone();
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
        let query_mode = self.query_mode;
        let icon = match query_mode {
            QueryMode::All => Icon::new(IconName::Asterisk),
            QueryMode::Prefix => Icon::new(CustomIconName::Activity),
            QueryMode::Exact => Icon::new(CustomIconName::Equal),
        };
        h_flex()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.keyword_state)
                    .w_full()
                    .flex_1()
                    .px_0()
                    .prefix(
                        DropdownButton::new("dropdown")
                            .button(
                                Button::new("key-tree-query-mode-btn")
                                    .ghost()
                                    .bg(cx.theme().background)
                                    .px_2()
                                    .icon(icon),
                            )
                            .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                                menu.menu_element_with_check(
                                    query_mode == QueryMode::All,
                                    Box::new(QueryMode::All),
                                    |_, cx| {
                                        Label::new(i18n_key_tree(cx, "query_mode_all"))
                                            .ml_2()
                                            .text_xs()
                                    },
                                )
                                .menu_element_with_check(
                                    query_mode == QueryMode::Prefix,
                                    Box::new(QueryMode::Prefix),
                                    |_, cx| {
                                        Label::new(i18n_key_tree(cx, "query_mode_prefix"))
                                            .ml_2()
                                            .text_xs()
                                    },
                                )
                                .menu_element_with_check(
                                    query_mode == QueryMode::Exact,
                                    Box::new(QueryMode::Exact),
                                    |_, cx| {
                                        Label::new(i18n_key_tree(cx, "query_mode_exact"))
                                            .ml_2()
                                            .text_xs()
                                    },
                                )
                            }),
                    )
                    .suffix(
                        Button::new("key-tree-search-btn")
                            .ghost()
                            .tooltip(i18n_key_tree(cx, "search_tooltip").to_string())
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
            .on_action(cx.listener(|this, e: &QueryMode, _window, cx| {
                let server = this.server_state.read(cx).server().to_string();
                let app_state = cx.global::<ZedisGlobalStore>().state();
                this.server_state.update(cx, |state, _cx| {
                    state.set_query_mode(*e);
                });
                app_state.update(cx, |state, cx| {
                    state.add_query_mode(server, *e);
                    let value = state.clone();
                    cx.spawn(async move |_, cx| {
                        cx.background_spawn(async move {
                            if let Err(e) = save_app_state(&value) {
                                error!(error = %e, "save app state failed");
                            } else {
                                info!(action = "save app state", "save app state success");
                            }
                        })
                        .await;
                    })
                    .detach();
                });
                this.query_mode = *e;
            }))
    }
}

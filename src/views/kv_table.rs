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
use crate::components::{INDEX_COLUMN_NAME, ZedisKvDelegate, ZedisKvFetcher};
use crate::constants::SIDEBAR_WIDTH;
use crate::helpers::get_key_tree_widths;
use crate::states::ServerEvent;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_kv_table;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::prelude::*;
use gpui::px;
use gpui::{Edges, Entity};
use gpui::{SharedString, div};
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::input::Input;
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::table::Column;
use gpui_component::table::{Table, TableState};
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, Disableable};
use gpui_component::{Icon, IconName};
use gpui_component::{PixelsExt, h_flex};
use tracing::info;

const KEYWORD_INPUT_WIDTH: f32 = 200.0;

#[derive(Clone, Default)]
pub struct KvTableColumn {
    name: SharedString,
    width: Option<f32>,
    align: Option<TextAlign>,
}
impl KvTableColumn {
    pub fn new(name: &str, width: Option<f32>) -> Self {
        Self {
            name: name.to_string().into(),
            width,
            ..Default::default()
        }
    }
}
pub struct ZedisKvTable<T: ZedisKvFetcher> {
    /// Reference to server state for Redis operations
    table_state: Entity<TableState<ZedisKvDelegate<T>>>,
    /// Input field state for keyword search/filter
    keyword_state: Entity<InputState>,

    items_count: usize,
    total_count: usize,
    done: bool,
    loading: bool,
    key_changed: bool,
    _subscriptions: Vec<Subscription>,
}
impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    fn new_values(server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) -> T {
        let value = server_state.read(cx).value().cloned().unwrap_or_default();
        T::new(server_state.clone(), value)
    }
    fn new_columns(columns: Vec<KvTableColumn>, window: &Window, cx: &mut Context<Self>) -> Vec<Column> {
        let window_width = window.viewport_size().width.as_f32();
        let key_tree_width = cx.global::<ZedisGlobalStore>().read(cx).key_tree_width();
        let (key_tree_width, _, _) = get_key_tree_widths(key_tree_width);
        let mut columns = columns.clone();
        columns.insert(
            0,
            KvTableColumn {
                name: INDEX_COLUMN_NAME.to_string().into(),
                width: Some(80.0),
                align: Some(TextAlign::Right),
            },
        );
        let mut rest_width = window_width - key_tree_width.as_f32() - SIDEBAR_WIDTH - 10.;
        let mut none_with_count = 0;
        for column in columns.iter() {
            if let Some(width) = column.width {
                rest_width -= width;
            } else {
                none_with_count += 1;
            }
        }

        let unit_width = if none_with_count != 0 {
            Some(rest_width / none_with_count as f32 - 5.)
        } else {
            None
        };
        for column in columns.iter_mut() {
            if column.width.is_none() {
                column.width = unit_width;
            }
        }
        columns
            .iter()
            .map(|item| {
                let name = item.name.clone();
                let mut column = Column::new(name.clone(), name.clone());
                if let Some(width) = item.width {
                    column = column.width(width);
                }
                if let Some(align) = item.align {
                    column.align = align;
                }
                column.paddings = Some(Edges {
                    top: px(2.),
                    bottom: px(2.),
                    left: px(10.),
                    right: px(10.),
                });
                column
            })
            .collect::<Vec<Column>>()
    }
    pub fn new(
        columns: Vec<KvTableColumn>,
        server_state: Entity<ZedisServerState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            let mut should_update_fetcher = false;
            match event {
                ServerEvent::ValuePaginationFinished(_) | ServerEvent::ValueLoaded(_) | ServerEvent::ValueAdded(_) => {
                    should_update_fetcher = true;
                }
                ServerEvent::KeySelected(_) => {
                    this.key_changed = true;
                }
                _ => {}
            }
            if !should_update_fetcher {
                return;
            }
            let set_values = Self::new_values(server_state.clone(), cx);
            this.loading = false;
            this.done = set_values.is_done();
            this.items_count = set_values.rows_count();
            this.total_count = set_values.count();
            this.table_state.update(cx, |this, _cx| {
                this.delegate_mut().set_fetcher(set_values);
            });
        }));

        // Initialize keyword search input field
        let keyword_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "keyword_placeholder"))
        });
        subscriptions.push(cx.subscribe(&keyword_state, |this, _model, event, cx| {
            if let InputEvent::PressEnter { .. } = &event {
                this.handle_filter(cx);
            }
        }));

        let set_values = Self::new_values(server_state.clone(), cx);
        let done = set_values.is_done();
        let items_count = set_values.rows_count();
        let total_count = set_values.count();
        let delegate = ZedisKvDelegate::new(Self::new_columns(columns, window, cx), set_values);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        info!("Creating new key value table view");
        Self {
            table_state,
            total_count,
            items_count,
            keyword_state,
            done,
            loading: false,
            key_changed: false,
            _subscriptions: subscriptions,
        }
    }
    fn handle_filter(&mut self, cx: &mut Context<Self>) {
        let keyword = self.keyword_state.read(cx).value();
        self.loading = true;
        self.table_state.update(cx, |this, cx| {
            this.delegate().fetcher().filter(keyword.clone(), cx);
        });
    }
}
impl<T: ZedisKvFetcher> Render for ZedisKvTable<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_color = cx.theme().muted_foreground;
        if self.key_changed {
            self.keyword_state.update(cx, |this, cx| {
                this.set_value(SharedString::new(""), window, cx);
            });
            self.key_changed = false;
        }
        let handle_add_value = cx.listener(move |this, _event, window, cx| {
            this.table_state.update(cx, |this, cx| {
                this.delegate().fetcher().handle_add_value(window, cx);
            });
        });
        let search_btn = Button::new("kv-table-search-btn")
            .ghost()
            .tooltip(i18n_kv_table(cx, "search_tooltip"))
            .loading(self.loading)
            .disabled(self.loading)
            .icon(IconName::Search)
            .on_click(cx.listener(|this, _, _, cx| {
                this.handle_filter(cx);
            }));

        let icon = if self.done {
            Icon::new(CustomIconName::CircleCheckBig)
        } else {
            Icon::new(CustomIconName::CircleDotDashed)
        };

        v_flex()
            .h_full()
            .w_full()
            .child(
                div().size_full().flex_1().child(
                    Table::new(&self.table_state)
                        .stripe(true) // Alternating row colors
                        .bordered(true) // Border around table
                        .scrollbar_visible(true, true),
                ),
            )
            .child(
                // Footer with search and count indicator
                h_flex()
                    .w_full()
                    .p_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("add-value-btn")
                                    .icon(CustomIconName::FilePlusCorner)
                                    .tooltip(i18n_kv_table(cx, "add_value_tooltip"))
                                    .on_click(handle_add_value),
                            )
                            .child(
                                Input::new(&self.keyword_state)
                                    .w(px(KEYWORD_INPUT_WIDTH))
                                    .suffix(search_btn)
                                    .cleanable(true),
                            )
                            .flex_1(),
                    )
                    .child(icon.text_color(text_color).mr_2())
                    .child(
                        Label::new(format!("{} / {}", self.items_count, self.total_count,))
                            .text_sm()
                            .text_color(text_color),
                    ),
            )
            .into_any_element()
    }
}

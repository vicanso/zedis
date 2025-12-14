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

use crate::components::FormDialog;
use crate::components::FormField;
use crate::components::ZedisKvFetcher;
use crate::components::open_add_value_dialog;
use crate::helpers::fast_contains_ignore_case;
use crate::states::RedisValue;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_list_editor;
use crate::views::KvTableColumn;
use crate::views::ZedisKvTable;
use gpui::App;
use gpui::Entity;
use gpui::SharedString;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui_component::WindowExt;
use std::rc::Rc;
use tracing::info;

struct ZedisListValues {
    visible_items: Vec<SharedString>,
    visible_item_indexes: Option<Vec<usize>>,

    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisListValues {
    fn recalc_visible_items(&mut self) {
        let Some(value) = self.value.list_value() else {
            return;
        };
        let keyword = value.keyword.clone().unwrap_or_default().to_lowercase();
        if keyword.is_empty() {
            self.visible_items = value.values.clone();
            self.visible_item_indexes = None;
            return;
        };

        let mut visible_item_indexes = Vec::with_capacity(10);
        let mut visible_items = Vec::with_capacity(10);
        for (index, item) in value.values.iter().enumerate() {
            if fast_contains_ignore_case(item.as_str(), &keyword) {
                visible_item_indexes.push(index);
                visible_items.push(item.clone());
            }
        }

        self.visible_items = visible_items;
        self.visible_item_indexes = Some(visible_item_indexes);
    }
}

impl ZedisKvFetcher for ZedisListValues {
    fn get(&self, row_ix: usize, _col_ix: usize) -> Option<SharedString> {
        let value = self.value.list_value()?;
        if value.keyword.is_some() {
            self.visible_items.get(row_ix).cloned()
        } else {
            value.values.get(row_ix).cloned()
        }
    }
    fn can_update(&self) -> bool {
        true
    }
    fn count(&self) -> usize {
        let Some(value) = self.value.list_value() else {
            return 0;
        };
        value.size
    }
    fn rows_count(&self) -> usize {
        if self.value.list_value().is_none() {
            return 0;
        };
        self.visible_items.len()
    }
    fn is_done(&self) -> bool {
        let Some(value) = self.value.list_value() else {
            return false;
        };
        value.values.len() == value.size
    }
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_list_value(cx);
        });
    }
    fn remove(&self, index: usize, cx: &mut App) {
        let real_index = self
            .visible_item_indexes
            .as_ref()
            .map(|indexes| indexes.get(index).copied().unwrap_or(index))
            .unwrap_or(index);
        self.server_state.update(cx, |this, cx| {
            this.remove_list_value(real_index, cx);
        });
    }
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_list_value(keyword.clone(), cx);
        });
    }
    fn handle_add_value(&self, window: &mut Window, cx: &mut App) {
        let server_state = self.server_state.clone();
        let handle_submit = Rc::new(move |values: Vec<SharedString>, window: &mut Window, cx: &mut App| {
            if values.len() != 2 {
                return false;
            }
            server_state.update(cx, |this, cx| {
                this.push_list_value(values[1].clone(), values[0].clone(), cx);
            });
            window.close_dialog(cx);
            true
        });
        let fields = vec![
            FormField::new(i18n_list_editor(cx, "positon"))
                .with_options(vec!["RPUSH".to_string().into(), "LPUSH".to_string().into()]),
            FormField::new(i18n_common(cx, "value"))
                .with_placeholder(i18n_common(cx, "value_placeholder"))
                .with_focus(),
        ];
        open_add_value_dialog(
            FormDialog {
                title: i18n_list_editor(cx, "add_value_title"),
                fields,
                handle_submit,
            },
            window,
            cx,
        );
    }
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        let mut this = Self {
            server_state,
            value,
            visible_items: Default::default(),
            visible_item_indexes: Default::default(),
        };
        this.recalc_visible_items();
        this
    }
}

pub struct ZedisListEditor {
    table_state: Entity<ZedisKvTable<ZedisListValues>>,
}
impl ZedisListEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisListValues>::new(
                vec![KvTableColumn::new("Value", None)],
                server_state.clone(),
                window,
                cx,
            )
        });
        info!("Creating new list editor view");
        Self { table_state }
    }
}

impl Render for ZedisListEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

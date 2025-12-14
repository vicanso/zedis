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
use crate::states::RedisValue;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_set_editor;
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
    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisListValues {
    fn get(&self, row_ix: usize, _col_ix: usize) -> Option<SharedString> {
        let value = self.value.list_value()?;
        value.values.get(row_ix).cloned()
    }
    fn count(&self) -> usize {
        let Some(value) = self.value.list_value() else {
            return 0;
        };
        value.size
    }
    fn rows_count(&self) -> usize {
        let Some(value) = self.value.list_value() else {
            return 0;
        };
        value.values.len()
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
        self.server_state.update(cx, |this, cx| {
            this.delete_list_item(index, cx);
        });
    }
    fn filter(&self, keyword: SharedString, _cx: &mut App) {
        // TODO
    }
    fn handle_add_value(&self, _window: &mut Window, _cx: &mut App) {
        // TODO
    }
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
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

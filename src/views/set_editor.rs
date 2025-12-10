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

use crate::components::ZedisKvFetcher;
use crate::states::RedisValue;
use crate::states::ZedisServerState;
use crate::views::KvTableColumn;
use crate::views::ZedisKvTable;
use gpui::App;
use gpui::Entity;
use gpui::SharedString;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use tracing::info;

struct ZedisSetValues {
    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisSetValues {
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        if col_ix == 0 {
            return Some((row_ix + 1).to_string().into());
        }
        let value = self.value.set_value()?;
        value.values.get(row_ix).cloned()
    }
    fn rows_count(&self) -> usize {
        let Some(value) = self.value.set_value() else {
            return 0;
        };
        value.values.len()
    }
    fn is_eof(&self) -> bool {
        let Some(value) = self.value.set_value() else {
            return false;
        };
        value.size > value.values.len()
        // true
    }

    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_set_value(cx);
        });
    }
}

pub struct ZedisSetEditor {
    /// Reference to server state for Redis operations
    table_state: Entity<ZedisKvTable<ZedisSetValues>>,
}
impl ZedisSetEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, server_state: Entity<ZedisServerState>) -> Self {
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisSetValues>::new(
                vec![KvTableColumn::new("Value", None)],
                server_state.clone(),
                window,
                cx,
            )
        });
        info!("Creating new set editor view");
        Self { table_state }
    }
}
impl Render for ZedisSetEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

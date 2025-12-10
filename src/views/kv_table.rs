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

use crate::components::{INDEX_COLUMN_NAME, ZedisKvDelegate, ZedisKvFetcher};
use crate::constants::SIDEBAR_WIDTH;
use crate::states::ServerEvent;
use crate::states::ZedisGlobalStore;
use crate::states::ZedisServerState;
use gpui::Entity;
use gpui::SharedString;
use gpui::Subscription;
use gpui::Window;
use gpui::prelude::*;
use gpui_component::PixelsExt;
use gpui_component::table::Column;
use gpui_component::table::{Table, TableState};
use gpui_component::v_flex;
use tracing::info;

#[derive(Clone)]
pub struct KvTableColumn {
    name: SharedString,
    width: Option<f32>,
}
impl KvTableColumn {
    pub fn new(name: &str, width: Option<f32>) -> Self {
        Self {
            name: name.to_string().into(),
            width,
        }
    }
}
pub struct ZedisKvTable<T: ZedisKvFetcher> {
    /// Reference to server state for Redis operations
    table_state: Entity<TableState<ZedisKvDelegate<T>>>,

    _subscriptions: Vec<Subscription>,
}
impl<T: ZedisKvFetcher> ZedisKvTable<T> {
    fn new_values(server_state: Entity<ZedisServerState>, cx: &mut Context<Self>) -> T {
        let value = server_state.read(cx).value().cloned().unwrap_or_default();
        T::new(server_state.clone(), value)
    }
    pub fn new(
        columns: Vec<KvTableColumn>,
        server_state: Entity<ZedisServerState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&server_state, |this, server_state, event, cx| {
            let should_update_fetcher = matches!(
                event,
                ServerEvent::ValuePaginationFinished(_) | ServerEvent::ValueLoaded(_)
            );
            if !should_update_fetcher {
                return;
            }
            let set_values = Self::new_values(server_state.clone(), cx);
            this.table_state.update(cx, |this, _cx| {
                this.delegate_mut().set_fetcher(set_values);
            });
        }));

        let window_width = window.viewport_size().width.as_f32();
        let key_tree_width = cx.global::<ZedisGlobalStore>().read(cx).key_tree_width().as_f32();
        let mut columns = columns.clone();
        columns.insert(
            0,
            KvTableColumn {
                name: INDEX_COLUMN_NAME.to_string().into(),
                width: Some(80.0),
            },
        );
        let mut rest_width = window_width - key_tree_width - SIDEBAR_WIDTH - 60.;
        let mut none_with_count = 0;
        for column in columns.iter() {
            if let Some(width) = column.width {
                rest_width -= width;
            } else {
                none_with_count += 1;
            }
        }

        let unit_width = if none_with_count != 0 {
            Some(rest_width / none_with_count as f32 - 10.)
        } else {
            None
        };
        for column in columns.iter_mut() {
            if column.width.is_none() {
                column.width = unit_width;
            }
        }

        let set_values = Self::new_values(server_state.clone(), cx);
        let delegate = ZedisKvDelegate::new(
            columns
                .iter()
                .map(|item| {
                    let name = item.name.clone();
                    let mut column = Column::new(name.clone(), name.clone());
                    if let Some(width) = item.width {
                        column = column.width(width);
                    }
                    column
                })
                .collect::<Vec<Column>>(),
            set_values,
        );
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        info!("Creating new key value table view");
        Self {
            table_state,
            _subscriptions: subscriptions,
        }
    }
}
impl<T: ZedisKvFetcher> Render for ZedisKvTable<T> {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w_full()
            .child(
                Table::new(&self.table_state)
                    .stripe(true) // Alternating row colors
                    .bordered(true) // Border around table
                    .scrollbar_visible(true, true),
            )
            .into_any_element()
    }
}

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
use crate::error::Error;
use crate::helpers::fast_contains_ignore_case;
use crate::states::RedisValue;
use crate::states::ServerEvent;
use crate::states::ZedisGlobalStore;
use crate::states::i18n_list_editor;
use crate::states::{RedisSetValue, ZedisServerState};
use crate::views::{INDEX_COLUMN_NAME, ZedisTableDelegate, ZedisTableFetcher};
use gpui::App;
use gpui::AsyncApp;
use gpui::Entity;
use gpui::Hsla;
use gpui::SharedString;
use gpui::Subscription;
use gpui::TextAlign;
use gpui::Window;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::radio::RadioGroup;
use gpui_component::table::{Column, ColumnFixed, ColumnSort, Table, TableDelegate, TableEvent, TableState};
use gpui_component::v_flex;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::{Disableable, IndexPath};
use gpui_component::{Icon, IconName};
use gpui_component::{WindowExt, h_flex};
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::info;
type Result<T, E = Error> = std::result::Result<T, E>;

struct ZedisSetValues {
    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisSetValues {
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }
}

impl ZedisTableFetcher for ZedisSetValues {
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        if col_ix == 0 {
            return Some((row_ix + 1).to_string().into());
        }
        let Some(value) = self.value.set_value() else {
            return None;
        };
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

    fn load_more(&self, window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_set_value(cx);
        });
        return;
    }
}

pub struct ZedisSetEditor {
    // set_values: ArcSwap<ZedisSetValues>,
    /// Reference to server state for Redis operations
    table_state: Entity<TableState<ZedisTableDelegate<ZedisSetValues>>>,

    _subscriptions: Vec<Subscription>,
}
impl ZedisSetEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, server_state: Entity<ZedisServerState>) -> Self {
        let mut subscriptions = Vec::new();
        let value = server_state.read(cx).value().cloned().unwrap_or_default();
        subscriptions.push(
            cx.subscribe(&server_state, |this, server_state, event, cx| match event {
                ServerEvent::LoadMoreValueFinish(_) => {
                    let value = server_state.read(cx).value().cloned().unwrap_or_default();
                    let set_values = ZedisSetValues::new(server_state.clone(), value);
                    this.table_state.update(cx, |this, cx| {
                        this.delegate_mut().set_fetcher(set_values);
                    });
                }
                _ => {}
            }),
        );

        let fetcher = ZedisSetValues::new(server_state.clone(), value);
        let delegate = ZedisTableDelegate::new(
            vec![INDEX_COLUMN_NAME.to_string().into(), "Value".to_string().into()],
            fetcher,
        );
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        info!("Creating new set editor view");
        Self {
            table_state,
            _subscriptions: subscriptions,
        }
    }
}
impl Render for ZedisSetEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

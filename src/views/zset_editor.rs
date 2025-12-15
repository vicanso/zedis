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
use crate::components::open_add_form_dialog;
use crate::states::RedisValue;
use crate::states::ZedisServerState;
use crate::states::i18n_common;
use crate::states::i18n_zset_editor;
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

struct ZedisZsetValues {
    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisZsetValues {
    fn get(&self, row_ix: usize, col_ix: usize) -> Option<SharedString> {
        let value = self.value.zset_value()?;
        let (value, score) = value.values.get(row_ix)?;
        if col_ix == 2 {
            Some(score.to_string().into())
        } else {
            Some(value.clone())
        }
    }
    fn count(&self) -> usize {
        let Some(value) = self.value.zset_value() else {
            return 0;
        };
        value.size
    }
    fn rows_count(&self) -> usize {
        let Some(value) = self.value.zset_value() else {
            return 0;
        };
        value.values.len()
    }

    fn can_update(&self) -> bool {
        false
    }
    fn is_done(&self) -> bool {
        let Some(value) = self.value.zset_value() else {
            return false;
        };
        if value.values.len() == value.size {
            return true;
        }
        value.done
    }
    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_zset_value(cx);
        });
    }
    fn remove(&self, index: usize, cx: &mut App) {
        let Some(zset) = self.value.zset_value() else {
            return;
        };
        let Some(value) = zset.values.get(index) else {
            return;
        };
        self.server_state.update(cx, |this, cx| {
            this.remove_zset_value(value.0.clone(), cx);
        });
    }
    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_zset_value(keyword.clone(), cx);
        });
    }
    fn handle_add_value(&self, window: &mut Window, cx: &mut App) {
        let server_state = self.server_state.clone();
        let handle_submit = Rc::new(move |values: Vec<SharedString>, window: &mut Window, cx: &mut App| {
            if values.len() != 2 {
                return false;
            }
            let score = values[1].parse::<f64>().unwrap_or(0.0);
            server_state.update(cx, |this, cx| {
                this.add_zset_value(values[0].clone(), score, cx);
            });
            window.close_dialog(cx);
            true
        });
        let fields = vec![
            FormField::new(i18n_common(cx, "value"))
                .with_placeholder(i18n_common(cx, "value_placeholder"))
                .with_focus(),
            FormField::new(i18n_common(cx, "score"))
                .with_placeholder(i18n_common(cx, "score_placeholder"))
                .with_focus(),
        ];
        open_add_form_dialog(
            FormDialog {
                title: i18n_zset_editor(cx, "add_value_title"),
                fields,
                handle_submit,
            },
            window,
            cx,
        );
    }
    fn new(server_state: Entity<ZedisServerState>, value: RedisValue) -> Self {
        Self { server_state, value }
    }
}

pub struct ZedisZsetEditor {
    table_state: Entity<ZedisKvTable<ZedisZsetValues>>,
}
impl ZedisZsetEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_state = cx.new(|cx| {
            ZedisKvTable::<ZedisZsetValues>::new(
                vec![
                    KvTableColumn::new("Value", None),
                    KvTableColumn::new("Score", Some(100.)),
                ],
                server_state.clone(),
                window,
                cx,
            )
        });
        Self { table_state }
    }
}

impl Render for ZedisZsetEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.table_state.clone()).into_any_element()
    }
}

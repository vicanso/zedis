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
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use std::cell::Cell;
use std::rc::Rc;
use tracing::info;

struct ZedisSetValues {
    value: RedisValue,
    server_state: Entity<ZedisServerState>,
}

impl ZedisKvFetcher for ZedisSetValues {
    fn handle_add_value(&self, window: &mut Window, cx: &mut App) {
        let value_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_common(cx, "value_placeholder"))
        });
        let focus_handle_done = Cell::new(false);
        let server_state = self.server_state.clone();
        let value_state_clone = value_state.clone();
        let handle_submit = Rc::new(move |window: &mut Window, cx: &mut App| {
            server_state.update(cx, |this, cx| {
                this.add_set_value(value_state_clone.read(cx).value(), cx);
            });
            window.close_dialog(cx);
            true
        });

        window.open_dialog(cx, move |dialog, window, cx| {
            dialog
                .title(i18n_set_editor(cx, "add_value_title"))
                .overlay(true)
                .overlay_closable(true)
                .child({
                    if !focus_handle_done.get() {
                        value_state.clone().update(cx, |this, cx| {
                            this.focus(window, cx);
                        });
                        focus_handle_done.set(true);
                    }
                    v_form().child(field().label(i18n_common(cx, "value")).child(Input::new(&value_state)))
                })
                .on_ok({
                    let handle = handle_submit.clone();
                    move |_, window, cx| handle(window, cx)
                })
                .footer({
                    let handle = handle_submit.clone();
                    move |_, _, _, cx| {
                        let confirm_label = i18n_common(cx, "confirm");
                        let cancel_label = i18n_common(cx, "cancel");
                        vec![
                            // Submit button - validates and saves server configuration
                            Button::new("ok").primary().label(confirm_label).on_click({
                                let handle = handle.clone();
                                move |_, window, cx| {
                                    handle.clone()(window, cx);
                                }
                            }),
                            // Cancel button - closes dialog without saving
                            Button::new("cancel").label(cancel_label).on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                        ]
                    }
                })
        });
    }
    fn is_initial_load(&self) -> bool {
        self.value.set_value().is_some()
    }
    fn count(&self) -> usize {
        let Some(value) = self.value.set_value() else {
            return 0;
        };
        value.size
    }
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
        !self.is_done()
    }
    fn is_done(&self) -> bool {
        let Some(value) = self.value.set_value() else {
            return false;
        };
        value.done
    }

    fn load_more(&self, _window: &mut Window, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.load_more_set_value(cx);
        });
    }

    fn filter(&self, keyword: SharedString, cx: &mut App) {
        self.server_state.update(cx, |this, cx| {
            this.filter_set_value(keyword.clone(), cx);
        });
    }
}

pub struct ZedisSetEditor {
    /// Reference to server state for Redis operations
    table_state: Entity<ZedisKvTable<ZedisSetValues>>,
}
impl ZedisSetEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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

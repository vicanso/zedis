// Copyright 2026 Tree xie.
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

use crate::db::{ProtoConfig, ProtoManager};
use crate::helpers::get_font_family;
use crate::states::ZedisServerState;
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::Button;
use gpui_component::h_flex;
use gpui_component::highlighter::Language;
use gpui_component::label::Label;
use gpui_component::radio::RadioGroup;
use gpui_component::table::{Column, Table, TableDelegate, TableState};
use gpui_component::{
    IndexPath,
    form::{field, v_form},
    input::{Input, InputState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct KeyValueOption {
    key: SharedString,
    value: SharedString,
}

impl KeyValueOption {
    pub fn new(key: SharedString, value: SharedString) -> Self {
        Self { key, value }
    }
}
impl SelectItem for KeyValueOption {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.key.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.value
    }
}

struct ProtoTableDelegate {
    data: Arc<Vec<ProtoConfig>>,
    columns: Vec<Column>,
    servers: Vec<KeyValueOption>,
}

impl ProtoTableDelegate {
    fn new(data: Vec<ProtoConfig>, servers: Vec<KeyValueOption>) -> Self {
        let columns = vec![
            Column::new("server_name", "Server Name").width(px(150.)),
            Column::new("name", "Name").width(px(150.)),
            Column::new("match_pattern", "Match Pattern").width(px(200.)),
            Column::new("mode", "Mode").width(px(100.)),
            Column::new("target_message", "Target Message").width(px(200.)),
        ];
        Self {
            data: Arc::new(data),
            columns,
            servers,
        }
    }
}

impl TableDelegate for ProtoTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.data.len()
    }

    fn column(&self, index: usize, _: &App) -> &Column {
        &self.columns[index]
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let proto = self.data.get(row_ix);
        let text = if let Some(proto) = proto {
            match col_ix {
                0 => {
                    // Convert server_id to server_name
                    self.servers
                        .iter()
                        .find(|s| s.value.as_ref() == proto.server_id)
                        .map(|s| s.key.to_string())
                        .unwrap_or_else(|| proto.server_id.clone())
                }
                1 => proto.name.clone(),
                2 => proto.match_pattern.clone(),
                3 => format!("{:?}", proto.mode),
                4 => proto.target_message.clone().unwrap_or_default(),
                _ => String::new(),
            }
        } else {
            String::new()
        };

        div().size_full().child(Label::new(text))
    }
}

enum ViewMode {
    Table,
    Edit,
}

pub struct ZedisProtoEditor {
    server_select_state: Entity<SelectState<Vec<KeyValueOption>>>,
    name_state: Entity<InputState>,
    match_pattern_state: Entity<InputState>,
    match_mode_select_state: Entity<usize>,
    content_state: Entity<InputState>,
    target_message_state: Entity<InputState>,

    server_id: SharedString,
    view_mode: ViewMode,
    table_state: Entity<TableState<ProtoTableDelegate>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisProtoEditor {
    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_id = server_state.read(cx).server_id().to_string();
        let protos = ProtoManager::list_protos();
        let mut subscriptions = Vec::new();
        let servers = server_state
            .read(cx)
            .servers()
            .unwrap_or_default()
            .iter()
            .map(|server| KeyValueOption::new(server.name.clone().into(), server.id.clone().into()))
            .collect::<Vec<_>>();
        let name_state = cx.new(|cx| InputState::new(window, cx));
        let match_pattern_state = cx.new(|cx| InputState::new(window, cx));
        let content_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(Language::from_str("json").name())
                .line_number(true)
                .indent_guides(true)
                .searchable(true)
                .soft_wrap(true)
        });
        let target_message_state = cx.new(|cx| InputState::new(window, cx));
        let match_mode_select_state = cx.new(|_cx| 0_usize);
        let found = servers
            .iter()
            .position(|item| item.value == server_id)
            .map(IndexPath::new);
        let servers_for_delegate = servers.clone();
        let server_select_state = cx.new(|cx| SelectState::new(servers, found, window, cx));
        subscriptions.push(cx.subscribe(&server_select_state, |this, _, event, _cx| {
            if let SelectEvent::Confirm(Some(server_id)) = event {
                this.server_id = server_id.clone();
            }
        }));

        let delegate = ProtoTableDelegate::new(protos.clone(), servers_for_delegate);
        let table_state = cx.new(|cx| TableState::new(delegate, window, cx));

        Self {
            server_select_state,
            name_state,
            match_pattern_state,
            match_mode_select_state,
            content_state,
            target_message_state,
            view_mode: ViewMode::Table,
            table_state,
            server_id: SharedString::default(),
            _subscriptions: subscriptions,
        }
    }
    fn handle_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let name = self.name_state.read(cx).value();
        let match_pattern = self.match_pattern_state.read(cx).value();
        let match_mode = self.match_mode_select_state.read(cx);
        let content = self.content_state.read(cx).value();
        let target_message = self.target_message_state.read(cx).value();
        if server_id.is_empty() || name.is_empty() || match_pattern.is_empty() || content.is_empty() {
            return;
        }
        let id = Uuid::now_v7().to_string();
        let config = ProtoConfig {
            server_id: server_id.to_string(),
            name: name.to_string(),
            match_pattern: match_pattern.to_string(),
            mode: (*match_mode).into(),
            content: Some(content.to_string()),
            target_message: Some(target_message.to_string()),
        };
        cx.spawn(async move |_handle, cx| {
            cx.background_spawn(async move {
                if let Err(e) = ProtoManager::add_proto(&id, config) {
                    error!(error = %e, "add proto fail",);
                }
            })
            .await;
        })
        .detach();
    }
    fn render_edit_form(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let match_mode_select_state_clone = self.match_mode_select_state.clone();
        let match_mode_select_state = self.match_mode_select_state.read(cx);
        v_flex()
            .p_5()
            .size_full()
            .gap_3()
            .child(
                v_form()
                    .w_full()
                    .columns(2)
                    .child(field().label("Server").child(Select::new(&self.server_select_state)))
                    .child(field().label("Name").child(Input::new(&self.name_state)))
                    .child(
                        field()
                            .label("Match Pattern")
                            .child(Input::new(&self.match_pattern_state)),
                    )
                    .child(
                        field().label("Match Mode").child(
                            RadioGroup::horizontal("match-mode-group")
                                .mt(px(8.))
                                .children(vec!["Prefix", "Suffix", "Regex", "Exact"])
                                .selected_index(Some(*match_mode_select_state))
                                .on_click(move |index, _, cx| {
                                    match_mode_select_state_clone.update(cx, |state, _cx| {
                                        *state = *index;
                                    });
                                }),
                        ),
                    )
                    .child(
                        field()
                            .label("Target Message")
                            .child(Input::new(&self.target_message_state))
                            .col_span(2),
                    ),
            )
            .child(
                v_flex().w_full().flex_1().h_full().child(
                    v_flex().size_full().child(Label::new("Content").text_sm()).child(
                        div().flex_1().size_full().child(
                            Input::new(&self.content_state)
                                .p_0()
                                .w_full()
                                .h_full()
                                .font_family(get_font_family())
                                .focus_bordered(false),
                        ),
                    ),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("proto-editor-btn-cancel")
                            .label("Cancel")
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, _cx| {
                                this.view_mode = ViewMode::Table;
                            })),
                    )
                    .child(
                        Button::new("proto-editor-btn-save")
                            .label("Save")
                            .flex_1()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.handle_save(window, cx);
                            })),
                    ),
            )
    }
    fn render_table_view(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new("Proto Configurations").text_xl()),
            )
            .child(
                div().flex_1().w_full().child(
                    Table::new(&self.table_state)
                        .stripe(true)
                        .bordered(true)
                        .scrollbar_visible(true, true),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .p_2()
                    .child(
                        Button::new("add-proto-bottom-btn")
                            .label("Add Proto")
                            .on_click(cx.listener(|this, _, _, _cx| {
                                this.view_mode = ViewMode::Edit;
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ZedisProtoEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.view_mode {
            ViewMode::Table => self.render_table_view(window, cx).into_any_element(),
            ViewMode::Edit => self.render_edit_form(window, cx).into_any_element(),
        }
    }
}

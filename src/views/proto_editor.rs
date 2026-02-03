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

use crate::assets::CustomIconName;
use crate::db::{ProtoConfig, ProtoManager};
use crate::error::Error;
use crate::helpers::get_font_family;
use crate::states::ZedisServerState;
use crate::states::i18n_proto_editor;
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::label::Label;
use gpui_component::radio::RadioGroup;
use gpui_component::table::{Column, Table, TableDelegate, TableState};
use gpui_component::{IconName, h_flex};
use gpui_component::{
    IndexPath, WindowExt,
    alert::Alert,
    form::{field, v_form},
    input::{Input, InputEvent, InputState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    text::TextView,
    v_flex,
};
use rust_i18n::t;
use std::collections::HashMap;
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

type OnProtoAction = Arc<dyn Fn(usize, &mut Window, &mut Context<TableState<ProtoTableDelegate>>) + Send + Sync>;

struct ProtoTableDelegate {
    data: Arc<Vec<(String, ProtoConfig)>>,
    columns: Vec<Column>,
    servers: Vec<KeyValueOption>,
    on_edit: OnProtoAction,
    on_delete: OnProtoAction,
}

impl ProtoTableDelegate {
    fn new<F1, F2>(
        data: Arc<Vec<(String, ProtoConfig)>>,
        servers: Vec<KeyValueOption>,
        columns: Vec<Column>,
        on_edit: F1,
        on_delete: F2,
    ) -> Self
    where
        F1: Fn(usize, &mut Window, &mut Context<TableState<ProtoTableDelegate>>) + Send + Sync + 'static,
        F2: Fn(usize, &mut Window, &mut Context<TableState<ProtoTableDelegate>>) + Send + Sync + 'static,
    {
        Self {
            data,
            columns,
            servers,
            on_edit: Arc::new(on_edit),
            on_delete: Arc::new(on_delete),
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
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let proto = self.data.get(row_ix);
        if col_ix == self.columns_count(cx) - 1 {
            let on_edit = self.on_edit.clone();
            let on_delete = self.on_delete.clone();
            return div().size_full().flex().items_center().child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("edit-proto-btn")
                            .icon(CustomIconName::FilePenLine)
                            .ghost()
                            .on_click(cx.listener(move |_this, _, window, cx| {
                                (on_edit)(row_ix, window, cx);
                            })),
                    )
                    .child(
                        Button::new("delete-proto-btn")
                            .icon(CustomIconName::X)
                            .ghost()
                            .on_click(cx.listener(move |_this, _, window, cx| {
                                (on_delete)(row_ix, window, cx);
                            })),
                    ),
            );
        }

        let text = if let Some((_, proto)) = proto {
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

        div().size_full().flex().items_center().child(Label::new(text))
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
    includes_state: Entity<InputState>,
    content_state: Entity<InputState>,
    target_message_state: Entity<SelectState<Vec<String>>>,
    field_errors: Entity<HashMap<String, SharedString>>,

    target_messages: Option<Vec<String>>,
    target_message_selected_index: Option<IndexPath>,

    protos: Arc<Vec<(String, ProtoConfig)>>,
    servers: Vec<KeyValueOption>,
    server_id: SharedString,
    edit_proto_id: Option<String>,
    view_mode: ViewMode,
    table_state: Entity<TableState<ProtoTableDelegate>>,
    needs_table_recreate: Option<bool>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisProtoEditor {
    fn create_table_state(
        protos: Arc<Vec<(String, ProtoConfig)>>,
        servers: Vec<KeyValueOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<ProtoTableDelegate>> {
        let view_update_entity = cx.entity();
        let view_delete_entity = cx.entity();

        let on_edit = move |row_ix: usize, window: &mut Window, cx: &mut Context<TableState<ProtoTableDelegate>>| {
            view_update_entity.update(cx, |this, cx| {
                this.handle_update(row_ix, window, cx);
            });
        };

        let on_delete = move |row_ix: usize, window: &mut Window, cx: &mut Context<TableState<ProtoTableDelegate>>| {
            view_delete_entity.update(cx, |this, cx| {
                this.handle_delete(row_ix, window, cx);
            });
        };
        let columns = vec![
            Column::new("server_name", i18n_proto_editor(cx, "server_name")).width(px(150.)),
            Column::new("name", i18n_proto_editor(cx, "name")).width(px(150.)),
            Column::new("match_pattern", i18n_proto_editor(cx, "match_pattern")).width(px(200.)),
            Column::new("mode", i18n_proto_editor(cx, "mode")).width(px(100.)),
            Column::new("target_message", i18n_proto_editor(cx, "target_message")).width(px(200.)),
            Column::new("actions", i18n_proto_editor(cx, "actions")).width(px(150.)),
        ];

        let delegate = ProtoTableDelegate::new(protos, servers, columns, on_edit, on_delete);
        cx.new(|cx| TableState::new(delegate, window, cx))
    }

    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_id = server_state.read(cx).server_id().to_string();
        let protos = ProtoManager::list_protos_with_id();
        let mut subscriptions = Vec::new();
        let servers = server_state
            .read(cx)
            .servers()
            .unwrap_or_default()
            .iter()
            .map(|server| KeyValueOption::new(server.name.clone().into(), server.id.clone().into()))
            .collect::<Vec<_>>();
        let name_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_proto_editor(cx, "name_placeholder"))
        });
        let match_pattern_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_proto_editor(cx, "match_pattern_placeholder"))
        });
        let includes_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_proto_editor(cx, "includes_placeholder"))
        });
        let content_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_proto_editor(cx, "content_placeholder"))
                .auto_grow(2, 10)
        });
        let target_message_state = cx.new(|cx| SelectState::new(vec![], None, window, cx));
        let match_mode_select_state = cx.new(|_cx| 0_usize);
        let found = servers
            .iter()
            .position(|item| item.value == server_id)
            .map(IndexPath::new);
        let servers_for_delegate = servers.clone();
        let server_select_state = cx.new(|cx| SelectState::new(servers, found, window, cx));
        let field_errors = cx.new(|_cx| HashMap::new());

        let field_errors_clone = field_errors.clone();
        subscriptions.push(cx.subscribe(&server_select_state, move |this, view, event, cx| {
            if let SelectEvent::Confirm(Some(server_id)) = event {
                this.server_id = server_id.clone();
                let id = view.entity_id().to_string();
                if field_errors_clone.read(cx).get(&id).is_some() {
                    field_errors_clone.update(cx, |state, _cx| {
                        state.remove(&id);
                    });
                }
            }
        }));
        for item in [name_state.clone(), match_pattern_state.clone()] {
            subscriptions.push(
                cx.subscribe_in(&item.clone(), window, move |view, state, event, _window, cx| {
                    if let InputEvent::Blur = event {
                        let id = state.entity_id().to_string();
                        if view.field_errors.read(cx).get(&id).is_some() {
                            view.field_errors.update(cx, |state, _cx| {
                                state.remove(&id);
                            });
                        }
                    }
                }),
            );
        }
        subscriptions.push(
            cx.subscribe_in(&content_state, window, move |view, state, event, window, cx| {
                let id = state.entity_id().to_string();
                match event {
                    InputEvent::Blur => {
                        view.update_target_message_select_state("".to_string(), window, cx);
                    }
                    InputEvent::Focus => {
                        if view.field_errors.read(cx).get(&id).is_some() {
                            view.field_errors.update(cx, |state, _cx| {
                                state.remove(&id);
                            });
                        }
                    }
                    _ => {}
                }
            }),
        );

        let protos = Arc::new(protos);
        let table_state = Self::create_table_state(protos.clone(), servers_for_delegate.clone(), window, cx);

        Self {
            server_select_state,
            name_state,
            match_pattern_state,
            match_mode_select_state,
            includes_state,
            content_state,
            target_message_state,
            view_mode: ViewMode::Table,
            table_state,
            protos,
            servers: servers_for_delegate,
            server_id: server_id.into(),
            needs_table_recreate: None,
            edit_proto_id: None,
            field_errors,
            target_message_selected_index: None,
            target_messages: None,
            _subscriptions: subscriptions,
        }
    }

    fn handle_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let name = self.name_state.read(cx).value();
        let match_pattern = self.match_pattern_state.read(cx).value();
        let match_mode = self.match_mode_select_state.read(cx).to_owned();
        let content = self.content_state.read(cx).value();
        let includes = self.includes_state.read(cx).value();
        let target_message = self
            .target_message_state
            .read(cx)
            .selected_value()
            .map(|item| item.to_string())
            .unwrap_or_default();
        let field_errors = self.field_errors.clone();
        field_errors.update(cx, |state, _cx| {
            state.clear();
        });

        if server_id.is_empty() {
            field_errors.update(cx, |state, _cx| {
                state.insert(
                    self.server_select_state.entity_id().to_string(),
                    "server is required".into(),
                );
            });
        }
        if name.is_empty() {
            field_errors.update(cx, |state, _cx| {
                state.insert(self.name_state.entity_id().to_string(), "name is required".into());
            });
        }
        if match_pattern.is_empty() {
            field_errors.update(cx, |state, _cx| {
                state.insert(
                    self.match_pattern_state.entity_id().to_string(),
                    "match pattern is required".into(),
                );
            });
        }
        if !field_errors.read(cx).is_empty() {
            return;
        }
        let content = if content.is_empty() {
            None
        } else {
            Some(content.to_string())
        };
        let includes = if includes.is_empty() {
            None
        } else {
            Some(includes.to_string())
        };

        let id = self.edit_proto_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
        let config = ProtoConfig {
            server_id: server_id.to_string(),
            name: name.to_string(),
            match_pattern: match_pattern.to_string(),
            mode: match_mode.into(),
            includes,
            content,
            target_message: Some(target_message.to_string()),
        };
        cx.spawn(async move |handle, cx| {
            let result: Result<(String, ProtoConfig), Error> = cx
                .background_spawn(async move {
                    ProtoManager::upsert_proto(&id, config.clone())?;
                    Ok((id.to_string(), config))
                })
                .await;
            match result {
                Ok((id, config)) => {
                    let _ = handle.update(cx, |this, cx| {
                        // Update protos: replace if exists, otherwise add new
                        let mut new_protos = this.protos.as_ref().clone();
                        if let Some(pos) = new_protos.iter().position(|(existing_id, _)| existing_id == &id) {
                            // Replace existing proto
                            new_protos[pos] = (id, config);
                        } else {
                            // Add new proto
                            new_protos.push((id, config));
                        }
                        this.protos = Arc::new(new_protos);

                        // Mark for recreation of table on next render
                        this.needs_table_recreate = Some(true);
                        this.view_mode = ViewMode::Table;
                        cx.notify();
                    });
                }
                Err(e) => {
                    error!(error = %e, "add proto fail",);
                }
            }
        })
        .detach();
    }
    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_proto_id = None;
        self.name_state.update(cx, |state, cx| {
            state.set_value(String::new(), window, cx);
        });
        self.match_pattern_state.update(cx, |state, cx| {
            state.set_value(String::new(), window, cx);
        });
        self.match_mode_select_state.update(cx, |state, _cx| {
            *state = 0;
        });
        self.target_message_state.update(cx, |state, cx| {
            state.set_selected_index(None, window, cx);
        });
        self.content_state.update(cx, |state, cx| {
            state.set_value(String::new(), window, cx);
        });
    }
    fn update_target_message_select_state(
        &mut self,
        target_message: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let content = self.content_state.read(cx).value();
        let includes = self.includes_state.read(cx).value();
        cx.spawn(async move |handle, cx| {
            let result = cx
                .background_spawn(async move { ProtoManager::parse_protobuf(&content, &includes) })
                .await;
            match result {
                Ok((_, messages)) => {
                    let found = messages
                        .iter()
                        .position(|item| *item == target_message)
                        .map(IndexPath::new);
                    let _ = handle.update(cx, move |this, cx| {
                        this.target_messages = Some(messages);
                        this.target_message_selected_index = found;
                        cx.notify();
                    });
                }
                Err(e) => {
                    error!(error = %e, "invalid protobuf",);
                }
            };
        })
        .detach();
    }
    fn handle_update(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.protos.get(row_ix) else {
            return;
        };
        let Ok(proto) = ProtoManager::get_proto(id) else {
            return;
        };
        self.edit_proto_id = Some(id.clone());
        let selected_index = self
            .servers
            .iter()
            .position(|s| s.value == proto.server_id)
            .map(IndexPath::new);
        self.server_id = proto.server_id.into();
        self.server_select_state.update(cx, |state, cx| {
            state.set_selected_index(selected_index, window, cx);
        });
        self.name_state.update(cx, |state, cx| {
            state.set_value(proto.name.clone(), window, cx);
        });
        self.match_pattern_state.update(cx, |state, cx| {
            state.set_value(proto.match_pattern.clone(), window, cx);
        });
        self.match_mode_select_state.update(cx, |state, _cx| {
            *state = proto.mode.clone().into();
        });
        let includes = proto.includes.clone().unwrap_or_default();
        self.includes_state.update(cx, |state, cx| {
            state.set_value(includes, window, cx);
        });
        let content = proto.content.clone().unwrap_or_default();

        let target_message = proto.target_message.clone().unwrap_or_default();
        self.content_state.update(cx, |state, cx| {
            state.set_value(content, window, cx);
        });
        self.view_mode = ViewMode::Edit;

        self.update_target_message_select_state(target_message, window, cx);
    }
    fn handle_delete(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, proto)) = self.protos.get(row_ix) else {
            return;
        };
        let name = proto.name.clone();

        let id = id.to_string();
        let view_handle = cx.entity();
        window.open_dialog(cx, move |dialog, _, _cx| {
            let id = id.clone();
            let view_handle = view_handle.clone();
            let text = t!("remove_proto_prompt", name = name).to_string();
            dialog.confirm().child(text).on_ok(move |_, _window, cx| {
                let id = id.clone();
                let view_handle = view_handle.clone();
                cx.spawn(async move |cx| {
                    let result: Result<String, Error> = cx
                        .background_spawn({
                            let id = id.clone();
                            async move {
                                ProtoManager::delete_proto(&id)?;
                                Ok(id)
                            }
                        })
                        .await;
                    match result {
                        Ok(deleted_id) => {
                            let _ = view_handle.update(cx, |this, cx| {
                                // Remove deleted proto from the list
                                let new_protos: Vec<_> = this
                                    .protos
                                    .iter()
                                    .filter(|(id, _)| id != &deleted_id)
                                    .cloned()
                                    .collect();
                                this.protos = Arc::new(new_protos);

                                // Mark for recreation of table on next render
                                this.needs_table_recreate = Some(true);
                                cx.notify();
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "delete proto fail",);
                        }
                    }
                })
                .detach();
                true
            })
        });
    }
    fn render_edit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child(
                        field()
                            .label(i18n_proto_editor(cx, "server_name"))
                            .required(true)
                            .child(Select::new(&self.server_select_state)),
                    )
                    .child(
                        field()
                            .label(i18n_proto_editor(cx, "name"))
                            .required(true)
                            .child(Input::new(&self.name_state)),
                    )
                    .child(
                        field()
                            .label(i18n_proto_editor(cx, "match_pattern"))
                            .required(true)
                            .child(Input::new(&self.match_pattern_state)),
                    )
                    .child(
                        field().label(i18n_proto_editor(cx, "mode")).required(true).child(
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
                            .col_span(2)
                            .label(i18n_proto_editor(cx, "includes"))
                            .child(Input::new(&self.includes_state)),
                    )
                    .child(
                        field()
                            .col_span(2)
                            .label(i18n_proto_editor(cx, "content"))
                            .required(true)
                            .child(Input::new(&self.content_state).w_full().font_family(get_font_family())),
                    )
                    .child(
                        field()
                            .label(i18n_proto_editor(cx, "target_message"))
                            .child(Select::new(&self.target_message_state)),
                    ),
            )
            .child(v_flex().w_full().flex_1().h_full())
            .when(!self.field_errors.read(cx).is_empty(), |this| {
                let title = i18n_proto_editor(cx, "field_errors_title");
                let list = self
                    .field_errors
                    .read(cx)
                    .values()
                    .map(|value| format!("- {value}"))
                    .collect::<Vec<String>>()
                    .join("\n");
                let markdown = t!("proto_editor.field_errors_message", errors = list);
                this.child(
                    Alert::error(
                        "proto-editor-form-errors",
                        TextView::markdown("proto-editor-form-errors-message", markdown, window, cx),
                    )
                    .title(title)
                    .mt_4(),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("proto-editor-btn-cancel")
                            .icon(IconName::CircleX)
                            .label(i18n_proto_editor(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _, _cx| {
                                this.view_mode = ViewMode::Table;
                            })),
                    )
                    .child(
                        Button::new("proto-editor-btn-save")
                            .primary()
                            .icon(CustomIconName::Save)
                            .label(i18n_proto_editor(cx, "save"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.handle_save(window, cx);
                            })),
                    ),
            )
    }
    fn render_table_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(true) = self.needs_table_recreate.take() {
            self.table_state = Self::create_table_state(self.protos.clone(), self.servers.clone(), window, cx);
        }
        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new(i18n_proto_editor(cx, "title")).text_xl()),
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
                h_flex().w_full().justify_end().p_2().child(
                    Button::new("add-proto-bottom-btn")
                        .primary()
                        .icon(CustomIconName::FilePlusCorner)
                        .label(i18n_proto_editor(cx, "add"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reset_form(window, cx);
                            this.view_mode = ViewMode::Edit;
                        })),
                ),
            )
            .into_any_element()
    }
}

impl Render for ZedisProtoEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(target_messages) = self.target_messages.take() {
            self.target_message_state.update(cx, |state, cx| {
                state.set_items(target_messages, window, cx);
            });
        }
        if let Some(target_message_selected_index) = self.target_message_selected_index.take() {
            self.target_message_state.update(cx, |state, cx| {
                state.set_selected_index(Some(target_message_selected_index), window, cx);
            });
        }
        match self.view_mode {
            ViewMode::Table => self.render_table_view(window, cx).into_any_element(),
            ViewMode::Edit => self.render_edit_form(window, cx).into_any_element(),
        }
    }
}

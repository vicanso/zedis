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
use crate::connection::get_servers;
use crate::db::{MatchMode, ScriptConfig, ScriptManager};
use crate::error::Error;
use crate::helpers::get_mono_font_family;
use crate::states::ZedisGlobalStore;
use crate::states::i18n_script_editor;
use crate::states::{ZedisServerState, dialog_button_props};
use gpui::{App, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::label::Label;
use gpui_component::radio::RadioGroup;
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme, IconName, Sizable, h_flex};
use gpui_component::{
    alert::Alert,
    form::{field, v_form},
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    text::TextView,
    v_flex,
};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;
use zedis_ui::ZedisDialog;

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

type OnAction = Arc<dyn Fn(usize, &mut Window, &mut Context<TableState<ScriptTableDelegate>>) + Send + Sync>;

struct ScriptTableDelegate {
    data: Arc<Vec<(String, ScriptConfig)>>,
    columns: Vec<Column>,
    servers: Vec<KeyValueOption>,
    on_edit: OnAction,
    on_delete: OnAction,
}

impl ScriptTableDelegate {
    fn new<F1, F2>(
        data: Arc<Vec<(String, ScriptConfig)>>,
        servers: Vec<KeyValueOption>,
        columns: Vec<Column>,
        on_edit: F1,
        on_delete: F2,
    ) -> Self
    where
        F1: Fn(usize, &mut Window, &mut Context<TableState<ScriptTableDelegate>>) + Send + Sync + 'static,
        F2: Fn(usize, &mut Window, &mut Context<TableState<ScriptTableDelegate>>) + Send + Sync + 'static,
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

impl TableDelegate for ScriptTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.data.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        self.columns[index].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let item = self.data.get(row_ix);
        if col_ix == self.columns_count(cx) - 1 {
            let on_edit = self.on_edit.clone();
            let on_delete = self.on_delete.clone();
            return div().size_full().flex().items_center().child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("edit-sv-btn")
                            .icon(CustomIconName::FilePenLine)
                            .ghost()
                            .on_click(cx.listener(move |_, _, window, cx| {
                                (on_edit)(row_ix, window, cx);
                            })),
                    )
                    .child(
                        Button::new("delete-sv-btn")
                            .icon(CustomIconName::X)
                            .ghost()
                            .on_click(cx.listener(move |_, _, window, cx| {
                                (on_delete)(row_ix, window, cx);
                            })),
                    ),
            );
        }

        let text = if let Some((_, cfg)) = item {
            match col_ix {
                0 => self
                    .servers
                    .iter()
                    .find(|s| s.value.as_ref() == cfg.server_id)
                    .map(|s| s.key.to_string())
                    .unwrap_or_else(|| cfg.server_id.clone()),
                1 => cfg.name.clone(),
                2 => cfg.shell_command.clone(),
                3 => cfg.match_pattern.clone(),
                4 => format!("{:?}", cfg.mode),
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

/// `(label, command)` starter templates for the shell-command field, covering
/// serialization formats that have no built-in decoder. Labels are tool names
/// (not translated); commands assume the tool is on PATH — the row's i18n
/// label says so. Invocations differ per platform where the stock tooling
/// does (`python3` vs `python`, `xxd` vs PowerShell's `Format-Hex`).
fn decode_templates() -> [(&'static str, &'static str); 9] {
    let base64 = if cfg!(windows) {
        "powershell -command \"[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String((Get-Content -Raw '{RAW_FILE}')))\""
    } else {
        "base64 --decode {RAW_FILE}"
    };
    let pickle = if cfg!(windows) {
        "python -c \"import pickle,pprint;pprint.pprint(pickle.load(open('{RAW_FILE}','rb')))\""
    } else {
        "python3 -c \"import pickle,pprint;pprint.pprint(pickle.load(open('{RAW_FILE}','rb')))\""
    };
    let hex_dump = if cfg!(windows) {
        "powershell -command \"Format-Hex '{RAW_FILE}'\""
    } else {
        "xxd {RAW_FILE}"
    };
    // ZIP is an archive, not a stream — native decode can't know which entry
    // to show, so it stays a viewer concern. `-p`/`-O` cat every entry to
    // stdout; Windows 10+ ships bsdtar (`tar`), which reads zip natively.
    let unzip = if cfg!(windows) {
        "tar -xOf {RAW_FILE}"
    } else {
        "unzip -p {RAW_FILE}"
    };
    [
        ("Base64", base64),
        ("Python Pickle", pickle),
        (
            // Rails puts Marshal-dumped objects in Redis caches/sessions.
            "Ruby Marshal",
            "ruby -e \"require 'pp'; pp Marshal.load(File.binread('{RAW_FILE}'))\"",
        ),
        (
            "PHP serialize",
            "php -r \"print_r(unserialize(file_get_contents('{RAW_FILE}')));\"",
        ),
        ("Protobuf (decode_raw)", "protoc --decode_raw < {RAW_FILE}"),
        ("ZIP", unzip),
        // Brotli has no built-in decoder (unlike gzip/zstd/snappy/LZ4).
        ("Brotli", "brotli -dc {RAW_FILE}"),
        ("Hex dump", hex_dump),
        ("Java serialized", "jdeserialize {RAW_FILE}"),
    ]
}

pub struct ZedisScriptEditor {
    server_select_state: Entity<SelectState<Vec<KeyValueOption>>>,
    name_state: Entity<InputState>,
    shell_command_state: Entity<TextareaState>,
    match_pattern_state: Entity<InputState>,
    match_mode_select_state: Entity<usize>,
    field_errors: Entity<HashMap<String, SharedString>>,

    items: Arc<Vec<(String, ScriptConfig)>>,
    servers: Vec<KeyValueOption>,
    server_id: SharedString,
    edit_id: Option<String>,
    view_mode: ViewMode,
    table_state: Entity<TableState<ScriptTableDelegate>>,
    needs_table_recreate: Option<bool>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisScriptEditor {
    fn create_table_state(
        items: Arc<Vec<(String, ScriptConfig)>>,
        servers: Vec<KeyValueOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<ScriptTableDelegate>> {
        let view_edit = cx.entity();
        let view_delete = cx.entity();

        let on_edit = move |row_ix: usize, window: &mut Window, cx: &mut Context<TableState<ScriptTableDelegate>>| {
            view_edit.update(cx, |this, cx| this.handle_update(row_ix, window, cx));
        };
        let on_delete = move |row_ix: usize, window: &mut Window, cx: &mut Context<TableState<ScriptTableDelegate>>| {
            view_delete.update(cx, |this, cx| this.handle_delete(row_ix, window, cx));
        };

        let columns = vec![
            Column::new("server_name", i18n_script_editor(cx, "server_name")).width(px(130.)),
            Column::new("name", i18n_script_editor(cx, "name")).width(px(120.)),
            Column::new("shell_command", i18n_script_editor(cx, "shell_command")).width(px(220.)),
            Column::new("match_pattern", i18n_script_editor(cx, "match_pattern")).width(px(150.)),
            Column::new("mode", i18n_script_editor(cx, "mode")).width(px(80.)),
            Column::new("actions", i18n_script_editor(cx, "actions")).width(px(100.)),
        ];

        let delegate = ScriptTableDelegate::new(items, servers, columns, on_edit, on_delete);
        cx.new(|cx| TableState::new(delegate, window, cx))
    }

    pub fn new(server_state: Entity<ZedisServerState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_id = server_state.read(cx).server_id().to_string();
        let items = ScriptManager::list_with_id();
        let mut subscriptions = Vec::new();

        let servers: Vec<KeyValueOption> = get_servers()
            .unwrap_or_default()
            .iter()
            .map(|s| KeyValueOption::new(s.name.clone().into(), s.id.clone().into()))
            .collect();

        let name_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_script_editor(cx, "name_placeholder"))
        });
        let shell_command_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_script_editor(cx, "shell_command_placeholder"))
                .auto_grow(2, 6)
        });
        let match_pattern_state = cx.new(|cx| {
            InputState::new(window, cx)
                .clean_on_escape()
                .placeholder(i18n_script_editor(cx, "match_pattern_placeholder"))
        });

        let match_mode_select_state = cx.new(|_| 0_usize);
        let found = servers
            .iter()
            .position(|s| s.value == server_id)
            .map(gpui_component::IndexPath::new);
        let servers_for_delegate = servers.clone();
        let server_select_state = cx.new(|cx| SelectState::new(servers, found, window, cx));
        let field_errors = cx.new(|_| HashMap::new());

        let field_errors_clone = field_errors.clone();
        subscriptions.push(cx.subscribe(&server_select_state, move |this, view, event, cx| {
            if let SelectEvent::Confirm(Some(server_id)) = event {
                this.server_id = server_id.clone();
                let id = view.entity_id().to_string();
                if field_errors_clone.read(cx).get(&id).is_some() {
                    field_errors_clone.update(cx, |s, _| {
                        s.remove(&id);
                    });
                }
            }
        }));

        // Clear a field's error once the user leaves it. The shell command is
        // a `TextareaState` and the other two are `InputState`s — three
        // different types since gpui-component split the input engines — so
        // the shared body goes through a macro instead of a loop over an
        // array.
        macro_rules! clear_error_on_blur {
            ($state:expr) => {
                subscriptions.push(
                    cx.subscribe_in(&$state, window, move |view, state, event, _window, cx| {
                        if let InputEvent::Blur = event {
                            let id = state.entity_id().to_string();
                            if view.field_errors.read(cx).get(&id).is_some() {
                                view.field_errors.update(cx, |s, _| {
                                    s.remove(&id);
                                });
                            }
                        }
                    }),
                );
            };
        }
        clear_error_on_blur!(name_state);
        clear_error_on_blur!(shell_command_state);
        clear_error_on_blur!(match_pattern_state);

        let items = Arc::new(items);
        let table_state = Self::create_table_state(items.clone(), servers_for_delegate.clone(), window, cx);

        Self {
            server_select_state,
            name_state,
            shell_command_state,
            match_pattern_state,
            match_mode_select_state,
            field_errors,
            items,
            servers: servers_for_delegate,
            server_id: server_id.into(),
            edit_id: None,
            view_mode: ViewMode::Table,
            table_state,
            needs_table_recreate: None,
            _subscriptions: subscriptions,
        }
    }

    fn handle_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let server_id = self.server_id.clone();
        let name = self.name_state.read(cx).value();
        let shell_command = self.shell_command_state.read(cx).value();
        let match_pattern = self.match_pattern_state.read(cx).value();
        let match_mode = *self.match_mode_select_state.read(cx);
        let field_errors = self.field_errors.clone();

        field_errors.update(cx, |s, _| s.clear());

        if server_id.is_empty() {
            field_errors.update(cx, |s, _| {
                s.insert(
                    self.server_select_state.entity_id().to_string(),
                    "server is required".into(),
                );
            });
        }
        if name.is_empty() {
            field_errors.update(cx, |s, _| {
                s.insert(self.name_state.entity_id().to_string(), "name is required".into());
            });
        }
        if shell_command.is_empty() {
            field_errors.update(cx, |s, _| {
                s.insert(
                    self.shell_command_state.entity_id().to_string(),
                    "shell command is required".into(),
                );
            });
        }
        if match_pattern.is_empty() {
            field_errors.update(cx, |s, _| {
                s.insert(
                    self.match_pattern_state.entity_id().to_string(),
                    "match pattern is required".into(),
                );
            });
        }
        if !field_errors.read(cx).is_empty() {
            return;
        }

        let id = self.edit_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
        let config = ScriptConfig {
            server_id: server_id.to_string(),
            name: name.to_string(),
            shell_command: shell_command.to_string(),
            match_pattern: match_pattern.to_string(),
            mode: MatchMode::from(match_mode),
        };

        cx.spawn(async move |handle, cx| {
            let result: Result<(String, ScriptConfig), Error> = cx
                .background_spawn(async move {
                    ScriptManager::upsert(&id, config.clone())?;
                    Ok((id, config))
                })
                .await;
            match result {
                Ok((id, config)) => {
                    let _ = handle.update(cx, |this, cx| {
                        let mut items = this.items.as_ref().clone();
                        if let Some(pos) = items.iter().position(|(eid, _)| eid == &id) {
                            items[pos] = (id, config);
                        } else {
                            items.push((id, config));
                        }
                        this.items = Arc::new(items);
                        this.needs_table_recreate = Some(true);
                        this.view_mode = ViewMode::Table;
                        cx.notify();
                    });
                }
                Err(e) => error!(error = %e, "save script viewer fail"),
            }
        })
        .detach();
    }

    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_id = None;
        self.name_state
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.shell_command_state
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.match_pattern_state
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.match_mode_select_state.update(cx, |s, _| *s = 0);
        self.field_errors.update(cx, |s, _| s.clear());
    }

    fn handle_update(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, cfg)) = self.items.get(row_ix).cloned() else {
            return;
        };
        self.edit_id = Some(id.clone());

        let selected_index = self
            .servers
            .iter()
            .position(|s| s.value == cfg.server_id)
            .map(gpui_component::IndexPath::new);
        self.server_id = cfg.server_id.clone().into();
        self.server_select_state
            .update(cx, |s, cx| s.set_selected_index(selected_index, window, cx));
        self.name_state
            .update(cx, |s, cx| s.set_value(cfg.name.clone(), window, cx));
        self.shell_command_state
            .update(cx, |s, cx| s.set_value(cfg.shell_command.clone(), window, cx));
        self.match_pattern_state
            .update(cx, |s, cx| s.set_value(cfg.match_pattern.clone(), window, cx));
        self.match_mode_select_state
            .update(cx, |s, _| *s = cfg.mode.clone().into());
        self.field_errors.update(cx, |s, _| s.clear());
        self.view_mode = ViewMode::Edit;
        cx.notify();
    }

    fn handle_delete(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, cfg)) = self.items.get(row_ix).cloned() else {
            return;
        };
        let name = cfg.name.clone();
        let view_handle = cx.entity();
        let text = t!(
            "script_editor.remove_prompt",
            name = name,
            locale = cx.global::<ZedisGlobalStore>().read(cx).locale()
        )
        .to_string();

        ZedisDialog::new_alert(i18n_script_editor(cx, "remove_title"), text)
            .button_props(dialog_button_props(cx))
            .on_ok(move |_, _window, cx| {
                let id = id.clone();
                let view_handle = view_handle.clone();
                cx.spawn(async move |cx| {
                    let result: Result<String, Error> = cx
                        .background_spawn({
                            let id = id.clone();
                            async move {
                                ScriptManager::delete(&id)?;
                                Ok(id)
                            }
                        })
                        .await;
                    match result {
                        Ok(deleted_id) => {
                            view_handle.update(cx, |this, cx| {
                                let new_items: Vec<_> =
                                    this.items.iter().filter(|(id, _)| id != &deleted_id).cloned().collect();
                                this.items = Arc::new(new_items);
                                this.needs_table_recreate = Some(true);
                                cx.notify();
                            });
                        }
                        Err(e) => error!(error = %e, "delete script viewer fail"),
                    }
                })
                .detach();
                true
            })
            .open(window, cx);
    }

    fn render_edit_form(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let match_mode_state = self.match_mode_select_state.clone();
        let match_mode = *self.match_mode_select_state.read(cx);

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
                            .label(i18n_script_editor(cx, "server_name"))
                            .required(true)
                            .child(Select::new(&self.server_select_state)),
                    )
                    .child(
                        field()
                            .label(i18n_script_editor(cx, "name"))
                            .required(true)
                            .child(Input::new(&self.name_state)),
                    )
                    .child(
                        field()
                            .label(i18n_script_editor(cx, "match_pattern"))
                            .required(true)
                            .child(Input::new(&self.match_pattern_state).font_family(get_mono_font_family())),
                    )
                    .child(
                        field().label(i18n_script_editor(cx, "mode")).required(true).child(
                            RadioGroup::horizontal("script-viewer-mode-group")
                                .mt(px(8.))
                                .children(vec!["Prefix", "Suffix", "Regex", "Exact"])
                                .selected_index(Some(match_mode))
                                .on_click(move |index, _, cx| {
                                    match_mode_state.update(cx, |s, _| *s = *index);
                                }),
                        ),
                    )
                    .child(
                        field()
                            .col_span(2)
                            .label(i18n_script_editor(cx, "shell_command"))
                            .required(true)
                            .description(i18n_script_editor(cx, "shell_command_hint"))
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        Textarea::new(&self.shell_command_state)
                                            .w_full()
                                            // Shell command text reads like code — mono.
                                            .font_family(get_mono_font_family()),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Label::new(i18n_script_editor(cx, "templates"))
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground),
                                            )
                                            .children(decode_templates().into_iter().enumerate().map(
                                                |(ix, (label, command))| {
                                                    Button::new(("script-template", ix))
                                                        .outline()
                                                        .xsmall()
                                                        .label(label)
                                                        // Show what will be inserted before clicking.
                                                        .tooltip(command)
                                                        .on_click(cx.listener(move |this, _, window, cx| {
                                                            this.shell_command_state
                                                                .update(cx, |s, cx| s.set_value(command, window, cx));
                                                        }))
                                                },
                                            )),
                                    ),
                            ),
                    ),
            )
            .when(!self.field_errors.read(cx).is_empty(), |this| {
                let title = i18n_script_editor(cx, "field_errors_title");
                let list = self
                    .field_errors
                    .read(cx)
                    .values()
                    .map(|v| format!("- {v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let markdown = t!("script_editor.field_errors_message", errors = list);
                this.child(
                    Alert::error("sv-form-errors", TextView::markdown("sv-form-errors-msg", markdown))
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
                        Button::new("sv-btn-cancel")
                            .icon(IconName::CircleX)
                            .label(i18n_script_editor(cx, "cancel"))
                            .on_click(cx.listener(|this, _, _, _| this.view_mode = ViewMode::Table)),
                    )
                    .child(
                        Button::new("sv-btn-save")
                            .primary()
                            .icon(CustomIconName::Save)
                            .label(i18n_script_editor(cx, "save"))
                            .on_click(cx.listener(|this, _, window, cx| this.handle_save(window, cx))),
                    ),
            )
    }

    fn render_table_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(true) = self.needs_table_recreate.take() {
            self.table_state = Self::create_table_state(self.items.clone(), self.servers.clone(), window, cx);
        }
        v_flex()
            .size_full()
            .p_5()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new(i18n_script_editor(cx, "title")).text_xl()),
            )
            .child(
                div().flex_1().w_full().relative().child(
                    div().absolute().inset_0().size_full().overflow_hidden().child(
                        DataTable::new(&self.table_state)
                            .stripe(true)
                            .bordered(true)
                            .scrollbar_visible(true, true),
                    ),
                ),
            )
            .child(
                h_flex().w_full().justify_end().p_2().child(
                    Button::new("add-sv-btn")
                        .primary()
                        .icon(CustomIconName::FilePlusCorner)
                        .label(i18n_script_editor(cx, "add"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reset_form(window, cx);
                            this.view_mode = ViewMode::Edit;
                        })),
                ),
            )
            .into_any_element()
    }
}

impl Render for ZedisScriptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.view_mode {
            ViewMode::Table => self.render_table_view(window, cx).into_any_element(),
            ViewMode::Edit => self.render_edit_form(window, cx).into_any_element(),
        }
    }
}

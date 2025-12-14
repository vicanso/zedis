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

use crate::states::i18n_common;
use dashmap::DashMap;
use gpui::App;
use gpui::SharedString;
use gpui::Window;
use gpui::prelude::*;
use gpui_component::WindowExt;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants;
use gpui_component::form::field;
use gpui_component::form::v_form;
use gpui_component::input::Input;
use gpui_component::input::InputState;
use gpui_component::radio::RadioGroup;
use std::cell::Cell;
use std::rc::Rc;

type SubmitHandler = Rc<dyn Fn(Vec<SharedString>, &mut Window, &mut App) -> bool>;

pub struct FormDialog {
    pub title: SharedString,
    pub fields: Vec<FormField>,
    pub handle_submit: SubmitHandler,
}

#[derive(Clone, Default)]
pub enum FormFieldType {
    #[default]
    Input,
    RadioGroup,
}

#[derive(Clone, Default)]
pub struct FormField {
    ty: FormFieldType,
    label: SharedString,
    placeholder: SharedString,
    focus: bool,
    options: Option<Vec<SharedString>>,
}

impl FormField {
    pub fn new(label: SharedString) -> Self {
        Self {
            label,
            ..Default::default()
        }
    }
    pub fn with_focus(mut self) -> Self {
        self.focus = true;
        self
    }
    pub fn with_placeholder(mut self, placeholder: SharedString) -> Self {
        self.placeholder = placeholder;
        self
    }
    pub fn with_options(mut self, options: Vec<SharedString>) -> Self {
        self.ty = FormFieldType::RadioGroup;
        self.options = Some(options);
        self
    }
}

pub fn open_add_value_dialog(params: FormDialog, window: &mut Window, cx: &mut App) {
    let value_states = Rc::new(DashMap::new());
    let radio_group_states = Rc::new(DashMap::new());
    let mut should_foucus_index = None;
    let focus_handle_done = Cell::new(false);
    let fields = params.fields.clone();
    for (index, field) in fields.iter().enumerate() {
        match field.ty {
            FormFieldType::Input => {
                let value_state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .clean_on_escape()
                        .placeholder(field.placeholder.clone())
                });
                if should_foucus_index.is_none() && field.focus {
                    should_foucus_index = Some(index);
                }
                value_states.insert(index, value_state);
            }
            FormFieldType::RadioGroup => {
                radio_group_states.insert(index, Cell::new(0_usize));
            }
        }
    }

    let title = params.title.clone();
    let fields_clone = fields.clone();
    let handle_submit = params.handle_submit.clone();
    let value_states_clone = value_states.clone();
    let radio_group_states_clone = radio_group_states.clone();

    let handle = Rc::new(move |window: &mut Window, cx: &mut App| {
        let mut values = Vec::with_capacity(fields_clone.len());
        for (index, field) in fields_clone.iter().enumerate() {
            match field.ty {
                FormFieldType::Input => {
                    let Some(value_state) = value_states.get(&index) else {
                        continue;
                    };
                    let value = value_state.read(cx).value();
                    values.push(value);
                }
                FormFieldType::RadioGroup => {
                    let Some(select_index) = radio_group_states.get(&index) else {
                        continue;
                    };
                    let select_index = select_index.get();
                    values.push(select_index.to_string().into());
                }
            }
        }
        handle_submit(values, window, cx)
    });

    window.open_dialog(cx, move |dialog, window, cx| {
        dialog
            .title(title.clone())
            .overlay(true)
            .overlay_closable(true)
            .child({
                let mut form = v_form();

                for (index, item) in fields.iter().enumerate() {
                    match item.ty {
                        FormFieldType::Input => {
                            let Some(value_state) = value_states_clone.get(&index) else {
                                continue;
                            };
                            if should_foucus_index.is_some()
                                && should_foucus_index.unwrap_or_default() == index
                                && !focus_handle_done.get()
                            {
                                focus_handle_done.set(true);
                                value_state.update(cx, |this, cx| {
                                    this.focus(window, cx);
                                });
                            }
                            form = form.child(field().label(item.label.clone()).child(Input::new(&value_state)));
                        }
                        FormFieldType::RadioGroup => {
                            let Some(select_index) = radio_group_states_clone.get(&index) else {
                                continue;
                            };
                            let radio_group_states_clone = radio_group_states_clone.clone();
                            form = form.child(
                                field().label(item.label.clone()).child(
                                    RadioGroup::horizontal(("dialog-radio-group", index))
                                        .children(item.options.clone().unwrap_or_default())
                                        .selected_index(Some(select_index.get()))
                                        .on_click({
                                            move |select_index, _, cx| {
                                                let Some(item) = radio_group_states_clone.get_mut(&index) else {
                                                    return;
                                                };
                                                item.set(*select_index);
                                                cx.stop_propagation();
                                            }
                                        }),
                                ),
                            );
                        }
                    }
                }
                form
            })
            .on_ok({
                let handle = handle.clone();
                move |_, window, cx| handle(window, cx)
            })
            .footer({
                let handle = handle.clone();
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

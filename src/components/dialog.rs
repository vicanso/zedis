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

use crate::helpers::is_windows;
use crate::states::i18n_common;
use gpui::{App, Entity, SharedString, Window, prelude::*};
use gpui_component::{
    WindowExt,
    button::{Button, ButtonVariants},
    form::{field, v_form},
    input::{Input, InputState},
    radio::RadioGroup,
};
use std::{cell::Cell, rc::Rc};

/// Handler closure to process form submission.
/// Returns `true` if the dialog should be closed, `false` otherwise.
type SubmitHandler = Rc<dyn Fn(Vec<SharedString>, &mut Window, &mut App) -> bool>;

/// Handler closure to validate input fields.
/// Returns `true` if valid, `false` otherwise.
type ValidateHandler = Rc<dyn Fn(&str) -> bool>;

/// Configuration for a dynamic form dialog.
pub struct FormDialog {
    /// Title of the dialog.
    pub title: SharedString,
    /// Fields of the dialog.
    pub fields: Vec<FormField>,
    /// Handler to submit the form.
    pub handle_submit: SubmitHandler,
}

/// Defines the type of UI component to render for a field.
#[derive(Clone, Default)]
pub enum FormFieldType {
    /// Input field.
    #[default]
    Input,
    /// Radio group field.
    RadioGroup,
}

#[derive(Clone, Default)]
pub struct FormField {
    /// Type of the field.
    field_type: FormFieldType,
    /// Label of the field.
    label: SharedString,
    /// Placeholder of the field.
    placeholder: SharedString,
    /// Whether to focus the field when the dialog opens.
    focus: bool,
    /// Options of the field.
    options: Option<Vec<SharedString>>,
    /// Handler to validate the field.
    validate_handler: Option<ValidateHandler>,
}

impl FormField {
    /// Creates a new field with a label and default settings (Input type).
    pub fn new(label: SharedString) -> Self {
        Self {
            label,
            ..Default::default()
        }
    }
    /// Sets the field to be auto-focused when the dialog opens.
    pub fn with_focus(mut self) -> Self {
        self.focus = true;
        self
    }
    /// Sets a placeholder text for input fields.
    pub fn with_placeholder(mut self, placeholder: SharedString) -> Self {
        self.placeholder = placeholder;
        self
    }
    /// Configures the field as a RadioGroup with the provided options.
    pub fn with_options(mut self, options: Vec<SharedString>) -> Self {
        self.field_type = FormFieldType::RadioGroup;
        self.options = Some(options);
        self
    }
    /// Configures the field to be validated with the provided function.
    pub fn with_validate<F>(mut self, validate: F) -> Self
    where
        F: Fn(&str) -> bool + 'static,
    {
        self.validate_handler = Some(Rc::new(validate));
        self
    }
}

/// Internal enum to hold the runtime state of a field.
/// This replaces the complex DashMap logic.
#[derive(Clone)]
enum FieldState {
    Input(Entity<InputState>),
    Radio(Rc<Cell<usize>>),
}

/// Opens a modal dialog containing a dynamically generated form.
///
/// This function handles:
/// 1. Initializing state for all fields (Inputs, RadioGroups).
/// 2. Binding validation logic.
/// 3. Rendering the UI.
/// 4. Collecting values and triggering the submit handler.
pub fn open_add_form_dialog(params: FormDialog, window: &mut Window, cx: &mut App) {
    // 1. Initialize State Containers
    // We use DashMap for interior mutability to share state easily across closures.
    // Key: Field Index, Value: State Entity (Input) or Cell (Radio)
    let mut states = Vec::with_capacity(params.fields.len());
    let mut focus_target = None;

    // Get the fields from the parameters
    for field in params.fields.iter() {
        match field.field_type {
            FormFieldType::Input => {
                let validator = field.validate_handler.clone();
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .clean_on_escape()
                        .placeholder(field.placeholder.clone())
                        .validate(move |s, _| validator.as_ref().is_none_or(|v| v(s)))
                });

                // Capture the first field marked for focus
                if field.focus && focus_target.is_none() {
                    focus_target = Some(state.clone());
                }
                states.push(FieldState::Input(state));
            }
            FormFieldType::RadioGroup => {
                states.push(FieldState::Radio(Rc::new(Cell::new(0))));
            }
        }
    }

    // Prepare data for closures
    let title = params.title;
    let fields_def = params.fields;
    let submit_handler = params.handle_submit;
    let states = Rc::new(states); // Share states between submit handler and renderer
    let focus_applied = Rc::new(Cell::new(false)); // Ensure focus only happens once

    // We create a single closure to collect values from all fields and submit them.
    // This avoids re-creating closures for each field in the loop above.
    let states_for_submit = states.clone();
    let do_submit = Rc::new(move |window: &mut Window, cx: &mut App| {
        let values: Vec<SharedString> = states_for_submit
            .iter()
            .map(|state| match state {
                FieldState::Input(entity) => entity.read(cx).value(),
                FieldState::Radio(cell) => cell.get().to_string().into(),
            })
            .collect();

        submit_handler(values, window, cx)
    });

    window.open_dialog(cx, move |dialog, window, cx| {
        dialog
            .title(title.clone())
            .overlay(true)
            .overlay_closable(true)
            .child({
                let mut form = v_form();
                for (index, (def, state)) in fields_def.iter().zip(states.iter()).enumerate() {
                    match (state, &def.field_type) {
                        (FieldState::Input(entity), _) => {
                            if let Some(target) = &focus_target
                                && target == entity
                                && !focus_applied.get()
                            {
                                focus_applied.set(true);
                                let entity = entity.clone();
                                entity.update(cx, |this, cx| this.focus(window, cx));
                            }
                            form = form.child(
                                field()
                                    .label(def.label.clone())
                                    .child(Input::new(entity).cleanable(true)),
                            );
                        }
                        (FieldState::Radio(cell), FormFieldType::RadioGroup) => {
                            let cell = cell.clone();
                            form = form.child(
                                field().label(def.label.clone()).child(
                                    RadioGroup::horizontal(("dialog-radio-group", index))
                                        .children(def.options.clone().unwrap_or_default())
                                        .selected_index(Some(cell.get()))
                                        .on_click({
                                            move |select_index, _, cx| {
                                                cell.set(*select_index);
                                                cx.stop_propagation();
                                            }
                                        }),
                                ),
                            );
                        }
                        _ => {}
                    }
                }
                form
            })
            .on_ok({
                let do_submit = do_submit.clone();
                move |_, window, cx| do_submit(window, cx)
            })
            .on_cancel(|_, window, cx| {
                window.close_dialog(cx);
                true
            })
            .footer({
                let do_submit = do_submit.clone();
                move |_, _, _, cx| {
                    let confirm_label = i18n_common(cx, "confirm");
                    let cancel_label = i18n_common(cx, "cancel");
                    let mut buttons = vec![
                        // Cancel button - closes dialog without saving
                        Button::new("cancel").label(cancel_label).on_click(|_, window, cx| {
                            window.close_dialog(cx);
                        }),
                        // Submit button - validates and saves server configuration
                        Button::new("ok").primary().label(confirm_label).on_click({
                            let do_submit = do_submit.clone();
                            move |_, window, cx| {
                                do_submit.clone()(window, cx);
                            }
                        }),
                    ];
                    if is_windows() {
                        buttons.reverse();
                    }
                    buttons
                }
            })
    });
}

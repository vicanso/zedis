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

use gpui::{AnyElement, ElementId, Entity, FontWeight, Render, SharedString, Subscription, Window, div, prelude::*};
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::form::{field, v_form};
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use gpui_component::label::Label;
use gpui_component::radio::RadioGroup;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, h_flex};
use std::collections::HashMap;
use std::mem::take;
use std::rc::Rc;

/// Callback invoked on form submission with all field values collected as a map.
/// Returns `true` if the submission was handled successfully.
type ZedisFormSubmitHandler =
    Rc<dyn Fn(HashMap<SharedString, String>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Per-field validation callback. Returns `Some(error_message)` on failure, `None` on success.
type ZedisFormValidateHandler = Rc<dyn Fn(&str) -> Option<SharedString> + 'static>;

/// Callback invoked when the cancel button is clicked.
/// Returns `true` if the cancellation was handled.
type ZedisFormCancelHandler = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Callback invoked to build the action buttons for the footer.
pub type ZedisFormActionsBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> Vec<AnyElement>>;

/// Supported field widget types for the form builder.
#[derive(Clone, Default, PartialEq)]
pub enum ZedisFormFieldType {
    #[default]
    Input,
    InputNumber,
    RadioGroup,
    Checkbox,
    /// Auto-growing text area with `(min_rows, max_rows)`.
    AutoGrow(usize, usize),
}

/// Declarative field descriptor used to configure a form field before the
/// form entity is created. Uses the builder pattern for ergonomic construction.
#[derive(Clone)]
pub struct ZedisFormField {
    name: SharedString,
    label: SharedString,
    placeholder: SharedString,
    /// When set, the field is only visible on the tab at this index.
    tab_index: Option<usize>,
    default_value: Option<SharedString>,
    field_type: ZedisFormFieldType,
    /// Options list for `RadioGroup` fields.
    options: Option<Vec<SharedString>>,
    validate: Option<ZedisFormValidateHandler>,
    mask: bool,
    required: bool,
    /// Whether this field should receive focus on the first render.
    focus: bool,
    readonly: bool,
}

/// Runtime state wrapper for each field type, holding a GPUI entity handle.
enum ZedisFormFieldState {
    Input(Entity<InputState>),
    RadioGroup(Entity<usize>),
    Checkbox(Entity<bool>),
}

impl ZedisFormField {
    /// Create a new field descriptor with the given internal name and display label.
    pub fn new(name: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            placeholder: SharedString::default(),
            default_value: None,
            field_type: ZedisFormFieldType::Input,
            options: None,
            validate: None,
            tab_index: None,
            required: false,
            focus: false,
            mask: false,
            readonly: false,
        }
    }

    /// Set the placeholder text shown when the field is empty.
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Set the widget type for this field (defaults to `Input`).
    pub fn field_type(mut self, ty: ZedisFormFieldType) -> Self {
        self.field_type = ty;
        self
    }

    /// Mark the field as required; empty values will trigger a validation error.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Set the list of options for `RadioGroup` fields.
    pub fn options(mut self, options: Vec<SharedString>) -> Self {
        self.options = Some(options);
        self
    }

    /// Attach a custom validation function to this field.
    pub fn validate(mut self, validate: ZedisFormValidateHandler) -> Self {
        self.validate = Some(validate);
        self
    }

    /// Set the initial value for this field.
    pub fn default_value(mut self, value: impl Into<SharedString>) -> Self {
        self.default_value = Some(value.into());
        self
    }

    /// Assign this field to a specific tab index for multi-tab forms.
    pub fn tab_index(mut self, index: usize) -> Self {
        self.tab_index = Some(index);
        self
    }

    /// Enable password masking on this field.
    pub fn mask(mut self) -> Self {
        self.mask = true;
        self
    }

    /// Request that this field receives keyboard focus on the first render.
    pub fn focus(mut self) -> Self {
        self.focus = true;
        self
    }

    /// Mark this field as read-only (renders the widget as disabled).
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }
}

/// Configuration for constructing a [`ZedisForm`]. Collects field descriptors,
/// tab labels, button labels, and event handlers before entity creation.
pub struct ZedisFormOptions {
    title: Option<SharedString>,
    description: Option<SharedString>,
    tabs: Option<Vec<SharedString>>,
    fields: Vec<ZedisFormField>,
    required_error_msg: SharedString,
    confirm_label: SharedString,
    cancel_label: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
}

impl Default for ZedisFormOptions {
    fn default() -> Self {
        Self {
            tabs: None,
            title: None,
            description: None,
            fields: Vec::new(),
            required_error_msg: "Required".into(),
            confirm_label: "Confirm".into(),
            cancel_label: "Cancel".into(),
            on_submit: None,
            on_cancel: None,
            foot_actions: None,
        }
    }
}

impl ZedisFormOptions {
    /// Create form options from a list of field descriptors.
    pub fn new(fields: Vec<ZedisFormField>) -> Self {
        Self {
            fields,
            ..Default::default()
        }
    }

    /// Set the tab labels for a multi-tab form layout.
    pub fn tabs(mut self, tabs: Vec<SharedString>) -> Self {
        self.tabs = Some(tabs);
        self
    }

    /// Override the default "Required" validation error message.
    pub fn required_error_msg(mut self, msg: impl Into<SharedString>) -> Self {
        self.required_error_msg = msg.into();
        self
    }

    /// Set the label for the confirm/submit button.
    pub fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = label.into();
        self
    }

    /// Set the label for the cancel button.
    pub fn cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel_label = label.into();
        self
    }

    /// Set the title of the form.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the description of the form.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Attach a submit handler that receives all field values on form submission.
    pub fn on_submit(mut self, on_submit: ZedisFormSubmitHandler) -> Self {
        self.on_submit = Some(on_submit);
        self
    }

    /// Attach a cancel handler invoked when the cancel button is clicked.
    pub fn on_cancel(mut self, on_cancel: ZedisFormCancelHandler) -> Self {
        self.on_cancel = Some(on_cancel);
        self
    }

    /// Set the action buttons for the footer.
    pub fn foot_actions<F, I>(mut self, builder: F) -> Self
    where
        F: Fn(&mut Window, &mut Context<ZedisForm>) -> I + 'static,
        I: IntoIterator,
        I::Item: IntoElement,
    {
        self.foot_actions = Some(Rc::new(move |window, cx| {
            builder(window, cx).into_iter().map(|e| e.into_any_element()).collect()
        }));
        self
    }
}

/// A dynamic form component built on GPUI. Manages a heterogeneous list of
/// form fields (text inputs, number inputs, checkboxes, radio groups), optional
/// tab-based grouping, validation, and submit/cancel actions.
///
/// Construct via [`ZedisFormOptions`] and `cx.new(|cx| ZedisForm::new(...))`.
pub struct ZedisForm {
    id: ElementId,
    title: Option<SharedString>,
    description: Option<SharedString>,
    confirm_label: SharedString,
    cancel_label: SharedString,
    /// One-shot flag: focus the designated field on the first render only.
    should_focus: bool,
    field_states: Vec<(ZedisFormField, ZedisFormFieldState)>,
    tab_selected_index: Entity<usize>,
    errors: HashMap<SharedString, SharedString>,
    required_msg: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
    tabs: Option<Vec<SharedString>>,
    _subscriptions: Vec<Subscription>,
}

impl ZedisForm {
    /// Create a new form entity from the given options.
    ///
    /// This wires up GPUI entities for each field and subscribes to input
    /// change events so validation errors are cleared as the user types.
    pub fn new(
        id: impl Into<ElementId>,
        options: ZedisFormOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id = id.into();
        let fields = options.fields;
        let mut subscriptions = Vec::new();
        let mut field_states = Vec::with_capacity(fields.len());

        for field in &fields {
            let name = field.name.clone();
            match field.field_type {
                ZedisFormFieldType::Input | ZedisFormFieldType::InputNumber | ZedisFormFieldType::AutoGrow(_, _) => {
                    let state = cx.new(|cx| {
                        let mut state = InputState::new(window, cx)
                            .placeholder(field.placeholder.clone())
                            .masked(field.mask);
                        if let ZedisFormFieldType::AutoGrow(min_rows, max_rows) = field.field_type {
                            state = state.auto_grow(min_rows, max_rows);
                        }
                        state
                    });
                    if let Some(default_value) = &field.default_value {
                        state.update(cx, |state, cx| {
                            state.set_value(default_value, window, cx);
                        });
                    }

                    // Clear validation errors when the user edits the field.
                    let name_clone = name.clone();
                    subscriptions.push(
                        cx.subscribe_in(&state, window, move |this, _state, event, _window, cx| {
                            if let InputEvent::Change = event {
                                this.on_value_change(name_clone.clone(), cx);
                            }
                        }),
                    );

                    // Handle increment/decrement steps for number inputs.
                    if field.field_type == ZedisFormFieldType::InputNumber {
                        subscriptions.push(cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                            let NumberInputEvent::Step(action) = event;
                            let Ok(value) = state.read(cx).value().parse::<i64>() else {
                                return;
                            };
                            let new_val = match action {
                                StepAction::Increment => value.saturating_add(1),
                                StepAction::Decrement => value.saturating_sub(1),
                            };
                            if new_val != value {
                                state.update(cx, |state, cx| {
                                    state.set_value(new_val.to_string(), window, cx);
                                });
                            }
                            this.on_value_change(name.clone(), cx);
                        }));
                    }

                    field_states.push((field.clone(), ZedisFormFieldState::Input(state)));
                }
                ZedisFormFieldType::Checkbox => {
                    let default_value = field.default_value.as_ref().map(|v| v == "true").unwrap_or(false);
                    let state = cx.new(|_cx| default_value);
                    field_states.push((field.clone(), ZedisFormFieldState::Checkbox(state)));
                }
                ZedisFormFieldType::RadioGroup => {
                    let default_value = field
                        .default_value
                        .as_ref()
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let state = cx.new(|_cx| default_value);
                    field_states.push((field.clone(), ZedisFormFieldState::RadioGroup(state)));
                }
            }
        }

        Self {
            id,
            field_states,
            errors: HashMap::new(),
            required_msg: options.required_error_msg,
            title: options.title,
            description: options.description,
            confirm_label: options.confirm_label,
            cancel_label: options.cancel_label,
            tabs: options.tabs,
            on_submit: options.on_submit,
            on_cancel: options.on_cancel,
            tab_selected_index: cx.new(|_cx| 0),
            should_focus: true,
            foot_actions: options.foot_actions,
            _subscriptions: subscriptions,
        }
    }

    /// Clear the validation error for a specific field when its value changes.
    fn on_value_change(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.errors.remove(&name);
        cx.notify();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(on_cancel) = &self.on_cancel {
            on_cancel(window, cx);
        }
    }

    /// Validate all fields, collect their values, and invoke the submit handler.
    /// Runs required-checks first, then custom validators per field.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.errors.clear();
        let mut has_errors = false;
        let mut values = HashMap::new();

        for (field, state) in &self.field_states {
            let value = match state {
                ZedisFormFieldState::Input(state) => state.read(cx).value().to_string(),
                ZedisFormFieldState::RadioGroup(state) => state.read(cx).to_string(),
                ZedisFormFieldState::Checkbox(state) => state.read(cx).to_string(),
            };
            let value = value.trim().to_string();

            if field.required && value.is_empty() {
                self.errors.insert(field.name.clone(), self.required_msg.clone());
                has_errors = true;
                continue;
            }

            if let Some(validate_fn) = &field.validate
                && let Some(err_msg) = validate_fn(&value)
            {
                self.errors.insert(field.name.clone(), err_msg);
                has_errors = true;
            }
            values.insert(field.name.clone(), value);
        }

        if has_errors {
            cx.notify();
            return;
        }
        if let Some(on_submit) = &self.on_submit {
            on_submit(values, window, cx);
        }
    }
}

impl Render for ZedisForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Auto-focus the designated field on the first render, then clear the flag.
        if take(&mut self.should_focus) {
            for (field, state) in &self.field_states {
                if field.focus
                    && let ZedisFormFieldState::Input(state) = state
                {
                    state.update(cx, |state, cx| {
                        state.focus(window, cx);
                    });
                    break;
                }
            }
        }

        let mut form_container = v_form()
            .w_full()
            .gap_2()
            .when_some(self.title.clone(), |this, title| {
                this.child(field().child(Label::new(title).text_lg().font_weight(FontWeight::BOLD)))
            })
            .when_some(self.description.clone(), |this, description| {
                this.child(
                    field().child(
                        Label::new(description)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                    ),
                )
            });
        let parent_id = Box::new(self.id.clone());

        // Render optional tab bar for multi-tab forms.
        if let Some(tabs) = &self.tabs {
            let tab_selected_index = self.tab_selected_index.clone();
            let tab_bar_id = ElementId::NamedChild(parent_id.clone(), "tab-bar".into());
            let mut tab_bar = TabBar::new(tab_bar_id)
                .underline()
                .mb_3()
                .selected_index(*tab_selected_index.read(cx))
                .on_click(move |selected_index, _, cx| {
                    tab_selected_index.update(cx, |state, cx| {
                        *state = *selected_index;
                        cx.notify();
                    });
                });
            for tab in tabs {
                tab_bar = tab_bar.child(Tab::new().label(tab.clone()));
            }
            form_container = form_container.child(field().child(tab_bar));
        }

        let new_field = |item: &ZedisFormField| field().required(item.required).label(item.label.clone());

        // Read the active tab index once to avoid repeated entity reads inside the loop.
        let active_tab_index = *self.tab_selected_index.read(cx);

        for (index, (field, field_state)) in self.field_states.iter().enumerate() {
            // Skip fields that belong to a different tab.
            if let Some(tab_index) = field.tab_index
                && tab_index != active_tab_index
            {
                continue;
            }

            match field_state {
                ZedisFormFieldState::Input(state) => {
                    if field.field_type == ZedisFormFieldType::InputNumber {
                        form_container = form_container
                            .child(new_field(field).child(NumberInput::new(state).disabled(field.readonly)));
                    } else {
                        form_container = form_container.child(
                            new_field(field).child(
                                Input::new(state)
                                    .disabled(field.readonly)
                                    .when(field.mask, |this| this.mask_toggle()),
                            ),
                        );
                    }
                }
                ZedisFormFieldState::Checkbox(state) => {
                    let id = ElementId::NamedChild(parent_id.clone(), index.to_string().into());
                    let state_clone = state.clone();
                    form_container = form_container.child(
                        new_field(field).child(
                            Checkbox::new(id)
                                .label(field.placeholder.clone())
                                .checked(*state.read(cx))
                                .disabled(field.readonly)
                                .on_click(move |check, _, cx| {
                                    state_clone.update(cx, |state, _| {
                                        *state = *check;
                                    });
                                }),
                        ),
                    );
                }
                ZedisFormFieldState::RadioGroup(state) => {
                    let id = ElementId::NamedChild(parent_id.clone(), index.to_string().into());
                    let state = state.clone();
                    let selected = *state.read(cx);
                    form_container = form_container.child(
                        new_field(field).child(
                            RadioGroup::horizontal(id)
                                .children(field.options.clone().unwrap_or_default())
                                .selected_index(Some(selected))
                                .disabled(field.readonly)
                                .on_click(move |index, _, cx| {
                                    state.update(cx, |state, _| {
                                        *state = *index;
                                    });
                                }),
                        ),
                    );
                }
            }
        }

        // Render validation errors as a markdown alert.
        if !self.errors.is_empty() {
            let alert_id = ElementId::NamedChild(parent_id.clone(), "alert".into());
            let textview_id = ElementId::NamedChild(parent_id.clone(), "textview".into());
            let error_text = self
                .errors
                .iter()
                .map(|(name, value)| format!("- {name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n");
            form_container = form_container.child(field().child(Alert::error(
                alert_id,
                TextView::markdown(textview_id, error_text, window, cx),
            )));
        }

        // Build action buttons (cancel on the left, confirm/primary on the right).
        let mut buttons = Vec::with_capacity(2);
        if self.on_cancel.is_some() {
            let button_id = ElementId::NamedChild(parent_id.clone(), "cancel".into());
            buttons.push(
                Button::new(button_id)
                    .label(self.cancel_label.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.cancel(window, cx);
                    })),
            );
        }
        if self.on_submit.is_some() {
            let button_id = ElementId::NamedChild(parent_id.clone(), "confirm".into());
            buttons.push(
                Button::new(button_id)
                    .label(self.confirm_label.clone())
                    .primary()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit(window, cx);
                    })),
            );
        }

        // Windows convention: primary button on the left; macOS/Linux: on the right.
        if cfg!(target_os = "windows") {
            buttons.reverse();
        }
        let mut right_buttons = h_flex().justify_end().gap_4();
        let mut left_buttons = h_flex().justify_start().gap_4();

        let mut exists_buttons = false;
        if !buttons.is_empty() {
            right_buttons = right_buttons.children(buttons);
            exists_buttons = true;
        }
        if let Some(builder) = &self.foot_actions {
            let custom_elements = builder(window, cx);
            left_buttons = left_buttons.children(custom_elements);
            exists_buttons = true;
        }
        if exists_buttons {
            form_container = form_container
                .child(field().child(h_flex().justify_end().child(left_buttons).child(right_buttons).gap_4()));
        }

        div().child(form_container).overflow_y_scrollbar()
    }
}

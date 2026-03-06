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

use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, Pixels, Render, SharedString, StyleRefinement, Subscription,
    Window, div, prelude::*,
};
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::form::{field, v_form};
use gpui_component::highlighter::Language;
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, Position, StepAction};
use gpui_component::label::Label;
use gpui_component::radio::RadioGroup;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, IconName, StyledExt, WindowExt, h_flex};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::mem::take;
use std::rc::Rc;

/// Arranges buttons following platform conventions.
///
/// - macOS/Linux: primary button on the right
/// - Windows: primary button on the left
pub fn platform_buttons(mut buttons: Vec<Button>) -> Vec<Button> {
    if cfg!(target_os = "windows") {
        buttons.reverse();
    }
    buttons
}
use std::sync::Arc;

/// Callback invoked on form submission with all field values collected as a map.
/// Returns `true` if the submission was handled successfully.
type ZedisFormSubmitHandler =
    Rc<dyn Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Per-field validation callback. Returns `Some(error_message)` on failure, `None` on success.
type ZedisFormValidateHandler = Rc<dyn Fn(&str) -> Option<SharedString> + 'static>;

/// Callback invoked when the cancel button is clicked.
/// Returns `true` if the cancellation was handled.
type ZedisFormCancelHandler = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static>;

/// Callback invoked to build the action buttons for the footer.
pub type ZedisFormActionsBuilder = Rc<dyn Fn(&mut Window, &mut Context<ZedisForm>) -> Vec<AnyElement>>;

/// Supported field widget types for the form builder.
#[derive(Clone, Default, PartialEq, Debug)]
pub enum ZedisFormFieldType {
    #[default]
    Input,
    InputNumber,
    RadioGroup,
    Checkbox,
    /// Auto-growing text area with `(min_rows, max_rows)`.
    AutoGrow(usize, usize),
    Editor,
}

/// Declarative field descriptor used to configure a form field before the
/// form entity is created. Uses the builder pattern for ergonomic construction.
#[derive(Clone)]
pub struct ZedisFormField {
    style: StyleRefinement,
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
    /// When set, the field is only visible when the referenced RadioGroup's
    /// selected index is in the given list.
    visible_on: Option<(SharedString, Vec<usize>)>,
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
            visible_on: None,
            style: StyleRefinement::default(),
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
    pub fn validate(mut self, validate: impl Fn(&str) -> Option<SharedString> + 'static) -> Self {
        self.validate = Some(Rc::new(validate));
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

    /// Make this field visible only when the RadioGroup with the given name
    /// has its selected index in `indices`.
    pub fn visible_on(mut self, name: impl Into<SharedString>, indices: &[usize]) -> Self {
        self.visible_on = Some((name.into(), indices.to_vec()));
        self
    }
}

impl Styled for ZedisFormField {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl gpui::prelude::FluentBuilder for ZedisFormField {}

/// Configuration for constructing a [`ZedisForm`]. Collects field descriptors,
/// tab labels, button labels, and event handlers before entity creation.
pub struct ZedisFormOptions {
    title: Option<SharedString>,
    description: Option<SharedString>,
    tabs: Option<Vec<SharedString>>,
    fields: Vec<ZedisFormField>,
    required_error_msg: SharedString,
    confirm_label: SharedString,
    confirm_tooltip: Option<SharedString>,
    cancel_label: SharedString,
    add_field_placeholder: SharedString,
    add_value_placeholder: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
    dialog_submit: Option<ZedisFormDialogSubmitHandler>,
    dialog_max_height: Option<Pixels>,
    support_add_fields: bool,
    /// When set, the add-fields section is only shown when the referenced
    /// RadioGroup's selected index is in the given list.
    support_add_fields_on: Option<(SharedString, Vec<usize>)>,
}

impl Default for ZedisFormOptions {
    fn default() -> Self {
        Self {
            tabs: None,
            title: None,
            description: None,
            fields: Vec::new(),
            required_error_msg: "Required".into(),
            confirm_tooltip: None,
            confirm_label: "Confirm".into(),
            cancel_label: "Cancel".into(),
            add_field_placeholder: "Enter field".into(),
            add_value_placeholder: "Enter value".into(),
            on_submit: None,
            on_cancel: None,
            foot_actions: None,
            dialog_submit: None,
            dialog_max_height: None,
            support_add_fields: false,
            support_add_fields_on: None,
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

    /// Set the tooltip for the confirm/submit button.
    pub fn confirm_tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.confirm_tooltip = Some(tooltip.into());
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
    pub fn on_submit(
        mut self,
        on_submit: impl Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut Context<ZedisForm>) -> bool + 'static,
    ) -> Self {
        self.on_submit = Some(Rc::new(on_submit));
        self
    }

    /// Attach a cancel handler invoked when the cancel button is clicked.
    pub fn on_cancel(mut self, on_cancel: impl Fn(&mut Window, &mut Context<ZedisForm>) -> bool + 'static) -> Self {
        self.on_cancel = Some(Rc::new(on_cancel));
        self
    }

    /// Support adding fields to the form.
    pub fn support_add_fields(mut self) -> Self {
        self.support_add_fields = true;
        self
    }

    /// Conditionally show the add-fields section only when the referenced
    /// RadioGroup's selected index is in `indices`.
    pub fn support_add_fields_on(mut self, name: impl Into<SharedString>, indices: &[usize]) -> Self {
        self.support_add_fields = true;
        self.support_add_fields_on = Some((name.into(), indices.to_vec()));
        self
    }

    /// Set the placeholder for the add field input.
    pub fn add_field_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.add_field_placeholder = placeholder.into();
        self
    }

    /// Set the placeholder for the add value input.
    pub fn add_value_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.add_value_placeholder = placeholder.into();
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

impl gpui::prelude::FluentBuilder for ZedisFormOptions {}

/// Handler closure invoked on dialog form submission.
/// Receives field values as an `IndexMap`, and should return `true` if the
/// submission was handled successfully (the dialog will be closed automatically).
type ZedisFormDialogSubmitHandler =
    Rc<dyn Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut App) -> bool + 'static>;

impl ZedisFormOptions {
    /// Opens this form inside a modal dialog overlay.
    ///
    /// This is a convenience method that wraps the form in a `window.open_dialog`
    /// call, automatically handling dialog close on submit/cancel.
    ///
    /// The submit handler receives `IndexMap<SharedString, SharedString>` and
    /// `&mut App` (not `&mut Context<ZedisForm>`), making it easier to use from
    /// any context without needing a reference to the form entity.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// ZedisFormOptions::new(vec![
    ///     ZedisFormField::new("name", "Name").placeholder("Enter name").focus(),
    ///     ZedisFormField::new("age", "Age").field_type(ZedisFormFieldType::InputNumber),
    /// ])
    /// .title("Add User")
    /// .on_dialog_submit(move |values, window, cx| {
    ///     let name = values.get("name").cloned().unwrap_or_default();
    ///     // ... handle submission ...
    ///     true
    /// })
    /// .open_dialog(window, cx);
    /// ```
    pub fn on_dialog_submit(
        mut self,
        handler: impl Fn(IndexMap<SharedString, SharedString>, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.dialog_submit = Some(Rc::new(handler));
        self
    }

    /// Set the maximum height for the form content area when opened in a dialog.
    pub fn dialog_max_height(mut self, max_height: Pixels) -> Self {
        self.dialog_max_height = Some(max_height);
        self
    }

    pub fn open_dialog(mut self, window: &mut Window, cx: &mut App) {
        // Wrap the dialog-level submit handler into a form-level on_submit.
        // The wrapper auto-closes the dialog when the handler returns true.
        if let Some(dialog_handler) = self.dialog_submit.take() {
            self.on_submit = Some(Rc::new(move |values, window, cx| {
                if dialog_handler(values, window, cx) {
                    window.close_dialog(cx);
                    return true;
                }
                false
            }));
        }

        // Set on_cancel to close the dialog.
        self.on_cancel = Some(Rc::new(|window, cx| {
            window.close_dialog(cx);
            true
        }));

        let title = self.title.clone();
        self.title = None;
        let max_height = self.dialog_max_height.take();
        // Create the form entity once; the dialog closure (called every frame)
        // just clones the entity handle.
        let form = cx.new(|cx| {
            let mut f = ZedisForm::new("dialog-form", self, window, cx);
            f.in_dialog = true;
            f
        });

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let mut d = dialog.overlay(true).overlay_closable(true);
            if let Some(t) = &title {
                d = d.title(t.clone());
            }
            // Wrap in a max-height container if specified.
            let child: AnyElement = if let Some(mh) = max_height {
                div().max_h(mh).child(form.clone()).into_any_element()
            } else {
                form.clone().into_any_element()
            };
            // Override the Dialog's default on_ok (which closes on Enter)
            // to trigger form validation and submit instead.
            let form_for_ok = form.clone();
            d.child(child).on_ok(move |_, window, cx| {
                form_for_ok.update(cx, |form, cx| {
                    form.submit(window, cx);
                });
                // Don't close here — the submit handler closes on success.
                false
            })
        });
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
    confirm_tooltip: Option<SharedString>,
    cancel_label: SharedString,
    /// One-shot flag: focus the designated field on the first render only.
    should_focus: bool,
    field_states: Vec<(ZedisFormField, ZedisFormFieldState)>,
    add_field_states: Vec<(Entity<InputState>, Entity<InputState>)>,
    add_field_placeholder: SharedString,
    add_value_placeholder: SharedString,
    tab_selected_index: Entity<usize>,
    support_add_fields: bool,
    support_add_fields_on: Option<(SharedString, Vec<usize>)>,
    errors: HashMap<SharedString, SharedString>,
    required_msg: SharedString,
    on_submit: Option<ZedisFormSubmitHandler>,
    on_cancel: Option<ZedisFormCancelHandler>,
    foot_actions: Option<ZedisFormActionsBuilder>,
    tabs: Option<Vec<SharedString>>,
    _subscriptions: Vec<Subscription>,
    pub is_processing: bool,
    in_dialog: bool,
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
                ZedisFormFieldType::Input
                | ZedisFormFieldType::InputNumber
                | ZedisFormFieldType::AutoGrow(_, _)
                | ZedisFormFieldType::Editor => {
                    let state = cx.new(|cx| {
                        let mut state = InputState::new(window, cx)
                            .placeholder(field.placeholder.clone())
                            .masked(field.mask);
                        match field.field_type {
                            ZedisFormFieldType::Editor => {
                                state = state
                                    .code_editor(Language::from_str("json").name())
                                    .line_number(true)
                                    .indent_guides(true)
                                    .searchable(true)
                                    .soft_wrap(true)
                            }
                            ZedisFormFieldType::AutoGrow(min_rows, max_rows) => {
                                state = state.auto_grow(min_rows, max_rows);
                            }
                            _ => {}
                        }
                        state
                    });
                    if let Some(default_value) = &field.default_value {
                        state.update(cx, |state, cx| {
                            state.set_value(default_value, window, cx);
                        });
                    }

                    // Clear validation errors when the user edits the field.
                    // For single-line inputs, also submit the form on Enter.
                    let name_clone = name.clone();
                    let is_single_line = matches!(
                        field.field_type,
                        ZedisFormFieldType::Input | ZedisFormFieldType::InputNumber
                    );
                    subscriptions.push(cx.subscribe_in(&state, window, move |this, _state, event, window, cx| {
                        match event {
                            InputEvent::Change => {
                                this.on_value_change(name_clone.clone(), cx);
                            }
                            InputEvent::PressEnter { .. } if is_single_line && !this.in_dialog => {
                                this.submit(window, cx);
                            }
                            _ => {}
                        }
                    }));

                    // Handle increment/decrement steps for number inputs.
                    if field.field_type == ZedisFormFieldType::InputNumber {
                        subscriptions.push(cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                            let NumberInputEvent::Step(action) = event;
                            let value = state.read(cx).value().parse::<i64>().unwrap_or_default();
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

        let mut this = Self {
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
            confirm_tooltip: options.confirm_tooltip,
            tab_selected_index: cx.new(|_cx| 0),
            should_focus: true,
            foot_actions: options.foot_actions,
            add_field_states: Vec::with_capacity(1),
            add_field_placeholder: options.add_field_placeholder,
            add_value_placeholder: options.add_value_placeholder,
            support_add_fields: options.support_add_fields,
            support_add_fields_on: options.support_add_fields_on,
            is_processing: false,
            in_dialog: false,
            _subscriptions: subscriptions,
        };
        if this.support_add_fields {
            this.add_field(window, cx);
        }
        this
    }

    /// Returns `true` if the field should be displayed given the current tab
    /// and RadioGroup state.
    fn is_field_visible(&self, field: &ZedisFormField, active_tab_index: usize, cx: &App) -> bool {
        if let Some(tab_index) = field.tab_index
            && tab_index != active_tab_index
        {
            return false;
        }
        self.is_field_visible_on(field, cx)
    }

    /// Returns `true` if the field should be displayed given the current RadioGroup state.
    fn is_field_visible_on(&self, field: &ZedisFormField, cx: &App) -> bool {
        if let Some((ref radio_name, ref indices)) = field.visible_on {
            let selected = self.radio_group_selected(radio_name, cx);
            if !indices.contains(&selected) {
                return false;
            }
        }
        true
    }

    /// Returns the selected index of a RadioGroup field by name.
    fn radio_group_selected(&self, name: &str, cx: &App) -> usize {
        self.field_states
            .iter()
            .find_map(|(f, s)| {
                if f.name.as_ref() == name
                    && let ZedisFormFieldState::RadioGroup(state) = s
                {
                    return Some(*state.read(cx));
                }
                None
            })
            .unwrap_or(0)
    }

    /// Returns `true` if the dynamic add-fields section should be displayed.
    fn should_show_add_fields(&self, cx: &App) -> bool {
        if !self.support_add_fields {
            return false;
        }
        if let Some((ref radio_name, ref indices)) = self.support_add_fields_on {
            let selected = self.radio_group_selected(radio_name, cx);
            return indices.contains(&selected);
        }
        true
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

    pub fn try_get_values(&mut self, cx: &mut Context<Self>) -> Option<IndexMap<SharedString, SharedString>> {
        self.errors.clear();
        let mut has_errors = false;
        let mut values = IndexMap::new();

        for (field, state) in &self.field_states {
            if !self.is_field_visible_on(field, cx) {
                continue;
            }
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
            values.insert(field.name.clone(), value.into());
        }

        if has_errors {
            cx.notify();
            return None;
        }
        if self.should_show_add_fields(cx) {
            for (field_state, value_state) in &self.add_field_states {
                let field = field_state.read(cx).value();
                let value = value_state.read(cx).value();
                values.insert(field, value);
            }
        }
        Some(values)
    }

    /// Validate all fields, collect their values, and invoke the submit handler.
    /// Runs required-checks first, then custom validators per field.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_processing {
            return;
        }
        let Some(values) = self.try_get_values(cx) else {
            return;
        };
        let Some(on_submit) = &self.on_submit else {
            return;
        };
        if on_submit(values, window, cx) {
            self.is_processing = true;
            cx.notify();
        }
    }
    fn remove_add_field(&mut self, index: usize, cx: &mut Context<Self>) {
        self.add_field_states.remove(index);
        cx.notify();
    }
    fn add_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_field_states.push((
            cx.new(|cx| InputState::new(window, cx).placeholder(self.add_field_placeholder.clone())),
            cx.new(|cx| InputState::new(window, cx).placeholder(self.add_value_placeholder.clone())),
        ));
        cx.notify();
    }

    pub fn reset_form(
        &mut self,
        values: &IndexMap<SharedString, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (field, state) in &self.field_states {
            if let Some(value) = values.get(&field.name) {
                match state {
                    ZedisFormFieldState::Input(state) => {
                        state.update(cx, |state, cx| {
                            state.set_value(value.clone(), window, cx);
                        });
                    }
                    ZedisFormFieldState::RadioGroup(state) => {
                        state.update(cx, |state, _cx| {
                            *state = value.parse::<usize>().unwrap_or(0);
                        });
                    }
                    ZedisFormFieldState::Checkbox(state) => {
                        state.update(cx, |state, _cx| {
                            *state = value == "true";
                        });
                    }
                }
            }
        }
        self.should_focus = true;
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
                        state.set_cursor_position(Position::new(0, u32::MAX), window, cx);
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
        let parent_id = Arc::new(self.id.clone());

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
            if !self.is_field_visible(field, active_tab_index, cx) {
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
                                    .when(field.mask, |this| this.mask_toggle())
                                    .refine_style(&field.style),
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
                    let form_entity = cx.entity().clone();
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
                                    form_entity.update(cx, |_, cx| cx.notify());
                                }),
                        ),
                    );
                }
            }
        }

        let show_add_fields = self.should_show_add_fields(cx);
        if show_add_fields {
            for (index, (field_state, value_state)) in self.add_field_states.iter().enumerate() {
                form_container = form_container.child(
                    field().child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(field_state))
                            .child(Input::new(value_state))
                            .child(
                                Button::new(("remove-add-field", index))
                                    .icon(IconName::CircleX)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_add_field(index, cx);
                                    })),
                            ),
                    ),
                )
            }
        }
        if show_add_fields {
            form_container =
                form_container.child(field().child(h_flex().justify_end().child(
                    Button::new("add-add-field").icon(IconName::Plus).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.add_field(window, cx);
                        },
                    )),
                )));
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
            form_container = form_container
                .child(field().child(Alert::error(alert_id, TextView::markdown(textview_id, error_text))));
        }

        // Build action buttons (cancel on the left, confirm/primary on the right).
        let mut buttons = Vec::with_capacity(2);
        if self.on_cancel.is_some() {
            let button_id = ElementId::NamedChild(parent_id.clone(), "cancel".into());
            buttons.push(
                Button::new(button_id)
                    .label(self.cancel_label.clone())
                    .disabled(self.is_processing)
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
                    .disabled(self.is_processing)
                    .when_some(self.confirm_tooltip.clone(), |this, tooltip| this.tooltip(tooltip))
                    .primary()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit(window, cx);
                    })),
            );
        }

        let buttons = platform_buttons(buttons);
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
            form_container = form_container.child(
                field().child(
                    h_flex()
                        .justify_between()
                        .child(left_buttons)
                        .child(right_buttons)
                        .gap_4(),
                ),
            );
        }

        div().child(form_container).overflow_y_scrollbar()
    }
}
